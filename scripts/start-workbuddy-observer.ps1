param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$RunRoot,
    [Parameter(Mandatory = $true)]
    [string]$IdentityPath,
    [Parameter(Mandatory = $true)]
    [int]$HealthIntervalSeconds,
    [Parameter(Mandatory = $true)]
    [long]$RollSizeBytes,
    [Parameter(Mandatory = $true)]
    [int]$RollIntervalSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$source = [IO.Path]::GetFullPath($SourceRoot)
$run = [IO.Path]::GetFullPath($RunRoot)
$identityFile = [IO.Path]::GetFullPath($IdentityPath)
$collectorPath = Join-Path $source "target\debug\etw-collector.exe"
foreach ($requiredPath in @($collectorPath, $identityFile)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "未找到 Observer 启动依赖 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $run) {
    throw "Observer 运行目录已存在，拒绝覆盖 path=$run"
}
if ($HealthIntervalSeconds -lt 1 -or $RollSizeBytes -lt 1 -or $RollIntervalSeconds -lt 1) {
    throw "Observer 周期和滚动参数必须为正整数 health_interval_seconds=$HealthIntervalSeconds roll_size_bytes=$RollSizeBytes roll_interval_seconds=$RollIntervalSeconds"
}

$identity = Get-Content -LiteralPath $identityFile -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not (Test-Path -LiteralPath $identity.executable_path)) {
    throw "身份文件中的 WorkBuddy 路径不存在 path=$($identity.executable_path)"
}
$actualHash = (Get-FileHash -LiteralPath $identity.executable_path -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne ([string]$identity.sha256).ToLowerInvariant()) {
    throw "身份文件哈希与当前程序不一致 path=$($identity.executable_path) expected=$($identity.sha256) actual=$actualHash"
}

New-Item -ItemType Directory -Path $run | Out-Null
$paths = [ordered]@{
    stop_signal = Join-Path $run "observer-stop.requested"
    health = Join-Path $run "observer-health.ndjson"
    segments = Join-Path $run "segments"
    stdout = Join-Path $run "observer-stdout.json"
    stderr = Join-Path $run "observer-stderr.ndjson"
    exit_code = Join-Path $run "observer-exit-code.txt"
    state = Join-Path $run "observer-process.json"
}
$arguments = '--observe-until-stopped --stop-signal "{0}" --health-output "{1}" --health-interval-seconds {2} --output-directory "{3}" --roll-size-bytes {4} --roll-interval-seconds {5} --identity "{6}"' -f $paths.stop_signal, $paths.health, $HealthIntervalSeconds, $paths.segments, $RollSizeBytes, $RollIntervalSeconds, $identityFile
$commandLine = 'cmd.exe /d /v:on /s /c ""{0}" {1} 1>"{2}" 2>"{3}" & set "AgentReinsExit=!errorlevel!" & >"{4}" echo !AgentReinsExit! & exit /b !AgentReinsExit!"' -f $collectorPath, $arguments, $paths.stdout, $paths.stderr, $paths.exit_code
$creation = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $commandLine }
if ($creation.ReturnValue -ne 0) {
    throw "无法通过 WMI 启动独立 Observer 宿主 return_value=$($creation.ReturnValue)"
}
$hostProcessId = [int]$creation.ProcessId

$started = $false
$collectorProcess = $null
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline) {
        $hostProcess = Get-Process -Id $hostProcessId -ErrorAction SilentlyContinue
        if ($null -eq $hostProcess) {
            $diagnostic = if (Test-Path -LiteralPath $paths.stderr) { Get-Content -LiteralPath $paths.stderr -Raw -Encoding UTF8 } else { "未生成错误输出" }
            throw "Observer 宿主在写入首个健康快照前退出 host_process_id=$hostProcessId diagnostic=$diagnostic"
        }
        if ((Test-Path -LiteralPath $paths.health) -and (Get-Item -LiteralPath $paths.health).Length -gt 0) {
            $collectorInstance = Get-CimInstance -ClassName Win32_Process -Filter "ParentProcessId = $hostProcessId" |
                Where-Object { $_.ExecutablePath -eq $collectorPath } |
                Select-Object -First 1
            if ($null -ne $collectorInstance) {
                $collectorProcess = Get-Process -Id $collectorInstance.ProcessId
                $started = $true
                break
            }
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $started) {
        throw "Observer 未在期限内写入首个健康快照 host_process_id=$hostProcessId"
    }
    $state = [pscustomobject]@{
        schema_version = "0.1.0"
        host_process_id = $hostProcessId
        collector_process_id = $collectorProcess.Id
        collector_started_at = $collectorProcess.StartTime.ToUniversalTime().ToString("o")
        collector_path = $collectorPath
        run_root = $run
        identity_path = $identityFile
        stop_signal_path = $paths.stop_signal
        health_output_path = $paths.health
        output_directory = $paths.segments
        stdout_path = $paths.stdout
        stderr_path = $paths.stderr
        exit_code_path = $paths.exit_code
        health_interval_seconds = $HealthIntervalSeconds
        roll_size_bytes = $RollSizeBytes
        roll_interval_seconds = $RollIntervalSeconds
    }
    [IO.File]::WriteAllText($paths.state, ($state | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $state | ConvertTo-Json -Compress
}
finally {
    if (-not $started) {
        if (-not (Test-Path -LiteralPath $paths.stop_signal)) {
            [IO.File]::WriteAllText($paths.stop_signal, "stop", [Text.UTF8Encoding]::new($false))
        }
        Start-Sleep -Seconds 1
        $remainingHost = Get-Process -Id $hostProcessId -ErrorAction SilentlyContinue
        if ($null -ne $remainingHost) {
            Stop-Process -Id $hostProcessId -Force
        }
    }
}
