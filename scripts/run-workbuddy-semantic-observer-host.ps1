[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CollectorPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$IdentityPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SessionId,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3600)]
    [int]$PollIntervalSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-ProjectSnapshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $files = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Filter '*.jsonl' | Sort-Object FullName)
    $signatureSource = @($files | ForEach-Object {
        "$($_.FullName)|$($_.Length)|$($_.LastWriteTimeUtc.Ticks)"
    }) -join "`n"
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $signatureBytes = [Text.Encoding]::UTF8.GetBytes($signatureSource)
        $signature = ([BitConverter]::ToString($sha256.ComputeHash($signatureBytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }
    [pscustomobject]@{
        file_count = $files.Count
        signature = $signature
    }
}

function Get-VerifiedRootProcesses {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath
    )

    $imageName = [IO.Path]::GetFileName($ExecutablePath).Replace("'", "''")
    $verified = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = '$imageName'" | Where-Object {
        -not [string]::IsNullOrWhiteSpace([string]$_.ExecutablePath) -and
        [StringComparer]::OrdinalIgnoreCase.Equals([IO.Path]::GetFullPath([string]$_.ExecutablePath), $ExecutablePath)
    })
    $verifiedProcessIds = [Collections.Generic.HashSet[uint32]]::new()
    foreach ($process in $verified) {
        $verifiedProcessIds.Add([uint32]$process.ProcessId) | Out-Null
    }
    @($verified | Where-Object { -not $verifiedProcessIds.Contains([uint32]$_.ParentProcessId) })
}

function Write-HealthRecord {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Status,
        [Parameter(Mandatory = $true)]
        [string]$CoverageStatus,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [uint32[]]$RootProcessIds,
        [Parameter(Mandatory = $true)]
        [int]$ProjectFileCount,
        [Parameter(Mandatory = $true)]
        [bool]$ProjectChanged,
        [Parameter(Mandatory = $false)]
        [object]$SyncResult,
        [Parameter(Mandatory = $false)]
        [string]$ErrorMessage
    )

    $record = [ordered]@{
        schema_version = '0.1.0'
        captured_at_unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        status = $Status
        coverage_status = $CoverageStatus
        verified_root_count = $RootProcessIds.Count
        verified_root_process_ids = $RootProcessIds
        project_file_count = $ProjectFileCount
        project_changed = $ProjectChanged
        unattributed_event_count = if ($null -eq $SyncResult) { 0L } else { [long]$SyncResult.unattributed_event_count }
        sync_result = $SyncResult
        error = $ErrorMessage
    }
    [IO.File]::AppendAllLines($Path, [string[]]@(($record | ConvertTo-Json -Depth 6 -Compress)), [Text.UTF8Encoding]::new($false))
}

function Write-JsonAtomic {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    $temporaryPath = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporaryPath, ($Value | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$collector = [IO.Path]::GetFullPath($CollectorPath)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$identityFile = [IO.Path]::GetFullPath($IdentityPath)
$run = [IO.Path]::GetFullPath($RunRoot)
$updaterPath = Join-Path $source 'scripts\update-workbuddy-semantic-evidence.ps1'
foreach ($requiredPath in @($projects, $identityFile, $updaterPath, $collector)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "语义 Observer 依赖不存在 path=$requiredPath"
    }
}
$identity = Get-Content -LiteralPath $identityFile -Raw -Encoding UTF8 | ConvertFrom-Json
$executablePath = [IO.Path]::GetFullPath([string]$identity.executable_path)
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "WorkBuddy 身份文件引用的程序不存在 path=$executablePath"
}
$actualHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne ([string]$identity.sha256).ToLowerInvariant()) {
    throw "WorkBuddy 身份哈希不一致 path=$executablePath expected=$($identity.sha256) actual=$actualHash"
}

$paths = [ordered]@{
    stop_signal = Join-Path $run 'semantic-observer-stop.requested'
    health = Join-Path $run 'semantic-observer-health.ndjson'
    output = Join-Path $run 'workbuddy-semantic.full.ndjson'
    published_manifest = Join-Path $run 'semantic-evidence-manifest.json'
    sync_state = Join-Path $run 'semantic-sync-state.json'
    summary = Join-Path $run 'semantic-observer-summary.json'
}
$observerStartedAt = [DateTimeOffset]::UtcNow
$lastSignature = $null
$pollCount = 0L
$syncCount = 0L
$ambiguousPollCount = 0L
$lastSyncResult = $null
$initialSnapshot = Get-ProjectSnapshot -Root $projects
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText($paths.output, '', $utf8)
Write-JsonAtomic -Path $paths.published_manifest -Value ([ordered]@{
    schema_version = '0.1.0'
    published_at_unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    session_id = $SessionId
    semantic_file = $paths.output
    published_bytes = 0L
    semantic_events = 0L
})
Write-HealthRecord -Path $paths.health -Status 'starting' -CoverageStatus 'partial' -RootProcessIds ([uint32[]]@()) -ProjectFileCount $initialSnapshot.file_count -ProjectChanged $true -SyncResult $null -ErrorMessage $null

try {
    while (-not (Test-Path -LiteralPath $paths.stop_signal)) {
        $pollCount += 1
        $snapshot = Get-ProjectSnapshot -Root $projects
        $rootProcesses = @(Get-VerifiedRootProcesses -ExecutablePath $executablePath)
        $rootProcessIds = [uint32[]]@($rootProcesses | ForEach-Object { [uint32]$_.ProcessId })
        $projectChanged = $snapshot.signature -ne $lastSignature
        if ($rootProcesses.Count -eq 0) {
            Write-HealthRecord -Path $paths.health -Status 'idle' -CoverageStatus 'idle' -RootProcessIds $rootProcessIds -ProjectFileCount $snapshot.file_count -ProjectChanged $projectChanged -SyncResult $lastSyncResult -ErrorMessage $null
        }
        elseif ($rootProcesses.Count -gt 1) {
            $ambiguousPollCount += 1
            Write-HealthRecord -Path $paths.health -Status 'ambiguous_roots' -CoverageStatus 'degraded' -RootProcessIds $rootProcessIds -ProjectFileCount $snapshot.file_count -ProjectChanged $projectChanged -SyncResult $lastSyncResult -ErrorMessage "发现多个已验证 WorkBuddy 根进程，暂停语义归因"
        }
        elseif ($snapshot.file_count -eq 0) {
            Write-HealthRecord -Path $paths.health -Status 'waiting_for_project' -CoverageStatus 'partial' -RootProcessIds $rootProcessIds -ProjectFileCount 0 -ProjectChanged $projectChanged -SyncResult $lastSyncResult -ErrorMessage $null
        }
        elseif ($projectChanged) {
            $untilUnixMs = [DateTimeOffset]::UtcNow.AddMinutes(1).ToUnixTimeMilliseconds()
            $lastSyncResult = & $updaterPath -SourceRoot $source -CollectorPath $collector -ProjectRoot $projects -StatePath $paths.sync_state -OutputPath $paths.output -PublishedManifestPath $paths.published_manifest -SessionId $SessionId -WorkBuddyProcessId $rootProcessIds[0] -SinceUnixMs $observerStartedAt.ToUnixTimeMilliseconds() -UntilUnixMs $untilUnixMs | ConvertFrom-Json
            $lastSignature = $snapshot.signature
            $syncCount += 1
            Write-HealthRecord -Path $paths.health -Status 'running' -CoverageStatus 'healthy' -RootProcessIds $rootProcessIds -ProjectFileCount $snapshot.file_count -ProjectChanged $true -SyncResult $lastSyncResult -ErrorMessage $null
        }
        else {
            Write-HealthRecord -Path $paths.health -Status 'running' -CoverageStatus 'healthy' -RootProcessIds $rootProcessIds -ProjectFileCount $snapshot.file_count -ProjectChanged $false -SyncResult $lastSyncResult -ErrorMessage $null
        }
        Start-Sleep -Seconds $PollIntervalSeconds
    }
}
catch {
    Write-HealthRecord -Path $paths.health -Status 'failed' -CoverageStatus 'degraded' -RootProcessIds ([uint32[]]@()) -ProjectFileCount 0 -ProjectChanged $false -SyncResult $lastSyncResult -ErrorMessage $_.Exception.Message
    throw
}

$summary = [ordered]@{
    schema_version = '0.1.0'
    started_at = $observerStartedAt.ToString('o')
    stopped_at = [DateTimeOffset]::UtcNow.ToString('o')
    stop_reason = 'stop_signal'
    exit_code = 0
    session_id = $SessionId
    poll_count = $pollCount
    sync_count = $syncCount
    ambiguous_poll_count = $ambiguousPollCount
    output_path = $paths.output
    published_manifest_path = $paths.published_manifest
    sync_state_path = $paths.sync_state
    health_path = $paths.health
}
$temporarySummary = "$($paths.summary).$([Guid]::NewGuid().ToString('N')).tmp"
[IO.File]::WriteAllText($temporarySummary, ($summary | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporarySummary -Destination $paths.summary
$summary | ConvertTo-Json -Compress
