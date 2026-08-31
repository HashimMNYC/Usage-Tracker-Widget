use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::Arc,
};

use serde_json::{Map, Value};
use windows_sys::Win32::{
    Foundation::GetLastError,
    Storage::FileSystem::{MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH},
};

use crate::state_store::{ClaudeTrackingIdentity, StateMutation, StateStore};

pub const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeTrackingState {
    Disabled,
    Enabled,
    NeedsRepair,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeSetupError {
    #[error("Claude settings are missing")]
    SettingsMissing,
    #[error("Claude settings are invalid")]
    SettingsInvalid,
    #[error("Claude status line is already configured")]
    SettingsConflict,
    #[error("Claude settings changed during update")]
    SettingsChanged,
    #[error("the executable path is unsafe for a status command")]
    UnsafeExecutablePath,
    #[error("Claude settings could not be updated")]
    SettingsWriteFailed,
}

pub struct ClaudeSettingsManager {
    settings_path: PathBuf,
    store: Arc<dyn StateStore>,
}

impl ClaudeSettingsManager {
    pub fn new(settings_path: PathBuf, store: Arc<dyn StateStore>) -> Self {
        Self {
            settings_path,
            store,
        }
    }

    pub fn from_environment(store: Arc<dyn StateStore>) -> Result<Self, ClaudeSetupError> {
        let settings_path = match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(directory) if !directory.is_empty() => {
                PathBuf::from(directory).join("settings.json")
            }
            _ => std::env::var_os("USERPROFILE")
                .filter(|profile| !profile.is_empty())
                .map(PathBuf::from)
                .map(|profile| profile.join(".claude").join("settings.json"))
                .ok_or(ClaudeSetupError::SettingsMissing)?,
        };
        Ok(Self::new(settings_path, store))
    }

    pub fn enable(&self, exe: &Path, now: i64) -> Result<ClaudeTrackingState, ClaudeSetupError> {
        let installed_exe = normalized_executable(exe)?;
        let (original_bytes, mut settings) = read_settings(&self.settings_path)?;
        if settings
            .get("statusLine")
            .is_some_and(|value| !value.is_null())
        {
            return Err(ClaudeSetupError::SettingsConflict);
        }
        let state = self
            .store
            .load(now)
            .map_err(|_| ClaudeSetupError::SettingsWriteFailed)?;
        if state.claude_tracking.is_some() {
            return Err(ClaudeSetupError::SettingsConflict);
        }
        let status_line = status_line_for(&installed_exe)?;
        settings.insert("statusLine".into(), status_line.clone());
        let updated_bytes = serialize_settings(&settings)?;
        require_unchanged(&self.settings_path, &original_bytes)?;
        create_backup(&self.settings_path, &original_bytes, now)?;
        replace_if_unchanged(&self.settings_path, &original_bytes, &updated_bytes)?;
        let identity = Some(ClaudeTrackingIdentity {
            installed_exe,
            installed_status_line: status_line,
        });
        self.commit_identity_or_rollback(now, identity, &original_bytes, &updated_bytes)?;
        Ok(ClaudeTrackingState::Enabled)
    }

    pub fn disable(&self, now: i64) -> Result<ClaudeTrackingState, ClaudeSetupError> {
        let (original_bytes, mut settings) = read_settings(&self.settings_path)?;
        let state = self
            .store
            .load(now)
            .map_err(|_| ClaudeSetupError::SettingsWriteFailed)?;
        let Some(identity) = state.claude_tracking else {
            return if settings.get("statusLine").is_none_or(Value::is_null) {
                Ok(ClaudeTrackingState::Disabled)
            } else {
                Err(ClaudeSetupError::SettingsConflict)
            };
        };
        if settings.get("statusLine") != Some(&identity.installed_status_line) {
            return Err(ClaudeSetupError::SettingsConflict);
        }

        settings.remove("statusLine");
        let updated_bytes = serialize_settings(&settings)?;
        replace_if_unchanged(&self.settings_path, &original_bytes, &updated_bytes)?;
        self.commit_identity_or_rollback(now, None, &original_bytes, &updated_bytes)?;
        Ok(ClaudeTrackingState::Disabled)
    }

    pub fn repair(&self, exe: &Path, now: i64) -> Result<ClaudeTrackingState, ClaudeSetupError> {
        let installed_exe = normalized_executable(exe)?;
        let (original_bytes, mut settings) = read_settings(&self.settings_path)?;
        let state = self
            .store
            .load(now)
            .map_err(|_| ClaudeSetupError::SettingsWriteFailed)?;
        let identity = state
            .claude_tracking
            .ok_or(ClaudeSetupError::SettingsConflict)?;
        if settings.get("statusLine") != Some(&identity.installed_status_line) {
            return Err(ClaudeSetupError::SettingsConflict);
        }

        let status_line = status_line_for(&installed_exe)?;
        settings.insert("statusLine".into(), status_line.clone());
        let updated_bytes = serialize_settings(&settings)?;
        replace_if_unchanged(&self.settings_path, &original_bytes, &updated_bytes)?;
        self.commit_identity_or_rollback(
            now,
            Some(ClaudeTrackingIdentity {
                installed_exe,
                installed_status_line: status_line,
            }),
            &original_bytes,
            &updated_bytes,
        )?;
        Ok(ClaudeTrackingState::Enabled)
    }

    pub fn status(&self, exe: &Path, now: i64) -> ClaudeTrackingState {
        let Ok(installed_exe) = normalized_executable(exe) else {
            return ClaudeTrackingState::Conflict;
        };
        let Ok((_, settings)) = read_settings(&self.settings_path) else {
            return ClaudeTrackingState::Conflict;
        };
        let Ok(state) = self.store.load(now) else {
            return ClaudeTrackingState::Conflict;
        };
        let Some(identity) = state.claude_tracking else {
            return if settings.get("statusLine").is_none_or(Value::is_null) {
                ClaudeTrackingState::Disabled
            } else {
                ClaudeTrackingState::Conflict
            };
        };
        if settings.get("statusLine") != Some(&identity.installed_status_line) {
            return ClaudeTrackingState::Conflict;
        }
        let Ok(expected_status_line) = status_line_for(&installed_exe) else {
            return ClaudeTrackingState::Conflict;
        };
        if identity.installed_exe == installed_exe
            && identity.installed_status_line == expected_status_line
        {
            ClaudeTrackingState::Enabled
        } else {
            ClaudeTrackingState::NeedsRepair
        }
    }

    fn commit_identity_or_rollback(
        &self,
        now: i64,
        identity: Option<ClaudeTrackingIdentity>,
        original_bytes: &[u8],
        updated_bytes: &[u8],
    ) -> Result<(), ClaudeSetupError> {
        if self
            .store
            .apply(now, StateMutation::SetClaudeTracking(identity))
            .is_ok()
        {
            return Ok(());
        }

        let current =
            read_bounded(&self.settings_path).map_err(|_| ClaudeSetupError::SettingsChanged)?;
        if current != updated_bytes {
            return Err(ClaudeSetupError::SettingsChanged);
        }
        replace_if_unchanged(&self.settings_path, updated_bytes, original_bytes)?;
        Err(ClaudeSetupError::SettingsWriteFailed)
    }
}

fn read_settings(path: &Path) -> Result<(Vec<u8>, Map<String, Value>), ClaudeSetupError> {
    let bytes = read_bounded(path)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| ClaudeSetupError::SettingsInvalid)?;
    let object = value
        .as_object()
        .cloned()
        .ok_or(ClaudeSetupError::SettingsInvalid)?;
    Ok((bytes, object))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ClaudeSetupError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClaudeSetupError::SettingsMissing)
        }
        Err(_) => return Err(ClaudeSetupError::SettingsInvalid),
    };
    if file
        .metadata()
        .map_err(|_| ClaudeSetupError::SettingsInvalid)?
        .len()
        > MAX_SETTINGS_BYTES as u64
    {
        return Err(ClaudeSetupError::SettingsInvalid);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_SETTINGS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ClaudeSetupError::SettingsInvalid)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(ClaudeSetupError::SettingsInvalid);
    }
    Ok(bytes)
}

fn normalized_executable(exe: &Path) -> Result<PathBuf, ClaudeSetupError> {
    let absolute = std::path::absolute(exe).map_err(|_| ClaudeSetupError::UnsafeExecutablePath)?;
    let display = absolute
        .as_os_str()
        .to_str()
        .ok_or(ClaudeSetupError::UnsafeExecutablePath)?;
    if display.chars().any(|character| {
        matches!(
            character,
            '\"' | '\r' | '\n' | '%' | '!' | '^' | '&' | '|' | '<' | '>'
        )
    }) {
        return Err(ClaudeSetupError::UnsafeExecutablePath);
    }
    Ok(absolute)
}

fn status_line_for(exe: &Path) -> Result<Value, ClaudeSetupError> {
    let path = exe
        .as_os_str()
        .to_str()
        .ok_or(ClaudeSetupError::UnsafeExecutablePath)?;
    Ok(serde_json::json!({
        "type": "command",
        "command": format!("\"{path}\" claude-capture")
    }))
}

fn serialize_settings(settings: &Map<String, Value>) -> Result<Vec<u8>, ClaudeSetupError> {
    let bytes = serde_json::to_vec(&Value::Object(settings.clone()))
        .map_err(|_| ClaudeSetupError::SettingsWriteFailed)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(ClaudeSetupError::SettingsWriteFailed);
    }
    Ok(bytes)
}

fn require_unchanged(path: &Path, expected: &[u8]) -> Result<(), ClaudeSetupError> {
    match read_bounded(path) {
        Ok(current) if current == expected => Ok(()),
        _ => Err(ClaudeSetupError::SettingsChanged),
    }
}

fn create_backup(path: &Path, bytes: &[u8], now: i64) -> Result<(), ClaudeSetupError> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(ClaudeSetupError::SettingsWriteFailed)?;
    let backup_path = path.with_file_name(format!("{file_name}.backup.{now}"));
    let mut backup = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup_path)
        .map_err(|_| ClaudeSetupError::SettingsWriteFailed)?;
    backup
        .write_all(bytes)
        .and_then(|_| backup.flush())
        .and_then(|_| backup.sync_all())
        .map_err(|_| ClaudeSetupError::SettingsWriteFailed)
}

fn replace_if_unchanged(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
) -> Result<(), ClaudeSetupError> {
    require_unchanged(path, expected)?;
    atomic_replace_bytes(path, replacement).map_err(|_| ClaudeSetupError::SettingsWriteFailed)
}

fn atomic_replace_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("settings path has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.as_file_mut().write_all(bytes)?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    let (file, temporary_path) = temporary.keep().map_err(|error| error.error)?;
    drop(file);

    let temporary_wide = wide_path(&temporary_path);
    let destination_wide = wide_path(path);
    let replaced = unsafe {
        if path.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temporary_wide.as_ptr(),
                ptr::null(),
                0,
                ptr::null(),
                ptr::null(),
            )
        } else {
            MoveFileExW(
                temporary_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        let error = std::io::Error::from_raw_os_error(unsafe { GetLastError() as i32 });
        let _ = fs::remove_file(temporary_path);
        Err(error)
    } else {
        Ok(())
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
