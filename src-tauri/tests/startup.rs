use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use usage_widget::{
    claude_settings::ClaudeTrackingState,
    providers::claude::capture_mode_from_args,
    shell::{
        clamp_position, clamp_widget_height, claude_tray_action, claude_tray_label,
        finish_gui_setup, gui_start_error_message, startup_tray_action, startup_tray_action_state,
        GuiStartError, IntegrationStatus, TrayActionState, TrayIntegrationAction, WorkArea,
        ALWAYS_ON_TOP_LABEL, REFRESH_LABEL, SHOW_HIDE_LABEL, STARTUP_MANUAL_REVIEW_MESSAGE,
    },
    startup::{
        disable_startup, enable_startup, repair_startup, startup_status, StartupError,
        StartupRegistration, StartupRegistrationSnapshot,
    },
    state_store::{
        PersistedState, StartupIdentity, StateError, StateMutation, StateStore, WindowPlacement,
    },
};

const NOW: i64 = 2_000_000_000;

#[test]
fn gui_setup_boundary_always_returns_ok_and_requests_one_fixed_report_and_exit() {
    for (error, expected_message) in [
        (
            GuiStartError::LocalState,
            "Usage Widget could not read or repair its local state.",
        ),
        (
            GuiStartError::WebViewRuntime,
            "Usage Widget could not start. Check that Windows WebView2 Runtime is available.",
        ),
        (
            GuiStartError::Runtime,
            "Usage Widget could not start its Windows GUI.",
        ),
        (
            GuiStartError::Tray,
            "Usage Widget could not complete that tray action.",
        ),
    ] {
        let mut requests = Vec::new();
        let result = finish_gui_setup(Err(error), |reported| {
            requests.push((gui_start_error_message(reported), 1));
        });

        assert!(result.is_ok());
        assert_eq!(requests, vec![(expected_message, 1)]);
    }

    let mut requests = Vec::new();
    let result = finish_gui_setup(Ok(()), |reported| {
        requests.push((gui_start_error_message(reported), 1));
    });
    assert!(result.is_ok());
    assert!(requests.is_empty());
}

#[test]
fn tray_labels_match_the_approved_text_exactly() {
    assert_eq!(SHOW_HIDE_LABEL, "Show/Hide");
    assert_eq!(REFRESH_LABEL, "Refresh");
    assert_eq!(ALWAYS_ON_TOP_LABEL, "Always on Top");

    assert_eq!(
        startup_tray_action_state(IntegrationStatus::Disabled),
        TrayActionState {
            label: "Launch at Sign-in",
            checked: false,
        }
    );
    assert_eq!(
        startup_tray_action_state(IntegrationStatus::Enabled),
        TrayActionState {
            label: "Launch at Sign-in",
            checked: true,
        }
    );
    assert_eq!(
        startup_tray_action_state(IntegrationStatus::NeedsRepair),
        TrayActionState {
            label: "Repair Launch at Sign-in",
            checked: false,
        }
    );
    assert_eq!(
        startup_tray_action_state(IntegrationStatus::Conflict),
        TrayActionState {
            label: "Launch at Sign-in",
            checked: false,
        }
    );

    assert_eq!(
        claude_tray_label(ClaudeTrackingState::Disabled),
        "Enable Claude Tracking"
    );
    assert_eq!(
        claude_tray_label(ClaudeTrackingState::Enabled),
        "Disable Claude Tracking"
    );
    assert_eq!(
        claude_tray_label(ClaudeTrackingState::NeedsRepair),
        "Repair Claude Tracking"
    );
    assert_eq!(
        claude_tray_label(ClaudeTrackingState::Conflict),
        "Repair Claude Tracking"
    );
    assert_eq!(
        startup_tray_action(IntegrationStatus::NeedsRepair),
        TrayIntegrationAction::Repair
    );
    assert_eq!(
        startup_tray_action(IntegrationStatus::Conflict),
        TrayIntegrationAction::ManualReview
    );
    assert_eq!(
        claude_tray_action(ClaudeTrackingState::Conflict),
        TrayIntegrationAction::ManualReview
    );
    assert_eq!(
        STARTUP_MANUAL_REVIEW_MESSAGE,
        "Launch at Sign-in needs manual review in Windows Startup Apps."
    );
}

struct FakeRegistration {
    operations: Mutex<Vec<&'static str>>,
    enable_result: Mutex<Result<(), StartupError>>,
    disable_result: Mutex<Result<(), StartupError>>,
    enabled: Mutex<Result<bool, StartupError>>,
    registered_value: Mutex<Option<PathBuf>>,
    startup_approved_value: Mutex<Option<Vec<u8>>>,
    enable_value: Option<PathBuf>,
    enable_mutates_before_error: bool,
    disable_mutates_before_error: bool,
}

impl Default for FakeRegistration {
    fn default() -> Self {
        Self {
            operations: Mutex::new(Vec::new()),
            enable_result: Mutex::new(Ok(())),
            disable_result: Mutex::new(Ok(())),
            enabled: Mutex::new(Ok(false)),
            registered_value: Mutex::new(None),
            startup_approved_value: Mutex::new(None),
            enable_value: None,
            enable_mutates_before_error: false,
            disable_mutates_before_error: false,
        }
    }
}

impl StartupRegistration for FakeRegistration {
    fn snapshot(&self) -> Result<StartupRegistrationSnapshot, StartupError> {
        self.operations.lock().unwrap().push("snapshot");
        Ok(StartupRegistrationSnapshot {
            run_value: self
                .registered_value
                .lock()
                .unwrap()
                .as_ref()
                .map(|path| path.to_string_lossy().as_bytes().to_vec()),
            startup_approved_value: self.startup_approved_value.lock().unwrap().clone(),
        })
    }

    fn restore(&self, snapshot: &StartupRegistrationSnapshot) -> Result<(), StartupError> {
        self.operations.lock().unwrap().push("restore");
        *self.registered_value.lock().unwrap() = snapshot
            .run_value
            .as_ref()
            .map(|bytes| {
                String::from_utf8(bytes.clone())
                    .map(PathBuf::from)
                    .map_err(|_| StartupError::OperationFailed)
            })
            .transpose()?;
        *self.startup_approved_value.lock().unwrap() = snapshot.startup_approved_value.clone();
        Ok(())
    }

    fn enable(&self) -> Result<(), StartupError> {
        self.operations.lock().unwrap().push("enable");
        let result = *self.enable_result.lock().unwrap();
        if result.is_ok() || self.enable_mutates_before_error {
            *self.registered_value.lock().unwrap() = self.enable_value.clone();
            *self.startup_approved_value.lock().unwrap() = Some(vec![2, 0, 0, 0]);
        }
        result
    }

    fn disable(&self) -> Result<(), StartupError> {
        self.operations.lock().unwrap().push("disable");
        let result = *self.disable_result.lock().unwrap();
        if result.is_ok() || self.disable_mutates_before_error {
            *self.registered_value.lock().unwrap() = None;
        }
        result
    }

    fn is_enabled(&self) -> Result<bool, StartupError> {
        self.operations.lock().unwrap().push("is_enabled");
        *self.enabled.lock().unwrap()
    }
}

#[test]
fn enable_restores_a_partial_registration_when_the_plugin_returns_an_error() {
    let current = PathBuf::from(r"C:\Current App\usage-widget.exe");
    let store = FakeStore::new(PersistedState::default());
    let registration = FakeRegistration {
        enable_result: Mutex::new(Err(StartupError::OperationFailed)),
        enable_value: Some(current.clone()),
        enable_mutates_before_error: true,
        ..FakeRegistration::default()
    };

    assert_eq!(
        enable_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(*registration.registered_value.lock().unwrap(), None);
    assert_eq!(*registration.startup_approved_value.lock().unwrap(), None);
    assert_eq!(store.snapshot(), PersistedState::default());
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "restore"]
    );
}

#[test]
fn disable_restores_a_partial_registration_when_the_plugin_returns_an_error() {
    let installed = PathBuf::from(r"C:\Installed App\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    let registration = FakeRegistration {
        disable_result: Mutex::new(Err(StartupError::OperationFailed)),
        registered_value: Mutex::new(Some(installed.clone())),
        startup_approved_value: Mutex::new(Some(vec![3, 9, 8, 7])),
        disable_mutates_before_error: true,
        ..FakeRegistration::default()
    };

    assert_eq!(
        disable_startup(&registration, &store, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(
        *registration.registered_value.lock().unwrap(),
        Some(installed.clone())
    );
    assert_eq!(
        *registration.startup_approved_value.lock().unwrap(),
        Some(vec![3, 9, 8, 7])
    );
    assert_eq!(store.snapshot(), requested_state(&installed));
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "disable", "restore"]
    );
}

#[test]
fn repair_restores_a_partial_registration_when_enable_returns_an_error() {
    let installed = PathBuf::from(r"C:\Old App\usage-widget.exe");
    let current = PathBuf::from(r"C:\Current App\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    let registration = FakeRegistration {
        enable_result: Mutex::new(Err(StartupError::OperationFailed)),
        registered_value: Mutex::new(Some(installed.clone())),
        startup_approved_value: Mutex::new(Some(vec![3, 4, 5, 6])),
        enable_value: Some(current.clone()),
        enable_mutates_before_error: true,
        ..FakeRegistration::default()
    };

    assert_eq!(
        repair_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(
        *registration.registered_value.lock().unwrap(),
        Some(installed.clone())
    );
    assert_eq!(
        *registration.startup_approved_value.lock().unwrap(),
        Some(vec![3, 4, 5, 6])
    );
    assert_eq!(store.snapshot(), requested_state(&installed));
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "restore"]
    );
}

struct FakeStore {
    state: Mutex<PersistedState>,
    fail_apply: Mutex<bool>,
}

impl FakeStore {
    fn new(state: PersistedState) -> Self {
        Self {
            state: Mutex::new(state),
            fail_apply: Mutex::new(false),
        }
    }

    fn snapshot(&self) -> PersistedState {
        self.state.lock().unwrap().clone()
    }
}

impl StateStore for FakeStore {
    fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
        Ok(self.snapshot())
    }

    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
        if *self.fail_apply.lock().unwrap() {
            return Err(StateError::Io);
        }
        let mut state = self.state.lock().unwrap();
        state.apply_mutation(mutation, now)?;
        Ok(state.clone())
    }
}

#[test]
fn enable_restores_the_absent_registration_when_state_persistence_fails() {
    let current = PathBuf::from(r"C:\Current App\usage-widget.exe");
    let store = FakeStore::new(PersistedState::default());
    *store.fail_apply.lock().unwrap() = true;
    let registration = FakeRegistration {
        enable_value: Some(current.clone()),
        ..FakeRegistration::default()
    };

    assert_eq!(
        enable_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(*registration.registered_value.lock().unwrap(), None);
    assert_eq!(*registration.startup_approved_value.lock().unwrap(), None);
    assert_eq!(store.snapshot(), PersistedState::default());
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "restore"]
    );
}

#[test]
fn disable_restores_the_prior_registration_when_state_persistence_fails() {
    let installed = PathBuf::from(r"C:\Installed App\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    *store.fail_apply.lock().unwrap() = true;
    let registration = FakeRegistration {
        registered_value: Mutex::new(Some(installed.clone())),
        startup_approved_value: Mutex::new(Some(vec![3, 9, 8, 7])),
        ..FakeRegistration::default()
    };

    assert_eq!(
        disable_startup(&registration, &store, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(
        *registration.registered_value.lock().unwrap(),
        Some(installed.clone())
    );
    assert_eq!(store.snapshot(), requested_state(&installed));
    assert_eq!(
        *registration.startup_approved_value.lock().unwrap(),
        Some(vec![3, 9, 8, 7])
    );
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "disable", "restore"]
    );
}

#[test]
fn repair_restores_the_prior_path_when_state_persistence_fails() {
    let installed = PathBuf::from(r"C:\Old App\usage-widget.exe");
    let current = PathBuf::from(r"C:\Current App\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    *store.fail_apply.lock().unwrap() = true;
    let registration = FakeRegistration {
        enabled: Mutex::new(Ok(true)),
        registered_value: Mutex::new(Some(installed.clone())),
        startup_approved_value: Mutex::new(Some(vec![3, 4, 5, 6])),
        enable_value: Some(current.clone()),
        ..FakeRegistration::default()
    };

    assert_eq!(
        repair_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(
        *registration.registered_value.lock().unwrap(),
        Some(installed.clone())
    );
    assert_eq!(store.snapshot(), requested_state(&installed));
    assert_eq!(
        *registration.startup_approved_value.lock().unwrap(),
        Some(vec![3, 4, 5, 6])
    );
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "is_enabled", "restore"]
    );
}

fn requested_state(installed_exe: &Path) -> PersistedState {
    PersistedState {
        launch_at_signin_requested: true,
        startup_identity: Some(StartupIdentity {
            installed_exe: installed_exe.to_path_buf(),
        }),
        ..PersistedState::default()
    }
}

#[test]
fn measured_window_height_is_bounded_without_reclassifying_content() {
    assert_eq!(clamp_widget_height(0), 80.0);
    assert_eq!(clamp_widget_height(190), 190.0);
    assert_eq!(clamp_widget_height(10_000), 640.0);
}

#[test]
fn moved_startup_identity_requires_repair() {
    assert_eq!(
        startup_status(
            true,
            Path::new(r"C:\A\usage-widget.exe"),
            Path::new(r"C:\B\usage-widget.exe")
        ),
        IntegrationStatus::NeedsRepair
    );
}

#[test]
fn capture_mode_is_decided_before_the_gui_path() {
    assert!(capture_mode_from_args([
        "usage-widget.exe",
        "claude-capture"
    ]));
    assert!(!capture_mode_from_args(["usage-widget.exe"]));
}

#[test]
fn restored_position_is_clamped_into_the_nearest_monitor_work_area() {
    let monitors = [
        WorkArea::new(0, 0, 1920, 1040),
        WorkArea::new(1920, 0, 3840, 1040),
    ];

    assert_eq!(
        clamp_position(WindowPlacement { x: 4000, y: 1200 }, 356, 36, &monitors),
        WindowPlacement { x: 3484, y: 1004 }
    );
    assert_eq!(
        clamp_position(WindowPlacement { x: 2000, y: 200 }, 356, 36, &monitors),
        WindowPlacement { x: 2000, y: 200 }
    );
}

#[test]
fn extreme_persisted_position_is_clamped_without_integer_overflow() {
    assert_eq!(
        clamp_position(
            WindowPlacement {
                x: i32::MIN,
                y: i32::MIN
            },
            356,
            36,
            &[WorkArea::new(0, 0, 1920, 1040)]
        ),
        WindowPlacement { x: 0, y: 0 }
    );
}

#[test]
fn enable_persists_the_current_identity_only_after_registration_succeeds() {
    let current = PathBuf::from(r"C:\Current\usage-widget.exe");
    let store = FakeStore::new(PersistedState::default());
    let registration = FakeRegistration {
        enable_result: Mutex::new(Err(StartupError::OperationFailed)),
        enabled: Mutex::new(Ok(true)),
        ..FakeRegistration::default()
    };

    assert_eq!(
        enable_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert!(!store.snapshot().launch_at_signin_requested);
    assert_eq!(store.snapshot().startup_identity, None);

    *registration.enable_result.lock().unwrap() = Ok(());
    assert_eq!(
        enable_startup(&registration, &store, &current, NOW),
        Ok(IntegrationStatus::Enabled)
    );
    assert!(store.snapshot().launch_at_signin_requested);
    assert_eq!(
        store.snapshot().startup_identity,
        Some(StartupIdentity {
            installed_exe: current
        })
    );
}

#[test]
fn disable_clears_the_identity_only_after_registration_succeeds() {
    let installed = PathBuf::from(r"C:\Current\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    let registration = FakeRegistration {
        disable_result: Mutex::new(Err(StartupError::OperationFailed)),
        ..FakeRegistration::default()
    };

    assert_eq!(
        disable_startup(&registration, &store, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(store.snapshot(), requested_state(&installed));

    *registration.disable_result.lock().unwrap() = Ok(());
    assert_eq!(
        disable_startup(&registration, &store, NOW),
        Ok(IntegrationStatus::Disabled)
    );
    assert!(!store.snapshot().launch_at_signin_requested);
    assert_eq!(store.snapshot().startup_identity, None);
}

#[test]
fn repair_enables_and_confirms_without_first_removing_the_previous_registration() {
    let installed = PathBuf::from(r"C:\Old\usage-widget.exe");
    let current = PathBuf::from(r"C:\Current\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    let registration = FakeRegistration {
        enable_result: Mutex::new(Ok(())),
        enabled: Mutex::new(Ok(true)),
        ..FakeRegistration::default()
    };

    assert_eq!(
        repair_startup(&registration, &store, &current, NOW),
        Ok(IntegrationStatus::Enabled)
    );
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "is_enabled"]
    );
    assert_eq!(
        store.snapshot().startup_identity,
        Some(StartupIdentity {
            installed_exe: current
        })
    );
}

#[test]
fn repair_keeps_the_previous_identity_when_confirmation_fails() {
    let installed = PathBuf::from(r"C:\Old\usage-widget.exe");
    let current = PathBuf::from(r"C:\Current\usage-widget.exe");
    let store = FakeStore::new(requested_state(&installed));
    let registration = FakeRegistration {
        enable_result: Mutex::new(Ok(())),
        enabled: Mutex::new(Ok(false)),
        ..FakeRegistration::default()
    };

    assert_eq!(
        repair_startup(&registration, &store, &current, NOW),
        Err(StartupError::OperationFailed)
    );
    assert_eq!(store.snapshot(), requested_state(&installed));
    assert_eq!(
        *registration.operations.lock().unwrap(),
        vec!["snapshot", "enable", "is_enabled", "restore"]
    );
}
