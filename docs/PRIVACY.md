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

**Launch at Sign-in** uses a fixed per-user Windows startup entry named `Usage Widget`. The requested flag and stored EXE path drive disabled, enabled, and moved-path repair status. Repair rewrites that widget-named entry for the current EXE. Enable, disable, and repair snapshot the relevant prior registration values and attempt to restore that exact snapshot if the registration operation or app-state persistence fails. This integration does not claim command-byte ownership or ambiguity detection for the fixed entry.

Moving the EXE makes either enabled path-based integration require explicit repair.

## Network verification

`scripts/check-no-network.ps1` is a read-only observation tool. Given a root PID, it requires that process to exist with a stable creation identity, recursively follows new `ParentProcessId` descendants, retains their PID-plus-creation identities, and samples TCP state for the tracked tree across a complete 30-second window. Each normal CIM and TCP provider wait is capped by the remaining inspection budget. If that deadline expires, exit 0 is withheld while the script requests cooperative cancellation and independently attempts every applicable cleanup action for the owned inspector pipeline and runspace. Any cleanup failure exits 2, and cleanup is allowed to extend beyond the nominal 30-second window if a provider is slow to honor cancellation. Exit 0 means every required inspection completed, all tracked identities stayed stable through the final TCP inspection, and no connecting or established entry was observed. A missing, disappeared, or reused tracked identity, an inspection error or timeout, or an incomplete window exits 2 instead of passing. Detected connections are the only rows printed, with PID, state, and remote address only. The script does not terminate a process, modify the firewall, or write system state.
