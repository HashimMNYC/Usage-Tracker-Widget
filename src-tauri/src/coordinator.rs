use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, TryRecvError},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::{
    diagnostics::DiagnosticCode,
    model::ProviderSnapshot,
    providers::codex::{CodexCollector, CollectResult},
    state_store::{PersistedState, StateMutation, StateStore},
};

pub const DEBOUNCE: Duration = Duration::from_millis(500);
pub const FALLBACK_RESCAN: Duration = Duration::from_secs(60);
const WORKER_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CoordinatorError {
    #[error("collector refresh failed")]
    Collect,
    #[error("state update failed")]
    State,
    #[error("filesystem watcher failed")]
    Watch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshAction {
    None,
    Changed(BTreeSet<PathBuf>),
    Full,
}

pub struct RefreshScheduler {
    last_full: Duration,
    last_change: Option<Duration>,
    changed: BTreeSet<PathBuf>,
    force_full: bool,
}

impl RefreshScheduler {
    pub fn new(started_at: Duration) -> Self {
        Self {
            last_full: started_at,
            last_change: None,
            changed: BTreeSet::new(),
            force_full: false,
        }
    }

    pub fn note_change(&mut self, path: PathBuf, now: Duration) {
        if !is_jsonl(&path) {
            return;
        }
        self.changed.insert(path);
        self.last_change = Some(now);
    }

    pub fn note_event(&mut self, event: &Event, now: Duration) {
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }
        for path in &event.paths {
            self.note_change(path.clone(), now);
        }
    }

    pub fn note_watcher_error(&mut self) {
        self.force_full = true;
    }

    pub fn request_full_refresh(&mut self) {
        self.force_full = true;
    }

    pub fn due(&mut self, now: Duration) -> RefreshAction {
        if self.force_full || now.saturating_sub(self.last_full) >= FALLBACK_RESCAN {
            self.force_full = false;
            self.last_full = now;
            self.last_change = None;
            self.changed.clear();
            return RefreshAction::Full;
        }
        if self
            .last_change
            .is_some_and(|changed_at| now.saturating_sub(changed_at) >= DEBOUNCE)
        {
            self.last_change = None;
            return RefreshAction::Changed(std::mem::take(&mut self.changed));
        }
        RefreshAction::None
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

trait CollectorBackend: Send + Sync {
    fn full_rescan(&self, now: i64) -> CollectResult;
    fn refresh_changed(&self, paths: &BTreeSet<PathBuf>, now: i64) -> CollectResult;
}

impl CollectorBackend for CodexCollector {
    fn full_rescan(&self, now: i64) -> CollectResult {
        self.full_rescan(now)
    }

    fn refresh_changed(&self, paths: &BTreeSet<PathBuf>, now: i64) -> CollectResult {
        self.refresh_changed(paths, now)
    }
}

struct CoordinatorCore<C: CollectorBackend> {
    collector: Arc<C>,
    store: Arc<dyn StateStore>,
    current: RwLock<PersistedState>,
}

impl<C: CollectorBackend> CoordinatorCore<C> {
    fn load(
        collector: Arc<C>,
        store: Arc<dyn StateStore>,
        now: i64,
    ) -> Result<Self, CoordinatorError> {
        let current = store.load(now).map_err(|_| CoordinatorError::State)?;
        Ok(Self {
            collector,
            store,
            current: RwLock::new(current),
        })
    }

    fn refresh_now(&self, now: i64) -> Result<(), CoordinatorError> {
        self.apply_collection(self.collector.full_rescan(now), now)
    }

    fn refresh_changed(&self, paths: &BTreeSet<PathBuf>, now: i64) -> Result<(), CoordinatorError> {
        self.apply_collection(self.collector.refresh_changed(paths, now), now)
    }

    fn apply_collection(&self, result: CollectResult, now: i64) -> Result<(), CoordinatorError> {
        let Some(snapshot) = result.snapshot else {
            return Ok(());
        };
        snapshot
            .validate(now)
            .map_err(|_| CoordinatorError::Collect)?;

        let mut current = self.current.write().map_err(|_| CoordinatorError::State)?;
        let stored = self
            .store
            .apply(now, StateMutation::UpsertSnapshot(snapshot))
            .map_err(|_| CoordinatorError::State)?;
        *current = stored;
        Ok(())
    }

    fn current_snapshots(&self, now: i64) -> Vec<ProviderSnapshot> {
        self.current
            .read()
            .map(|state| state.current_snapshots(now).into_values().collect())
            .unwrap_or_default()
    }
}

pub struct CollectionCoordinator {
    inner: CoordinatorCore<CodexCollector>,
}

impl CollectionCoordinator {
    pub fn load(
        collector: Arc<CodexCollector>,
        store: Arc<dyn StateStore>,
        now: i64,
    ) -> Result<Self, CoordinatorError> {
        Ok(Self {
            inner: CoordinatorCore::load(collector, store, now)?,
        })
    }

    pub fn refresh_now(&self, now: i64) -> Result<(), CoordinatorError> {
        self.inner.refresh_now(now)
    }

    pub fn refresh_changed(
        &self,
        paths: &BTreeSet<PathBuf>,
        now: i64,
    ) -> Result<(), CoordinatorError> {
        self.inner.refresh_changed(paths, now)
    }

    pub fn current_snapshots(&self, now: i64) -> Vec<ProviderSnapshot> {
        self.inner.current_snapshots(now)
    }

    fn roots(&self) -> &[PathBuf] {
        self.inner.collector.roots()
    }
}

enum WatchSignal {
    Event(Event),
    Error(DiagnosticCode),
}

pub struct CollectorSupervisor {
    stop: Option<mpsc::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

pub fn start_supervisor(
    coordinator: Arc<CollectionCoordinator>,
) -> Result<CollectorSupervisor, CoordinatorError> {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let signal = match result {
            Ok(event) => WatchSignal::Event(event),
            Err(error) => WatchSignal::Error(watch_error_code(error)),
        };
        let _ = event_tx.send(signal);
    })
    .map_err(|_| CoordinatorError::Watch)?;

    let roots = coordinator.roots().to_vec();
    let mut watched = BTreeMap::new();
    let force_full = reconcile_watches(&mut watcher, &roots, &mut watched);

    let (stop_tx, stop_rx) = mpsc::channel();
    let join = thread::spawn(move || {
        let started = Instant::now();
        let mut scheduler = RefreshScheduler::new(Duration::ZERO);
        if force_full {
            scheduler.note_watcher_error();
        }

        loop {
            match stop_rx.recv_timeout(WORKER_POLL) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let elapsed = started.elapsed();
            loop {
                match event_rx.try_recv() {
                    Ok(WatchSignal::Event(event)) => scheduler.note_event(&event, elapsed),
                    Ok(WatchSignal::Error(_code)) => scheduler.note_watcher_error(),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        scheduler.note_watcher_error();
                        break;
                    }
                }
            }

            let now = unix_now();
            let _ = match scheduler.due(elapsed) {
                RefreshAction::None => Ok(()),
                RefreshAction::Changed(paths) => coordinator.refresh_changed(&paths, now),
                RefreshAction::Full => {
                    let result = coordinator.refresh_now(now);
                    let _ = reconcile_watches(&mut watcher, &roots, &mut watched);
                    result
                }
            };
        }
    });

    Ok(CollectorSupervisor {
        stop: Some(stop_tx),
        join: Some(join),
    })
}

impl CollectorSupervisor {
    pub fn stop_and_join(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for CollectorSupervisor {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn nearest_watch(target: &Path) -> Option<(PathBuf, RecursiveMode)> {
    if target.is_dir() {
        return Some((target.to_path_buf(), RecursiveMode::Recursive));
    }
    target
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.is_dir())
        .map(|ancestor| (ancestor.to_path_buf(), RecursiveMode::NonRecursive))
}

fn watch_plan(roots: &[PathBuf]) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut plan = BTreeMap::new();
    for root in roots {
        let Some((path, mode)) = nearest_watch(root) else {
            continue;
        };
        plan.entry(path)
            .and_modify(|existing| {
                if mode == RecursiveMode::Recursive {
                    *existing = mode;
                }
            })
            .or_insert(mode);
    }
    plan
}

fn reconcile_watches(
    watcher: &mut notify::RecommendedWatcher,
    roots: &[PathBuf],
    watched: &mut BTreeMap<PathBuf, RecursiveMode>,
) -> bool {
    let desired = watch_plan(roots);
    let stale = watched
        .iter()
        .filter(|(path, mode)| desired.get(*path) != Some(*mode))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let mut failed = roots
        .iter()
        .any(|root| (root.exists() && !root.is_dir()) || nearest_watch(root).is_none());

    for path in stale {
        if watcher.unwatch(&path).is_err() {
            failed = true;
        }
        watched.remove(&path);
    }
    for (path, mode) in desired {
        if watched.get(&path) == Some(&mode) {
            continue;
        }
        if watcher.watch(&path, mode).is_ok() {
            watched.insert(path, mode);
        } else {
            failed = true;
        }
    }
    failed
}

fn watch_error_code(error: notify::Error) -> DiagnosticCode {
    if matches!(error.kind, notify::ErrorKind::MaxFilesWatch) {
        DiagnosticCode::WatcherOverflow
    } else {
        DiagnosticCode::WatcherUnavailable
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{
        model::{ProviderId, WindowSnapshot},
        state_store::{StateError, StateMutation},
    };

    struct FakeCollector {
        result: Mutex<CollectResult>,
    }

    impl CollectorBackend for FakeCollector {
        fn full_rescan(&self, _now: i64) -> CollectResult {
            self.result.lock().unwrap().clone()
        }

        fn refresh_changed(&self, _paths: &BTreeSet<PathBuf>, _now: i64) -> CollectResult {
            self.result.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct FakeStore {
        state: Mutex<PersistedState>,
    }

    impl StateStore for FakeStore {
        fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
            let mut state = self.state.lock().unwrap();
            state.apply_mutation(mutation, now)?;
            Ok(state.clone())
        }
    }

    #[test]
    fn invalid_fake_collection_does_not_replace_current_state() {
        const NOW: i64 = 2_000_000_000;
        let current = valid_snapshot(NOW - 20, NOW + 100);
        let mut initial = PersistedState::default();
        initial.snapshots.insert(ProviderId::Codex, current.clone());
        let store = Arc::new(FakeStore {
            state: Mutex::new(initial),
        });
        let mut invalid = valid_snapshot(NOW - 10, NOW + 200);
        invalid.short_window.used_percent = 101.0;
        let collector = Arc::new(FakeCollector {
            result: Mutex::new(CollectResult {
                snapshot: Some(invalid),
                diagnostic: None,
            }),
        });
        let coordinator = CoordinatorCore::load(collector, store.clone(), NOW).unwrap();

        assert_eq!(coordinator.refresh_now(NOW), Err(CoordinatorError::Collect));
        assert_eq!(coordinator.current_snapshots(NOW), vec![current.clone()]);
        assert_eq!(
            store.state.lock().unwrap().snapshots[&ProviderId::Codex],
            current
        );
    }

    #[test]
    fn missing_target_uses_nearest_existing_parent_non_recursively() {
        let temp = tempfile::TempDir::new().unwrap();
        let target = temp.path().join("missing").join("sessions");

        assert_eq!(
            nearest_watch(&target),
            Some((temp.path().to_path_buf(), RecursiveMode::NonRecursive))
        );
    }

    #[test]
    fn watch_plan_promotes_a_recovered_target_to_recursive() {
        let temp = tempfile::TempDir::new().unwrap();
        let target = temp.path().join("sessions");
        let roots = vec![target.clone()];

        assert_eq!(
            watch_plan(&roots).get(temp.path()),
            Some(&RecursiveMode::NonRecursive)
        );
        std::fs::create_dir(&target).unwrap();
        assert_eq!(
            watch_plan(&roots).get(&target),
            Some(&RecursiveMode::Recursive)
        );
        assert!(!watch_plan(&roots).contains_key(temp.path()));
    }

    fn valid_snapshot(observed_at: i64, resets_at: i64) -> ProviderSnapshot {
        ProviderSnapshot {
            provider: ProviderId::Codex,
            observed_at,
            short_window: WindowSnapshot {
                duration_minutes: 300,
                used_percent: 20.0,
                resets_at,
            },
            weekly_window: WindowSnapshot {
                duration_minutes: 10_080,
                used_percent: 30.0,
                resets_at,
            },
        }
    }
}
