[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int]::MaxValue)]
    [int]$RootPid
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-ProcessTreeIds {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$RootPid,

        [Parameter()]
        [AllowEmptyCollection()]
        [object[]]$Processes
    )

    if (-not $PSBoundParameters.ContainsKey('Processes')) {
        $Processes = @(
            Get-CimInstance -ClassName Win32_Process -Property ProcessId, ParentProcessId -ErrorAction Stop
        )
    }

    $processIds = [System.Collections.Generic.HashSet[int]]::new()
    [void]$processIds.Add($RootPid)

    do {
        $added = $false
        foreach ($process in $Processes) {
            $parentProcessId = [int]$process.ParentProcessId
            $processId = [int]$process.ProcessId
            if ($processIds.Contains($parentProcessId) -and $processIds.Add($processId)) {
                $added = $true
            }
        }
    } while ($added)

    @($processIds | Sort-Object)
}

function Select-ProcessTreeConnections {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int[]]$ProcessIds,

        [Parameter()]
        [AllowEmptyCollection()]
        [object[]]$Connections
    )

    if (-not $PSBoundParameters.ContainsKey('Connections')) {
        $Connections = @(Get-NetTCPConnection -ErrorAction Stop)
    }

    $owners = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($processId in $ProcessIds) {
        [void]$owners.Add($processId)
    }

    foreach ($connection in $Connections) {
        $state = [string]$connection.State
        $owner = [int]$connection.OwningProcess
        if ($owners.Contains($owner) -and $state -in @('Established', 'SynSent', 'SynReceived')) {
            [pscustomobject][ordered]@{
                PID           = $owner
                State         = $state
                RemoteAddress = [string]$connection.RemoteAddress
            }
        }
    }
}

function Invoke-NetworkSample {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$RootPid,

        [Parameter(Mandatory = $true)]
        [TimeSpan]$Duration
    )

    if ($Duration -le [TimeSpan]::Zero) {
        throw 'The sample duration must be positive.'
    }

    $observed = [System.Collections.Generic.Dictionary[string, object]]::new()
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        while ($timer.Elapsed -lt $Duration) {
            $processIds = @(Get-ProcessTreeIds -RootPid $RootPid)
            foreach ($connection in @(Select-ProcessTreeConnections -ProcessIds $processIds)) {
                $key = '{0}|{1}|{2}' -f $connection.PID, $connection.State, $connection.RemoteAddress
                if (-not $observed.ContainsKey($key)) {
                    $observed.Add($key, $connection)
                }
            }

            $remaining = $Duration - $timer.Elapsed
            if ($remaining -le [TimeSpan]::Zero) {
                break
            }
            $sleepMilliseconds = [Math]::Min(200, [Math]::Max(1, [Math]::Ceiling($remaining.TotalMilliseconds)))
            Start-Sleep -Milliseconds $sleepMilliseconds
        }
    }
    finally {
        $timer.Stop()
    }

    @($observed.Values)
}

if ($MyInvocation.InvocationName -ne '.') {
    try {
        $connections = @(Invoke-NetworkSample -RootPid $RootPid -Duration ([TimeSpan]::FromSeconds(30)))
        $connections | Write-Output
        if ($connections.Count -eq 0) {
            exit 0
        }
        exit 1
    }
    catch {
        Write-Error 'Network inspection failed.'
        exit 2
    }
}
