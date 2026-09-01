# Usage Widget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and verify a portable Windows 11 x64 Tauri widget that displays exact locally observed Claude Code and Codex five-hour and seven-day limits as remaining percentages and reset countdowns.

**Architecture:** A Rust core owns provider parsing, validation, atomic state, filesystem watching, Claude settings integration, window/tray behavior, and autostart. A static embedded HTML/CSS/JavaScript frontend receives only normalized numeric view data through four narrow Tauri commands and renders the approved terminal-style interface without a web server or network access.

**Tech Stack:** Rust 1.97.1, Tauri 2.11, vanilla ES modules, Node 24 built-in test runner, Windows WebView2, `notify` 8, Serde, and Windows file-replacement APIs.

**Spec:** `docs/superpowers/specs/2026-08-31-usage-widget-design.md`

## Global Constraints

- Target Windows 11 x64 and produce one portable `usage-widget.exe`; do not create an installer.
- Do not add HTTP listeners, provider SDKs, API keys, OAuth, cookies, telemetry, analytics, update checks, remote assets, or application-originated network requests.
- Read only local Codex rollout JSONL and opt-in Claude status-line stdin.
- Never persist, display, log, fixture, or return prompts, transcripts, credentials, account data, raw provider records, or source paths.
- Show a provider only when both exact 300-minute and 10,080-minute windows validate and have future reset timestamps.
- Derive remaining percentage as `round(clamp(100 - used_percent, 0, 100))`; never estimate from token counts.
- Keep `usagewidget.py` unchanged as an unshipped reference until Tauri parity is verified.
- Treat the approved Unicode box-drawing and block glyphs as the intended terminal-style interface.
- Claude configuration and launch-at-sign-in are opt-in. Automated tests must use temporary files/fakes and must not alter the real Claude settings or Windows startup registration.
- Use `apply_patch` for source/document edits, preserve unrelated work, and commit after every task passes its focused verification.

## File map

```text
.gitignore                              generated/build exclusions
package.json                            Node tests and pinned Tauri CLI
package-lock.json                       reproducible CLI dependency lock
README.md                               end-user launch and controls
USAGE-WIDGET-README.md                  unchanged Python prototype documentation
usagewidget.py                          unchanged Python prototype
assets/icon.svg                         source for the pixel tray/app icon
docs/PRIVACY.md                         local data and credential boundary
docs/RELEASE.md                         portable EXE and WebView2 caveats
docs/smoke-checklist.md                 final manual acceptance sequence
scripts/check-no-network.ps1            read-only process-tree connection check
tests/frontend/ui-model.test.mjs        pure presentation tests
ui/index.html                           embedded document and accessibility structure
ui/styles.css                           approved terminal/8-bit presentation
ui/ui-model.js                          pure meter/countdown/visibility functions
ui/render.js                            safe DOM construction using textContent
ui/bridge.js                            only JavaScript-to-Tauri boundary
ui/app.js                               timers, rendering, hide, and layout lifecycle
src-tauri/Cargo.toml                    Rust dependencies and release profile
src-tauri/Cargo.lock                    reproducible Rust dependency lock
src-tauri/build.rs                      Tauri build hook
src-tauri/tauri.conf.json               window, CSP, static UI, no bundle
src-tauri/capabilities/default.json     main-window core IPC capability only
src-tauri/icons/*                       generated local application icons
src-tauri/src/main.rs                   GUI versus claude-capture mode dispatch
src-tauri/src/lib.rs                    Tauri builder and module exports
src-tauri/src/model.rs                  normalized snapshots and validation
src-tauri/src/paths.rs                  local path resolution without secret reads
src-tauri/src/diagnostics.rs            coarse non-sensitive status codes
src-tauri/src/providers/mod.rs          provider adapter interfaces
src-tauri/src/providers/codex.rs        bounded Codex discovery and JSONL parsing
src-tauri/src/providers/claude.rs       bounded status-line parsing and capture output
src-tauri/src/state_store.rs            persisted state and atomic replacement
src-tauri/src/claude_settings.rs        enable/disable/repair ownership logic
src-tauri/src/coordinator.rs            refresh, watcher, debounce, and fallback scan
src-tauri/src/startup.rs                autostart identity and repair abstraction
src-tauri/src/shell.rs                  window, tray, commands, and shutdown
src-tauri/tests/model.rs                model validation tests
src-tauri/tests/codex.rs                synthetic Codex adapter tests
src-tauri/tests/state_store.rs          atomic/corrupt-state tests
src-tauri/tests/claude.rs               capture and settings tests
src-tauri/tests/coordinator.rs          deterministic scheduler tests
src-tauri/tests/startup.rs              fake startup-registration tests
```

---

### Task 1: Rust core scaffold and exact snapshot model

**Files:**
- Create: `.gitignore`
- Create: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/lib.rs`
- Create: `src-tauri/src/model.rs`
- Create: `src-tauri/src/diagnostics.rs`
- Create: `src-tauri/tests/model.rs`
- Track unchanged: `usagewidget.py`
- Track unchanged: `USAGE-WIDGET-README.md`

**Interfaces:**
- Consumes: the constants and validation rules in the approved spec.
- Produces: `ProviderId`, `WindowSnapshot`, `ProviderSnapshot`, `ValidationError`, `remaining_percent`, and `DiagnosticCode` for every later task.

- [ ] **Step 1: Create the manifest, ignore rules, and failing model tests**

Use this dependency set in `src-tauri/Cargo.toml`; the first successful dependency resolution creates `Cargo.lock`, which is committed:

```toml
[package]
name = "usage-widget"
version = "0.1.0"
edition = "2021"

[lib]
name = "usage_widget"
path = "src/lib.rs"

[dependencies]
chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }
dirs = "6.0.0"
notify = "8.2.0"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tempfile = "3.27.0"
thiserror = "2"
walkdir = "2.5.0"
windows-sys = { version = "0.61.2", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_Threading"] }

[profile.release]
codegen-units = 1
lto = "thin"
opt-level = "s"
strip = "symbols"
```

Use these ignore entries:

```gitignore
/node_modules/
/src-tauri/target/
/src-tauri/gen/
/release/
/.firecrawl/
*.log
```

Create `src-tauri/src/lib.rs` with `pub mod diagnostics; pub mod model;`. In `src-tauri/tests/model.rs`, define a complete snapshot fixture and assert:

```rust
use usage_widget::model::{
    remaining_percent, ProviderId, ProviderSnapshot, ValidationError, WindowSnapshot,
};

const NOW: i64 = 2_000_000_000;

fn valid_snapshot() -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at: NOW - 10,
        short_window: WindowSnapshot {
            duration_minutes: 300,
            used_percent: 38.4,
            resets_at: NOW + 3_600,
        },
        weekly_window: WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 62.0,
            resets_at: NOW + 86_400,
        },
    }
}

#[test]
fn validates_both_exact_windows_and_derives_remaining() {
    assert_eq!(valid_snapshot().validate(NOW), Ok(()));
    assert_eq!(remaining_percent(38.4), 62);
    assert_eq!(remaining_percent(100.0), 0);
}

#[test]
fn rejects_expired_or_wrong_duration_windows() {
    let mut expired = valid_snapshot();
    expired.short_window.resets_at = NOW;
    assert_eq!(expired.validate(NOW), Err(ValidationError::ExpiredReset));

    let mut wrong = valid_snapshot();
    wrong.weekly_window.duration_minutes = 1_440;
    assert_eq!(wrong.validate(NOW), Err(ValidationError::WrongDuration));
}

#[test]
fn rejects_non_finite_and_out_of_range_percentages() {
    for value in [f64::NAN, f64::INFINITY, -0.1, 100.1] {
        let mut snapshot = valid_snapshot();
        snapshot.short_window.used_percent = value;
        assert_eq!(snapshot.validate(NOW), Err(ValidationError::InvalidPercent));
    }
}
```

- [ ] **Step 2: Run the focused tests and confirm the intended failure**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test model`

Expected: compilation fails because `model` and `diagnostics` do not yet define the imported types.

- [ ] **Step 3: Implement the model and diagnostic vocabulary**

Implement these exact public definitions in `model.rs`:

```rust
use serde::{Deserialize, Serialize};

pub const SHORT_WINDOW_MINUTES: u32 = 300;
pub const WEEKLY_WINDOW_MINUTES: u32 = 10_080;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId { Codex, Claude }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowSnapshot {
    pub duration_minutes: u32,
    pub used_percent: f64,
    pub resets_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderSnapshot {
    pub provider: ProviderId,
    pub observed_at: i64,
    pub short_window: WindowSnapshot,
    pub weekly_window: WindowSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("invalid observation time")] InvalidObservedAt,
    #[error("unexpected window duration")] WrongDuration,
    #[error("invalid percentage")] InvalidPercent,
    #[error("reset has expired")] ExpiredReset,
}

impl ProviderSnapshot {
    pub fn validate(&self, now: i64) -> Result<(), ValidationError> {
        if self.observed_at <= 0 { return Err(ValidationError::InvalidObservedAt); }
        for (window, expected) in [
            (&self.short_window, SHORT_WINDOW_MINUTES),
            (&self.weekly_window, WEEKLY_WINDOW_MINUTES),
        ] {
            if window.duration_minutes != expected { return Err(ValidationError::WrongDuration); }
            if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) {
                return Err(ValidationError::InvalidPercent);
            }
            if window.resets_at <= now { return Err(ValidationError::ExpiredReset); }
        }
        Ok(())
    }

    pub fn is_current(&self, now: i64) -> bool { self.validate(now).is_ok() }
}

pub fn remaining_percent(used_percent: f64) -> u8 {
    (100.0 - used_percent).clamp(0.0, 100.0).round() as u8
}
```

Define `DiagnosticCode` in `diagnostics.rs` as a snake-case Serde enum containing only the categories listed by the design: `NoFiles`, `NoExactLimits`, `ExpiredSnapshot`, `SourceUnreadable`, `OversizedRecord`, `MalformedRecord`, `InvalidSchema`, `AmbiguousWindow`, `WatcherUnavailable`, `WatcherOverflow`, `CorruptState`, `StateWriteFailed`, `ClaudeDisabled`, `ClaudeInputInvalid`, `ClaudeInputOversized`, `SettingsMissing`, `SettingsInvalid`, `SettingsConflict`, `SettingsChangedDuringUpdate`, `SettingsWriteFailed`, `StartupUnavailable`, `StartupNeedsRepair`, and `StartupWriteFailed`.

- [ ] **Step 4: Run model tests and formatting**

Run: `cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check`

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test model`

Expected: all model tests pass.

- [ ] **Step 5: Commit the model scaffold**

```powershell
git add -A -- .gitignore src-tauri usagewidget.py USAGE-WIDGET-README.md
git commit -m "feat: add validated usage snapshot model"
```

---

### Task 2: Bounded Codex collector

**Files:**
- Create: `src-tauri/src/paths.rs`
- Create: `src-tauri/src/providers/mod.rs`
- Create: `src-tauri/src/providers/codex.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/codex.rs`

**Interfaces:**
- Consumes: `ProviderSnapshot`, `WindowSnapshot`, `ProviderId`, and `DiagnosticCode` from Task 1.
- Produces: `CollectResult`, `CodexCollector::initial_scan`, `CodexCollector::refresh_changed`, `CodexCollector::full_rescan`, `read_jsonl_reverse`, and `extract_codex_snapshot`.

- [ ] **Step 1: Write synthetic failing tests for complete, ambiguous, partial, and oversized data**

Use only generated temporary files. The central accepted record must have this shape:

```rust
let complete = serde_json::json!({
    "timestamp": "2033-05-18T03:33:10Z",
    "payload": {
        "type": "token_count",
        "rate_limits": {
            "secondary": {"used_percent": 62.0, "window_minutes": 10080, "resets_at": NOW + 86400},
            "primary": {"used_percent": 38.4, "window_minutes": 300, "resets_at": NOW + 3600}
        }
    }
});
let snapshot = extract_codex_snapshot(&complete, NOW - 10, NOW).unwrap();
assert_eq!(snapshot.provider, ProviderId::Codex);
assert_eq!(snapshot.short_window.duration_minutes, 300);
assert_eq!(snapshot.weekly_window.duration_minutes, 10_080);
```

Add tests that verify:

- reversed key order still classifies by duration;
- a record with two 300-minute windows returns `ExtractError::AmbiguousWindow`;
- a partial final JSONL line is ignored and the preceding complete record is returned;
- numeric milliseconds and RFC 3339 reset timestamps normalize correctly;
- a record over 64 KiB is skipped without returning its body;
- a newer malformed record cannot mask an older valid current snapshot;
- discovery searches only `sessions` and `archived_sessions`, sorts by modification time, and caps at 128 files.

- [ ] **Step 2: Run the Codex tests and confirm they fail**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test codex`

Expected: compilation fails because the provider modules and collector interfaces do not exist.

- [ ] **Step 3: Implement path resolution and bounded reverse JSONL reading**

Implement these constants and signatures:

```rust
pub const MAX_CANDIDATE_FILES: usize = 128;
pub const MAX_JSONL_TAIL_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_JSONL_RECORD_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct CandidateFile { pub path: std::path::PathBuf, pub modified_at: i64 }

#[derive(Clone, Debug)]
pub struct ReverseReadResult {
    pub records: Vec<serde_json::Value>,
    pub diagnostics: Vec<DiagnosticCode>,
}

#[derive(Clone, Debug)]
pub struct CollectResult {
    pub snapshot: Option<ProviderSnapshot>,
    pub diagnostic: Option<DiagnosticCode>,
}

pub fn resolve_codex_roots(codex_home: Option<&std::ffi::OsStr>, user_profile: &std::path::Path)
    -> Vec<std::path::PathBuf>;
pub fn discover_candidate_files(roots: &[std::path::PathBuf]) -> Vec<CandidateFile>;
pub fn read_jsonl_reverse(path: &std::path::Path) -> ReverseReadResult;
```

For `read_jsonl_reverse`, seek to `max(file_len - MAX_JSONL_TAIL_BYTES, 0)`, discard the first partial line when seeking into a file, split remaining bytes from the end, reject a line before parsing when it exceeds `MAX_JSONL_RECORD_BYTES`, and accept an unterminated final line only when it is valid complete JSON. Return parsed `serde_json::Value` records and coarse diagnostic codes; never return record text.

- [ ] **Step 4: Implement exact window extraction and newest-valid selection**

Use these public APIs:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractError { MissingRateLimits, MissingWindow, AmbiguousWindow, InvalidField, Expired }

pub fn extract_codex_snapshot(
    record: &serde_json::Value,
    observed_at_fallback: i64,
    now: i64,
) -> Result<ProviderSnapshot, ExtractError>;

pub struct CodexCollector { roots: Vec<std::path::PathBuf> }

impl CodexCollector {
    pub fn new(roots: Vec<std::path::PathBuf>) -> Self;
    pub fn initial_scan(&self, now: i64) -> CollectResult;
    pub fn refresh_changed(&self, paths: &std::collections::BTreeSet<std::path::PathBuf>, now: i64)
        -> CollectResult;
    pub fn full_rescan(&self, now: i64) -> CollectResult;
}
```

Traverse only a record's `payload.rate_limits` and its `primary`/`secondary` children, plus documented field-name variants. Accept used percentage keys `used_percent`, `usedPercentage`, `used_percentage`, `percent_used`, and `percentUsed`; duration keys `window_minutes`, `windowMinutes`, `window_duration_mins`, `windowDurationMins`, and `window_duration_minutes`; reset keys `resets_at`, `resetsAt`, `reset_at`, and `resetAt`. Accept epoch seconds, epoch milliseconds, or RFC 3339 for reset times. Do not infer relative reset durations or fall back to object order.

Classify exactly one 300-minute and one 10,080-minute window. Validate the completed snapshot and select the valid candidate with the newest `observed_at`, using modification time and normalized path only as deterministic tie breakers.

- [ ] **Step 5: Run the focused and core suites**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test codex --test model`

Expected: all tests pass without reading the real user profile.

- [ ] **Step 6: Commit the Codex adapter**

```powershell
git add -A -- src-tauri/src src-tauri/tests
git commit -m "feat: add bounded Codex limit collector"
```

---

### Task 3: Atomic state and current-snapshot projection

**Files:**
- Create: `src-tauri/src/state_store.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/state_store.rs`

**Interfaces:**
- Consumes: validated snapshots and provider IDs from Task 1.
- Produces: `PersistedState`, `JsonStateStore`, `StateStore`, `WindowPlacement`, `ClaudeTrackingIdentity`, and `StartupIdentity`.

- [ ] **Step 1: Write failing state tests**

Create tests using `tempfile::TempDir` that prove:

```rust
let store = JsonStateStore::new(temp.path().join("state.json"));
store.apply(NOW, StateMutation::UpsertSnapshot(valid_snapshot())).unwrap();
assert_eq!(store.load(NOW).unwrap().snapshots[&ProviderId::Codex], valid_snapshot());
```

Also verify that an invalid candidate cannot replace a current snapshot, an expired stored snapshot is omitted by `current_snapshots(NOW)`, corrupt JSON is moved to a `state.corrupt.<epoch>.json` sibling and yields defaults, and an injected replacement failure leaves the original bytes unchanged. Start two stores against the same temporary path on separate threads, update different fields, and prove both changes survive; this is the regression test for GUI/capture lost updates.

- [ ] **Step 2: Run the state tests and confirm they fail**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test state_store`

Expected: compilation fails because `state_store` does not exist.

- [ ] **Step 3: Implement the persisted schema and object-safe store**

Use this contract:

```rust
pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WindowPlacement { pub x: i32, pub y: i32 }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct StartupIdentity { pub installed_exe: std::path::PathBuf }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ClaudeTrackingIdentity {
    pub installed_exe: std::path::PathBuf,
    pub installed_status_line: serde_json::Value,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PersistedState {
    pub schema_version: u32,
    pub snapshots: std::collections::BTreeMap<ProviderId, ProviderSnapshot>,
    pub window: Option<WindowPlacement>,
    pub always_on_top: bool,
    pub launch_at_signin_requested: bool,
    pub startup_identity: Option<StartupIdentity>,
    pub claude_tracking: Option<ClaudeTrackingIdentity>,
}

#[derive(Clone, Debug)]
pub enum StateMutation {
    UpsertSnapshot(ProviderSnapshot),
    SetWindow(Option<WindowPlacement>),
    SetAlwaysOnTop(bool),
    SetStartup { requested: bool, identity: Option<StartupIdentity> },
    SetClaudeTracking(Option<ClaudeTrackingIdentity>),
}

pub trait StateStore: Send + Sync {
    fn load(&self, now: i64) -> Result<PersistedState, StateError>;
    fn apply(&self, now: i64, mutation: StateMutation) -> Result<PersistedState, StateError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("local state directory is unavailable")] DirectoryUnavailable,
    #[error("local state is oversized")] Oversized,
    #[error("local state is invalid")] Invalid,
    #[error("local state schema is unsupported")] UnsupportedSchema,
    #[error("local state lock timed out")] LockTimeout,
    #[error("local state operation failed")] Io,
}

pub trait AtomicReplace: Send + Sync {
    fn replace(&self, temporary: &std::path::Path, destination: &std::path::Path)
        -> std::io::Result<()>;
}

pub struct JsonStateStore {
    path: std::path::PathBuf,
    replacer: std::sync::Arc<dyn AtomicReplace>,
}

impl JsonStateStore {
    pub fn new(path: std::path::PathBuf) -> Self;
    pub fn with_replacer(path: std::path::PathBuf, replacer: std::sync::Arc<dyn AtomicReplace>)
        -> Self;
}
```

`PersistedState::default()` must set schema version 1, empty snapshots, no placement or integration identities, `always_on_top: true`, and `launch_at_signin_requested: false`. Add `current_snapshots(now)` and `apply_mutation(mutation, now)` methods; `UpsertSnapshot` validates before mutation. `default_state_path()` resolves exactly `%LOCALAPPDATA%\UsageWidget\state.json` through `dirs::data_local_dir`.

- [ ] **Step 4: Implement same-directory atomic replacement on Windows**

Serialize each `load` and each load-modify-replace transaction with a named per-user Windows mutex derived deterministically from the normalized state-file path. Acquire it with a five-second `WaitForSingleObject`; accept `WAIT_OBJECT_0` and `WAIT_ABANDONED`, and return `LockTimeout` on timeout. Always call `ReleaseMutex` and `CloseHandle` through an RAII guard.

While holding the mutex, load the latest state, apply exactly one `StateMutation`, write the new JSON to `tempfile::NamedTempFile::new_in(destination.parent())`, flush and `sync_all`, close it with `keep`, then use `ReplaceFileW` for an existing destination or `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` for a new destination. Convert paths using `std::os::windows::ffi::OsStrExt`; on any false return, convert `GetLastError` to `std::io::Error` and retain the old destination.

On load, enforce a 1 MiB state-file bound, reject unknown schema versions, and quarantine malformed JSON using a collision-resistant timestamped sibling name. Do not put exception strings, source paths, or raw records into the serialized state.

- [ ] **Step 5: Run state and model tests**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test state_store --test model`

Expected: all tests pass.

- [ ] **Step 6: Commit atomic state storage**

```powershell
git add -A -- src-tauri/src src-tauri/tests
git commit -m "feat: persist validated widget state atomically"
```

---

### Task 4: Claude status-line capture and settings ownership

**Files:**
- Create: `src-tauri/src/providers/claude.rs`
- Create: `src-tauri/src/claude_settings.rs`
- Create: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/providers/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/claude.rs`

**Interfaces:**
- Consumes: `ProviderSnapshot`, `PersistedState`, `StateStore`, and atomic replacement from Tasks 1 and 3.
- Produces: `parse_claude_statusline`, `run_claude_capture`, `ClaudeSettingsManager`, `ClaudeTrackingState`, and an entrypoint that routes capture before GUI startup.

- [ ] **Step 1: Write failing capture tests**

Use this exact synthetic input and never use a real transcript:

```rust
let input = serde_json::json!({
    "rate_limits": {
        "five_hour": {"used_percentage": 23.0, "resets_at": NOW + 3600},
        "seven_day": {"used_percentage": 41.0, "resets_at": NOW + 86400}
    },
    "ignored_prompt": "must never be stored"
});
let snapshot = parse_claude_statusline(input.to_string().as_bytes(), NOW).unwrap();
assert_eq!(snapshot.provider, ProviderId::Claude);
assert_eq!(snapshot.short_window.duration_minutes, 300);
assert_eq!(render_capture_status(&snapshot), "USAGE 5H 77% LEFT | 7D 59% LEFT");
```

Add tests for missing seven-day data, expired resets, malformed JSON, 64 KiB plus one byte, non-numeric fields, and store failure. Every rejected case must leave an existing valid Claude snapshot unchanged; expected missing/invalid data returns the fixed stdout line `USAGE: NO EXACT LIMITS` without echoing input.

- [ ] **Step 2: Write failing settings tests**

Using a temporary `settings.json`, assert that enable preserves unrelated nested keys and creates a backup, enable refuses a non-null existing `statusLine` with byte-for-byte unchanged settings, disable refuses a user-edited status-line object, repair changes only the widget-owned full object, a changed file between read and replace fails without mutation, and missing/malformed/non-object settings are not created or rewritten.

- [ ] **Step 3: Run Claude tests and confirm they fail**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test claude`

Expected: compilation fails because the Claude parser and settings manager do not exist.

- [ ] **Step 4: Implement bounded capture parsing and the pre-Tauri mode switch**

Use these signatures:

```rust
pub const MAX_CLAUDE_STDIN_BYTES: usize = 64 * 1024;
pub const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("capture input is oversized")] Oversized,
    #[error("capture input is invalid")] Invalid,
    #[error("capture input has no complete exact limits")] MissingLimits,
    #[error("capture input contains expired limits")] Expired,
}

pub fn parse_claude_statusline(bytes: &[u8], now: i64)
    -> Result<ProviderSnapshot, CaptureError>;
pub fn render_capture_status(snapshot: &ProviderSnapshot) -> String;
pub fn run_claude_capture(
    stdin: impl std::io::Read,
    stdout: impl std::io::Write,
    stderr: impl std::io::Write,
    store: &dyn StateStore,
    now: i64,
) -> i32;
pub fn capture_mode_from_args<I, S>(args: I) -> bool
where I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>;
```

Read at most 64 KiB plus one sentinel byte. Access only the four exact paths `rate_limits.five_hour.used_percentage`, `rate_limits.five_hour.resets_at`, `rate_limits.seven_day.used_percentage`, and `rate_limits.seven_day.resets_at`; assign durations 300 and 10,080 and `observed_at = now`; discard every other key.

Expected absent, incomplete, malformed, or oversized payloads print `USAGE: NO EXACT LIMITS`, write no state, and return 0. A valid payload calls `StateStore::apply(StateMutation::UpsertSnapshot(snapshot))`, prints the fixed-format remaining line, and returns 0. A state-write failure prints only `USAGE: LOCAL STATE ERROR` to stderr and returns 2.

`main.rs` must inspect arguments before any Tauri initialization. Exactly one `claude-capture` argument constructs `JsonStateStore::new(default_state_path())`, runs capture, and exits with its result. Until Task 7 adds the GUI, every other invocation prints the fixed stderr line `Usage Widget GUI is not available in this intermediate build.` and exits 1; this keeps the Task 4 binary fail-closed and compilable. Task 7 replaces only that non-capture branch with `run_gui`.

- [ ] **Step 5: Implement fail-closed Claude settings ownership**

Use these public types:

```rust
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeTrackingState { Disabled, Enabled, NeedsRepair, Conflict }

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeSetupError {
    #[error("Claude settings are missing")] SettingsMissing,
    #[error("Claude settings are invalid")] SettingsInvalid,
    #[error("Claude status line is already configured")] SettingsConflict,
    #[error("Claude settings changed during update")] SettingsChanged,
    #[error("the executable path is unsafe for a status command")] UnsafeExecutablePath,
    #[error("Claude settings could not be updated")] SettingsWriteFailed,
}

pub struct ClaudeSettingsManager {
    settings_path: std::path::PathBuf,
    store: std::sync::Arc<dyn StateStore>,
}

impl ClaudeSettingsManager {
    pub fn enable(&self, exe: &std::path::Path, now: i64)
        -> Result<ClaudeTrackingState, ClaudeSetupError>;
    pub fn disable(&self, now: i64) -> Result<ClaudeTrackingState, ClaudeSetupError>;
    pub fn repair(&self, exe: &std::path::Path, now: i64)
        -> Result<ClaudeTrackingState, ClaudeSetupError>;
    pub fn status(&self, exe: &std::path::Path, now: i64) -> ClaudeTrackingState;
}
```

Resolve `CLAUDE_CONFIG_DIR\settings.json` or `%USERPROFILE%\.claude\settings.json`. Reject files over 1 MiB, invalid/non-object JSON, and executable paths containing double quote, CR, LF, `%`, `!`, `^`, `&`, `|`, `<`, or `>`. Format the installed command as a quoted absolute executable path followed by ` claude-capture`.

Before enable, create a timestamped backup with `create_new`. Store the exact installed `statusLine` JSON object and normalized executable path in `ClaudeTrackingIdentity`. Before atomic replacement, re-read the settings and require byte equality with the originally read bytes. Disable and repair require equality of the entire current `statusLine` value with the stored owned value; otherwise return conflict without writing.

Treat the settings file and widget identity as a recoverable two-resource update. After writing settings, apply the corresponding `SetClaudeTracking` state mutation. If that state mutation fails, re-read settings, verify the widget-written full value is still present, and atomically restore the pre-operation bytes from memory; never roll back over a concurrent user edit. Tests inject a state-store failure for enable, disable, and repair and assert either complete rollback or a fixed `SettingsChanged` conflict with the user's newer bytes preserved.

- [ ] **Step 6: Run all Claude/state tests**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test claude --test state_store --test model`

Expected: all tests pass, and the tests touch temporary files only.

- [ ] **Step 7: Commit Claude capture and safe setup**

```powershell
git add -A -- src-tauri/src src-tauri/tests
git commit -m "feat: add opt-in Claude limit capture"
```

---

### Task 5: Refresh coordinator, watcher, and expiry

**Files:**
- Create: `src-tauri/src/coordinator.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/coordinator.rs`

**Interfaces:**
- Consumes: `CodexCollector`, state storage, current-snapshot validation, and diagnostic codes.
- Produces: `RefreshScheduler`, `CollectionCoordinator`, `CollectorSupervisor`, `refresh_now`, and clean shutdown.

- [ ] **Step 1: Write deterministic failing scheduler tests**

Test a pure scheduler with synthetic `std::time::Duration` values rather than sleeping:

```rust
let mut scheduler = RefreshScheduler::new(Duration::ZERO);
scheduler.note_change(PathBuf::from("a.jsonl"), Duration::from_millis(10));
assert_eq!(scheduler.due(Duration::from_millis(509)), RefreshAction::None);
assert_eq!(
    scheduler.due(Duration::from_millis(510)),
    RefreshAction::Changed(BTreeSet::from([PathBuf::from("a.jsonl")]))
);
assert_eq!(scheduler.due(Duration::from_secs(60)), RefreshAction::Full);
```

Add tests for debounce extension, irrelevant extensions, watcher errors forcing a full scan, removal/rename events, manual refresh, and orderly stop/join. Use fake collectors and stores to prove that an invalid candidate does not replace current state and an expired stored snapshot disappears from the projection immediately.

- [ ] **Step 2: Run coordinator tests and confirm they fail**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test coordinator`

Expected: compilation fails because the coordinator interfaces do not exist.

- [ ] **Step 3: Implement the pure scheduler and coordinator services**

Use exact timings:

```rust
pub const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);
pub const FALLBACK_RESCAN: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CoordinatorError {
    #[error("collector refresh failed")] Collect,
    #[error("state update failed")] State,
    #[error("filesystem watcher failed")] Watch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshAction {
    None,
    Changed(std::collections::BTreeSet<std::path::PathBuf>),
    Full,
}

pub struct CollectionCoordinator {
    collector: std::sync::Arc<CodexCollector>,
    store: std::sync::Arc<dyn StateStore>,
    current: std::sync::RwLock<PersistedState>,
}

impl CollectionCoordinator {
    pub fn load(collector: std::sync::Arc<CodexCollector>, store: std::sync::Arc<dyn StateStore>, now: i64)
        -> Result<Self, CoordinatorError>;
    pub fn refresh_now(&self, now: i64) -> Result<(), CoordinatorError>;
    pub fn refresh_changed(&self, paths: &std::collections::BTreeSet<std::path::PathBuf>, now: i64)
        -> Result<(), CoordinatorError>;
    pub fn current_snapshots(&self, now: i64) -> Vec<ProviderSnapshot>;
}
```

State changes validate before `StateStore::apply`; an unsuccessful transaction leaves the in-memory and on-disk old state unchanged. After a successful transaction, replace the coordinator's in-memory copy with the returned state. `current_snapshots` filters expiry on every call.

- [ ] **Step 4: Add the real `notify` adapter and clean shutdown**

Use `notify::recommended_watcher` recursively on existing `sessions` and `archived_sessions`. When a target directory is absent, watch its nearest existing parent non-recursively and rely on the 60-second full scan for recovery. Filter create, modify, rename, and remove events to `.jsonl`. Feed callback events into a worker channel; the worker owns `RefreshScheduler`, performs changed/full scans, and exits when the stop channel disconnects.

Expose:

```rust
pub struct CollectorSupervisor { stop: Option<std::sync::mpsc::Sender<()>>, join: Option<std::thread::JoinHandle<()>> }
pub fn start_supervisor(coordinator: std::sync::Arc<CollectionCoordinator>)
    -> Result<CollectorSupervisor, CoordinatorError>;
impl CollectorSupervisor { pub fn stop_and_join(&mut self); }
impl Drop for CollectorSupervisor { fn drop(&mut self) { self.stop_and_join(); } }
```

Do not print event paths or watcher error strings. Map failures to `DiagnosticCode` only.

- [ ] **Step 5: Run coordinator and collector suites**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test coordinator --test codex --test state_store`

Expected: all tests pass without timing sleeps or real profile access.

- [ ] **Step 6: Commit the coordinator**

```powershell
git add -A -- src-tauri/src src-tauri/tests
git commit -m "feat: refresh local limits on file changes"
```

---

### Task 6: Pure ASCII/8-bit frontend and narrow bridge

**Files:**
- Create: `package.json`
- Create: `tests/frontend/ui-model.test.mjs`
- Create: `ui/ui-model.js`
- Create: `ui/index.html`
- Create: `ui/styles.css`
- Create: `ui/render.js`
- Create: `ui/bridge.js`
- Create: `ui/app.js`

**Interfaces:**
- Consumes: the snake-case numeric `WidgetView` contract finalized in Task 7; until then tests use plain synthetic objects.
- Produces: `remainingPercent`, `meterText`, `meterTone`, `formatCountdown`, `visibleProviders`, `layoutForProviderCount`, safe DOM rendering, and four bridge calls.

- [ ] **Step 1: Add pinned Tauri CLI metadata and failing pure frontend tests**

Create `package.json`:

```json
{
  "name": "usage-widget",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {
    "test": "node --test tests/frontend/*.test.mjs",
    "tauri": "tauri"
  },
  "devDependencies": {
    "@tauri-apps/cli": "2.11.4"
  }
}
```

Create tests that import from `ui/ui-model.js` and assert:

```js
import test from "node:test";
import assert from "node:assert/strict";
import {
  formatCountdown, layoutForProviderCount, meterText, meterTone,
  remainingPercent, visibleProviders
} from "../../ui/ui-model.js";

test("renders the approved ten-cell block meter", () => {
  assert.equal(remainingPercent(38.4), 62);
  assert.equal(meterText(62), "[██████░░░░]");
  assert.equal(meterText(96), "[██████████]");
  assert.equal(meterTone(30), "provider");
  assert.equal(meterTone(29), "amber");
  assert.equal(meterTone(9), "red");
});

test("formats reset countdowns", () => {
  const now = 1_000_000;
  assert.equal(formatCountdown(now / 1000 + 90_061, now), "1D 01H");
  assert.equal(formatCountdown(now / 1000 + 3_661, now), "01H 01M");
  assert.equal(formatCountdown(now / 1000 + 59, now), "00M 59S");
});

test("hides incomplete and expired providers", () => {
  const complete = {
    provider: "codex",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };
  assert.deepEqual(visibleProviders({providers: [complete]}, 100), [complete]);
  assert.deepEqual(visibleProviders({providers: [complete]}, 200), []);
  assert.equal(layoutForProviderCount(0), "empty");
  assert.equal(layoutForProviderCount(1), "single");
  assert.equal(layoutForProviderCount(2), "dual");
});
```

- [ ] **Step 2: Run frontend tests and confirm they fail**

Run: `node --test .\tests\frontend\ui-model.test.mjs`

Expected: module-not-found failure for `ui/ui-model.js`.

- [ ] **Step 3: Implement pure presentation functions**

`ui-model.js` must export:

```js
export const METER_CELLS = 10;
export const clamp = (value, min, max) => Math.min(max, Math.max(min, value));
export const remainingPercent = (used) => Math.round(clamp(100 - used, 0, 100));
export function meterText(remaining) {
  const cells = clamp(Math.round(remaining / 10), 0, METER_CELLS);
  return `[${"█".repeat(cells)}${"░".repeat(METER_CELLS - cells)}]`;
}
export const meterTone = (remaining) => remaining < 10 ? "red" : remaining < 30 ? "amber" : "provider";
export function formatCountdown(resetsAtSeconds, nowMs) {
  let seconds = Math.max(0, Math.floor(resetsAtSeconds - nowMs / 1000));
  const days = Math.floor(seconds / 86400); seconds %= 86400;
  const hours = Math.floor(seconds / 3600); seconds %= 3600;
  const minutes = Math.floor(seconds / 60); const secs = seconds % 60;
  const pad = (n) => String(n).padStart(2, "0");
  if (days >= 1) return `${days}D ${pad(hours)}H`;
  if (hours >= 1) return `${pad(hours)}H ${pad(minutes)}M`;
  return `${pad(minutes)}M ${pad(secs)}S`;
}
export function visibleProviders(view, nowSeconds) {
  const order = new Map([["codex", 0], ["claude", 1]]);
  return (view.providers ?? []).filter((item) =>
    order.has(item.provider) &&
    item.short_window?.duration_minutes === 300 &&
    item.weekly_window?.duration_minutes === 10080 &&
    item.short_window.resets_at > nowSeconds &&
    item.weekly_window.resets_at > nowSeconds &&
    Number.isFinite(item.short_window.used_percent) &&
    Number.isFinite(item.weekly_window.used_percent) &&
    item.short_window.used_percent >= 0 && item.short_window.used_percent <= 100 &&
    item.weekly_window.used_percent >= 0 && item.weekly_window.used_percent <= 100
  ).sort((a, b) => order.get(a.provider) - order.get(b.provider));
}
export const layoutForProviderCount = (count) => count === 0 ? "empty" : count === 1 ? "single" : "dual";
```

- [ ] **Step 4: Build the static accessible DOM and styling**

`index.html` contains an external stylesheet and module script, a body-level `data-tauri-drag-region="deep"` surface, a real `[x]` button opted out with `data-tauri-drag-region="false"` and `aria-label="Hide usage widget"`, and `<main id="providers" aria-live="polite"></main>`. Do not use inline scripts/styles or remote assets.

`render.js` must create provider cards with `document.createElement`, `textContent`, `replaceChildren`, and fixed labels only. Give meters `role="progressbar"`, `aria-valuemin="0"`, `aria-valuemax="100"`, and the exact remaining value. Never use `innerHTML`.

`styles.css` uses the approved near-black palette, Cascadia Mono/Consolas/system monospace stack, green Codex and orange Claude variables, amber/red threshold classes, `font-variant-numeric: tabular-nums`, native-surface `grab`/`grabbing` cursor feedback, visible `:focus-visible`, no transitions/animations, and a `prefers-contrast: more` override.

- [ ] **Step 5: Implement the global-Tauri bridge and app lifecycle**

Because the frontend is static and unbundled, `bridge.js` must use the injected global rather than import `@tauri-apps/api`:

```js
const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);
export const getWidgetView = () => invoke("get_widget_view");
export const hideWidget = () => invoke("hide_widget");
export const setWidgetHeight = (height) => invoke("set_widget_height", {height});
```

`app.js` loads and renders once, polls the cached `get_widget_view` projection every five seconds without forcing a rescan, updates countdown text every second without file access, and synchronizes the measured body height only when needed. Native filesystem watching, the 60-second fallback, and the tray Refresh action remain responsible for source rescans. `[x]` and Escape call `hideWidget`; stop pointer propagation on `[x]` so it is not a drag gesture. When no provider is visible, render exactly `NO CURRENT LIMIT DATA`.

- [ ] **Step 6: Run frontend tests**

Run: `node --test .\tests\frontend\ui-model.test.mjs`

Expected: all frontend tests pass.

- [ ] **Step 7: Install and lock only the build CLI, then commit**

Run: `npm.cmd install --package-lock-only`

Run: `npm.cmd ci`

Run: `npm.cmd test`

Expected: `package-lock.json` is created, only development tooling is installed, and tests pass.

```powershell
git add -A -- package.json package-lock.json tests ui
git commit -m "feat: add ASCII usage widget interface"
```

---

### Task 7: Tauri window, tray, single instance, and startup integration

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/build.rs`
- Create: `src-tauri/tauri.conf.json`
- Create: `src-tauri/capabilities/default.json`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/src/startup.rs`
- Create: `src-tauri/src/shell.rs`
- Create: `src-tauri/tests/startup.rs`

**Interfaces:**
- Consumes: coordinator, state, Claude settings manager, and the four frontend bridge command names.
- Produces: `run_gui`, `WidgetView`, Tauri commands `get_widget_view`, `refresh`, `hide_widget`, `set_widget_layout`, tray lifecycle, always-on-top/position persistence, `StartupRegistration`, and single-instance behavior.

- [ ] **Step 1: Add failing pure shell/startup tests**

Test the pure boundaries before Tauri wiring:

```rust
assert_eq!(height_for_layout(Layout::Empty), 102.0);
assert_eq!(height_for_layout(Layout::Single), 178.0);
assert_eq!(height_for_layout(Layout::Dual), 254.0);
assert_eq!(startup_status(true, Path::new("C:\\A\\usage-widget.exe"), Path::new("C:\\B\\usage-widget.exe")), IntegrationStatus::NeedsRepair);
assert!(capture_mode_from_args(["usage-widget.exe", "claude-capture"]));
assert!(!capture_mode_from_args(["usage-widget.exe"]));
```

Use a fake `StartupRegistration` to prove enable persists identity only after success, disable clears it only after success, and repair does not first remove the previous registration.

- [ ] **Step 2: Run startup tests and confirm they fail**

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml --test startup`

Expected: compilation fails because startup and shell interfaces are absent.

- [ ] **Step 3: Add pinned Tauri dependencies and build configuration**

Append:

```toml
[build-dependencies]
tauri-build = "2.6.3"

[dependencies.tauri]
version = "2.11.5"
features = ["tray-icon"]

[dependencies.tauri-plugin-autostart]
version = "2.5.1"

[dependencies.tauri-plugin-dialog]
version = "2.7.3"

[dependencies.tauri-plugin-single-instance]
version = "2.4.4"
```

Extend the existing `windows-sys` feature list with `Win32_UI_WindowsAndMessaging` so startup failures can be reported without a WebView.

`build.rs` is exactly `fn main() { tauri_build::build() }`.

Configure one initially hidden `main` window at width 356, height 102, non-resizable, frameless, always-on-top, skipped from the taskbar, and with `withGlobalTauri: true`. Set `frontendDist` to `../ui`, `bundle.active` to false, and use this production CSP:

```text
default-src 'self'; base-uri 'none'; form-action 'none'; object-src 'none';
frame-src 'none'; connect-src ipc: http://ipc.localhost; img-src 'self' data:; style-src 'self';
script-src 'self'; font-src 'self'; media-src 'none'; worker-src 'none';
manifest-src 'none'
```

The two `connect-src` entries are Tauri's internal IPC transports, not external network destinations. The capability file targets only `main` and grants `core:default`. Do not add filesystem, shell, HTTP, notification, updater, dialog, autostart, or generic plugin permissions to the frontend; dialog and autostart are called from Rust.

- [ ] **Step 4: Implement view DTOs and the four narrow commands**

Use:

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct WidgetView { pub providers: Vec<ProviderSnapshot> }

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus { Disabled, Enabled, NeedsRepair, Conflict }

#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandError { pub code: &'static str, pub message: &'static str }

#[tauri::command]
pub fn get_widget_view(state: tauri::State<'_, ShellState>) -> Result<WidgetView, CommandError>;
#[tauri::command]
pub async fn refresh(state: tauri::State<'_, ShellState>) -> Result<WidgetView, CommandError>;
#[tauri::command]
pub fn hide_widget(app: tauri::AppHandle) -> Result<(), CommandError>;
#[tauri::command]
pub fn set_widget_height(app: tauri::AppHandle, height: u32) -> Result<(), CommandError>;
```

Define `ShellState` with `Arc<CollectionCoordinator>`, `Arc<dyn StateStore>`, `ClaudeSettingsManager`, `Arc<dyn StartupRegistration>`, and a mutex-owned optional `CollectorSupervisor`. No field contains raw provider data outside normalized persisted state.

`get_widget_view` filters expiry using the current epoch. `refresh` clones the coordinator `Arc`, performs the full rescan inside `tauri::async_runtime::spawn_blocking`, and returns the same projection so disk scanning never blocks WebView event handling. Command errors contain a fixed snake-case code and fixed user-safe message only. `set_widget_height` clamps the measured content height to the native safety range while keeping the width fixed at 356.

- [ ] **Step 5: Implement native window and tray lifecycle**

Register the single-instance plugin first. Its callback shows, unminimizes, focuses, and refreshes the existing `main` window. Register autostart and dialog plugins, manage `ShellState`, build the tray in setup, start the collector supervisor, restore/clamp saved position, apply saved topmost preference, size for current providers, then show the window.

Intercept `CloseRequested`, call `prevent_close`, and hide. Persist moved positions through a 300 ms debounce. Clamp restored coordinates so at least the title row intersects a current monitor's work area.

Build tray items with fixed IDs: `show_hide`, `refresh`, `always_on_top`, `launch_at_sign_in`, `claude_tracking`, and `quit`. Use checked items for topmost and startup. The Claude label is Enable, Disable, or Repair based on ownership state. Left-clicking the tray shows/focuses the window. Quit stops and joins watchers, saves current state, disposes the tray, and calls `app.exit(0)`.

Tray integration failures show a fixed native dialog through the Rust dialog plugin; do not expose paths or raw errors.

- [ ] **Step 6: Implement autostart identity and repair**

Define:

```rust
pub trait StartupRegistration: Send + Sync {
    fn enable(&self) -> Result<(), StartupError>;
    fn disable(&self) -> Result<(), StartupError>;
    fn is_enabled(&self) -> Result<bool, StartupError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("startup registration is unavailable")] Unavailable,
    #[error("startup registration operation failed")] OperationFailed,
}

pub fn startup_status(requested: bool, installed: &std::path::Path, current: &std::path::Path)
    -> IntegrationStatus;
```

The Tauri adapter calls `app.autolaunch().enable()`, `.disable()`, and `.is_enabled()`. Enable/disable/repair update persisted request and path identity only after the plugin operation succeeds. Repair calls enable for the current EXE without disabling first, then confirms `is_enabled` before replacing the stored identity. A moved path displays Repair until that succeeds.

Real startup toggling is excluded from automated tests. It is performed only if the user separately authorizes changing the current Windows sign-in registration.

- [ ] **Step 7: Finish GUI/capture dispatch and startup failure reporting**

Define a public zero-data `GuiStartError` marker and `pub fn run_gui() -> Result<(), GuiStartError>`. `main.rs` routes capture before GUI creation and replaces Task 4's intermediate non-capture error branch with a call to this function. If Tauri cannot start, use a Windows-native fixed message box stating `Usage Widget could not start. Windows WebView2 Runtime is required.` and exit nonzero. Do not display the underlying error string.

- [ ] **Step 8: Run Rust tests and compile-check the Tauri APIs**

Run: `cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check`

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml`

Run: `cargo check --manifest-path .\src-tauri\Cargo.toml --all-targets`

Expected: all tests pass and current pinned Tauri tray, dialog, autostart, single-instance, and capability APIs compile.

- [ ] **Step 9: Commit the desktop shell**

```powershell
git add -A -- src-tauri
git commit -m "feat: add Tauri widget window and tray"
```

---

### Task 8: Icon, documentation, and privacy checks

**Files:**
- Create: `assets/icon.svg`
- Create: `src-tauri/icons/*` through Tauri icon generation
- Create: `README.md`
- Create: `docs/PRIVACY.md`
- Create: `docs/RELEASE.md`
- Create: `docs/smoke-checklist.md`
- Create: `scripts/check-no-network.ps1`
- Modify: `src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: finished source behavior from Tasks 1-7.
- Produces: local pixel icon, end-user operating instructions, release caveats, and a repeatable read-only network check.

- [ ] **Step 1: Add the local pixel icon source and generate Tauri icons**

Create a 512-by-512 SVG using only crisp rectangular shapes: near-black background, green border, and an off-white pixel letter `U`. Set `shape-rendering="crispEdges"`; do not reference fonts, URLs, or embedded remote data.

Run: `npm.cmd run tauri -- icon .\assets\icon.svg`

Expected: Tauri generates the Windows ICO/PNG assets under `src-tauri/icons`. Reference the generated icon paths in `tauri.conf.json` and use the default app icon for the tray.

- [ ] **Step 2: Write user and privacy documentation**

`README.md` must document:

- launch by double-clicking `usage-widget.exe`;
- exact local Codex source and opt-in Claude status-line capture;
- remaining percentage/reset countdown semantics;
- close-to-tray and every tray action;
- Claude hidden until a valid exact payload arrives after a Claude response;
- no API spend, estimates, credentials, network calls, or telemetry;
- moving the EXE requires repairing optional Claude/startup paths;
- `usagewidget.py` is a legacy reference and not the release app.

`docs/PRIVACY.md` enumerates every stored field and explicitly excludes raw records, prompts, transcripts, credentials, source paths, and account identifiers. `docs/RELEASE.md` states Windows 11 x64, Evergreen WebView2 reliance, unsigned SmartScreen caveat, no installer/updater, and checksum verification. `docs/smoke-checklist.md` reproduces Task 9's exact checks and separates synthetic/read-only evidence from opt-in system mutations.

- [ ] **Step 3: Add a read-only network check script**

The PowerShell script accepts a required root PID, discovers descendants by `ParentProcessId` using `Get-CimInstance Win32_Process`, samples `Get-NetTCPConnection` for those PIDs for 30 seconds, and exits 0 only when no established or connecting TCP entry appears. It prints PID, state, and remote address only; it never terminates processes or changes firewall state.

- [ ] **Step 4: Run static privacy/security checks**

Run:

```powershell
rg -n "https?://|fetch\(|WebSocket|TcpListener|HttpServer|reqwest|oauth|cookie|telemetry|analytics" ui src-tauri/src package.json src-tauri/Cargo.toml
```

Expected: only the intentional Tauri schema/documentation URLs outside runtime source, or no runtime matches. Review each match and remove runtime network code.

Run: `npm.cmd test`

Run: `cargo test --manifest-path .\src-tauri\Cargo.toml`

Expected: all tests pass.

- [ ] **Step 5: Commit documentation and assets**

```powershell
git add -A -- assets src-tauri/icons src-tauri/tauri.conf.json README.md docs/PRIVACY.md docs/RELEASE.md docs/smoke-checklist.md scripts
git commit -m "docs: add portable widget usage and privacy guide"
```

---

### Task 9: Full verification and portable release artifact

**Files:**
- Modify only when a verification failure identifies a concrete defect.
- Generate, do not commit: `release/usage-widget.exe`
- Generate, do not commit: `release/usage-widget.exe.sha256`
- Generate, do not commit: `release/build-info.txt`

**Interfaces:**
- Consumes: the complete app from Tasks 1-8.
- Produces: passing verification evidence, one portable EXE, exact size, and SHA-256 checksum.

- [ ] **Step 1: Run the complete automated quality gate**

Run:

```powershell
npm.cmd ci
npm.cmd test
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml
```

Expected: every command exits 0. If a command fails, use the systematic-debugging workflow, add or preserve a reproducing test, fix only the root cause, and rerun the full gate.

- [ ] **Step 2: Build the direct portable executable without an installer**

Run:

```powershell
npm.cmd run tauri -- build --no-bundle --target x86_64-pc-windows-msvc
```

Expected: the direct executable exists at `src-tauri\target\x86_64-pc-windows-msvc\release\usage-widget.exe`, and no MSI or NSIS installer is produced.

- [ ] **Step 3: Create the local release copy, checksum, and build facts**

Resolve the exact source artifact, create `release`, copy it as `release\usage-widget.exe`, calculate lowercase SHA-256 into `release\usage-widget.exe.sha256` in the form `<hash> *usage-widget.exe`, and write toolchain versions, Git HEAD, build timestamp, target triple, byte size, and hash to `release\build-info.txt`. Values must come from the completed build and commands such as `rustc --version`, `cargo --version`, `node --version`, `npm.cmd --version`, `git rev-parse HEAD`, `Get-Item`, and `Get-FileHash`; do not estimate any value.

- [ ] **Step 4: Perform the read-only packaged smoke test**

Launch the release EXE, wait up to ten seconds for its process/window, launch it a second time, and confirm a second persistent GUI process is not created. Compare the current Codex card with a sanitized projection from `python .\usagewidget.py once`: pipe its JSON through `ConvertFrom-Json` and output only provider name plus each window's numeric used percentage and reset epoch, never the source or roots fields. Check:

- terminal-style glyphs, remaining math, and reset countdown;
- fixed width, drag behavior, topmost default, and saved on-screen position;
- `[x]` and Escape hide without exiting;
- tray Show restores the same process;
- Refresh does not freeze the UI;
- Quit exits and removes the tray icon;
- the Claude card stays hidden until a valid captured sample exists;
- `scripts/check-no-network.ps1` reports no connection for the app process tree during the 30-second sample.

Use the local image-viewing workflow to inspect a screenshot of the running widget. Record only aggregate normalized percentages in the handoff; never include provider source paths or records.

- [ ] **Step 5: Verify opt-in boundaries without mutating real settings**

Exercise valid synthetic capture through the Rust integration boundary with a temporary `JsonStateStore`, and run enable/disable/repair against a temporary `CLAUDE_CONFIG_DIR`. Invoke the packaged `claude-capture` entrypoint with empty and malformed stdin, confirm it prints only `USAGE: NO EXACT LIMITS`, and compare the real state file hash before and after to prove the rejected input wrote nothing. Confirm the real `%USERPROFILE%\.claude\settings.json` and Windows sign-in registration remain unchanged.

A real Claude settings smoke or real launch-at-sign-in toggle requires a separate explicit user approval because it changes external user configuration. If approval is not provided, report those two live mutation checks as unrun while retaining the passing isolated integration evidence.

- [ ] **Step 6: Review the final diff and commit source fixes only**

Run: `git status --short`

Run: `git diff --check`

Run: `git log --oneline --decorate -10`

Expected: only intentionally untracked/ignored release artifacts remain; source and documentation are committed. If Task 9 required a source correction, commit the tested correction with a focused message. Do not commit `release/`.

- [ ] **Step 7: Report the verified handoff**

Provide clickable paths to the EXE, checksum, README, privacy guide, and smoke checklist. Report automated test counts/results, exact byte size, SHA-256, Git HEAD, observed provider coverage, and each manual smoke result. Keep synthetic, read-only local, and opt-in-but-unrun evidence clearly separated.
