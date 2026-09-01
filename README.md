# Usage Widget

Usage Widget is a portable Windows 11 x64 desktop app for exact, locally observed Claude Code and Codex subscription-limit windows. It shows the rounded percentage remaining and the time until reset for each available five-hour or seven-day window. It does not show API billing or spend.

## Run it

Keep `usage-widget.exe` wherever you want to run it, then double-click it. There is no installer. Windows 11's Evergreen WebView2 Runtime is required.

The first GUI process opens the widget. Starting the EXE again shows and focuses the existing widget instead of keeping a second GUI instance. The whole widget surface is draggable except `[x]`. `[x]` and Escape hide the window; they do not exit the process.

The notification-area menu is the settings surface:

- **Show/Hide** toggles the window. A left-click on the tray icon also shows it.
- **Refresh** immediately rescans the local Codex sources. The app watches for changes in the background, while the visible widget reads the cached result without repeatedly rescanning history.
- **Always on Top** toggles and saves the topmost preference. It is enabled by default.
- **Launch at Sign-in** opts into a per-user Windows startup entry. When the EXE has moved, this becomes **Repair Launch at Sign-in**.
- **Enable Claude Tracking** adds the widget's status-line command only when Claude's settings file is valid and has no existing status line. It becomes **Disable Claude Tracking** after setup or **Repair Claude Tracking** when the installed EXE path no longer matches. A conflict is left unchanged for manual review.
- **Quit** stops collection, removes the tray icon, and exits.

## What the numbers mean

Codex data comes from local rollout JSONL files under `%CODEX_HOME%\sessions` and `%CODEX_HOME%\archived_sessions`, or `%USERPROFILE%\.codex\sessions` and `archived_sessions` when `CODEX_HOME` is unset. Current records identified as the general `codex` limit may contain one or both exact 300-minute and 10,080-minute windows; the widget shows only the windows present in that record. Named model-specific limits are not substituted for the general account limit. Legacy records without a limit identifier are accepted only when both exact windows are present.

Claude data arrives only through an opt-in Claude Code status-line command. The settings file is `%CLAUDE_CONFIG_DIR%\settings.json`, or `%USERPROFILE%\.claude\settings.json` when `CLAUDE_CONFIG_DIR` is unset. After tracking is enabled, Claude remains hidden until a later Claude response supplies one valid payload containing both exact windows. If the payload is incomplete, invalid, ambiguous, or expired, Claude stays hidden.

For each valid window, the app calculates `round(clamp(100 - used percent, 0, 100))`. A missing window is hidden rather than copied from another limit or estimated from tokens, messages, elapsed time, or a plan assumption. Reset text is a countdown to the exact locally observed reset timestamp. Data is only as fresh as the latest valid local CLI record or status-line payload.

If neither provider has a valid current snapshot, the widget displays `NO CURRENT LIMIT DATA`.

## Local-only boundary

The release app:

- makes no provider API or other application-originated network calls;
- has no HTTP listener, telemetry, analytics, updater, OAuth, cookie access, or remote asset;
- does not request or extract API keys or credentials, and never puts them, prompts, transcripts, account identifiers, or provider source paths in app-owned state;
- stores only normalized limits, window position, preferences, and the identities needed to manage the two optional integrations.

See [docs/PRIVACY.md](docs/PRIVACY.md) for the exact stored fields and mutation boundaries.

## Moving the EXE

Claude tracking and launch-at-sign-in store the absolute path of the EXE. If you move `usage-widget.exe`, use the corresponding **Repair** tray action before relying on that integration. Claude repair refuses to rewrite a status line that no longer exactly matches the widget-owned object. Launch-at-sign-in repair uses the fixed per-user **Usage Widget** entry and the stored EXE path, rewrites that widget-named entry for the current EXE, and attempts to restore the exact prior registration snapshot if the operation fails.

`usagewidget.py` is a legacy local prototype and an unshipped reference. It is not the release application.
