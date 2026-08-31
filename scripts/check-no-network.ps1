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

function Invoke-BoundedCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Command,

        [Parameter(Mandatory = $true)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$TimeoutMilliseconds
    )

    $powerShell = [System.Management.Automation.PowerShell]::Create()
    $disposePowerShell = $true
    $asynchronous = $null
    try {
        [void]$powerShell.AddScript($Command.ToString())
        $asynchronous = $powerShell.BeginInvoke()
        if (-not $asynchronous.AsyncWaitHandle.WaitOne($TimeoutMilliseconds)) {
            try {
                [void]$powerShell.BeginStop($null, $null)
            }
            catch {
            }
            $disposePowerShell = $false
            throw [System.TimeoutException]::new('Inspection command timed out.')
        }

        $output = @($powerShell.EndInvoke($asynchronous))
        if ($powerShell.HadErrors -or $powerShell.Streams.Error.Count -ne 0) {
            throw [System.InvalidOperationException]::new('Inspection command failed.')
        }
        $output
    }
    finally {
        if ($null -ne $asynchronous -and $disposePowerShell) {
            $asynchronous.AsyncWaitHandle.Close()
        }
        if ($disposePowerShell) {
            $powerShell.Dispose()
        }
    }
}

function Get-BoundedProcessSnapshot {
    param([int]$TimeoutMilliseconds)

    @(
        Invoke-BoundedCommand -TimeoutMilliseconds $TimeoutMilliseconds -Command {
            Get-CimInstance `
                -ClassName Win32_Process `
                -Property ProcessId, ParentProcessId, CreationDate `
                -ErrorAction Stop
        }
    )
}

function Get-BoundedTcpSnapshot {
    param(
        [int[]]$ProcessIds,
        [int]$TimeoutMilliseconds
    )

    $connections = @(
        Invoke-BoundedCommand -TimeoutMilliseconds $TimeoutMilliseconds -Command {
            Get-NetTCPConnection -ErrorAction Stop
        }
    )
    @(Select-ProcessTreeConnections -ProcessIds $ProcessIds -Connections $connections)
}

function Get-ProcessCreationIdentity {
    param([Parameter(Mandatory = $true)][object]$Process)

    $creationDate = $Process.CreationDate
    if ($null -eq $creationDate) {
        throw 'Process creation identity is unavailable.'
    }
    if ($creationDate -is [DateTime]) {
        return 'datetime:{0}' -f $creationDate.ToUniversalTime().Ticks
    }
    $identity = [Convert]::ToString($creationDate, [Globalization.CultureInfo]::InvariantCulture)
    if ([string]::IsNullOrWhiteSpace($identity)) {
        throw 'Process creation identity is unavailable.'
    }
    'value:{0}' -f $identity
}

function Update-TrackedProcessTree {
    param(
        [Parameter(Mandatory = $true)]
        [int]$RootPid,

        [Parameter(Mandatory = $true)]
        [object[]]$Processes,

        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.Dictionary[int, string]]$Tracked
    )

    $byProcessId = [System.Collections.Generic.Dictionary[int, object]]::new()
    foreach ($process in $Processes) {
        $processId = [int]$process.ProcessId
        if ($processId -le 0 -or $byProcessId.ContainsKey($processId)) {
            throw 'Process identity snapshot is ambiguous.'
        }
        $byProcessId.Add($processId, $process)
    }

    if ($Tracked.Count -eq 0) {
        if (-not $byProcessId.ContainsKey($RootPid)) {
            throw 'The root process is unavailable.'
        }
        $Tracked.Add($RootPid, (Get-ProcessCreationIdentity -Process $byProcessId[$RootPid]))
    }

    foreach ($trackedProcessId in @($Tracked.Keys)) {
        if (-not $byProcessId.ContainsKey($trackedProcessId)) {
            throw 'A tracked process disappeared.'
        }
        $currentIdentity = Get-ProcessCreationIdentity -Process $byProcessId[$trackedProcessId]
        if ($Tracked[$trackedProcessId] -ne $currentIdentity) {
            throw 'A tracked process identifier was reused.'
        }
    }

    do {
        $added = $false
        foreach ($process in $Processes) {
            $processId = [int]$process.ProcessId
            $parentProcessId = [int]$process.ParentProcessId
            if (-not $Tracked.ContainsKey($processId) -and $Tracked.ContainsKey($parentProcessId)) {
                $Tracked.Add($processId, (Get-ProcessCreationIdentity -Process $process))
                $added = $true
            }
        }
    } while ($added)
}

function New-NetworkSampleResult {
    param(
        [bool]$Complete,
        [object[]]$Connections = @()
    )

    [pscustomobject][ordered]@{
        Complete    = $Complete
        Connections = @($Connections)
    }
}

function Get-InspectionTimeoutMilliseconds {
    param(
        [TimeSpan]$Now,
        [TimeSpan]$HardDeadline,
        [int]$MaximumMilliseconds
    )

    $remaining = $HardDeadline - $Now
    if ($remaining -le [TimeSpan]::Zero) {
        return 0
    }
    [int][Math]::Min(
        $MaximumMilliseconds,
        [Math]::Max(1, [Math]::Ceiling($remaining.TotalMilliseconds))
    )
}

function Invoke-NetworkSample {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$RootPid,

        [Parameter(Mandatory = $true)]
        [TimeSpan]$Duration,

        [scriptblock]$ProcessProvider,

        [scriptblock]$ConnectionProvider,

        [scriptblock]$NowProvider,

        [scriptblock]$SleepProvider
    )

    if ($Duration -le [TimeSpan]::Zero) {
        return New-NetworkSampleResult -Complete $false
    }

    $inspectionTimeoutMilliseconds = 2000
    $timer = $null
    if ($null -eq $NowProvider) {
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        $NowProvider = { $timer.Elapsed }.GetNewClosure()
    }
    if ($null -eq $SleepProvider) {
        $SleepProvider = { param([int]$Milliseconds) Start-Sleep -Milliseconds $Milliseconds }
    }
    if ($null -eq $ProcessProvider) {
        $ProcessProvider = {
            param([int]$TimeoutMilliseconds)
            @(Get-BoundedProcessSnapshot -TimeoutMilliseconds $TimeoutMilliseconds)
        }
    }
    if ($null -eq $ConnectionProvider) {
        $ConnectionProvider = {
            param([int[]]$ProcessIds, [int]$TimeoutMilliseconds)
            @(Get-BoundedTcpSnapshot -ProcessIds $ProcessIds -TimeoutMilliseconds $TimeoutMilliseconds)
        }
    }

    $observed = [System.Collections.Generic.Dictionary[string, object]]::new()
    $tracked = [System.Collections.Generic.Dictionary[int, string]]::new()
    try {
        $startedAt = [TimeSpan](& $NowProvider)
        $lastObservedAt = $startedAt
        $sampleDeadline = $startedAt + $Duration
        $hardDeadline = $sampleDeadline + [TimeSpan]::FromMilliseconds($inspectionTimeoutMilliseconds)

        while ($true) {
            $observedAt = [TimeSpan](& $NowProvider)
            if ($observedAt -lt $lastObservedAt) {
                return New-NetworkSampleResult -Complete $false
            }
            $lastObservedAt = $observedAt
            $timeoutMilliseconds = Get-InspectionTimeoutMilliseconds `
                -Now $observedAt `
                -HardDeadline $hardDeadline `
                -MaximumMilliseconds $inspectionTimeoutMilliseconds
            if ($timeoutMilliseconds -eq 0) {
                return New-NetworkSampleResult -Complete $false
            }

            $processes = @(& $ProcessProvider -TimeoutMilliseconds $timeoutMilliseconds)
            Update-TrackedProcessTree -RootPid $RootPid -Processes $processes -Tracked $tracked
            $processIds = @($tracked.Keys | Sort-Object)

            $observedAt = [TimeSpan](& $NowProvider)
            if ($observedAt -lt $lastObservedAt) {
                return New-NetworkSampleResult -Complete $false
            }
            $lastObservedAt = $observedAt
            $timeoutMilliseconds = Get-InspectionTimeoutMilliseconds `
                -Now $observedAt `
                -HardDeadline $hardDeadline `
                -MaximumMilliseconds $inspectionTimeoutMilliseconds
            if ($timeoutMilliseconds -eq 0) {
                return New-NetworkSampleResult -Complete $false
            }

            foreach ($connection in @(
                & $ConnectionProvider `
                    -ProcessIds $processIds `
                    -TimeoutMilliseconds $timeoutMilliseconds
            )) {
                $key = '{0}|{1}|{2}' -f $connection.PID, $connection.State, $connection.RemoteAddress
                if (-not $observed.ContainsKey($key)) {
                    $observed.Add($key, $connection)
                }
            }

            $observedAt = [TimeSpan](& $NowProvider)
            if ($observedAt -lt $lastObservedAt) {
                return New-NetworkSampleResult -Complete $false
            }
            $lastObservedAt = $observedAt
            $timeoutMilliseconds = Get-InspectionTimeoutMilliseconds `
                -Now $observedAt `
                -HardDeadline $hardDeadline `
                -MaximumMilliseconds $inspectionTimeoutMilliseconds
            if ($timeoutMilliseconds -eq 0) {
                return New-NetworkSampleResult -Complete $false
            }

            $trackedBeforeFinalIdentityCheck = $tracked.Count
            $processes = @(& $ProcessProvider -TimeoutMilliseconds $timeoutMilliseconds)
            Update-TrackedProcessTree -RootPid $RootPid -Processes $processes -Tracked $tracked
            $lineageExpandedAfterTcpInspection = $tracked.Count -ne $trackedBeforeFinalIdentityCheck

            $observedAt = [TimeSpan](& $NowProvider)
            if ($observedAt -lt $lastObservedAt) {
                return New-NetworkSampleResult -Complete $false
            }
            $lastObservedAt = $observedAt
            if ($observedAt -ge $sampleDeadline -and -not $lineageExpandedAfterTcpInspection) {
                return New-NetworkSampleResult -Complete $true -Connections @($observed.Values)
            }

            $remaining = $sampleDeadline - $observedAt
            if ($remaining -le [TimeSpan]::Zero) {
                continue
            }
            $sleepMilliseconds = [Math]::Min(200, [Math]::Max(1, [Math]::Ceiling($remaining.TotalMilliseconds)))
            & $SleepProvider $sleepMilliseconds
        }
    }
    catch {
        New-NetworkSampleResult -Complete $false
    }
    finally {
        if ($null -ne $timer) {
            $timer.Stop()
        }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    $result = Invoke-NetworkSample -RootPid $RootPid -Duration ([TimeSpan]::FromSeconds(30))
    if (-not $result.Complete) {
        exit 2
    }
    $result.Connections | Write-Output
    if ($result.Connections.Count -eq 0) {
        exit 0
    }
    exit 1
}
