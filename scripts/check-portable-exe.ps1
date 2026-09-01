[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Path
)

$ErrorActionPreference = 'Stop'
$reader = $null

try {
    $resolved = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    $stream = [System.IO.File]::Open(
        $resolved,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    $reader = [System.IO.BinaryReader]::new($stream)

    if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
        throw [System.IO.InvalidDataException]::new('invalid DOS header')
    }

    $stream.Position = 0x3C
    $peOffset = $reader.ReadUInt32()
    if ($peOffset -gt ($stream.Length - 94)) {
        throw [System.IO.InvalidDataException]::new('invalid PE offset')
    }

    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) {
        throw [System.IO.InvalidDataException]::new('invalid PE signature')
    }

    $machine = $reader.ReadUInt16()
    $stream.Position = $peOffset + 20
    $optionalHeaderSize = $reader.ReadUInt16()
    $optionalHeaderOffset = $peOffset + 24
    if ($optionalHeaderSize -lt 70 -or $optionalHeaderOffset -gt ($stream.Length - $optionalHeaderSize)) {
        throw [System.IO.InvalidDataException]::new('invalid optional header')
    }

    $stream.Position = $optionalHeaderOffset
    if ($reader.ReadUInt16() -ne 0x020B) {
        throw [System.IO.InvalidDataException]::new('not a PE32+ executable')
    }

    $stream.Position = $optionalHeaderOffset + 68
    $subsystem = $reader.ReadUInt16()
}
catch {
    [Console]::Error.WriteLine('PORTABLE EXE CHECK FAILED: invalid executable')
    exit 1
}
finally {
    if ($null -ne $reader) {
        $reader.Dispose()
    }
}

Write-Output ('PE_MACHINE=0x{0:X4}' -f $machine)
Write-Output ('PE_SUBSYSTEM={0}' -f $subsystem)

if ($machine -ne 0x8664 -or $subsystem -ne 2) {
    [Console]::Error.WriteLine('PORTABLE EXE CHECK FAILED: expected AMD64 Windows GUI')
    exit 1
}

Write-Output 'PORTABLE_EXE_CHECK=PASS'
