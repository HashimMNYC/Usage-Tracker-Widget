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
    Globalization::{LCMapStringEx, LCMAP_LOWERCASE, LOCALE_NAME_INVARIANT},
    Security::{GetLengthSid, GetTokenInformation, IsValidSid, TokenUser, TOKEN_QUERY, TOKEN_USER},
    Storage::FileSystem::{MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH},
    System::Threading::{
        CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, WaitForSingleObject,
    },
};

use crate::model::{ProviderId, ProviderSnapshot};

pub const STATE_SCHEMA_VERSION: u32 = 2;
const LEGACY_STATE_SCHEMA_VERSION: u32 = 1;
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
                let is_stale_or_ambiguous = self
                    .snapshots
                    .get(&snapshot.provider)
                    .is_some_and(|stored| stored.observed_at >= snapshot.observed_at);
                if !is_stale_or_ambiguous {
                    self.snapshots.insert(snapshot.provider, snapshot);
                }
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
        let Some(schema_version) = value.get("schema_version").and_then(Value::as_u64) else {
            self.quarantine_corrupt()?;
            return Ok(PersistedState::default());
        };
        let migrate_legacy = match schema_version {
            version if version == u64::from(STATE_SCHEMA_VERSION) => false,
            version if version == u64::from(LEGACY_STATE_SCHEMA_VERSION) => true,
            _ => return Err(StateError::UnsupportedSchema),
        };
        let mut state: PersistedState = match serde_json::from_value(value) {
            Ok(state) => state,
            Err(_) => {
                self.quarantine_corrupt()?;
                return Ok(PersistedState::default());
            }
        };
        if migrate_legacy {
            state.schema_version = STATE_SCHEMA_VERSION;
            state.snapshots.remove(&ProviderId::Codex);
        }
        if state.snapshots.iter().any(|(provider, snapshot)| {
            *provider != snapshot.provider || snapshot.validate(snapshot.observed_at).is_err()
        }) {
            self.quarantine_corrupt()?;
            return Ok(PersistedState::default());
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
        let user_scope = current_user_scope()?;
        let identity = state_mutex_identity(path, &user_scope)?;
        let name = format!("Global\\UsageWidget-State-{identity:032x}");
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

fn normalize_for_mutex(path: &Path) -> Result<Vec<u16>, StateError> {
    let absolute = std::path::absolute(path).map_err(|_| StateError::Io)?;
    let mut units = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(StateError::Invalid);
    }
    normalize_verbatim_prefix(&mut units);
    for unit in &mut units {
        if *unit == u16::from(b'/') {
            *unit = u16::from(b'\\');
        }
    }
    invariant_lowercase(&units)
}

#[doc(hidden)]
pub fn state_mutex_identity(path: &Path, user_scope: &[u8]) -> Result<u128, StateError> {
    if user_scope.is_empty() {
        return Err(StateError::Invalid);
    }
    normalize_for_mutex(path).map(|normalized| hash_mutex_identity(&normalized, user_scope))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    wide(path.as_os_str())
}

fn normalize_verbatim_prefix(path: &mut Vec<u16>) {
    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    if starts_with_ascii_case_insensitive(path, VERBATIM_UNC) {
        path.splice(..VERBATIM_UNC.len(), [b'\\' as u16, b'\\' as u16]);
    } else if path.starts_with(VERBATIM) {
        path.drain(..VERBATIM.len());
    }
}

fn starts_with_ascii_case_insensitive(value: &[u16], prefix: &[u16]) -> bool {
    value.len() >= prefix.len()
        && value
            .iter()
            .zip(prefix)
            .all(|(left, right)| ascii_lower(*left) == ascii_lower(*right))
}

fn ascii_lower(value: u16) -> u16 {
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

fn invariant_lowercase(value: &[u16]) -> Result<Vec<u16>, StateError> {
    let length = i32::try_from(value.len()).map_err(|_| StateError::Invalid)?;
    let required = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_LOWERCASE,
            value.as_ptr(),
            length,
            ptr::null_mut(),
            0,
            ptr::null(),
            ptr::null(),
            0,
        )
    };
    if required == 0 {
        return Err(StateError::Io);
    }
    let mut lowered = vec![0; required as usize];
    let written = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_LOWERCASE,
            value.as_ptr(),
            length,
            lowered.as_mut_ptr(),
            required,
            ptr::null(),
            ptr::null(),
            0,
        )
    };
    if written != required {
        return Err(StateError::Io);
    }
    Ok(lowered)
}

fn hash_mutex_identity(path: &[u16], user_scope: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    let mut update = |byte: u8| {
        hash = (hash ^ u128::from(byte)).wrapping_mul(PRIME);
    };
    for byte in b"UsageWidget state mutex v2" {
        update(*byte);
    }
    for byte in (user_scope.len() as u64).to_le_bytes() {
        update(byte);
    }
    for byte in user_scope {
        update(*byte);
    }
    for byte in (path.len() as u64).to_le_bytes() {
        update(byte);
    }
    for unit in path {
        for byte in unit.to_le_bytes() {
            update(byte);
        }
    }
    hash
}

fn current_user_scope() -> Result<Vec<u8>, StateError> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(StateError::Io);
    }
    let _token = TokenHandle(token);
    let mut required = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(StateError::Io);
    }
    let word_size = std::mem::size_of::<usize>();
    let mut buffer = vec![0usize; (required as usize).div_ceil(word_size)];
    let mut written = required;
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut written,
        )
    } == 0
        || written > required
    {
        return Err(StateError::Io);
    }
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let sid = token_user.User.Sid;
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(StateError::Io);
    }
    let sid_length = unsafe { GetLengthSid(sid) } as usize;
    if sid_length == 0 {
        return Err(StateError::Io);
    }
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), sid_length) }.to_vec())
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
