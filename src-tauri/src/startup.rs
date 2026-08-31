use std::path::Path;

use crate::{
    shell::IntegrationStatus,
    state_store::{StartupIdentity, StateMutation, StateStore},
};

pub trait StartupRegistration: Send + Sync {
    fn enable(&self) -> Result<(), StartupError>;
    fn disable(&self) -> Result<(), StartupError>;
    fn is_enabled(&self) -> Result<bool, StartupError>;
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
    registration.enable()?;
    persist_startup(store, true, Some(current), now)?;
    Ok(IntegrationStatus::Enabled)
}

pub fn disable_startup(
    registration: &dyn StartupRegistration,
    store: &dyn StateStore,
    now: i64,
) -> Result<IntegrationStatus, StartupError> {
    registration.disable()?;
    persist_startup(store, false, None, now)?;
    Ok(IntegrationStatus::Disabled)
}

pub fn repair_startup(
    registration: &dyn StartupRegistration,
    store: &dyn StateStore,
    current: &Path,
    now: i64,
) -> Result<IntegrationStatus, StartupError> {
    registration.enable()?;
    if !registration.is_enabled()? {
        return Err(StartupError::OperationFailed);
    }
    persist_startup(store, true, Some(current), now)?;
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
