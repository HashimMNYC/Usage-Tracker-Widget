use std::{
    ffi::OsString,
    fs,
    os::windows::ffi::OsStringExt,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use serde_json::json;
use tempfile::TempDir;
use usage_widget::{
    model::{ProviderId, ProviderSnapshot, WindowSnapshot},
    state_store::{
        default_state_path, state_mutex_identity, AtomicReplace, ClaudeTrackingIdentity,
        JsonStateStore, PersistedState, StartupIdentity, StateError, StateMutation, StateStore,
        WindowPlacement, STATE_SCHEMA_VERSION,
    },
};

const NOW: i64 = 2_000_000_000;

fn valid_snapshot() -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at: NOW - 10,
        short_window: Some(WindowSnapshot {
            duration_minutes: 300,
            used_percent: 38.4,
            resets_at: NOW + 3_600,
        }),
        weekly_window: Some(WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 62.0,
            resets_at: NOW + 86_400,
        }),
    }
}

fn valid_claude_snapshot() -> ProviderSnapshot {
    let mut snapshot = valid_snapshot();
    snapshot.provider = ProviderId::Claude;
    snapshot
}

#[test]
fn persists_and_loads_a_valid_snapshot() {
    let temp = TempDir::new().unwrap();
    let store = JsonStateStore::new(temp.path().join("state.json"));

    store
        .apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot()))
        .unwrap();

    assert_eq!(
        store.load(NOW).unwrap().snapshots[&ProviderId::Codex],
        valid_snapshot()
    );
}

#[test]
fn a_delayed_older_writer_cannot_replace_a_newer_provider_snapshot() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let mut older = valid_snapshot();
    older.observed_at = NOW - 20;
    older.short_window.as_mut().unwrap().used_percent = 20.0;
    let mut newer = valid_snapshot();
    newer.observed_at = NOW - 5;
    newer.short_window.as_mut().unwrap().used_percent = 55.0;
    let (newer_committed_tx, newer_committed_rx) = mpsc::channel();
    let older_path = path.clone();
    let older_writer = thread::spawn(move || {
        newer_committed_rx.recv().unwrap();
        JsonStateStore::new(older_path)
            .apply(NOW, StateMutation::UpsertSnapshot(older))
            .unwrap()
    });

    JsonStateStore::new(path.clone())
        .apply(NOW, StateMutation::UpsertSnapshot(newer.clone()))
        .unwrap();
    newer_committed_tx.send(()).unwrap();
    let returned_to_older_writer = older_writer.join().unwrap();

    assert_eq!(
        returned_to_older_writer.snapshots[&ProviderId::Codex],
        newer
    );
    assert_eq!(
        JsonStateStore::new(path).load(NOW).unwrap().snapshots[&ProviderId::Codex],
        newer
    );
}

#[test]
fn a_delayed_equal_timestamp_writer_keeps_the_first_provider_snapshot() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let mut first = valid_snapshot();
    first.short_window.as_mut().unwrap().used_percent = 41.0;
    let mut delayed = valid_snapshot();
    delayed.short_window.as_mut().unwrap().used_percent = 77.0;
    let (first_committed_tx, first_committed_rx) = mpsc::channel();
    let delayed_path = path.clone();
    let delayed_writer = thread::spawn(move || {
        first_committed_rx.recv().unwrap();
        JsonStateStore::new(delayed_path)
            .apply(NOW, StateMutation::UpsertSnapshot(delayed))
            .unwrap()
    });

    JsonStateStore::new(path.clone())
        .apply(NOW, StateMutation::UpsertSnapshot(first.clone()))
        .unwrap();
    first_committed_tx.send(()).unwrap();
    let returned_to_delayed_writer = delayed_writer.join().unwrap();

    assert_eq!(
        returned_to_delayed_writer.snapshots[&ProviderId::Codex],
        first
    );
    assert_eq!(
        JsonStateStore::new(path).load(NOW).unwrap().snapshots[&ProviderId::Codex],
        first
    );
}

#[test]
fn rejects_an_invalid_candidate_without_replacing_current_state() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let store = JsonStateStore::new(path.clone());
    store
        .apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot()))
        .unwrap();
    let original_bytes = fs::read(&path).unwrap();
    let mut invalid = valid_snapshot();
    invalid.short_window.as_mut().unwrap().used_percent = 100.1;

    let result = store.apply(NOW, StateMutation::UpsertSnapshot(invalid));

    assert!(matches!(result, Err(StateError::Invalid)));
    assert_eq!(fs::read(&path).unwrap(), original_bytes);
    assert_eq!(
        store.load(NOW).unwrap().snapshots[&ProviderId::Codex],
        valid_snapshot()
    );
}

#[test]
fn current_projection_omits_an_expired_stored_snapshot() {
    let temp = TempDir::new().unwrap();
    let store = JsonStateStore::new(temp.path().join("state.json"));
    store
        .apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot()))
        .unwrap();

    let stored = store.load(NOW + 86_400).unwrap();

    assert_eq!(stored.snapshots.len(), 1);
    assert!(stored.current_snapshots(NOW + 86_400).is_empty());
}

#[test]
fn schema_one_migration_drops_untrusted_codex_and_preserves_valid_user_state() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let store = JsonStateStore::new(path.clone());
    let mut legacy = PersistedState {
        schema_version: 1,
        ..PersistedState::default()
    };
    legacy.snapshots.insert(ProviderId::Codex, valid_snapshot());
    legacy
        .snapshots
        .insert(ProviderId::Claude, valid_claude_snapshot());
    legacy.window = Some(WindowPlacement { x: 120, y: -40 });
    legacy.always_on_top = false;
    legacy.launch_at_signin_requested = true;
    legacy.startup_identity = Some(StartupIdentity {
        installed_exe: "C:\\Apps\\UsageWidget.exe".into(),
    });
    legacy.claude_tracking = Some(ClaudeTrackingIdentity {
        installed_exe: "C:\\Apps\\UsageWidget.exe".into(),
        installed_status_line: json!({"type": "command", "command": "widget"}),
    });
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let loaded = store.load(NOW).unwrap();

    assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
    assert!(!loaded.snapshots.contains_key(&ProviderId::Codex));
    assert_eq!(
        loaded.snapshots[&ProviderId::Claude],
        valid_claude_snapshot()
    );
    assert_eq!(loaded.window, legacy.window);
    assert_eq!(loaded.always_on_top, legacy.always_on_top);
    assert_eq!(
        loaded.launch_at_signin_requested,
        legacy.launch_at_signin_requested
    );
    assert_eq!(loaded.startup_identity, legacy.startup_identity);
    assert_eq!(loaded.claude_tracking, legacy.claude_tracking);

    store
        .apply(NOW, StateMutation::SetAlwaysOnTop(true))
        .unwrap();
    let persisted: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["schema_version"], json!(STATE_SCHEMA_VERSION));
    assert!(persisted["snapshots"].get("codex").is_none());
}

#[test]
fn partial_claude_state_is_quarantined_and_never_projected() {
    for missing in ["short_window", "weekly_window"] {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("state.json");
        let mut state = PersistedState::default();
        state
            .snapshots
            .insert(ProviderId::Claude, valid_claude_snapshot());
        let mut value = serde_json::to_value(state).unwrap();
        value["snapshots"]["claude"]
            .as_object_mut()
            .unwrap()
            .remove(missing);
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let loaded = JsonStateStore::new(path.clone()).load(NOW).unwrap();

        assert_eq!(loaded, PersistedState::default());
        assert!(!path.exists());
        let quarantined = quarantined_state_files(temp.path());
        assert_eq!(quarantined.len(), 1);
        assert_eq!(fs::read(&quarantined[0]).unwrap(), original);
    }
}

#[test]
fn malformed_json_is_quarantined_collision_resistently_and_defaults_are_loaded() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let store = JsonStateStore::new(path.clone());

    for body in [b"{first-invalid".as_slice(), b"{second-invalid".as_slice()] {
        fs::write(&path, body).unwrap();
        assert_eq!(store.load(NOW).unwrap(), PersistedState::default());
        assert!(!path.exists());
    }

    let mut quarantined = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    quarantined.sort();
    assert_eq!(quarantined.len(), 2);
    assert!(quarantined.iter().all(|path| {
        let name = path.file_name().unwrap().to_string_lossy();
        name.starts_with("state.corrupt.") && name.ends_with(".json")
    }));
    let bodies = quarantined
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(bodies.contains(&b"{first-invalid".to_vec()));
    assert!(bodies.contains(&b"{second-invalid".to_vec()));
}

fn quarantined_state_files(directory: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with("state.corrupt.") && name.ends_with(".json")
        })
        .collect()
}

#[test]
fn structurally_corrupt_current_schema_is_quarantined_and_defaults_are_loaded() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let original = format!(r#"{{"schema_version":{STATE_SCHEMA_VERSION},"snapshots":[],"window":null,"always_on_top":true,"launch_at_signin_requested":false,"startup_identity":null,"claude_tracking":null}}"#).into_bytes();
    fs::write(&path, &original).unwrap();

    let loaded = JsonStateStore::new(path.clone()).load(NOW).unwrap();

    assert_eq!(loaded, PersistedState::default());
    assert!(!path.exists());
    let quarantined = quarantined_state_files(temp.path());
    assert_eq!(quarantined.len(), 1);
    assert_eq!(fs::read(&quarantined[0]).unwrap(), original);
}

#[test]
fn semantically_corrupt_current_schema_is_quarantined_and_defaults_are_loaded() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let mut state = PersistedState::default();
    state.snapshots.insert(ProviderId::Codex, valid_snapshot());
    let mut value = serde_json::to_value(state).unwrap();
    value["snapshots"]["codex"]["short_window"]["used_percent"] = json!(101.0);
    let original = serde_json::to_vec(&value).unwrap();
    fs::write(&path, &original).unwrap();

    let loaded = JsonStateStore::new(path.clone()).load(NOW).unwrap();

    assert_eq!(loaded, PersistedState::default());
    assert!(!path.exists());
    let quarantined = quarantined_state_files(temp.path());
    assert_eq!(quarantined.len(), 1);
    assert_eq!(fs::read(&quarantined[0]).unwrap(), original);
}

#[test]
fn apply_rebuilds_state_after_quarantining_current_schema_corruption() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let original = format!(r#"{{"schema_version":{STATE_SCHEMA_VERSION},"snapshots":"invalid"}}"#)
        .into_bytes();
    fs::write(&path, &original).unwrap();
    let store = JsonStateStore::new(path.clone());

    let rebuilt = store
        .apply(NOW, StateMutation::SetAlwaysOnTop(false))
        .unwrap();

    assert!(!rebuilt.always_on_top);
    assert_eq!(store.load(NOW).unwrap(), rebuilt);
    let quarantined = quarantined_state_files(temp.path());
    assert_eq!(quarantined.len(), 1);
    assert_eq!(fs::read(&quarantined[0]).unwrap(), original);
}

struct FailingReplace;

impl AtomicReplace for FailingReplace {
    fn replace(&self, _temporary: &Path, _destination: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("injected replacement failure"))
    }
}

#[test]
fn replacement_failure_leaves_original_bytes_unchanged() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    JsonStateStore::new(path.clone())
        .apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot()))
        .unwrap();
    let original_bytes = fs::read(&path).unwrap();
    let failing = JsonStateStore::with_replacer(path.clone(), Arc::new(FailingReplace));

    let result = failing.apply(NOW, StateMutation::SetAlwaysOnTop(false));

    assert!(matches!(result, Err(StateError::Io)));
    assert_eq!(fs::read(path).unwrap(), original_bytes);
}

struct CoordinatedCopyReplace {
    entered: AtomicUsize,
    gate: Mutex<()>,
    wake: Condvar,
}

impl CoordinatedCopyReplace {
    fn new() -> Self {
        Self {
            entered: AtomicUsize::new(0),
            gate: Mutex::new(()),
            wake: Condvar::new(),
        }
    }
}

impl AtomicReplace for CoordinatedCopyReplace {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()> {
        let position = self.entered.fetch_add(1, Ordering::SeqCst);
        if position == 0 {
            let guard = self.gate.lock().unwrap();
            let _ = self
                .wake
                .wait_timeout_while(guard, Duration::from_secs(1), |_| {
                    self.entered.load(Ordering::SeqCst) < 2
                })
                .unwrap();
        } else {
            self.wake.notify_all();
        }
        fs::copy(temporary, destination)?;
        fs::remove_file(temporary)
    }
}

#[test]
fn concurrent_stores_preserve_updates_to_different_fields() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let replacer = Arc::new(CoordinatedCopyReplace::new());
    let store_one = JsonStateStore::with_replacer(path.clone(), replacer.clone());
    let store_two = JsonStateStore::with_replacer(path.clone(), replacer);

    let first = thread::spawn(move || {
        store_one
            .apply(
                NOW,
                StateMutation::SetWindow(Some(WindowPlacement { x: 120, y: -40 })),
            )
            .unwrap();
    });
    let second = thread::spawn(move || {
        store_two
            .apply(NOW, StateMutation::SetAlwaysOnTop(false))
            .unwrap();
    });
    first.join().unwrap();
    second.join().unwrap();

    let state = JsonStateStore::new(path).load(NOW).unwrap();
    assert_eq!(state.window, Some(WindowPlacement { x: 120, y: -40 }));
    assert!(!state.always_on_top);
}

struct BlockingCopyReplace {
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

impl AtomicReplace for BlockingCopyReplace {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()> {
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        fs::copy(temporary, destination)?;
        fs::remove_file(temporary)
    }
}

struct CompletingCopyReplace {
    completed: Sender<()>,
}

impl AtomicReplace for CompletingCopyReplace {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()> {
        fs::copy(temporary, destination)?;
        fs::remove_file(temporary)?;
        self.completed.send(()).unwrap();
        Ok(())
    }
}

#[test]
fn nonexistent_parent_transactions_share_one_stable_mutex() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("not-yet-created").join("state.json");
    let test_user_scope = b"deterministic-test-user";
    let identity_before_parent_creation = state_mutex_identity(&path, test_user_scope).unwrap();
    let (first_entered_tx, first_entered_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_store = JsonStateStore::with_replacer(
        path.clone(),
        Arc::new(BlockingCopyReplace {
            entered: first_entered_tx,
            release: Mutex::new(release_first_rx),
        }),
    );
    let first = thread::spawn(move || {
        first_store
            .apply(
                NOW,
                StateMutation::SetWindow(Some(WindowPlacement { x: 120, y: -40 })),
            )
            .unwrap();
    });
    first_entered_rx.recv().unwrap();

    let identity_after_parent_creation = state_mutex_identity(&path, test_user_scope).unwrap();
    let identities_match = identity_before_parent_creation == identity_after_parent_creation;
    let (second_completed_tx, second_completed_rx) = mpsc::channel();
    let second_store = JsonStateStore::with_replacer(
        path.clone(),
        Arc::new(CompletingCopyReplace {
            completed: second_completed_tx,
        }),
    );
    let (second_started_tx, second_started_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        second_started_tx.send(()).unwrap();
        second_store
            .apply(NOW, StateMutation::SetAlwaysOnTop(false))
            .unwrap();
    });
    second_started_rx.recv().unwrap();

    if identities_match {
        release_first_tx.send(()).unwrap();
    } else {
        second_completed_rx.recv().unwrap();
        release_first_tx.send(()).unwrap();
    }
    first.join().unwrap();
    second.join().unwrap();

    let state = JsonStateStore::new(path).load(NOW).unwrap();
    assert_eq!(state.window, Some(WindowPlacement { x: 120, y: -40 }));
    assert!(
        !state.always_on_top,
        "a mutex identity split lost the second transaction"
    );
    assert_eq!(
        identity_before_parent_creation, identity_after_parent_creation,
        "mutex identity changed when its parent directory was created"
    );
}

#[test]
fn mutex_identity_includes_explicit_user_scope() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");

    assert_ne!(
        state_mutex_identity(&path, b"user-a").unwrap(),
        state_mutex_identity(&path, b"user-b").unwrap()
    );
}

#[test]
fn mutex_identity_preserves_distinct_non_unicode_utf16_paths() {
    let temp = TempDir::new().unwrap();
    let first = temp
        .path()
        .join(OsString::from_wide(&[b'x' as u16, 0xd800]));
    let second = temp
        .path()
        .join(OsString::from_wide(&[b'x' as u16, 0xd801]));

    assert_ne!(
        state_mutex_identity(&first, b"same-user").unwrap(),
        state_mutex_identity(&second, b"same-user").unwrap()
    );
}

#[test]
fn applies_and_clears_only_the_listed_preferences_and_identities() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let store = JsonStateStore::new(path.clone());
    store
        .apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot()))
        .unwrap();
    store
        .apply(
            NOW,
            StateMutation::SetStartup {
                requested: true,
                identity: Some(StartupIdentity {
                    installed_exe: "C:\\Apps\\UsageWidget.exe".into(),
                }),
            },
        )
        .unwrap();
    store
        .apply(
            NOW,
            StateMutation::SetClaudeTracking(Some(ClaudeTrackingIdentity {
                installed_exe: "C:\\Apps\\UsageWidget.exe".into(),
                installed_status_line: json!({"type": "command", "command": "widget"}),
            })),
        )
        .unwrap();

    let state = store.load(NOW).unwrap();
    assert!(state.launch_at_signin_requested);
    assert!(state.startup_identity.is_some());
    assert!(state.claude_tracking.is_some());
    let serialized: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut keys = serialized
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "always_on_top",
            "claude_tracking",
            "launch_at_signin_requested",
            "schema_version",
            "snapshots",
            "startup_identity",
            "window",
        ]
    );

    store
        .apply(
            NOW,
            StateMutation::SetStartup {
                requested: false,
                identity: None,
            },
        )
        .unwrap();
    let cleared = store
        .apply(NOW, StateMutation::SetClaudeTracking(None))
        .unwrap();
    assert!(!cleared.launch_at_signin_requested);
    assert_eq!(cleared.startup_identity, None);
    assert_eq!(cleared.claude_tracking, None);
    assert_eq!(cleared.snapshots[&ProviderId::Codex], valid_snapshot());
}

#[test]
fn defaults_match_schema_contract_and_unsupported_or_oversized_files_are_rejected() {
    let defaults = PersistedState::default();
    assert_eq!(defaults.schema_version, STATE_SCHEMA_VERSION);
    assert!(defaults.snapshots.is_empty());
    assert_eq!(defaults.window, None);
    assert!(defaults.always_on_top);
    assert!(!defaults.launch_at_signin_requested);
    assert_eq!(defaults.startup_identity, None);
    assert_eq!(defaults.claude_tracking, None);

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("state.json");
    let mut unsupported = serde_json::to_value(PersistedState::default()).unwrap();
    unsupported["schema_version"] = json!(STATE_SCHEMA_VERSION + 1);
    let unsupported_bytes = serde_json::to_vec(&unsupported).unwrap();
    fs::write(&path, &unsupported_bytes).unwrap();
    assert!(matches!(
        JsonStateStore::new(path.clone()).load(NOW),
        Err(StateError::UnsupportedSchema)
    ));
    assert_eq!(fs::read(&path).unwrap(), unsupported_bytes);
    assert!(quarantined_state_files(temp.path()).is_empty());
    assert!(matches!(
        JsonStateStore::new(path.clone()).apply(NOW, StateMutation::SetAlwaysOnTop(false)),
        Err(StateError::UnsupportedSchema)
    ));
    assert_eq!(fs::read(&path).unwrap(), unsupported_bytes);
    assert!(quarantined_state_files(temp.path()).is_empty());

    fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        JsonStateStore::new(path).load(NOW),
        Err(StateError::Oversized)
    ));
}

#[test]
fn default_path_is_exactly_under_the_local_app_data_usage_widget_directory() {
    let local_app_data = dirs::data_local_dir().unwrap();

    assert_eq!(
        default_state_path().unwrap(),
        local_app_data.join("UsageWidget").join("state.json")
    );
}
