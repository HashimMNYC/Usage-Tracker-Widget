use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use notify::{
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
    Event, EventKind,
};
use tempfile::TempDir;
use usage_widget::{
    coordinator::{
        start_supervisor, CollectionCoordinator, RefreshAction, RefreshScheduler, DEBOUNCE,
        FALLBACK_RESCAN,
    },
    model::{ProviderId, ProviderSnapshot, WindowSnapshot},
    providers::codex::CodexCollector,
    state_store::{PersistedState, StateError, StateMutation, StateStore},
};

const NOW: i64 = 2_000_000_000;

fn snapshot(observed_at: i64, short_reset: i64, weekly_reset: i64) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at,
        short_window: WindowSnapshot {
            duration_minutes: 300,
            used_percent: 40.0,
            resets_at: short_reset,
        },
        weekly_window: WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 60.0,
            resets_at: weekly_reset,
        },
    }
}

fn write_snapshot(path: &std::path::Path, value: &ProviderSnapshot) {
    let record = serde_json::json!({
        "timestamp": value.observed_at,
        "payload": {
            "rate_limits": {
                "primary": {
                    "used_percent": value.short_window.used_percent,
                    "window_minutes": value.short_window.duration_minutes,
                    "resets_at": value.short_window.resets_at
                },
                "secondary": {
                    "used_percent": value.weekly_window.used_percent,
                    "window_minutes": value.weekly_window.duration_minutes,
                    "resets_at": value.weekly_window.resets_at
                }
            }
        }
    });
    fs::write(path, format!("{record}\n")).unwrap();
}

#[derive(Default)]
struct MemoryStore {
    state: Mutex<PersistedState>,
    fail_apply: bool,
}

impl MemoryStore {
    fn containing(value: ProviderSnapshot) -> Self {
        let mut state = PersistedState::default();
        state.snapshots.insert(value.provider, value);
        Self {
            state: Mutex::new(state),
            fail_apply: false,
        }
    }

    fn failing() -> Self {
        Self {
            fail_apply: true,
            ..Self::default()
        }
    }
}

impl StateStore for MemoryStore {
    fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
        Ok(self.state.lock().unwrap().clone())
    }

    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
        if self.fail_apply {
            return Err(StateError::Io);
        }
        let mut state = self.state.lock().unwrap();
        state.apply_mutation(mutation, now)?;
        Ok(state.clone())
    }
}

#[test]
fn scheduler_uses_exact_debounce_and_fallback_boundaries() {
    let mut scheduler = RefreshScheduler::new(Duration::ZERO);
    scheduler.note_change(PathBuf::from("a.jsonl"), Duration::from_millis(10));

    assert_eq!(DEBOUNCE, Duration::from_millis(500));
    assert_eq!(FALLBACK_RESCAN, Duration::from_secs(60));
    assert_eq!(
        scheduler.due(Duration::from_millis(509)),
        RefreshAction::None
    );
    assert_eq!(
        scheduler.due(Duration::from_millis(510)),
        RefreshAction::Changed(BTreeSet::from([PathBuf::from("a.jsonl")]))
    );
    assert_eq!(scheduler.due(Duration::from_secs(60)), RefreshAction::Full);
}

#[test]
fn scheduler_extends_debounce_and_coalesces_only_jsonl_paths() {
    let mut scheduler = RefreshScheduler::new(Duration::ZERO);
    scheduler.note_change(PathBuf::from("first.jsonl"), Duration::from_millis(10));
    scheduler.note_change(PathBuf::from("ignored.txt"), Duration::from_millis(200));
    scheduler.note_change(PathBuf::from("second.jsonl"), Duration::from_millis(300));

    assert_eq!(
        scheduler.due(Duration::from_millis(510)),
        RefreshAction::None
    );
    assert_eq!(
        scheduler.due(Duration::from_millis(799)),
        RefreshAction::None
    );
    assert_eq!(
        scheduler.due(Duration::from_millis(800)),
        RefreshAction::Changed(BTreeSet::from([
            PathBuf::from("first.jsonl"),
            PathBuf::from("second.jsonl"),
        ]))
    );
}

#[test]
fn scheduler_accepts_create_modify_rename_and_remove_events() {
    let mut scheduler = RefreshScheduler::new(Duration::ZERO);
    let events = [
        Event::new(EventKind::Create(CreateKind::File)).add_path("created.jsonl".into()),
        Event::new(EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content,
        )))
        .add_path("modified.jsonl".into()),
        Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("renamed-from.jsonl".into())
            .add_path("renamed-to.jsonl".into()),
        Event::new(EventKind::Remove(RemoveKind::File)).add_path("removed.jsonl".into()),
        Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path("ignored.jsonl".into()),
    ];
    for event in &events {
        scheduler.note_event(event, Duration::from_millis(10));
    }

    assert_eq!(
        scheduler.due(Duration::from_millis(510)),
        RefreshAction::Changed(BTreeSet::from([
            PathBuf::from("created.jsonl"),
            PathBuf::from("modified.jsonl"),
            PathBuf::from("removed.jsonl"),
            PathBuf::from("renamed-from.jsonl"),
            PathBuf::from("renamed-to.jsonl"),
        ]))
    );
}

#[test]
fn watcher_errors_and_manual_refresh_force_full_scans() {
    let mut watcher_error = RefreshScheduler::new(Duration::ZERO);
    watcher_error.note_watcher_error();
    assert_eq!(watcher_error.due(Duration::ZERO), RefreshAction::Full);

    let mut manual = RefreshScheduler::new(Duration::ZERO);
    manual.request_full_refresh();
    assert_eq!(manual.due(Duration::ZERO), RefreshAction::Full);
}

#[test]
fn coordinator_refreshes_full_and_changed_candidates_transactionally() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let path = sessions.join("rollout.jsonl");
    let initial = snapshot(NOW - 20, NOW + 3_600, NOW + 86_400);
    write_snapshot(&path, &initial);
    let store = Arc::new(MemoryStore::default());
    let coordinator = CollectionCoordinator::load(
        Arc::new(CodexCollector::new(vec![sessions])),
        store.clone(),
        NOW,
    )
    .unwrap();

    coordinator.refresh_now(NOW).unwrap();
    assert_eq!(coordinator.current_snapshots(NOW), vec![initial]);

    let changed = snapshot(NOW - 10, NOW + 7_200, NOW + 172_800);
    write_snapshot(&path, &changed);
    coordinator
        .refresh_changed(&BTreeSet::from([path]), NOW)
        .unwrap();
    assert_eq!(coordinator.current_snapshots(NOW), vec![changed.clone()]);
    assert_eq!(
        store.state.lock().unwrap().snapshots[&ProviderId::Codex],
        changed
    );
}

#[test]
fn state_failure_does_not_replace_the_in_memory_snapshot() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    write_snapshot(
        &sessions.join("rollout.jsonl"),
        &snapshot(NOW - 10, NOW + 3_600, NOW + 86_400),
    );
    let coordinator = CollectionCoordinator::load(
        Arc::new(CodexCollector::new(vec![sessions])),
        Arc::new(MemoryStore::failing()),
        NOW,
    )
    .unwrap();

    assert!(coordinator.refresh_now(NOW).is_err());
    assert!(coordinator.current_snapshots(NOW).is_empty());
}

#[test]
fn every_projection_hides_snapshots_as_soon_as_they_expire() {
    let current = snapshot(NOW - 10, NOW + 10, NOW + 20);
    let coordinator = CollectionCoordinator::load(
        Arc::new(CodexCollector::new(Vec::new())),
        Arc::new(MemoryStore::containing(current.clone())),
        NOW,
    )
    .unwrap();

    assert_eq!(coordinator.current_snapshots(NOW), vec![current]);
    assert!(coordinator.current_snapshots(NOW + 10).is_empty());
}

#[test]
fn supervisor_stop_is_idempotent_and_joins_the_worker() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    let archived = temp.path().join("archived_sessions");
    fs::create_dir(&sessions).unwrap();
    fs::create_dir(&archived).unwrap();
    let coordinator = Arc::new(
        CollectionCoordinator::load(
            Arc::new(CodexCollector::new(vec![sessions, archived])),
            Arc::new(MemoryStore::default()),
            NOW,
        )
        .unwrap(),
    );

    let mut supervisor = start_supervisor(coordinator).unwrap();
    supervisor.stop_and_join();
    supervisor.stop_and_join();
}
