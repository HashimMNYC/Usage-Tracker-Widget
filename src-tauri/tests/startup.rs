use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use usage_widget::{
    providers::claude::capture_mode_from_args,
    shell::{clamp_position, height_for_layout, IntegrationStatus, Layout, WorkArea},
    startup::{
        disable_startup, enable_startup, repair_startup, startup_status, StartupError,
        StartupRegistration,
    },
    state_store::{
        PersistedState, StartupIdentity, StateError, StateMutation, StateStore, WindowPlacement,
    },
};

const NOW: i64 = 2_000_000_000;

struct FakeRegistration {
    operations: Mutex<Vec<&'static str>>,
    enable_result: Mutex<Result<(), StartupError>>,
    disable_result: Mutex<Result<(), StartupError>>,
    enabled: Mutex<Result<bool, StartupError>>,
}

impl Default for FakeRegistration {
    fn default() -> Self {
        Self {
            operations: Mutex::new(Vec::new()),
            enable_result: Mutex::new(Ok(())),
            disable_result: Mutex::new(Ok(())),
            enabled: Mutex::new(Ok(false)),
        }
    }
}

impl StartupRegistration for FakeRegistration {
    fn enable(&self) -> Result<(), StartupError> {
        self.operations.lock().unwrap().push("enable");
        *self.enable_result.lock().unwrap()
    }

    fn disable(&self) -> Result<(), StartupError> {
        self.operations.lock().unwrap().push("disable");
        *self.disable_result.lock().unwrap()
    }

    fn is_enabled(&self) -> Result<bool, StartupError> {
        self.operations.lock().unwrap().push("is_enabled");
        *self.enabled.lock().unwrap()
    }
}

struct FakeStore {
    state: Mutex<PersistedState>,
}

impl FakeStore {
    fn new(state: PersistedState) -> Self {
        Self {
            state: Mutex::new(state),
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
        let mut state = self.state.lock().unwrap();
        state.apply_mutation(mutation, now)?;
        Ok(state.clone())
    }
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
fn layout_heights_match_the_three_widget_states() {
    assert_eq!(height_for_layout(Layout::Empty), 102.0);
    assert_eq!(height_for_layout(Layout::Single), 178.0);
    assert_eq!(height_for_layout(Layout::Dual), 254.0);
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
        vec!["enable", "is_enabled"]
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
        vec!["enable", "is_enabled"]
    );
}
