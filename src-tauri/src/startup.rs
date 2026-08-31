use std::path::Path;

use crate::{
    shell::IntegrationStatus,
    state_store::{StartupIdentity, StateMutation, StateStore},
};

pub trait StartupRegistration: Send + Sync {
    fn snapshot(&self) -> Result<StartupRegistrationSnapshot, StartupError>;
    fn restore(&self, snapshot: &StartupRegistrationSnapshot) -> Result<(), StartupError>;
    fn enable(&self) -> Result<(), StartupError>;
    fn disable(&self) -> Result<(), StartupError>;
    fn is_enabled(&self) -> Result<bool, StartupError>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupRegistrationSnapshot {
    pub run_value: Option<Vec<u8>>,
    pub startup_approved_value: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("startup registration is unavailable")]
    Unavailable,
    #[error("startup registration operation failed")]
    OperationFailed,
}

pub fn startup_status(requested: bool, installed: &Path, current: &Path) -> IntegrationStatus {
    if !requested {
        IntegrationStatus::Disabled
    } else if installed == current {
        IntegrationStatus::Enabled
    } else {
        IntegrationStatus::NeedsRepair
    }
}

pub fn enable_startup(
    registration: &dyn StartupRegistration,
    store: &dyn StateStore,
    current: &Path,
    now: i64,
) -> Result<IntegrationStatus, StartupError> {
    let previous = registration.snapshot()?;
    registration.enable()?;
    if persist_startup(store, true, Some(current), now).is_err() {
        registration.restore(&previous)?;
        return Err(StartupError::OperationFailed);
    }
    Ok(IntegrationStatus::Enabled)
}

pub fn disable_startup(
    registration: &dyn StartupRegistration,
    store: &dyn StateStore,
    now: i64,
) -> Result<IntegrationStatus, StartupError> {
    let previous = registration.snapshot()?;
    registration.disable()?;
    if persist_startup(store, false, None, now).is_err() {
        registration.restore(&previous)?;
        return Err(StartupError::OperationFailed);
    }
    Ok(IntegrationStatus::Disabled)
}

pub fn repair_startup(
    registration: &dyn StartupRegistration,
    store: &dyn StateStore,
    current: &Path,
    now: i64,
) -> Result<IntegrationStatus, StartupError> {
    let previous = registration.snapshot()?;
    registration.enable()?;
    if !registration.is_enabled().unwrap_or(false) {
        registration.restore(&previous)?;
        return Err(StartupError::OperationFailed);
    }
    if persist_startup(store, true, Some(current), now).is_err() {
        registration.restore(&previous)?;
        return Err(StartupError::OperationFailed);
    }
    Ok(IntegrationStatus::Enabled)
}

fn persist_startup(
    store: &dyn StateStore,
    requested: bool,
    installed: Option<&Path>,
    now: i64,
) -> Result<(), StartupError> {
    let identity = installed.map(|installed_exe| StartupIdentity {
        installed_exe: installed_exe.to_path_buf(),
    });
    store
        .apply(
            now,
            StateMutation::SetStartup {
                requested,
                identity,
            },
        )
        .map(|_| ())
        .map_err(|_| StartupError::OperationFailed)
}
