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
    [string]$InterruptedRunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RecoveryRunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$IdentityPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SessionId,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 3600)]
    [int]$PollIntervalSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-FileEvidence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    $item = Get-Item -LiteralPath $Path
    [pscustomobject]@{
        path = $item.FullName
        length = $item.Length
        sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Write-JsonAtomic {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [pscustomobject]$Value
    )

    $temporaryPath = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporaryPath, ($Value | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryPath -Destination $Path
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$collector = [IO.Path]::GetFullPath($CollectorPath)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$interruptedRun = [IO.Path]::GetFullPath($InterruptedRunRoot)
$recoveryRun = [IO.Path]::GetFullPath($RecoveryRunRoot)
$identityFile = [IO.Path]::GetFullPath($IdentityPath)
$startScript = Join-Path $source 'scripts\start-workbuddy-semantic-observer.ps1'
$statePath = Join-Path $interruptedRun 'semantic-observer-process.json'
$summaryPath = Join-Path $interruptedRun 'semantic-observer-summary.json'
$healthPath = Join-Path $interruptedRun 'semantic-observer-health.ndjson'
$outputPath = Join-Path $interruptedRun 'workbuddy-semantic.full.ndjson'
$publishedManifestPath = Join-Path $interruptedRun 'semantic-evidence-manifest.json'
$syncStatePath = Join-Path $interruptedRun 'semantic-sync-state.json'
$interruptionPath = Join-Path $interruptedRun 'semantic-observer-interruption.json'
foreach ($requiredPath in @($startScript, $collector, $projects, $identityFile, $statePath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "语义 Observer 恢复依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $summaryPath -PathType Leaf) {
    throw "语义 Observer 已正常生成摘要，不属于异常中断 run_root=$interruptedRun"
}
if (Test-Path -LiteralPath $interruptionPath) {
    throw "语义 Observer 中断标记已存在，拒绝覆盖 path=$interruptionPath"
}
if (Test-Path -LiteralPath $recoveryRun) {
    throw "语义 Observer 恢复目录已存在，拒绝覆盖 path=$recoveryRun"
}

$state = Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json
$process = $null
try {
    $process = [Diagnostics.Process]::GetProcessById([int]$state.observer_process_id)
}
catch [ArgumentException] {
    $process = $null
}
if ($null -ne $process) {
    try {
        $observedPath = $process.MainModule.FileName
        $observedStartTime = $process.StartTime.ToUniversalTime().ToString('o')
    }
    catch [ComponentModel.Win32Exception] {
        throw "当前权限无法确认旧语义 Observer 是否仍在运行 process_id=$($state.observer_process_id) error=$($_.Exception.Message)"
    }
    if ($observedPath -eq $state.observer_path -and $observedStartTime -eq $state.observer_started_at) {
        throw "旧语义 Observer 仍在运行，拒绝恢复 process_id=$($state.observer_process_id)"
    }
}

$lastHealth = $null
if (Test-Path -LiteralPath $healthPath -PathType Leaf) {
    $lastHealthLine = Get-Content -LiteralPath $healthPath -Tail 1 -Encoding UTF8
    if (-not [string]::IsNullOrWhiteSpace($lastHealthLine)) {
        $lastHealth = $lastHealthLine | ConvertFrom-Json
    }
}
$detectedAt = [DateTimeOffset]::UtcNow
$interruption = [pscustomobject][ordered]@{
    schema_version = '0.1.0'
    status = 'incomplete'
    reason = 'observer_process_missing'
    detected_at = $detectedAt.ToString('o')
    detected_at_unix_ms = $detectedAt.ToUnixTimeMilliseconds()
    interrupted_run_root = $interruptedRun
    interrupted_process_id = $state.observer_process_id
    interrupted_process_started_at = $state.observer_started_at
    last_health = $lastHealth
    semantic_output = Get-FileEvidence -Path $outputPath
    semantic_manifest = Get-FileEvidence -Path $publishedManifestPath
    sync_state = Get-FileEvidence -Path $syncStatePath
    recovery_run_root = $recoveryRun
}
Write-JsonAtomic -Path $interruptionPath -Value $interruption

$started = & $startScript -SourceRoot $source -CollectorPath $collector -ProjectRoot $projects -RunRoot $recoveryRun -IdentityPath $identityFile -SessionId $SessionId -PollIntervalSeconds $PollIntervalSeconds | ConvertFrom-Json
$recoveryRecord = [pscustomobject][ordered]@{
    schema_version = '0.1.0'
    status = 'running'
    recovered_at = [DateTimeOffset]::UtcNow.ToString('o')
    interrupted_run_root = $interruptedRun
    interruption_record = $interruptionPath
    recovery_run_root = $recoveryRun
    recovery_process_id = $started.observer_process_id
    session_id = $started.session_id
}
Write-JsonAtomic -Path (Join-Path $recoveryRun 'semantic-observer-recovery.json') -Value $recoveryRecord

[pscustomobject]@{
    interrupted_run_root = $interruptedRun
    interruption_record = $interruptionPath
    recovery_run_root = $recoveryRun
    recovery_process_id = $started.observer_process_id
    session_id = $started.session_id
} | ConvertTo-Json -Compress
