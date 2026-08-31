# Release notes and verification

## Supported release

- Target: Windows 11 x64 (`x86_64-pc-windows-msvc`).
- Artifact: one direct `usage-widget.exe`; there is no MSI, NSIS installer, or background service.
- Runtime dependency: the Evergreen Microsoft Edge WebView2 Runtime supplied with Windows 11.
- Updates: there is no automatic updater.
- Signing: the first release is unsigned. A local successful build does not establish Microsoft SmartScreen reputation, and Windows may show an unrecognized-app warning.

If WebView2 is unavailable, the app exits nonzero after a fixed local message explaining that the Windows WebView2 Runtime is required.

Moving the EXE does not move any app-owned state, which remains under `%LOCALAPPDATA%\UsageWidget`. It does invalidate the absolute paths used by optional Claude tracking and launch-at-sign-in; use both applicable **Repair** tray actions after a move.

## Release artifact gate

The source tree alone is not proof of a release artifact. The release handoff is complete only after the direct x64 executable has been built without bundling, copied to `release\usage-widget.exe`, and measured. No size, hash, signature status, or packaged smoke result should be inferred before that build.

The handoff should include:

- `release\usage-widget.exe`;
- `release\usage-widget.exe.sha256`, formatted as `<lowercase hash> *usage-widget.exe`;
- `release\build-info.txt`, containing the exact tool versions, Git commit, build timestamp, target triple, byte size, and SHA-256 from the completed build.

## Verify the checksum

From PowerShell in the `release` directory:

```powershell
$expected = (((Get-Content .\usage-widget.exe.sha256 -Raw) -split '\s+')[0]).ToLowerInvariant()
$actual = (Get-FileHash .\usage-widget.exe -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw 'usage-widget.exe checksum mismatch' }
```

A matching SHA-256 proves that the file matches the measured handoff artifact. It does not provide code-signing identity or SmartScreen reputation.

See [smoke-checklist.md](smoke-checklist.md) for the required verification sequence and the opt-in mutation boundary.
