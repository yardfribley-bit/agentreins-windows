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
$process = $null
try {
    $process = [Diagnostics.Process]::GetProcessById([int]$state.collector_process_id)
}
catch [ArgumentException] {
    $process = $null
}
$runningStatus = "stopped"
$running = $false
$processIdentityError = $null
if ($null -ne $process) {
    try {
        $mainModule = $process.MainModule
        if ($null -eq $mainModule) {
            $runningStatus = "unknown"
            $running = $null
            $processIdentityError = "当前权限无法读取采集器进程模块"
        }
        else {
            $observedPath = $mainModule.FileName
            $observedStartTime = $process.StartTime.ToUniversalTime().ToString("o")
            if ($observedPath -eq $state.collector_path -and $observedStartTime -eq $state.collector_started_at) {
                $runningStatus = "running"
                $running = $true
            }
            else {
                $runningStatus = "identity_mismatch"
            }
        }
    }
    catch [ComponentModel.Win32Exception] {
        $runningStatus = "unknown"
        $running = $null
        $processIdentityError = $_.Exception.Message
    }
}
$health = $null
if (Test-Path -LiteralPath $state.health_output_path) {
    $lastHealthLine = Get-Content -LiteralPath $state.health_output_path -Tail 1 -Encoding UTF8
    if (-not [string]::IsNullOrWhiteSpace($lastHealthLine)) {
        $health = $lastHealthLine | ConvertFrom-Json
    }
}
[pscustomobject]@{
    running = $running
    running_status = $runningStatus
    process_identity_error = $processIdentityError
    host_process_id = $state.host_process_id
    collector_process_id = $state.collector_process_id
    collector_started_at = $state.collector_started_at
    run_root = $state.run_root
    identity_path = $state.identity_path
    health = $health
} | ConvertTo-Json -Depth 8 -Compress
