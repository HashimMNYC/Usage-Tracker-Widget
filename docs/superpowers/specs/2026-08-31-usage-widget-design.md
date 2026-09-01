# Usage Widget Design

Status: approved in chat on 2026-08-31 and implemented in this repository.

## Purpose

Build a compact Windows 11 desktop widget that shows the exact remaining Claude Code and Codex subscription limits available from local CLI data. The widget is a portable x64 executable with no installer and no network access.

The product favors honesty over coverage: every displayed window has a validated percentage and reset timestamp. A locally reported general Codex limit may expose only one exact window, in which case the missing row is hidden; Claude still requires both status-line windows. The widget never substitutes a named model-specific quota or estimates a subscription percentage from token counts.

## Product decisions

- Platform: Windows 11 x64.
- Runtime: Tauri 2 with a Rust backend and embedded vanilla HTML, CSS, and JavaScript.
- Distribution: one portable `usage-widget.exe`, without an installer.
- Data boundary: local files and Claude Code status-line input only.
- Display: percentage remaining and reset countdown for each exact five-hour or seven-day window currently available.
- Visual style: compact ASCII interface with ten-cell, 8-bit meters.
- Lifecycle: frameless, draggable, always on top by default, and resident in the Windows notification area.
- Close behavior: the window hides; only the tray menu's Quit action exits the process.
- Missing data: hide the provider. If neither provider is valid, show `NO CURRENT LIMIT DATA`.
- Claude collection: disabled until the user explicitly selects Enable Claude Tracking.
- Launch at sign-in: available as an opt-in tray toggle and disabled by default.

## Non-goals

- API usage, API billing, or organization spend.
- General computer or application screen-time tracking.
- Calls to provider APIs, browser automation, cookie access, or credential access.
- Guessed percentages derived from tokens, message counts, elapsed time, or plan assumptions.
- Cross-platform packaging in the first release.
- History, charts, analytics, accounts, cloud sync, telemetry, or automatic updates.
- An installer, updater, or background Windows service.

## Architecture

```text
Codex rollout JSONL files -----\
                                +--> Rust collectors --> validated snapshot --> ASCII UI
Claude status-line JSON -------/           |
                                            +--> atomic local state
```

The application has four isolated units:

1. **Provider adapters** read one provider-specific source and return a normalized candidate snapshot. They do not know about the UI.
2. **Validation and state** accepts only current snapshots with at least one exact window and atomically persists the minimum required fields. Provider-specific adapters may impose stricter completeness rules.
3. **Tauri application shell** owns the single-instance lifecycle, window, tray, startup registration, and narrow commands exposed to the frontend.
4. **Presentation** renders normalized values and updates countdown text. It cannot read arbitrary files, launch a shell, or make network requests.

The existing `usagewidget.py` remains untouched as a reference until the Tauri implementation reaches parity. It is not included in the portable release artifact.

## Normalized data contract

Each visible provider has exactly this logical shape:

```text
ProviderSnapshot
  provider: "codex" | "claude"
  observed_at: UTC epoch seconds
  short_window: optional
    duration_minutes: 300
    used_percent: number from 0 through 100
    resets_at: future UTC epoch seconds
  weekly_window: optional
    duration_minutes: 10080
    used_percent: number from 0 through 100
    resets_at: future UTC epoch seconds
```

`remaining_percent` is derived at the presentation boundary as `clamp(100 - used_percent, 0, 100)`. It is not independently stored.

A snapshot is rejected unless at least one recognized window is present, all present numeric values are finite, percentages are in range, durations identify the expected windows, and reset timestamps are in the future. Unknown keys are ignored. Claude capture and legacy Codex records without a limit identifier remain stricter and require both windows. Rejected input never replaces the last valid snapshot.

Once any stored window expires, that provider snapshot is no longer current and is hidden until a new valid snapshot arrives. This prevents a stale pre-reset percentage from appearing current.

## Codex adapter

The Codex adapter reads JSONL files under the current user's configured Codex home, defaulting to `%USERPROFILE%\.codex`, including `sessions` and `archived_sessions`.

On startup it locates the most recently modified candidate files and searches newest records first for a usable general `rate_limits` payload. During runtime it watches the relevant directories, debounces file-change events for 500 milliseconds, and refreshes only changed or newly created candidates. A 60-second fallback rescan and a manual Refresh tray action recover from missed filesystem events.

The observed local schema is not treated as a stable provider contract. The current adapter recognizes `rate_limits.primary` and `rate_limits.secondary` plus semantic field variants for used percentage, window duration, and reset time. It classifies windows by duration rather than object order, treats a null primary or secondary slot as an absent window, accepts a partial exact window set only when `limit_id` is exactly `codex`, rejects named model-specific limits, tolerates partial final JSONL lines, and fails closed on malformed present windows, null required fields, or ambiguous data. Legacy records without `limit_id` require both exact windows.

The adapter never copies transcript text or exposes source paths to the frontend. Diagnostic messages report categories such as `no files`, `no exact limits`, or `expired snapshot`, not record contents.

## Claude adapter and opt-in setup

Claude Code can send exact `rate_limits.five_hour.used_percentage`, `rate_limits.five_hour.resets_at`, `rate_limits.seven_day.used_percentage`, and `rate_limits.seven_day.resets_at` fields to a configured status-line command. The widget uses this local handoff rather than reading credentials or estimating limits from transcripts. It assigns the known 300-minute and 10,080-minute durations when normalizing those named windows.

The tray action **Enable Claude Tracking** performs these steps:

1. Locate and parse the current user's Claude settings JSON, honoring `CLAUDE_CONFIG_DIR` and otherwise using `%USERPROFILE%\.claude\settings.json`.
2. If a status-line command already exists, do not overwrite or chain it. Report the conflict and leave settings unchanged.
3. Create a timestamped backup beside the settings file.
4. Preserve every existing setting and add a status-line command pointing to the current absolute executable path with the `claude-capture` argument.
5. Write the updated JSON through a temporary sibling file followed by an atomic replace.

If the settings file is missing, oversized, malformed, or not a JSON object, setup fails without creating or changing it. This preserves the backup-before-write guarantee and keeps configuration fail-closed.

When Claude invokes `usage-widget.exe claude-capture`, the process bypasses the GUI startup path, reads a size-bounded JSON object from standard input, allowlists only the five-hour and seven-day used percentages and reset timestamps, validates them, and atomically updates normalized state. It emits a concise one-line Claude status display containing the same remaining percentages so enabling the required status-line facility does not produce a blank status area. It writes no transcript, prompt, account, token, or raw input data.

Disabling tracking removes the status-line entry only when it still exactly matches the command installed by the widget. If the user or Claude has changed that setting, the widget refuses to modify it and explains that manual review is required.

Moving the portable executable invalidates the absolute command path. On the next GUI launch, the tray action changes to **Repair Claude Tracking**. Repair is explicit, updates only the widget-owned command, and refreshes the stored executable path.

## Local state

Application state is stored in `%LOCALAPPDATA%\UsageWidget\state.json` rather than beside the executable, so the EXE can run from a read-only or movable location.

State includes only:

- the latest normalized provider snapshots;
- last window position;
- always-on-top preference;
- whether launch at sign-in was requested;
- the widget-owned Claude command identity needed for safe repair or removal.

Writes use a temporary sibling and atomic replacement. Corrupt state is quarantined or ignored and rebuilt from provider sources. Raw provider records and credentials are never persisted.

## Window and tray behavior

- The GUI path is single-instance. Starting it again focuses and shows the existing window.
- First launch opens the widget. Later launches restore the last valid position, clamped to a currently connected monitor.
- The window is frameless, non-resizable, fixed-width, and automatically sized vertically for zero, one, or two visible providers.
- The whole widget surface is the drag region except `[x]`, which hides the window without ending collection.
- Always on Top defaults to enabled and is persisted when toggled from the tray.
- The tray menu contains Show/Hide, Refresh, Always on Top, Launch at Sign-in, Enable/Disable/Repair Claude Tracking as applicable, and Quit.
- Quit disposes the tray icon and filesystem watchers before terminating.
- Launch at sign-in uses Tauri's per-user autostart integration, registers the current executable path without elevation, and opens the widget normally at sign-in. Moving the EXE changes the action to Repair Launch at Sign-in.

## Interface

The interface uses a near-black background, a system monospace stack headed by Cascadia Mono and Consolas, off-white text, a green Codex accent, and an orange Claude accent. It downloads no fonts or assets.

Here, "ASCII interface" means the approved terminal-style presentation. The borders and meters intentionally use the Unicode box-drawing and block glyphs shown below rather than being restricted to the 7-bit ASCII character set.

```text
┌─ USAGE ──────────────── [x] ┐
│ CODEX                        │
│ 5H  [██████░░░░]  62% LEFT  │
│     RESET 01H 42M            │
│ 7D  [████░░░░░░]  38% LEFT  │
│     RESET 4D 09H             │
└──────────────────────────────┘
```

Each meter has ten text cells. The number of filled cells is the nearest integer to `remaining_percent / 10`, bounded from zero through ten. The exact rounded percentage remains visible beside the bar.

Meters use the provider accent at 30% remaining or above, amber below 30%, and red below 10%. Countdown formatting is compact and stable: days plus hours when at least one day remains, hours plus minutes when at least one hour remains, and minutes plus seconds below one hour. Countdown text updates once per second without rescanning files.

There are no animations, charts, token totals, settings window, provider placeholders, tooltips containing source paths, or external links. Keyboard Escape and `[x]` hide the window. The tray menu is the settings surface.

## Error handling

- Missing provider directories, access-denied files, partial writes, invalid JSON, unknown schemas, and expired snapshots are expected states, not crashes.
- A provider stays hidden unless its adapter returns at least one exact current window; unavailable rows are hidden rather than synthesized.
- If both providers are hidden, the window displays only `NO CURRENT LIMIT DATA`.
- A failed manual refresh leaves the last still-current snapshot in place.
- A failed Claude settings edit or startup registration is reported without partial configuration; the previous file or registration remains intact.
- Panics are contained at process boundaries where practical, and no provider input is interpolated into shell commands or HTML.

## Security and privacy

- Do not include an HTTP server or listener.
- Do not include provider SDKs, API keys, OAuth logic, cookie handling, telemetry, analytics, or update checks.
- Do not request Tauri shell or broad frontend filesystem permissions.
- Expose only narrow commands for snapshot retrieval, refresh, window/tray preferences, startup registration, and Claude setup.
- Apply a restrictive content security policy and embed all UI resources.
- Bound status-line stdin and JSONL record sizes before parsing.
- Treat all provider files as untrusted input and render only normalized numeric values and fixed labels.
- Never include source record content in state, UI errors, logs, tests, or crash output.

## Packaging

The release target is Windows 11 x64. The build produces the direct release executable rather than an MSI or NSIS installer. The executable relies on the Evergreen WebView2 Runtime supplied with Windows 11; the application detects a missing runtime and fails with a clear local message.

The release handoff includes:

- `usage-widget.exe`;
- its SHA-256 checksum;
- concise usage and privacy documentation;
- instructions noting that moving the EXE requires repairing optional startup and Claude integrations.

Code signing and automatic updating are outside the first release. The handoff must not imply that an unsigned local build has established SmartScreen reputation.

## Verification strategy

### Rust tests

- Parse exact current Codex and Claude fixture shapes.
- Accept supported naming variants while classifying by window duration.
- Reject missing windows, null fields, non-numeric values, out-of-range percentages, invalid timestamps, ambiguous windows, partial JSONL lines, and oversized input.
- Verify remaining-percentage clamping and expiration behavior.
- Verify newest-valid-snapshot selection and that rejected input cannot replace current valid state.
- Verify atomic state replacement and recovery from corrupt state.
- Verify Claude settings enable, conflict refusal, disable ownership checks, repair, and preservation of unrelated keys using temporary directories only.
- Verify launch-at-sign-in command quoting and moved-executable repair logic without modifying the real registry in tests.

### Frontend tests

Pure rendering and countdown functions are kept separate from Tauri calls and tested with Node's built-in test runner. Tests cover bar-cell rounding, threshold colors, countdown formats, provider ordering, provider hiding, and the empty state.

### Integration and release checks

- Run all Rust and frontend tests.
- Build the release-mode x64 executable directly, without an installer.
- Launch the EXE and verify single-instance behavior, dragging, always-on-top, adaptive height, saved position, and manual refresh.
- Verify closing hides to the tray, tray Show restores the same process, and Quit removes the tray icon and exits.
- Exercise Claude enable/disable/repair against an isolated temporary settings file before any real opt-in smoke.
- Exercise collectors against synthetic fixtures first, then compare read-only results with current local Codex data without printing source content.
- Confirm the release UI makes no outbound network request.
- Record the final executable size and SHA-256 checksum from the built artifact rather than estimating them.

## Acceptance criteria

The first release is accepted when:

1. A portable `usage-widget.exe` runs on Windows 11 x64 without an installer.
2. It shows every exact available five-hour or seven-day value with a reset countdown from current local data.
3. It never shows estimated, model-substituted, malformed, or expired provider values.
4. Claude tracking is explicit, reversible, settings-preserving, credential-free, and hidden until its first valid payload.
5. The ASCII interface, tray lifecycle, always-on-top behavior, saved position, and optional startup behavior match this design.
6. Automated tests pass and the packaged EXE completes the local smoke checks.
7. The release artifact and checksum are reported truthfully, with WebView2 and unsigned-binary caveats documented.
