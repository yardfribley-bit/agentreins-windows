[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$run = [IO.Path]::GetFullPath($RunRoot)
$statePath = Join-Path $run 'semantic-observer-process.json'
if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) {
    throw "语义 Observer 状态文件不存在 path=$statePath"
}
$state = Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json
$process = $null
try {
    $process = [Diagnostics.Process]::GetProcessById([int]$state.observer_process_id)
}
catch [ArgumentException] {
    throw "语义 Observer 进程不存在 process_id=$($state.observer_process_id) run_root=$run"
}
$mainModule = $process.MainModule
if ($null -eq $mainModule) {
    throw "当前权限无法验证语义 Observer 进程身份 process_id=$($state.observer_process_id)"
}
$observedPath = $mainModule.FileName
$observedStartTime = $process.StartTime.ToUniversalTime().ToString('o')
if ($observedPath -ne $state.observer_path -or $observedStartTime -ne $state.observer_started_at) {
    throw "语义 Observer PID 已被其他进程复用，拒绝发送停止信号 process_id=$($state.observer_process_id)"
}
if (Test-Path -LiteralPath $state.stop_signal_path) {
    throw "语义 Observer 停止信号已存在 path=$($state.stop_signal_path)"
}
[IO.File]::WriteAllText($state.stop_signal_path, 'stop', [Text.UTF8Encoding]::new($false))
if (-not $process.WaitForExit(15000)) {
    throw "语义 Observer 未在停止信号后退出 process_id=$($state.observer_process_id)"
}
$summaryPath = Join-Path $run 'semantic-observer-summary.json'
if (-not (Test-Path -LiteralPath $summaryPath -PathType Leaf)) {
    throw "语义 Observer 未生成最终摘要 path=$summaryPath"
}
$summary = Get-Content -LiteralPath $summaryPath -Raw -Encoding UTF8 | ConvertFrom-Json
$exitCode = [int]$summary.exit_code
if ($exitCode -ne 0 -or $summary.stop_reason -ne 'stop_signal') {
    throw "语义 Observer 未正常停止 exit_code=$exitCode stop_reason=$($summary.stop_reason)"
}
$publishedManifestPath = $null
if ($summary.PSObject.Properties.Name -contains 'published_manifest_path') {
    $publishedManifestPath = [string]$summary.published_manifest_path
}

[pscustomobject]@{
    observer_process_id = $state.observer_process_id
    exit_code = $exitCode
    stop_reason = $summary.stop_reason
    poll_count = $summary.poll_count
    sync_count = $summary.sync_count
    ambiguous_poll_count = $summary.ambiguous_poll_count
    output_path = $summary.output_path
    published_manifest_path = $publishedManifestPath
    health_path = $summary.health_path
} | ConvertTo-Json -Compress
