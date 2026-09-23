param(
    [Parameter(Mandatory = $true)]
    [string]$RunRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$run = [IO.Path]::GetFullPath($RunRoot)
$statePath = Join-Path $run "observer-process.json"
if (-not (Test-Path -LiteralPath $statePath)) {
    throw "Observer 状态文件不存在 path=$statePath"
}
$state = Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json
$process = Get-Process -Id $state.collector_process_id -ErrorAction SilentlyContinue
if ($null -eq $process) {
    throw "Observer 进程不存在 process_id=$($state.collector_process_id) run_root=$run"
}
if ($process.Path -ne $state.collector_path -or $process.StartTime.ToUniversalTime().ToString("o") -ne $state.collector_started_at) {
    throw "Observer PID 已被其他进程复用，拒绝发送停止信号 process_id=$($state.collector_process_id)"
}
if (Test-Path -LiteralPath $state.stop_signal_path) {
    throw "Observer 停止信号已存在 path=$($state.stop_signal_path)"
}
[IO.File]::WriteAllText($state.stop_signal_path, "stop", [Text.UTF8Encoding]::new($false))
if (-not $process.WaitForExit(15000)) {
    throw "Observer 未在停止信号后退出 process_id=$($state.collector_process_id)"
}
$exitCodeDeadline = [DateTime]::UtcNow.AddSeconds(5)
while ((-not (Test-Path -LiteralPath $state.exit_code_path) -or (Get-Item -LiteralPath $state.exit_code_path).Length -eq 0) -and [DateTime]::UtcNow -lt $exitCodeDeadline) {
    Start-Sleep -Milliseconds 50
}
if (-not (Test-Path -LiteralPath $state.exit_code_path) -or (Get-Item -LiteralPath $state.exit_code_path).Length -eq 0) {
    throw "Observer 宿主未生成退出码 path=$($state.exit_code_path)"
}
$exitCode = [int](Get-Content -LiteralPath $state.exit_code_path -Raw -Encoding ASCII).Trim()
if (-not (Test-Path -LiteralPath $state.stdout_path)) {
    throw "Observer 退出后未生成摘要 path=$($state.stdout_path)"
}
$summary = Get-Content -LiteralPath $state.stdout_path -Raw -Encoding UTF8 | ConvertFrom-Json
$manifestFile = Get-ChildItem -LiteralPath $state.output_directory -Filter "*-manifest-*.json" |
    Sort-Object Name |
    Select-Object -Last 1
if ($null -eq $manifestFile) {
    throw "Observer 退出后未生成滚动 manifest path=$($state.output_directory)"
}
$manifest = Get-Content -LiteralPath $manifestFile.FullName -Raw -Encoding UTF8 | ConvertFrom-Json
if ($summary.stop_reason -ne "stop_signal" -or $manifest.complete -ne $true) {
    throw "Observer 未完成优雅停止 stop_reason=$($summary.stop_reason) manifest_complete=$($manifest.complete)"
}
[pscustomobject]@{
    host_process_id = $state.host_process_id
    collector_process_id = $state.collector_process_id
    exit_code = $exitCode
    stop_reason = $summary.stop_reason
    coverage_status = $summary.coverage_status
    segments_published = $summary.segments_published
    final_manifest = $manifestFile.FullName
    manifest_complete = $manifest.complete
    events_lost = $summary.events_lost
    parse_failures = $summary.parse_failures
    write_failures = $summary.write_failures
    output_batches_dropped = $summary.output_batches_dropped
} | ConvertTo-Json -Compress

if ($exitCode -ne 0) {
    throw "Observer 返回非零退出码 exit_code=$exitCode stderr=$(Get-Content -LiteralPath $state.stderr_path -Raw -Encoding UTF8)"
}
