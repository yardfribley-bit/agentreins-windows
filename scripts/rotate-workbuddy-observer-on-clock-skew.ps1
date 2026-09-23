[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CurrentRunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RecoveryRunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateRange(60000, [long]::MaxValue)]
    [long]$MinimumSkewMs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-JsonAtomic {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    $temporaryPath = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporaryPath, ($Value | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$currentRun = [IO.Path]::GetFullPath($CurrentRunRoot)
$recoveryRun = [IO.Path]::GetFullPath($RecoveryRunRoot)
$statePath = Join-Path $currentRun 'observer-process.json'
$stopScript = Join-Path $source 'scripts\stop-workbuddy-observer.ps1'
$startScript = Join-Path $source 'scripts\start-workbuddy-observer.ps1'
foreach ($requiredPath in @($statePath, $stopScript, $startScript)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Observer 时钟恢复依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $recoveryRun) {
    throw "Observer 恢复运行目录已存在，拒绝覆盖 path=$recoveryRun"
}

$state = Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json
$lastHealthLine = Get-Content -LiteralPath $state.health_output_path -Tail 1 -Encoding UTF8
if ([string]::IsNullOrWhiteSpace($lastHealthLine)) {
    throw "Observer 没有可用健康记录 path=$($state.health_output_path)"
}
$lastHealth = $lastHealthLine | ConvertFrom-Json
if ($null -eq $lastHealth.event_clock_skew_ms) {
    throw "Observer 健康记录没有时钟偏差值 path=$($state.health_output_path)"
}
$observedSkewMs = [long]$lastHealth.event_clock_skew_ms
if ($observedSkewMs -le $MinimumSkewMs) {
    throw "Observer 时钟偏差未超过轮换阈值 observed_ms=$observedSkewMs threshold_ms=$MinimumSkewMs"
}

$rotationPath = Join-Path $currentRun 'observer-clock-skew-rotation.json'
if (Test-Path -LiteralPath $rotationPath) {
    throw "Observer 时钟轮换记录已存在，拒绝覆盖 path=$rotationPath"
}
$requestedAt = [DateTimeOffset]::UtcNow
$rotation = [ordered]@{
    schema_version = '0.1.0'
    status = 'requested'
    reason = 'event_clock_skew_exceeded'
    requested_at = $requestedAt.ToString('o')
    requested_at_unix_ms = $requestedAt.ToUnixTimeMilliseconds()
    observed_skew_ms = $observedSkewMs
    minimum_skew_ms = $MinimumSkewMs
    previous_run_root = $currentRun
    previous_session_id = [string]$lastHealth.session_id
    recovery_run_root = $recoveryRun
}
Write-JsonAtomic -Path $rotationPath -Value $rotation

try {
    $stopped = & $stopScript -RunRoot $currentRun | ConvertFrom-Json
    $started = & $startScript -SourceRoot $source -RunRoot $recoveryRun -IdentityPath ([string]$state.identity_path) -HealthIntervalSeconds ([int]$state.health_interval_seconds) -RollSizeBytes ([long]$state.roll_size_bytes) -RollIntervalSeconds ([int]$state.roll_interval_seconds) | ConvertFrom-Json
    $completedAt = [DateTimeOffset]::UtcNow
    $rotation.status = 'completed'
    $rotation.completed_at = $completedAt.ToString('o')
    $rotation.completed_at_unix_ms = $completedAt.ToUnixTimeMilliseconds()
    $rotation.previous_manifest = [string]$stopped.final_manifest
    $rotation.previous_manifest_complete = [bool]$stopped.manifest_complete
    $rotation.previous_exit_code = [int]$stopped.exit_code
    $rotation.recovery_collector_process_id = [int]$started.collector_process_id
    $rotation.recovery_health_output_path = [string]$started.health_output_path
    Write-JsonAtomic -Path $rotationPath -Value $rotation
    Write-JsonAtomic -Path (Join-Path $recoveryRun 'observer-rotation-from-previous.json') -Value $rotation
    $rotation | ConvertTo-Json -Depth 8 -Compress
}
catch {
    $rotation.status = 'failed'
    $rotation.failed_at = [DateTimeOffset]::UtcNow.ToString('o')
    $rotation.error = $_.Exception.Message
    Write-JsonAtomic -Path $rotationPath -Value $rotation
    throw
}
