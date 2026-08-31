# Privacy and local data boundary

Usage Widget is local-only. Runtime inputs are local Codex rollout JSONL files and, only after opt-in, JSON sent to the executable's standard input by Claude Code's status-line facility. The app has no provider SDK, account login, API-key flow, browser automation, network client, telemetry, analytics, or update check.

## App-owned state

The state file is `%LOCALAPPDATA%\UsageWidget\state.json`. It contains only these fields:

- `schema_version`;
- `snapshots`, keyed by `codex` or `claude`, where each snapshot contains:
  - the fixed provider identifier;
  - `observed_at`, as a UTC epoch second;
  - `short_window` and `weekly_window`, each containing only `duration_minutes`, `used_percent`, and `resets_at`;
- `window`, containing only the last `x` and `y` screen coordinates, or `null`;
- `always_on_top`;
- `launch_at_signin_requested`;
- `startup_identity`, containing the absolute installed EXE path needed to detect a moved startup target, or `null`;
- `claude_tracking`, containing the absolute installed EXE path and the exact widget-owned `statusLine` command identity needed for safe repair or removal, or `null`.

Writes use a temporary sibling and Windows atomic replacement. Malformed state may be renamed beside the state file with a `state.corrupt.<number>.json` name and replaced logically by defaults. Expired snapshots remain non-visible and are never presented as current.

The state file does **not** contain raw provider records, prompts, responses, transcripts, credentials, tokens, API keys, cookies, account identifiers, organization data, provider source paths, file contents, token totals, or API spend.

## Local reads

For Codex, the app reads only `.jsonl` candidates below the configured local `sessions` and `archived_sessions` roots. It bounds the number of candidates, tail bytes, and record bytes before parsing. It extracts only the normalized numeric fields listed above. Source paths and record content are not returned to the frontend or persisted.

For Claude capture, standard input is size-bounded. The app allowlists the five-hour and seven-day used percentages and reset timestamps, assigns the two known durations, validates the complete snapshot, and discards the raw input. Invalid input prints only a fixed status message and does not update a provider snapshot.

## Optional external changes

No optional integration is enabled by default.

**Claude Tracking** reads the current Claude `settings.json`. Enable refuses missing, oversized, malformed, non-object, or already configured settings. Before its first write, it creates a timestamped backup beside that file, preserves every existing setting, and adds only the widget-owned `statusLine` object. The backup necessarily contains the pre-existing settings bytes; the widget does not copy those unrelated settings into its own state. Disable or repair proceeds only while the full installed object still matches the widget's recorded identity. Otherwise the file is left unchanged for manual review.

**Launch at Sign-in** uses the per-user Windows startup integration and stores the current EXE path. Enable, disable, and repair are tray actions. A failed or ambiguous operation is rolled back when possible and reported without exposing registry contents.

Moving the EXE makes either enabled path-based integration require explicit repair.

## Network verification

`scripts/check-no-network.ps1` is a read-only observation tool. Given a root PID, it recursively follows `ParentProcessId`, samples TCP state for that process tree for 30 seconds, and reports only PID, state, and remote address for connecting or established entries. It does not terminate a process, modify the firewall, or write system state.
