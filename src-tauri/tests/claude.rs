use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};
use tempfile::TempDir;
use usage_widget::{
    claude_settings::{
        ClaudeSettingsManager, ClaudeSetupError, ClaudeTrackingState, MAX_SETTINGS_BYTES,
    },
    model::{ProviderId, ProviderSnapshot, WindowSnapshot},
    providers::claude::{
        capture_mode_from_args, parse_claude_statusline, render_capture_status, run_claude_capture,
        CaptureError, MAX_CLAUDE_STDIN_BYTES,
    },
    state_store::{ClaudeTrackingIdentity, PersistedState, StateError, StateMutation, StateStore},
};

const NOW: i64 = 2_000_000_000;

fn exact_input() -> Value {
    json!({
        "rate_limits": {
            "five_hour": {"used_percentage": 23.0, "resets_at": NOW + 3600},
            "seven_day": {"used_percentage": 41.0, "resets_at": NOW + 86400}
        },
        "ignored_prompt": "must never be stored"
    })
}

fn existing_claude_snapshot() -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Claude,
        observed_at: NOW - 10,
        short_window: WindowSnapshot {
            duration_minutes: 300,
            used_percent: 7.0,
            resets_at: NOW + 7_200,
        },
        weekly_window: WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 11.0,
            resets_at: NOW + 172_800,
        },
    }
}

type Edit = Box<dyn FnOnce() + Send>;

struct TestStore {
    state: Mutex<PersistedState>,
    fail_apply: bool,
    edit_on_load: Mutex<Option<Edit>>,
    edit_on_failed_apply: Mutex<Option<Edit>>,
}

impl TestStore {
    fn new(state: PersistedState) -> Self {
        Self {
            state: Mutex::new(state),
            fail_apply: false,
            edit_on_load: Mutex::new(None),
            edit_on_failed_apply: Mutex::new(None),
        }
    }

    fn failing(state: PersistedState) -> Self {
        Self {
            fail_apply: true,
            ..Self::new(state)
        }
    }

    fn edit_on_load(self, edit: impl FnOnce() + Send + 'static) -> Self {
        *self.edit_on_load.lock().unwrap() = Some(Box::new(edit));
        self
    }

    fn edit_on_failed_apply(self, edit: impl FnOnce() + Send + 'static) -> Self {
        *self.edit_on_failed_apply.lock().unwrap() = Some(Box::new(edit));
        self
    }

    fn snapshot(&self) -> PersistedState {
        self.state.lock().unwrap().clone()
    }
}

impl StateStore for TestStore {
    fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
        if let Some(edit) = self.edit_on_load.lock().unwrap().take() {
            edit();
        }
        Ok(self.snapshot())
    }

    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
        if self.fail_apply {
            if let Some(edit) = self.edit_on_failed_apply.lock().unwrap().take() {
                edit();
            }
            return Err(StateError::Io);
        }
        let mut state = self.state.lock().unwrap();
        state.apply_mutation(mutation, now)?;
        Ok(state.clone())
    }
}

#[test]
fn parses_only_complete_exact_limits_and_renders_remaining() {
    let input = exact_input();
    let snapshot = parse_claude_statusline(input.to_string().as_bytes(), NOW).unwrap();

    assert_eq!(snapshot.provider, ProviderId::Claude);
    assert_eq!(snapshot.observed_at, NOW);
    assert_eq!(snapshot.short_window.duration_minutes, 300);
    assert_eq!(snapshot.weekly_window.duration_minutes, 10_080);
    assert_eq!(
        render_capture_status(&snapshot),
        "USAGE 5H 77% LEFT | 7D 59% LEFT"
    );
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("ignored_prompt"));
    assert!(!serialized.contains("must never be stored"));
}

#[test]
fn classifies_missing_expired_invalid_and_oversized_capture_input() {
    let missing = json!({
        "rate_limits": {
            "five_hour": {"used_percentage": 23.0, "resets_at": NOW + 3600}
        }
    });
    assert_eq!(
        parse_claude_statusline(missing.to_string().as_bytes(), NOW),
        Err(CaptureError::MissingLimits)
    );

    let mut expired = exact_input();
    expired["rate_limits"]["seven_day"]["resets_at"] = json!(NOW);
    assert_eq!(
        parse_claude_statusline(expired.to_string().as_bytes(), NOW),
        Err(CaptureError::Expired)
    );
    assert_eq!(
        parse_claude_statusline(br#"{"rate_limits": "#, NOW),
        Err(CaptureError::Invalid)
    );

    let mut non_numeric = exact_input();
    non_numeric["rate_limits"]["five_hour"]["used_percentage"] = json!("23");
    assert_eq!(
        parse_claude_statusline(non_numeric.to_string().as_bytes(), NOW),
        Err(CaptureError::Invalid)
    );
    assert_eq!(
        parse_claude_statusline(&vec![b' '; MAX_CLAUDE_STDIN_BYTES + 1], NOW),
        Err(CaptureError::Oversized)
    );
}

#[test]
fn reset_numbers_must_have_an_exact_in_range_i64_representation() {
    let invalid_resets = [
        json!(i64::MAX as u64 + 1),
        json!(u64::MAX),
        json!(9_223_372_036_854_775_808.0_f64),
        json!(1.0e20_f64),
        json!(NOW as f64 + 3_600.5),
    ];
    for reset in invalid_resets {
        let mut input = exact_input();
        input["rate_limits"]["five_hour"]["resets_at"] = reset;
        assert_eq!(
            parse_claude_statusline(input.to_string().as_bytes(), NOW),
            Err(CaptureError::Invalid)
        );
    }

    for (reset, expected) in [
        (json!(i64::MAX), i64::MAX),
        (json!((NOW + 3_600) as f64), NOW + 3_600),
    ] {
        let mut input = exact_input();
        input["rate_limits"]["five_hour"]["resets_at"] = reset;
        assert_eq!(
            parse_claude_statusline(input.to_string().as_bytes(), NOW)
                .unwrap()
                .short_window
                .resets_at,
            expected
        );
    }
}

#[test]
fn rejected_capture_never_replaces_an_existing_snapshot_or_echoes_input() {
    let mut initial = PersistedState::default();
    initial
        .snapshots
        .insert(ProviderId::Claude, existing_claude_snapshot());
    let store = TestStore::new(initial.clone());
    let sensitive = br#"{"ignored_prompt":"SENSITIVE-CANARY"}"#;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit = run_claude_capture(sensitive.as_slice(), &mut stdout, &mut stderr, &store, NOW);

    assert_eq!(exit, 0);
    assert_eq!(stdout, b"USAGE: NO EXACT LIMITS\n");
    assert!(stderr.is_empty());
    assert!(!String::from_utf8_lossy(&stdout).contains("SENSITIVE-CANARY"));
    assert_eq!(store.snapshot(), initial);
}

#[test]
fn every_rejected_capture_shape_leaves_state_unchanged() {
    let mut initial = PersistedState::default();
    initial
        .snapshots
        .insert(ProviderId::Claude, existing_claude_snapshot());
    let rejected = [
        json!({"rate_limits":{"five_hour":{"used_percentage":23,"resets_at":NOW + 1}}})
            .to_string()
            .into_bytes(),
        br#"{"rate_limits": "#.to_vec(),
        vec![b' '; MAX_CLAUDE_STDIN_BYTES + 1],
        json!({"rate_limits":{"five_hour":{"used_percentage":23,"resets_at":NOW + 1},"seven_day":{"used_percentage":"x","resets_at":NOW + 2}}})
            .to_string()
            .into_bytes(),
        json!({"rate_limits":{"five_hour":{"used_percentage":23,"resets_at":NOW},"seven_day":{"used_percentage":41,"resets_at":NOW + 2}}})
            .to_string()
            .into_bytes(),
    ];

    for bytes in rejected {
        let store = TestStore::new(initial.clone());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_claude_capture(bytes.as_slice(), &mut stdout, &mut stderr, &store, NOW),
            0
        );
        assert_eq!(stdout, b"USAGE: NO EXACT LIMITS\n");
        assert!(stderr.is_empty());
        assert_eq!(store.snapshot(), initial);
    }
}

#[test]
fn valid_capture_persists_normalized_snapshot_before_printing() {
    let store = TestStore::new(PersistedState::default());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit = run_claude_capture(
        exact_input().to_string().as_bytes(),
        &mut stdout,
        &mut stderr,
        &store,
        NOW,
    );

    assert_eq!(exit, 0);
    assert_eq!(stdout, b"USAGE 5H 77% LEFT | 7D 59% LEFT\n");
    assert!(stderr.is_empty());
    let stored = &store.snapshot().snapshots[&ProviderId::Claude];
    assert_eq!(stored.short_window.used_percent, 23.0);
    assert_eq!(stored.weekly_window.used_percent, 41.0);
}

#[test]
fn state_failure_prints_only_fixed_stderr_and_returns_two() {
    let store = TestStore::failing(PersistedState::default());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit = run_claude_capture(
        exact_input().to_string().as_bytes(),
        &mut stdout,
        &mut stderr,
        &store,
        NOW,
    );

    assert_eq!(exit, 2);
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"USAGE: LOCAL STATE ERROR\n");
}

#[test]
fn capture_mode_requires_exactly_the_single_subcommand() {
    assert!(capture_mode_from_args([
        "usage-widget.exe",
        "claude-capture"
    ]));
    assert!(!capture_mode_from_args(["usage-widget.exe"]));
    assert!(!capture_mode_from_args([
        "usage-widget.exe",
        "claude-capture",
        "extra"
    ]));
    assert!(!capture_mode_from_args(["usage-widget.exe", "other"]));
    assert!(!capture_mode_from_args([
        OsString::from("usage-widget.exe"),
        OsString::from("CLAUDE-CAPTURE")
    ]));
}

fn setup_settings(body: &[u8]) -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("settings.json");
    fs::write(&path, body).unwrap();
    (temp, path)
}

fn widget_status_line(exe: &Path) -> Value {
    json!({
        "type": "command",
        "command": format!("\"{}\" claude-capture", exe.display())
    })
}

fn owned_state(exe: &Path, status_line: Value) -> PersistedState {
    PersistedState {
        claude_tracking: Some(ClaudeTrackingIdentity {
            installed_exe: exe.to_path_buf(),
            installed_status_line: status_line,
        }),
        ..PersistedState::default()
    }
}

fn settings_at_exact_limit(mut value: Value) -> Vec<u8> {
    value["padding"] = json!("");
    let empty = serde_json::to_vec(&value).unwrap();
    value["padding"] = json!("x".repeat(MAX_SETTINGS_BYTES - empty.len()));
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(bytes.len(), MAX_SETTINGS_BYTES);
    bytes
}

fn settings_manager(path: PathBuf, store: Arc<dyn StateStore>) -> ClaudeSettingsManager {
    ClaudeSettingsManager::new(path, store)
}

struct EnvironmentRestore {
    claude_config_dir: Option<OsString>,
    user_profile: Option<OsString>,
}

impl EnvironmentRestore {
    fn capture() -> Self {
        Self {
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR"),
            user_profile: std::env::var_os("USERPROFILE"),
        }
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        match &self.claude_config_dir {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        match &self.user_profile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}

#[test]
fn settings_path_honors_config_dir_then_uses_the_user_profile_fallback() {
    let _restore = EnvironmentRestore::capture();
    let temp = TempDir::new().unwrap();
    let config_dir = temp.path().join("custom-claude");
    let profile = temp.path().join("profile");
    let fallback_dir = profile.join(".claude");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&fallback_dir).unwrap();
    let configured_path = config_dir.join("settings.json");
    let fallback_path = fallback_dir.join("settings.json");
    fs::write(&configured_path, b"{}").unwrap();
    fs::write(&fallback_path, b"{}").unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &config_dir);
    std::env::set_var("USERPROFILE", &profile);
    let store: Arc<dyn StateStore> = Arc::new(TestStore::new(PersistedState::default()));

    let configured = ClaudeSettingsManager::from_environment(store.clone()).unwrap();
    assert_eq!(
        configured.enable(&temp.path().join("configured.exe"), NOW),
        Ok(ClaudeTrackingState::Enabled)
    );
    assert!(
        serde_json::from_slice::<Value>(&fs::read(&configured_path).unwrap()).unwrap()
            ["statusLine"]
            .is_object()
    );
    assert_eq!(fs::read(&fallback_path).unwrap(), b"{}");

    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let fallback = ClaudeSettingsManager::from_environment(Arc::new(TestStore::new(
        PersistedState::default(),
    )))
    .unwrap();
    assert_eq!(
        fallback.enable(&temp.path().join("fallback.exe"), NOW + 1),
        Ok(ClaudeTrackingState::Enabled)
    );
    assert!(
        serde_json::from_slice::<Value>(&fs::read(&fallback_path).unwrap()).unwrap()["statusLine"]
            .is_object()
    );
}

#[test]
fn enable_preserves_nested_settings_creates_backup_and_records_exact_identity() {
    let original = br#"{"theme":"dark","nested":{"keep":[1,{"secret":"local-only"}]}}"#;
    let (temp, path) = setup_settings(original);
    let exe = temp
        .path()
        .join("unused-segment")
        .join("..")
        .join("Usage Widget.exe");
    let normalized_exe = temp.path().join("Usage Widget.exe");
    let store = Arc::new(TestStore::new(PersistedState::default()));
    let manager = settings_manager(path.clone(), store.clone());

    assert_eq!(manager.enable(&exe, NOW), Ok(ClaudeTrackingState::Enabled));

    let updated: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(updated["theme"], "dark");
    assert_eq!(
        updated["nested"],
        json!({"keep":[1,{"secret":"local-only"}]})
    );
    let identity = store.snapshot().claude_tracking.unwrap();
    assert!(identity.installed_exe.is_absolute());
    assert_eq!(identity.installed_exe, normalized_exe);
    assert_eq!(updated["statusLine"], identity.installed_status_line);
    assert_eq!(manager.status(&exe, NOW), ClaudeTrackingState::Enabled);

    let backups = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|candidate| candidate != &path)
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
}

#[test]
fn enable_refuses_existing_non_null_status_line_without_any_byte_change() {
    let original = br#"{ "statusLine": {"type":"command","command":"user-tool"}, "keep": true }"#;
    let (_temp, path) = setup_settings(original);
    let manager = settings_manager(
        path.clone(),
        Arc::new(TestStore::new(PersistedState::default())),
    );

    assert_eq!(
        manager.enable(Path::new("C:\\Apps\\UsageWidget.exe"), NOW),
        Err(ClaudeSetupError::SettingsConflict)
    );
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn enable_rejects_an_update_over_the_settings_cap_before_backup_or_state_change() {
    let original = settings_at_exact_limit(json!({}));
    let (temp, path) = setup_settings(&original);
    let store = Arc::new(TestStore::new(PersistedState::default()));
    let manager = settings_manager(path.clone(), store.clone());

    assert_eq!(
        manager.enable(&temp.path().join("UsageWidget.exe"), NOW),
        Err(ClaudeSetupError::SettingsWriteFailed)
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(store.snapshot().claude_tracking, None);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn missing_malformed_non_object_and_oversized_settings_are_never_rewritten() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("missing.json");
    let manager = settings_manager(
        missing.clone(),
        Arc::new(TestStore::new(PersistedState::default())),
    );
    assert_eq!(
        manager.enable(Path::new("C:\\Apps\\UsageWidget.exe"), NOW),
        Err(ClaudeSetupError::SettingsMissing)
    );
    assert!(!missing.exists());

    for bytes in [
        br#"{"broken": "#.to_vec(),
        br#"["not", "an", "object"]"#.to_vec(),
        vec![b' '; MAX_SETTINGS_BYTES + 1],
    ] {
        let path = temp.path().join(format!("invalid-{}.json", bytes.len()));
        fs::write(&path, &bytes).unwrap();
        let manager = settings_manager(
            path.clone(),
            Arc::new(TestStore::new(PersistedState::default())),
        );
        assert_eq!(
            manager.enable(Path::new("C:\\Apps\\UsageWidget.exe"), NOW),
            Err(ClaudeSetupError::SettingsInvalid)
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn enable_rejects_every_unsafe_command_character() {
    let (temp, path) = setup_settings(b"{}");
    for unsafe_character in ['\"', '\r', '\n', '%', '!', '^', '&', '|', '<', '>'] {
        let exe = PathBuf::from(format!("C:\\Apps\\Usage{unsafe_character}Widget.exe"));
        let manager = settings_manager(
            path.clone(),
            Arc::new(TestStore::new(PersistedState::default())),
        );
        assert_eq!(
            manager.enable(&exe, NOW),
            Err(ClaudeSetupError::UnsafeExecutablePath)
        );
        assert_eq!(fs::read(&path).unwrap(), b"{}");
    }
    drop(temp);
}

#[test]
fn changed_settings_between_read_and_replace_are_preserved() {
    let original = br#"{"keep":"original"}"#;
    let newer = br#"{"keep":"user-newer"}"#.to_vec();
    let (_temp, path) = setup_settings(original);
    let edited_path = path.clone();
    let store = Arc::new(
        TestStore::new(PersistedState::default()).edit_on_load(move || {
            fs::write(edited_path, &newer).unwrap();
        }),
    );
    let manager = settings_manager(path.clone(), store);

    assert_eq!(
        manager.enable(Path::new("C:\\Apps\\UsageWidget.exe"), NOW),
        Err(ClaudeSetupError::SettingsChanged)
    );
    assert_eq!(fs::read(path).unwrap(), br#"{"keep":"user-newer"}"#);
}

#[test]
fn disable_refuses_a_user_edited_status_line_without_writing() {
    let exe = PathBuf::from("C:\\Apps\\UsageWidget.exe");
    let owned = widget_status_line(&exe);
    let current = json!({"statusLine":{"type":"command","command":"user-edited"},"keep":1});
    let bytes = serde_json::to_vec(&current).unwrap();
    let (_temp, path) = setup_settings(&bytes);
    let manager = settings_manager(
        path.clone(),
        Arc::new(TestStore::new(owned_state(&exe, owned))),
    );

    assert_eq!(
        manager.disable(NOW),
        Err(ClaudeSetupError::SettingsConflict)
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn disable_removes_only_the_owned_status_line_and_clears_identity() {
    let exe = PathBuf::from("C:\\Apps\\UsageWidget.exe");
    let owned = widget_status_line(&exe);
    let bytes = serde_json::to_vec(&json!({"statusLine":owned,"nested":{"keep":true}})).unwrap();
    let (_temp, path) = setup_settings(&bytes);
    let store = Arc::new(TestStore::new(owned_state(&exe, owned)));
    let manager = settings_manager(path.clone(), store.clone());

    assert_eq!(manager.disable(NOW), Ok(ClaudeTrackingState::Disabled));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
        json!({"nested":{"keep":true}})
    );
    assert_eq!(store.snapshot().claude_tracking, None);
}

#[test]
fn repair_changes_only_the_full_widget_owned_object_and_updates_identity() {
    let old_exe = PathBuf::from("C:\\Apps\\Old Widget.exe");
    let new_exe = PathBuf::from("C:\\Apps\\New Widget.exe");
    let owned = widget_status_line(&old_exe);
    let bytes = serde_json::to_vec(&json!({
        "statusLine": owned,
        "nested": {"keep": [1, 2, 3]}
    }))
    .unwrap();
    let (_temp, path) = setup_settings(&bytes);
    let store = Arc::new(TestStore::new(owned_state(&old_exe, owned)));
    let manager = settings_manager(path.clone(), store.clone());

    assert_eq!(
        manager.status(&new_exe, NOW),
        ClaudeTrackingState::NeedsRepair
    );
    assert_eq!(
        manager.repair(&new_exe, NOW),
        Ok(ClaudeTrackingState::Enabled)
    );

    let updated: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(updated["nested"], json!({"keep":[1,2,3]}));
    let identity = store.snapshot().claude_tracking.unwrap();
    assert_eq!(updated["statusLine"], identity.installed_status_line);
    assert_eq!(identity.installed_exe, new_exe);
}

#[test]
fn repair_rejects_an_update_over_the_settings_cap_without_file_or_identity_change() {
    let old_exe = PathBuf::from("C:\\A.exe");
    let new_exe = PathBuf::from("C:\\Apps\\A-Much-Longer-Usage-Widget-Name.exe");
    let owned = widget_status_line(&old_exe);
    let original = settings_at_exact_limit(json!({"statusLine":owned}));
    let (_temp, path) = setup_settings(&original);
    let initial_state = owned_state(&old_exe, owned);
    let store = Arc::new(TestStore::new(initial_state.clone()));
    let manager = settings_manager(path.clone(), store.clone());

    assert_eq!(
        manager.repair(&new_exe, NOW),
        Err(ClaudeSetupError::SettingsWriteFailed)
    );
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(store.snapshot(), initial_state);
}

fn assert_state_failure_rolls_settings_back(operation: &str) {
    let old_exe = PathBuf::from("C:\\Apps\\Old Widget.exe");
    let new_exe = PathBuf::from("C:\\Apps\\New Widget.exe");
    let owned = widget_status_line(&old_exe);
    let (initial_state, original_value) = match operation {
        "enable" => (PersistedState::default(), json!({"keep":"enable"})),
        "disable" | "repair" => (
            owned_state(&old_exe, owned.clone()),
            json!({"statusLine":owned,"keep":operation}),
        ),
        _ => unreachable!(),
    };
    let original = serde_json::to_vec(&original_value).unwrap();
    let (_temp, path) = setup_settings(&original);
    let manager = settings_manager(path.clone(), Arc::new(TestStore::failing(initial_state)));

    let result = match operation {
        "enable" => manager.enable(&old_exe, NOW),
        "disable" => manager.disable(NOW),
        "repair" => manager.repair(&new_exe, NOW),
        _ => unreachable!(),
    };

    assert_eq!(result, Err(ClaudeSetupError::SettingsWriteFailed));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn state_failure_rolls_back_enable_disable_and_repair_settings() {
    for operation in ["enable", "disable", "repair"] {
        assert_state_failure_rolls_settings_back(operation);
    }
}

fn assert_concurrent_edit_survives_failed_state_update(operation: &str) {
    let old_exe = PathBuf::from("C:\\Apps\\Old Widget.exe");
    let new_exe = PathBuf::from("C:\\Apps\\New Widget.exe");
    let owned = widget_status_line(&old_exe);
    let (initial_state, original_value) = match operation {
        "enable" => (PersistedState::default(), json!({"keep":"enable"})),
        "disable" | "repair" => (
            owned_state(&old_exe, owned.clone()),
            json!({"statusLine":owned,"keep":operation}),
        ),
        _ => unreachable!(),
    };
    let original = serde_json::to_vec(&original_value).unwrap();
    let (_temp, path) = setup_settings(&original);
    let newer_value = match operation {
        "enable" => json!({
            "statusLine": widget_status_line(&old_exe),
            "keep": "user-newer"
        }),
        "disable" => json!({"keep":"user-newer"}),
        "repair" => json!({
            "statusLine": widget_status_line(&new_exe),
            "keep": "user-newer"
        }),
        _ => unreachable!(),
    };
    let newer = serde_json::to_vec(&newer_value).unwrap();
    let edited_path = path.clone();
    let edit_bytes = newer.clone();
    let store = Arc::new(
        TestStore::failing(initial_state).edit_on_failed_apply(move || {
            fs::write(edited_path, edit_bytes).unwrap();
        }),
    );
    let manager = settings_manager(path.clone(), store);

    let result = match operation {
        "enable" => manager.enable(&old_exe, NOW),
        "disable" => manager.disable(NOW),
        "repair" => manager.repair(&new_exe, NOW),
        _ => unreachable!(),
    };

    assert_eq!(result, Err(ClaudeSetupError::SettingsChanged));
    assert_eq!(fs::read(path).unwrap(), newer);
}

#[test]
fn rollback_never_overwrites_concurrent_user_edits_for_any_operation() {
    for operation in ["enable", "disable", "repair"] {
        assert_concurrent_edit_survives_failed_state_update(operation);
    }
}
