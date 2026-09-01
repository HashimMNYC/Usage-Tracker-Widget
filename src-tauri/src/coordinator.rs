use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError, TrySendError},
        Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
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
const EVENT_QUEUE_CAPACITY: usize = 256;
const MAX_EVENTS_PER_TICK: usize = 64;

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

#[derive(Default)]
struct DiagnosticState {
    codes: Mutex<Vec<DiagnosticCode>>,
}

impl DiagnosticState {
    fn record(&self, code: DiagnosticCode) {
        let mut codes = lock_unpoisoned(&self.codes);
        if !codes.contains(&code) {
            codes.push(code);
        }
    }

    fn snapshot(&self) -> Vec<DiagnosticCode> {
        lock_unpoisoned(&self.codes).clone()
    }
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
    update_gate: Mutex<()>,
    current: RwLock<PersistedState>,
    diagnostics: DiagnosticState,
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
            update_gate: Mutex::new(()),
            current: RwLock::new(current),
            diagnostics: DiagnosticState::default(),
        })
    }

    fn refresh_now(&self, now: i64) -> Result<(), CoordinatorError> {
        self.apply_collection(self.collector.full_rescan(now), now)
    }

    fn refresh_changed(&self, paths: &BTreeSet<PathBuf>, now: i64) -> Result<(), CoordinatorError> {
        self.apply_collection(self.collector.refresh_changed(paths, now), now)
    }

    fn apply_collection(&self, result: CollectResult, now: i64) -> Result<(), CoordinatorError> {
        let diagnostic = result.diagnostic;
        let Some(snapshot) = result.snapshot else {
            if let Some(code) = diagnostic {
                self.diagnostics.record(code);
            }
            return Ok(());
        };
        if snapshot.validate(now).is_err() {
            self.diagnostics
                .record(diagnostic.unwrap_or(DiagnosticCode::InvalidSchema));
            return Err(CoordinatorError::Collect);
        }

        let _gate = lock_unpoisoned(&self.update_gate);
        let stored = match self
            .store
            .apply(now, StateMutation::UpsertSnapshot(snapshot))
        {
            Ok(stored) => stored,
            Err(_) => {
                self.diagnostics.record(DiagnosticCode::StateWriteFailed);
                return Err(CoordinatorError::State);
            }
        };
        *write_unpoisoned(&self.current) = stored;
        Ok(())
    }

    fn current_snapshots(&self, now: i64) -> Vec<ProviderSnapshot> {
        let _gate = lock_unpoisoned(&self.update_gate);
        match self.store.load(now) {
            Ok(stored) => *write_unpoisoned(&self.current) = stored,
            Err(_) => self.diagnostics.record(DiagnosticCode::CorruptState),
        }
        read_unpoisoned(&self.current)
            .current_snapshots(now)
            .into_values()
            .collect()
    }

    fn diagnostics(&self) -> Vec<DiagnosticCode> {
        self.diagnostics.snapshot()
    }
}

fn lock_unpoisoned<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    pub fn diagnostics(&self) -> Vec<DiagnosticCode> {
        self.inner.diagnostics()
    }

    fn roots(&self) -> &[PathBuf] {
        self.inner.collector.roots()
    }

    fn record_diagnostic(&self, code: DiagnosticCode) {
        self.inner.diagnostics.record(code);
    }
}

enum WatchSignal {
    Event(Event),
    Error(DiagnosticCode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkerStep {
    Stop,
    Action(RefreshAction),
}

pub struct CollectorSupervisor {
    stop: Option<mpsc::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

pub fn start_supervisor(
    coordinator: Arc<CollectionCoordinator>,
) -> Result<CollectorSupervisor, CoordinatorError> {
    let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let overflowed = Arc::new(AtomicBool::new(false));
    let callback_overflowed = overflowed.clone();
    let callback_coordinator = coordinator.clone();
    let watcher = notify::recommended_watcher(move |result| {
        let signal = match result {
            Ok(event) => WatchSignal::Event(event),
            Err(error) => WatchSignal::Error(watch_error_code(error)),
        };
        enqueue_watch_signal(
            &event_tx,
            signal,
            &callback_overflowed,
            &callback_coordinator.inner.diagnostics,
        );
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(_) => {
            coordinator.record_diagnostic(DiagnosticCode::WatcherUnavailable);
            return Err(CoordinatorError::Watch);
        }
    };

    let roots = coordinator.roots().to_vec();
    let mut watched = BTreeMap::new();
    let force_full = reconcile_watches(&mut watcher, &roots, &mut watched);
    if force_full {
        coordinator.record_diagnostic(DiagnosticCode::WatcherUnavailable);
    }

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
            let action = match worker_step(
                &stop_rx,
                &event_rx,
                &mut scheduler,
                started.elapsed(),
                &overflowed,
                &coordinator.inner.diagnostics,
            ) {
                WorkerStep::Stop => break,
                WorkerStep::Action(action) => action,
            };
            let now = unix_now();
            let _ = match action {
                RefreshAction::None => Ok(()),
                RefreshAction::Changed(paths) => coordinator.refresh_changed(&paths, now),
                RefreshAction::Full => {
                    let result = coordinator.refresh_now(now);
                    if reconcile_watches(&mut watcher, &roots, &mut watched) {
                        coordinator.record_diagnostic(DiagnosticCode::WatcherUnavailable);
                    }
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

fn enqueue_watch_signal(
    event_tx: &mpsc::SyncSender<WatchSignal>,
    signal: WatchSignal,
    overflowed: &AtomicBool,
    diagnostics: &DiagnosticState,
) {
    if let WatchSignal::Error(code) = &signal {
        diagnostics.record(*code);
    }
    match event_tx.try_send(signal) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            overflowed.store(true, Ordering::Release);
            diagnostics.record(DiagnosticCode::WatcherOverflow);
        }
        Err(TrySendError::Disconnected(_)) => {
            diagnostics.record(DiagnosticCode::WatcherUnavailable);
        }
    }
}

fn worker_step(
    stop_rx: &mpsc::Receiver<()>,
    event_rx: &mpsc::Receiver<WatchSignal>,
    scheduler: &mut RefreshScheduler,
    elapsed: Duration,
    overflowed: &AtomicBool,
    diagnostics: &DiagnosticState,
) -> WorkerStep {
    match stop_rx.try_recv() {
        Ok(()) | Err(TryRecvError::Disconnected) => return WorkerStep::Stop,
        Err(TryRecvError::Empty) => {}
    }
    if overflowed.swap(false, Ordering::AcqRel) {
        diagnostics.record(DiagnosticCode::WatcherOverflow);
        scheduler.note_watcher_error();
    }
    for _ in 0..MAX_EVENTS_PER_TICK {
        match event_rx.try_recv() {
            Ok(WatchSignal::Event(event)) => scheduler.note_event(&event, elapsed),
            Ok(WatchSignal::Error(code)) => {
                diagnostics.record(code);
                scheduler.note_watcher_error();
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                diagnostics.record(DiagnosticCode::WatcherUnavailable);
                scheduler.note_watcher_error();
                break;
            }
        }
    }
    match stop_rx.try_recv() {
        Ok(()) | Err(TryRecvError::Disconnected) => WorkerStep::Stop,
        Err(TryRecvError::Empty) => WorkerStep::Action(scheduler.due(elapsed)),
    }
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
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };

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

    struct ControlledStore {
        state: Mutex<PersistedState>,
        fail_load: AtomicBool,
        fail_apply: AtomicBool,
    }

    impl StateStore for ControlledStore {
        fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
            if self.fail_load.load(Ordering::SeqCst) {
                return Err(StateError::Io);
            }
            Ok(self.state.lock().unwrap().clone())
        }

        fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
            if self.fail_apply.load(Ordering::SeqCst) {
                return Err(StateError::Io);
            }
            let mut state = self.state.lock().unwrap();
            state.apply_mutation(mutation, now)?;
            Ok(state.clone())
        }
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

    struct PanickingOnceStore {
        state: Mutex<PersistedState>,
        panic_next_apply: AtomicBool,
    }

    impl StateStore for PanickingOnceStore {
        fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
            if self.panic_next_apply.swap(false, Ordering::SeqCst) {
                panic!("injected apply panic");
            }
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
        invalid.short_window.as_mut().unwrap().used_percent = 101.0;
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
        assert!(coordinator
            .diagnostics()
            .contains(&DiagnosticCode::InvalidSchema));
    }

    #[test]
    fn load_failure_preserves_cache_and_records_corrupt_state() {
        const NOW: i64 = 2_000_000_000;
        let current = valid_snapshot(NOW - 20, NOW + 100);
        let mut initial = PersistedState::default();
        initial.snapshots.insert(ProviderId::Codex, current.clone());
        let store = Arc::new(ControlledStore {
            state: Mutex::new(initial),
            fail_load: AtomicBool::new(false),
            fail_apply: AtomicBool::new(false),
        });
        let coordinator = CoordinatorCore::load(
            Arc::new(FakeCollector {
                result: Mutex::new(CollectResult {
                    snapshot: None,
                    diagnostic: Some(DiagnosticCode::NoFiles),
                }),
            }),
            store.clone(),
            NOW,
        )
        .unwrap();
        store.fail_load.store(true, Ordering::SeqCst);

        assert_eq!(coordinator.current_snapshots(NOW), vec![current]);
        assert!(coordinator
            .diagnostics()
            .contains(&DiagnosticCode::CorruptState));
    }

    #[test]
    fn failed_refresh_records_state_write_code() {
        const NOW: i64 = 2_000_000_000;
        let store = Arc::new(ControlledStore {
            state: Mutex::new(PersistedState::default()),
            fail_load: AtomicBool::new(false),
            fail_apply: AtomicBool::new(true),
        });
        let collector = Arc::new(FakeCollector {
            result: Mutex::new(CollectResult {
                snapshot: Some(valid_snapshot(NOW - 10, NOW + 100)),
                diagnostic: None,
            }),
        });
        let coordinator = CoordinatorCore::load(collector, store, NOW).unwrap();

        assert_eq!(coordinator.refresh_now(NOW), Err(CoordinatorError::State));
        assert!(coordinator
            .diagnostics()
            .contains(&DiagnosticCode::StateWriteFailed));
    }

    #[test]
    fn empty_collection_records_the_collectors_fixed_diagnostic() {
        const NOW: i64 = 2_000_000_000;
        let coordinator = CoordinatorCore::load(
            Arc::new(FakeCollector {
                result: Mutex::new(CollectResult {
                    snapshot: None,
                    diagnostic: Some(DiagnosticCode::SourceUnreadable),
                }),
            }),
            Arc::new(FakeStore::default()),
            NOW,
        )
        .unwrap();

        coordinator.refresh_now(NOW).unwrap();

        assert!(coordinator
            .diagnostics()
            .contains(&DiagnosticCode::SourceUnreadable));
    }

    #[test]
    fn notify_error_overflow_and_disconnect_record_safe_codes() {
        let diagnostics = DiagnosticState::default();
        let overflowed = AtomicBool::new(false);
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        enqueue_watch_signal(
            &event_tx,
            WatchSignal::Error(DiagnosticCode::WatcherUnavailable),
            &overflowed,
            &diagnostics,
        );
        enqueue_watch_signal(
            &event_tx,
            WatchSignal::Event(Event::new(EventKind::Any)),
            &overflowed,
            &diagnostics,
        );
        assert!(overflowed.load(Ordering::Acquire));
        assert!(diagnostics
            .snapshot()
            .contains(&DiagnosticCode::WatcherOverflow));
        let (_stop_tx, stop_rx) = mpsc::channel();
        let mut scheduler = RefreshScheduler::new(Duration::ZERO);
        assert_eq!(
            worker_step(
                &stop_rx,
                &event_rx,
                &mut scheduler,
                Duration::ZERO,
                &overflowed,
                &diagnostics,
            ),
            WorkerStep::Action(RefreshAction::Full)
        );
        assert!(diagnostics
            .snapshot()
            .contains(&DiagnosticCode::WatcherUnavailable));

        drop(event_rx);
        enqueue_watch_signal(
            &event_tx,
            WatchSignal::Event(Event::new(EventKind::Any)),
            &overflowed,
            &diagnostics,
        );
        assert!(diagnostics
            .snapshot()
            .contains(&DiagnosticCode::WatcherUnavailable));
    }

    #[test]
    fn notify_errors_map_to_fixed_codes_without_formatting_details() {
        assert_eq!(
            watch_error_code(notify::Error::new(notify::ErrorKind::MaxFilesWatch)),
            DiagnosticCode::WatcherOverflow
        );
        assert_eq!(
            watch_error_code(notify::Error::generic(
                "private path and raw watcher detail"
            )),
            DiagnosticCode::WatcherUnavailable
        );
    }

    #[test]
    fn apply_panic_preserves_last_current_and_later_refresh_recovers() {
        const NOW: i64 = 2_000_000_000;
        let current = valid_snapshot(NOW - 20, NOW + 100);
        let replacement = valid_snapshot(NOW - 10, NOW + 200);
        let mut initial = PersistedState::default();
        initial.snapshots.insert(ProviderId::Codex, current.clone());
        let store = Arc::new(PanickingOnceStore {
            state: Mutex::new(initial),
            panic_next_apply: AtomicBool::new(true),
        });
        let collector = Arc::new(FakeCollector {
            result: Mutex::new(CollectResult {
                snapshot: Some(replacement.clone()),
                diagnostic: None,
            }),
        });
        let coordinator = CoordinatorCore::load(collector, store, NOW).unwrap();

        let panic = catch_unwind(AssertUnwindSafe(|| coordinator.refresh_now(NOW)));

        assert!(panic.is_err());
        assert_eq!(coordinator.current_snapshots(NOW), vec![current]);
        coordinator.refresh_now(NOW).unwrap();
        assert_eq!(coordinator.current_snapshots(NOW), vec![replacement]);
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

    #[test]
    fn bounded_worker_step_runs_fallback_while_notify_traffic_remains() {
        let (event_tx, event_rx) = mpsc::sync_channel(MAX_EVENTS_PER_TICK + 1);
        for index in 0..=MAX_EVENTS_PER_TICK {
            event_tx
                .send(WatchSignal::Event(
                    Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                        .add_path(PathBuf::from(format!("{index}.jsonl"))),
                ))
                .unwrap();
        }
        let (_stop_tx, stop_rx) = mpsc::channel();
        let mut scheduler = RefreshScheduler::new(Duration::ZERO);
        let overflowed = AtomicBool::new(false);
        let diagnostics = DiagnosticState::default();

        assert_eq!(
            worker_step(
                &stop_rx,
                &event_rx,
                &mut scheduler,
                FALLBACK_RESCAN,
                &overflowed,
                &diagnostics,
            ),
            WorkerStep::Action(RefreshAction::Full)
        );
        assert!(event_rx.try_recv().is_ok(), "one event must remain queued");
    }

    #[test]
    fn pending_stop_wins_before_a_bounded_notify_drain() {
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        event_tx
            .send(WatchSignal::Event(
                Event::new(EventKind::Create(notify::event::CreateKind::File))
                    .add_path(PathBuf::from("pending.jsonl")),
            ))
            .unwrap();
        let (stop_tx, stop_rx) = mpsc::channel();
        stop_tx.send(()).unwrap();
        let mut scheduler = RefreshScheduler::new(Duration::ZERO);
        let overflowed = AtomicBool::new(false);
        let diagnostics = DiagnosticState::default();

        assert_eq!(
            worker_step(
                &stop_rx,
                &event_rx,
                &mut scheduler,
                Duration::ZERO,
                &overflowed,
                &diagnostics,
            ),
            WorkerStep::Stop
        );
        assert!(event_rx.try_recv().is_ok(), "stop must preempt event work");
    }

    #[test]
    fn saturated_ingress_does_not_block_supervisor_stop_and_join() {
        let (event_tx, event_rx) = mpsc::sync_channel(MAX_EVENTS_PER_TICK);
        for index in 0..MAX_EVENTS_PER_TICK {
            event_tx
                .send(WatchSignal::Event(
                    Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                        .add_path(PathBuf::from(format!("{index}.jsonl"))),
                ))
                .unwrap();
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let mut scheduler = RefreshScheduler::new(Duration::ZERO);
            let overflowed = AtomicBool::new(false);
            let diagnostics = DiagnosticState::default();
            loop {
                match stop_rx.recv_timeout(WORKER_POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if worker_step(
                    &stop_rx,
                    &event_rx,
                    &mut scheduler,
                    Duration::ZERO,
                    &overflowed,
                    &diagnostics,
                ) == WorkerStep::Stop
                {
                    break;
                }
            }
        });
        let mut supervisor = CollectorSupervisor {
            stop: Some(stop_tx),
            join: Some(join),
        };

        supervisor.stop_and_join();
        assert!(supervisor.join.is_none());
    }

    fn valid_snapshot(observed_at: i64, resets_at: i64) -> ProviderSnapshot {
        ProviderSnapshot {
            provider: ProviderId::Codex,
            observed_at,
            short_window: Some(WindowSnapshot {
                duration_minutes: 300,
                used_percent: 20.0,
                resets_at,
            }),
            weekly_window: Some(WindowSnapshot {
                duration_minutes: 10_080,
                used_percent: 30.0,
                resets_at,
            }),
        }
    }
}
