# Release notes and verification

## Supported release

- Target: Windows 11 x64 (`x86_64-pc-windows-msvc`).
- Artifact: one direct `usage-widget.exe`; there is no MSI, NSIS installer, or background service.
- Runtime dependency: the Evergreen Microsoft Edge WebView2 Runtime supplied with Windows 11.
- Updates: there is no automatic updater.
- Signing: the first release is unsigned. A local successful build does not establish Microsoft SmartScreen reputation, and Windows may show an unrecognized-app warning.

If WebView2 is unavailable, the app exits nonzero after a fixed, path-free WebView2 message. Local-state recovery failures and other GUI startup failures use different fixed, path-free messages.

Moving the EXE does not move any app-owned state, which remains under `%LOCALAPPDATA%\UsageWidget`. It does invalidate the absolute paths used by optional Claude tracking and launch-at-sign-in; use both applicable **Repair** tray actions after a move.

## Release artifact gate

The source tree alone is not proof of a release artifact. The release handoff is complete only after the direct x64 executable has been built without bundling, copied to `release\usage-widget.exe`, and measured. No size, hash, signature status, or packaged smoke result should be inferred before that build.

The handoff should include:

- `release\usage-widget.exe`;
- `release\usage-widget.exe.sha256`, formatted as `<lowercase hash> *usage-widget.exe`;
- `release\build-info.txt`, containing the exact tool versions, Git commit, build timestamp, target triple, byte size, and SHA-256 from the completed build.

After the no-bundle build and again after copying the handoff executable, verify that each real artifact is AMD64 PE32+ with the Windows GUI subsystem:

```powershell
.\scripts\check-portable-exe.ps1 -Path .\src-tauri\target\x86_64-pc-windows-msvc\release\usage-widget.exe
.\scripts\check-portable-exe.ps1 -Path .\release\usage-widget.exe
```

Each command must report `PE_MACHINE=0x8664`, `PE_SUBSYSTEM=2`, and `PORTABLE_EXE_CHECK=PASS` with exit code 0. The checker bounds the complete PE/COFF and PE32+ optional headers, including the declared data-directory extent, before enforcing AMD64 and Windows GUI. This behavioral artifact check does not infer the subsystem from source text.

## Verify the checksum

From PowerShell in the `release` directory:

```powershell
$expected = (((Get-Content .\usage-widget.exe.sha256 -Raw) -split '\s+')[0]).ToLowerInvariant()
$actual = (Get-FileHash .\usage-widget.exe -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw 'usage-widget.exe checksum mismatch' }
```

A matching SHA-256 proves that the file matches the measured handoff artifact. It does not provide code-signing identity or SmartScreen reputation.

See [smoke-checklist.md](smoke-checklist.md) for the required verification sequence and the opt-in mutation boundary.
