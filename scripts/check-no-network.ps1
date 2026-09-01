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

function Write-NetworkConnectionRows {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Connections,

        [Parameter()]
        [System.IO.TextWriter]$Writer = [Console]::Out
    )

    foreach ($connection in $Connections) {
        $Writer.WriteLine(
            ("{0}`t{1}`t{2}" -f `
                [int]$connection.PID, `
                [string]$connection.State, `
                [string]$connection.RemoteAddress)
        )
    }
    $Writer.Flush()
}

function New-InspectionAggregateException {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Message,

        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.List[System.Exception]]$Failures
    )

    [System.AggregateException]::new($Message, [System.Exception[]]$Failures.ToArray())
}

function Invoke-InspectionResourceCleanup {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$Resource,

        [Parameter(Mandatory = $true)]
        [hashtable]$Operations
    )

    $failures = [System.Collections.Generic.List[System.Exception]]::new()
    foreach ($requiredOperation in @(
        'DisposePowerShell',
        'GetRunspaceState',
        'CloseRunspace',
        'DisposeRunspace'
    )) {
        if ($Operations[$requiredOperation] -isnot [scriptblock]) {
            $failures.Add(
                [System.InvalidOperationException]::new('Inspection cleanup operations are incomplete.')
            )
        }
    }
    if ($failures.Count -ne 0) {
        throw (New-InspectionAggregateException `
                -Message 'Inspection cleanup configuration failed.' `
                -Failures $failures)
    }

    if ($null -ne $Resource.PowerShell) {
        try {
            [void](& $Operations['DisposePowerShell'] $Resource.PowerShell)
        }
        catch {
            $failures.Add($_.Exception)
        }
    }

    if ($null -ne $Resource.Runspace) {
        $runspaceIsClosed = $false
        try {
            $runspaceState = & $Operations['GetRunspaceState'] $Resource.Runspace
            $runspaceIsClosed = [string]$runspaceState -eq 'Closed'
        }
        catch {
            $failures.Add($_.Exception)
        }

        if (-not $runspaceIsClosed) {
            try {
                [void](& $Operations['CloseRunspace'] $Resource.Runspace)
            }
            catch {
                $failures.Add($_.Exception)
            }
        }
        try {
            [void](& $Operations['DisposeRunspace'] $Resource.Runspace)
        }
        catch {
            $failures.Add($_.Exception)
        }
    }

    if ($failures.Count -ne 0) {
        throw (New-InspectionAggregateException `
                -Message 'One or more inspection cleanup actions failed.' `
                -Failures $failures)
    }
}

function New-PowerShellResourceCleanupOperations {
    @{
        DisposePowerShell = {
            param([System.Management.Automation.PowerShell]$PowerShell)
            $PowerShell.Dispose()
        }
        GetRunspaceState = {
            param([System.Management.Automation.Runspaces.Runspace]$Runspace)
            $Runspace.RunspaceStateInfo.State
        }
        CloseRunspace = {
            param([System.Management.Automation.Runspaces.Runspace]$Runspace)
            $Runspace.Close()
        }
        DisposeRunspace = {
            param([System.Management.Automation.Runspaces.Runspace]$Runspace)
            $Runspace.Dispose()
        }
    }
}

function Invoke-GuardedInspectionResource {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [hashtable]$Operations
    )

    $resource = $null
    $resourceCreated = $false
    $result = @()
    $failures = [System.Collections.Generic.List[System.Exception]]::new()
    try {
        try {
            foreach ($requiredOperation in @('Create', 'Configure', 'Use', 'Dispose')) {
                if ($Operations[$requiredOperation] -isnot [scriptblock]) {
                    throw [System.InvalidOperationException]::new('Inspection resource operations are incomplete.')
                }
            }

            $resource = & $Operations['Create']
            if ($null -eq $resource) {
                throw [System.InvalidOperationException]::new('The inspection resource is unavailable.')
            }
            $resourceCreated = $true
            [void](& $Operations['Configure'] $resource)
            $result = @(& $Operations['Use'] $resource)
        }
        catch {
            $failures.Add($_.Exception)
        }
    }
    finally {
        if ($resourceCreated) {
            try {
                [void](& $Operations['Dispose'] $resource)
            }
            catch {
                $failures.Add($_.Exception)
            }
        }
    }

    if ($failures.Count -ne 0) {
        if ($failures.Count -eq 1) {
            throw $failures[0]
        }
        throw (New-InspectionAggregateException `
                -Message 'Inspection resource use and cleanup failed.' `
                -Failures $failures)
    }
    $result
}

function Invoke-InspectionLifecycle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [hashtable]$Operations,

        [Parameter(Mandatory = $true)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$TimeoutMilliseconds
    )

    $invocation = $null
    $stopInvocation = $null
    $timedOut = $false
    $output = @()
    $failures = [System.Collections.Generic.List[System.Exception]]::new()
    try {
        try {
            foreach ($requiredOperation in @(
                'BeginInvoke',
                'WaitForInvocation',
                'BeginStop',
                'WaitForStop',
                'StopSynchronously',
                'EndStop',
                'EndInvoke',
                'CloseStopWaitHandle',
                'CloseInvocationWaitHandle'
            )) {
                if ($Operations[$requiredOperation] -isnot [scriptblock]) {
                    throw [System.InvalidOperationException]::new('Inspection lifecycle operations are incomplete.')
                }
            }

            try {
                $invocation = & $Operations['BeginInvoke']
                if ($null -eq $invocation) {
                    throw [System.InvalidOperationException]::new('Inspection invocation did not start.')
                }
            }
            catch {
                $failures.Add($_.Exception)
            }

            if ($null -ne $invocation) {
                $invocationCompleted = $false
                try {
                    $invocationCompleted = [bool](& $Operations['WaitForInvocation'] `
                            $invocation `
                            $TimeoutMilliseconds)
                }
                catch {
                    $failures.Add($_.Exception)
                }

                if ($invocationCompleted) {
                    try {
                        $output = @(& $Operations['EndInvoke'] $invocation $false)
                    }
                    catch {
                        $failures.Add($_.Exception)
                    }
                }
                else {
                    $timedOut = $true
                    $synchronousStopRequired = $false
                    try {
                        $stopInvocation = & $Operations['BeginStop']
                        if ($null -eq $stopInvocation) {
                            throw [System.InvalidOperationException]::new('Inspection stop did not start.')
                        }
                    }
                    catch {
                        $failures.Add($_.Exception)
                        $synchronousStopRequired = $true
                    }

                    if ($null -ne $stopInvocation) {
                        try {
                            [void](& $Operations['WaitForStop'] $stopInvocation)
                        }
                        catch {
                            $failures.Add($_.Exception)
                            $synchronousStopRequired = $true
                        }
                        try {
                            [void](& $Operations['EndStop'] $stopInvocation)
                        }
                        catch {
                            $failures.Add($_.Exception)
                            $synchronousStopRequired = $true
                        }
                    }

                    if ($synchronousStopRequired) {
                        try {
                            [void](& $Operations['StopSynchronously'])
                        }
                        catch {
                            $failures.Add($_.Exception)
                        }
                    }

                    try {
                        [void](& $Operations['EndInvoke'] $invocation $true)
                    }
                    catch {
                        $failures.Add($_.Exception)
                    }
                }
            }
        }
        catch {
            $failures.Add($_.Exception)
        }
    }
    finally {
        if ($null -ne $stopInvocation) {
            try {
                [void](& $Operations['CloseStopWaitHandle'] $stopInvocation)
            }
            catch {
                $failures.Add($_.Exception)
            }
        }
        if ($null -ne $invocation) {
            try {
                [void](& $Operations['CloseInvocationWaitHandle'] $invocation)
            }
            catch {
                $failures.Add($_.Exception)
            }
        }
    }

    if ($failures.Count -ne 0) {
        throw (New-InspectionAggregateException `
                -Message 'Inspection lifecycle or cleanup failed.' `
                -Failures $failures)
    }
    if ($timedOut) {
        throw [System.TimeoutException]::new('Inspection command timed out.')
    }
    $output
}

function New-PowerShellLifecycleOperations {
    param(
        [Parameter(Mandatory = $true)]
        [System.Management.Automation.PowerShell]$PowerShell
    )

    @{
        BeginInvoke = { $PowerShell.BeginInvoke() }.GetNewClosure()
        WaitForInvocation = {
            param($AsyncResult, [int]$TimeoutMilliseconds)
            $AsyncResult.AsyncWaitHandle.WaitOne($TimeoutMilliseconds)
        }
        BeginStop = { $PowerShell.BeginStop($null, $null) }.GetNewClosure()
        WaitForStop = {
            param($AsyncResult)
            [void]$AsyncResult.AsyncWaitHandle.WaitOne()
        }
        StopSynchronously = { $PowerShell.Stop() }.GetNewClosure()
        EndStop = {
            param($AsyncResult)
            $PowerShell.EndStop($AsyncResult)
        }.GetNewClosure()
        EndInvoke = {
            param($AsyncResult, [bool]$ExpectedStop)
            try {
                $result = @($PowerShell.EndInvoke($AsyncResult))
            }
            catch [System.Management.Automation.PipelineStoppedException] {
                if (-not $ExpectedStop) {
                    throw
                }
                return @()
            }
            if ($PowerShell.HadErrors -or $PowerShell.Streams.Error.Count -ne 0) {
                throw [System.InvalidOperationException]::new('Inspection command failed.')
            }
            $result
        }.GetNewClosure()
        CloseStopWaitHandle = {
            param($AsyncResult)
            $AsyncResult.AsyncWaitHandle.Close()
        }
        CloseInvocationWaitHandle = {
            param($AsyncResult)
            $AsyncResult.AsyncWaitHandle.Close()
        }
    }
}

function Invoke-BoundedInspection {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [ValidateSet('ProcessSnapshot', 'TcpSnapshot')]
        [string]$InspectionKind,

        [Parameter(Mandatory = $true)]
        [ValidateRange(1, [int]::MaxValue)]
        [int]$TimeoutMilliseconds
    )

    $resourceOperations = @{
        Create = {
            [pscustomobject][ordered]@{
                InspectionKind      = $InspectionKind
                TimeoutMilliseconds = $TimeoutMilliseconds
                Runspace            = $null
                PowerShell          = $null
            }
        }.GetNewClosure()
        Configure = {
            param($Resource)
            $Resource.Runspace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
            if ($Resource.Runspace -isnot [System.Management.Automation.Runspaces.Runspace]) {
                throw [System.InvalidOperationException]::new('The inspection runspace is unavailable.')
            }
            $Resource.Runspace.Open()

            $Resource.PowerShell = [System.Management.Automation.PowerShell]::Create()
            if ($Resource.PowerShell -isnot [System.Management.Automation.PowerShell]) {
                throw [System.InvalidOperationException]::new('The inspection pipeline is unavailable.')
            }
            $Resource.PowerShell.Runspace = $Resource.Runspace
            $pipeline = $Resource.PowerShell

            $errorAction = [System.Management.Automation.ActionPreference]::Stop
            switch ($Resource.InspectionKind) {
                'ProcessSnapshot' {
                    $operationTimeoutSeconds = [int][Math]::Max(
                        1,
                        [Math]::Ceiling($Resource.TimeoutMilliseconds / 1000.0)
                    )
                    [void]$pipeline.AddCommand('Get-CimInstance')
                    [void]$pipeline.AddParameter('ClassName', 'Win32_Process')
                    [void]$pipeline.AddParameter(
                        'Property',
                        @('ProcessId', 'ParentProcessId', 'CreationDate')
                    )
                    [void]$pipeline.AddParameter('OperationTimeoutSec', $operationTimeoutSeconds)
                    [void]$pipeline.AddParameter('ErrorAction', $errorAction)
                }
                'TcpSnapshot' {
                    [void]$pipeline.AddCommand('Get-NetTCPConnection')
                    [void]$pipeline.AddParameter('ErrorAction', $errorAction)
                }
            }
        }
        Use = {
            param($Resource)
            if ($Resource.PowerShell -isnot [System.Management.Automation.PowerShell] -or
                $Resource.Runspace -isnot [System.Management.Automation.Runspaces.Runspace]) {
                throw [System.InvalidOperationException]::new('The configured inspection resource is unavailable.')
            }
            $lifecycleOperations = New-PowerShellLifecycleOperations -PowerShell $Resource.PowerShell
            Invoke-InspectionLifecycle `
                -Operations $lifecycleOperations `
                -TimeoutMilliseconds $Resource.TimeoutMilliseconds
        }
        Dispose = {
            param($Resource)
            $cleanupOperations = New-PowerShellResourceCleanupOperations
            Invoke-InspectionResourceCleanup `
                -Resource $Resource `
                -Operations $cleanupOperations
        }
    }
    Invoke-GuardedInspectionResource -Operations $resourceOperations
}

function Get-BoundedProcessSnapshot {
    param([int]$TimeoutMilliseconds)

    @(
        Invoke-BoundedInspection `
            -InspectionKind ProcessSnapshot `
            -TimeoutMilliseconds $TimeoutMilliseconds
    )
}

function Get-BoundedTcpSnapshot {
    param(
        [int[]]$ProcessIds,
        [int]$TimeoutMilliseconds
    )

    $connections = @(
        Invoke-BoundedInspection `
            -InspectionKind TcpSnapshot `
            -TimeoutMilliseconds $TimeoutMilliseconds
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
        if ($processId -eq 0) {
            continue
        }
        if ($processId -lt 0 -or $byProcessId.ContainsKey($processId)) {
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

function Write-NetworkSampleResult {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$Result,

        [Parameter()]
        [System.IO.TextWriter]$Writer = [Console]::Out
    )

    if (-not $Result.Complete) {
        return 2
    }
    $connections = @($Result.Connections)
    if ($connections.Count -eq 0) {
        return 0
    }
    try {
        Write-NetworkConnectionRows -Connections $connections -Writer $Writer
    }
    catch {
        return 2
    }
    1
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
    exit (Write-NetworkSampleResult -Result $result)
}
