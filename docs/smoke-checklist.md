# Release smoke checklist

Record commands, exit codes, test counts, observed results, exact artifact bytes, SHA-256, and Git commit. Never record raw provider input, transcript text, credentials, account data, or provider source paths.

## A. Automated and synthetic evidence

These checks use source, fixtures, temporary files, and fake startup adapters. They do not authorize changing the real Claude settings or Windows sign-in registration.

- [ ] Run the complete quality gate and require every command to exit 0:

  ```powershell
  npm.cmd ci
  npm.cmd test
  cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
  cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets -- -D warnings
  cargo test --manifest-path .\src-tauri\Cargo.toml
  ```

- [ ] Build only the direct portable x64 executable:

  ```powershell
  npm.cmd run tauri -- build --no-bundle --target x86_64-pc-windows-msvc
  ```

- [ ] Confirm `src-tauri\target\x86_64-pc-windows-msvc\release\usage-widget.exe` exists.
- [ ] Confirm no MSI or NSIS installer was produced.
- [ ] Create `release`, copy the executable to `release\usage-widget.exe`, and derive (never estimate) the lowercase SHA-256, byte size, build timestamp, toolchain versions, target triple, and `git rev-parse HEAD`.
- [ ] Write `release\usage-widget.exe.sha256` as `<hash> *usage-widget.exe` and the measured facts to `release\build-info.txt`.
- [ ] Exercise a valid Claude capture through the Rust integration boundary with a temporary `JsonStateStore`.
- [ ] Exercise Claude enable, disable, and repair against a temporary `CLAUDE_CONFIG_DIR` only.
- [ ] Invoke the packaged `claude-capture` entrypoint with empty and malformed standard input. Confirm each prints only `USAGE: NO EXACT LIMITS`.
- [ ] Hash the real app state file before and after those rejected packaged inputs and confirm its bytes did not change.
- [ ] Confirm the real `%USERPROFILE%\.claude\settings.json` and Windows sign-in registration remained unchanged.

## B. Packaged read-only-provider smoke

This section may update app-owned state such as window position, but it must not opt into or alter real Claude settings or Windows startup registration.

- [ ] Launch `release\usage-widget.exe` and wait up to ten seconds for its process and window.
- [ ] Launch the EXE a second time and confirm no second persistent GUI process remains; the original window is shown and focused.
- [ ] Compare the Codex card with a sanitized projection from `python .\usagewidget.py once`. Pipe JSON through `ConvertFrom-Json` and output only provider name plus each window's numeric used percentage and reset epoch. Do not output source or roots fields.
- [ ] Confirm the approved terminal-style glyphs, ten-cell meters, rounded remaining calculation, and reset countdowns.
- [ ] Confirm the fixed width, drag behavior, always-on-top default, and saved on-screen position.
- [ ] Confirm `[x]` hides without exiting, Escape hides without exiting, and tray **Show/Hide** restores the same process.
- [ ] Confirm tray **Refresh** completes without freezing the UI.
- [ ] Confirm the Claude card stays hidden until a valid captured sample exists.
- [ ] Inspect a screenshot with the local image-viewing workflow; do not upload it or infer unobserved behavior from source alone.
- [ ] While the app is running, perform the read-only 30-second process-tree check:

  ```powershell
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-no-network.ps1 -RootPid <usage-widget-pid>
  ```

  Exit 0 with no connection rows is the passing result only after the root and every recursively discovered descendant retain the same PID-plus-creation identity and every bounded CIM/TCP inspection completes throughout the 30-second window. Normal provider waits are capped by the remaining inspection budget. After a timeout, exit 0 is withheld while the script requests cooperative cancellation and independently attempts every applicable cleanup action for the owned inspector pipeline and runspace; any cleanup failure exits 2. A provider that is slow to cancel can therefore delay exit beyond the nominal 30 seconds. Exit 1 means at least one connecting or established entry was observed during that stable complete sample. Exit 2 means the root was absent, a tracked process disappeared or its PID was reused, an inspection or cleanup action failed or timed out, or the complete sample could not be trusted; it is not a pass. The script must print only PID, state, and remote address rows for detected connections.
- [ ] Use tray **Quit** and confirm the process exits and the tray icon disappears.

## C. Real opt-in system mutations: separate approval required

Do not perform either check merely because the synthetic and read-only checks passed. Record them as **UNRUN: approval required** unless the user separately authorizes the real mutation.

- [ ] **UNRUN: approval required:** Enable, disable, and repair Claude Tracking against the real Claude settings file. Verify the backup, preservation of unrelated settings, ownership refusal, and moved-EXE repair.
- [ ] **UNRUN: approval required:** Enable, disable, and repair Launch at Sign-in against the current user's real Windows startup registration.

## D. Final repository and handoff review

- [ ] Run `git status --short`.
- [ ] Run `git diff --check`.
- [ ] Run `git log --oneline --decorate -10`.
- [ ] Confirm only intentionally untracked or ignored release artifacts remain; commit any tested source correction separately and do not commit `release/`.
- [ ] Report automated test counts/results, exact artifact byte size, SHA-256, Git commit, observed provider coverage, and each manual smoke result.
- [ ] Keep synthetic, read-only local, and opt-in-but-unrun evidence clearly separated in the handoff.
