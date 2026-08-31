use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Storage::FileSystem::{MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH},
    System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
};

use crate::model::{ProviderId, ProviderSnapshot};

pub const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const LOCK_WAIT_MILLISECONDS: u32 = 5_000;
static QUARANTINE_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowPlacement {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupIdentity {
    pub installed_exe: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClaudeTrackingIdentity {
    pub installed_exe: PathBuf,
    pub installed_status_line: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PersistedState {
    pub schema_version: u32,
    pub snapshots: BTreeMap<ProviderId, ProviderSnapshot>,
    pub window: Option<WindowPlacement>,
    pub always_on_top: bool,
    pub launch_at_signin_requested: bool,
    pub startup_identity: Option<StartupIdentity>,
    pub claude_tracking: Option<ClaudeTrackingIdentity>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            snapshots: BTreeMap::new(),
            window: None,
            always_on_top: true,
            launch_at_signin_requested: false,
            startup_identity: None,
            claude_tracking: None,
        }
    }
}

impl PersistedState {
    pub fn current_snapshots(&self, now: i64) -> BTreeMap<ProviderId, ProviderSnapshot> {
        self.snapshots
            .iter()
            .filter(|(_, snapshot)| snapshot.is_current(now))
            .map(|(provider, snapshot)| (*provider, snapshot.clone()))
            .collect()
    }

    pub fn apply_mutation(&mut self, mutation: StateMutation, now: i64) -> Result<(), StateError> {
        match mutation {
            StateMutation::UpsertSnapshot(snapshot) => {
                snapshot.validate(now).map_err(|_| StateError::Invalid)?;
                self.snapshots.insert(snapshot.provider, snapshot);
            }
            StateMutation::SetWindow(window) => self.window = window,
            StateMutation::SetAlwaysOnTop(always_on_top) => {
                self.always_on_top = always_on_top;
            }
            StateMutation::SetStartup {
                requested,
                identity,
            } => {
                self.launch_at_signin_requested = requested;
                self.startup_identity = identity;
            }
            StateMutation::SetClaudeTracking(identity) => self.claude_tracking = identity,
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum StateMutation {
    UpsertSnapshot(ProviderSnapshot),
    SetWindow(Option<WindowPlacement>),
    SetAlwaysOnTop(bool),
    SetStartup {
        requested: bool,
        identity: Option<StartupIdentity>,
    },
    SetClaudeTracking(Option<ClaudeTrackingIdentity>),
}

pub trait StateStore: Send + Sync {
    fn load(&self, now: i64) -> Result<PersistedState, StateError>;
    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("local state directory is unavailable")]
    DirectoryUnavailable,
    #[error("local state is oversized")]
    Oversized,
    #[error("local state is invalid")]
    Invalid,
    #[error("local state schema is unsupported")]
    UnsupportedSchema,
    #[error("local state lock timed out")]
    LockTimeout,
    #[error("local state operation failed")]
    Io,
}

pub trait AtomicReplace: Send + Sync {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()>;
}

struct WindowsAtomicReplace;

impl AtomicReplace for WindowsAtomicReplace {
    fn replace(&self, temporary: &Path, destination: &Path) -> std::io::Result<()> {
        let destination_exists = destination.exists();
        let temporary = wide_path(temporary);
        let destination = wide_path(destination);
        let replaced = unsafe {
            if destination_exists {
                ReplaceFileW(
                    destination.as_ptr(),
                    temporary.as_ptr(),
                    ptr::null(),
                    0,
                    ptr::null(),
                    ptr::null(),
                )
            } else {
                MoveFileExW(
                    temporary.as_ptr(),
                    destination.as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                )
            }
        };
        if replaced == 0 {
            Err(std::io::Error::from_raw_os_error(unsafe {
                GetLastError() as i32
            }))
        } else {
            Ok(())
        }
    }
}

pub struct JsonStateStore {
    path: PathBuf,
    replacer: Arc<dyn AtomicReplace>,
}

impl JsonStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self::with_replacer(path, Arc::new(WindowsAtomicReplace))
    }

    pub fn with_replacer(path: PathBuf, replacer: Arc<dyn AtomicReplace>) -> Self {
        Self { path, replacer }
    }

    fn load_unlocked(&self) -> Result<PersistedState, StateError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PersistedState::default());
            }
            Err(_) => return Err(StateError::Io),
        };
        if file.metadata().map_err(|_| StateError::Io)?.len() > MAX_STATE_BYTES {
            return Err(StateError::Oversized);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| StateError::Io)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(StateError::Oversized);
        }

        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                self.quarantine_corrupt()?;
                return Ok(PersistedState::default());
            }
        };
        let schema_version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or(StateError::Invalid)?;
        if schema_version != u64::from(STATE_SCHEMA_VERSION) {
            return Err(StateError::UnsupportedSchema);
        }
        let state: PersistedState =
            serde_json::from_value(value).map_err(|_| StateError::Invalid)?;
        if state.snapshots.iter().any(|(provider, snapshot)| {
            *provider != snapshot.provider || snapshot.validate(snapshot.observed_at).is_err()
        }) {
            return Err(StateError::Invalid);
        }
        Ok(state)
    }

    fn persist_unlocked(&self, state: &PersistedState) -> Result<(), StateError> {
        let parent = self.path.parent().ok_or(StateError::DirectoryUnavailable)?;
        fs::create_dir_all(parent).map_err(|_| StateError::DirectoryUnavailable)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| StateError::Io)?;
        serde_json::to_writer(temporary.as_file_mut(), state).map_err(|_| StateError::Invalid)?;
        temporary
            .as_file_mut()
            .flush()
            .map_err(|_| StateError::Io)?;
        temporary.as_file().sync_all().map_err(|_| StateError::Io)?;
        let (file, temporary_path) = temporary.keep().map_err(|_| StateError::Io)?;
        drop(file);

        let result = self.replacer.replace(&temporary_path, &self.path);
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result.map_err(|_| StateError::Io)
    }

    fn quarantine_corrupt(&self) -> Result<(), StateError> {
        let parent = self.path.parent().ok_or(StateError::DirectoryUnavailable)?;
        let stem = self
            .path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or(StateError::Invalid)?;
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StateError::Io)?
            .as_nanos();
        loop {
            let nonce = QUARANTINE_NONCE.fetch_add(1, Ordering::Relaxed);
            let suffix = epoch.saturating_add(u128::from(nonce));
            let quarantine = parent.join(format!("{stem}.corrupt.{suffix}.json"));
            if quarantine.exists() {
                continue;
            }
            match fs::rename(&self.path, quarantine) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(StateError::Io),
            }
        }
    }
}

impl StateStore for JsonStateStore {
    fn load(&self, _now: i64) -> Result<PersistedState, StateError> {
        let _guard = StateMutexGuard::acquire(&self.path)?;
        self.load_unlocked()
    }

    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError> {
        let _guard = StateMutexGuard::acquire(&self.path)?;
        let mut state = self.load_unlocked()?;
        state.apply_mutation(mutation, now)?;
        self.persist_unlocked(&state)?;
        Ok(state)
    }
}

pub fn default_state_path() -> Result<PathBuf, StateError> {
    dirs::data_local_dir()
        .map(|directory| directory.join("UsageWidget").join("state.json"))
        .ok_or(StateError::DirectoryUnavailable)
}

struct StateMutexGuard {
    handle: HANDLE,
    acquired: bool,
}

impl StateMutexGuard {
    fn acquire(path: &Path) -> Result<Self, StateError> {
        let normalized = normalize_for_mutex(path)?;
        let name = format!(
            "Global\\UsageWidget-State-{:016x}",
            deterministic_path_hash(&normalized)
        );
        let wide_name = wide(OsStr::new(&name));
        let handle = unsafe { CreateMutexW(ptr::null(), 0, wide_name.as_ptr()) };
        if handle.is_null() {
            return Err(StateError::Io);
        }
        let mut guard = Self {
            handle,
            acquired: false,
        };
        match unsafe { WaitForSingleObject(handle, LOCK_WAIT_MILLISECONDS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                guard.acquired = true;
                Ok(guard)
            }
            WAIT_TIMEOUT => Err(StateError::LockTimeout),
            _ => Err(StateError::Io),
        }
    }
}

impl Drop for StateMutexGuard {
    fn drop(&mut self) {
        unsafe {
            if self.acquired {
                ReleaseMutex(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

fn normalize_for_mutex(path: &Path) -> Result<String, StateError> {
    let absolute = std::path::absolute(path).map_err(|_| StateError::Io)?;
    let normalized = match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(file_name)) => parent
            .canonicalize()
            .map(|parent| parent.join(file_name))
            .unwrap_or(absolute),
        _ => absolute,
    };
    Ok(normalized
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase())
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    wide(path.as_os_str())
}

fn deterministic_path_hash(path: &str) -> u64 {
    path.encode_utf16()
        .flat_map(u16::to_le_bytes)
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}
