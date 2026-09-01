[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Path
)

$ErrorActionPreference = 'Stop'
$reader = $null
$stream = $null

try {
    $resolved = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    $stream = [System.IO.File]::Open(
        $resolved,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    $reader = [System.IO.BinaryReader]::new($stream)

    $fileLength = [long]$stream.Length
    if ($fileLength -lt 0x40 -or $reader.ReadUInt16() -ne 0x5A4D) {
        throw [System.IO.InvalidDataException]::new('invalid DOS header')
    }

    $stream.Position = 0x3C
    $peOffset = [long]$reader.ReadUInt32()
    if ($peOffset -lt 0x40 -or ($peOffset + 24L) -gt $fileLength) {
        throw [System.IO.InvalidDataException]::new('invalid PE offset')
    }

    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) {
        throw [System.IO.InvalidDataException]::new('invalid PE signature')
    }

    $machine = $reader.ReadUInt16()
    $stream.Position = $peOffset + 20
    $optionalHeaderSize = [long]$reader.ReadUInt16()
    $optionalHeaderOffset = $peOffset + 24
    $optionalHeaderEnd = $optionalHeaderOffset + $optionalHeaderSize
    if ($optionalHeaderSize -lt 0x70 -or $optionalHeaderEnd -gt $fileLength) {
        throw [System.IO.InvalidDataException]::new('invalid optional header')
    }

    $stream.Position = $optionalHeaderOffset
    if ($reader.ReadUInt16() -ne 0x020B) {
        throw [System.IO.InvalidDataException]::new('not a PE32+ executable')
    }

    $stream.Position = $optionalHeaderOffset + 68
    $subsystem = $reader.ReadUInt16()

    $stream.Position = $optionalHeaderOffset + 108
    $dataDirectoryCount = [long]$reader.ReadUInt32()
    $declaredDirectoryExtent = 0x70L + ($dataDirectoryCount * 8L)
    if ($declaredDirectoryExtent -gt $optionalHeaderSize) {
        throw [System.IO.InvalidDataException]::new('invalid data directory extent')
    }
}
catch {
    [Console]::Error.WriteLine('PORTABLE EXE CHECK FAILED: invalid executable')
    exit 1
}
finally {
    if ($null -ne $reader) {
        $reader.Dispose()
    }
    elseif ($null -ne $stream) {
        $stream.Dispose()
    }
}

Write-Output ('PE_MACHINE=0x{0:X4}' -f $machine)
Write-Output ('PE_SUBSYSTEM={0}' -f $subsystem)

if ($machine -ne 0x8664 -or $subsystem -ne 2) {
    [Console]::Error.WriteLine('PORTABLE EXE CHECK FAILED: expected AMD64 Windows GUI')
    exit 1
}

Write-Output 'PORTABLE_EXE_CHECK=PASS'
