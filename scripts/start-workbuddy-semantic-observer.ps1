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
    [string]$RunRoot,

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

$source = [IO.Path]::GetFullPath($SourceRoot)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$run = [IO.Path]::GetFullPath($RunRoot)
$identityFile = [IO.Path]::GetFullPath($IdentityPath)
$hostScript = Join-Path $source 'scripts\run-workbuddy-semantic-observer-host.ps1'
$updaterScript = Join-Path $source 'scripts\update-workbuddy-semantic-evidence.ps1'
$collectorPath = [IO.Path]::GetFullPath($CollectorPath)
foreach ($requiredPath in @($projects, $identityFile, $hostScript, $updaterScript, $collectorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "语义 Observer 启动依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $run) {
    throw "语义 Observer 运行目录已存在，拒绝覆盖 path=$run"
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

New-Item -ItemType Directory -Path $run | Out-Null
$paths = [ordered]@{
    stop_signal = Join-Path $run 'semantic-observer-stop.requested'
    health = Join-Path $run 'semantic-observer-health.ndjson'
    published_manifest = Join-Path $run 'semantic-evidence-manifest.json'
    state = Join-Path $run 'semantic-observer-process.json'
}
$powerShellPath = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$hostCommand = "& '$($hostScript.Replace("'", "''"))' -SourceRoot '$($source.Replace("'", "''"))' -CollectorPath '$($collectorPath.Replace("'", "''"))' -ProjectRoot '$($projects.Replace("'", "''"))' -IdentityPath '$($identityFile.Replace("'", "''"))' -RunRoot '$($run.Replace("'", "''"))' -SessionId '$($SessionId.Replace("'", "''"))' -PollIntervalSeconds $PollIntervalSeconds"
$encodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($hostCommand))
$commandLine = "$powerShellPath -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand $encodedCommand"
$creation = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $commandLine }
if ($creation.ReturnValue -ne 0) {
    throw "无法通过 WMI 启动独立语义 Observer return_value=$($creation.ReturnValue)"
}
$observerProcessId = [int]$creation.ProcessId

$started = $false
$observerProcess = $null
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    while ([DateTime]::UtcNow -lt $deadline) {
        try {
            $observerProcess = [Diagnostics.Process]::GetProcessById($observerProcessId)
        }
        catch [ArgumentException] {
            $diagnostic = if (Test-Path -LiteralPath $paths.health) { Get-Content -LiteralPath $paths.health -Tail 1 -Encoding UTF8 } else { '未生成健康记录' }
            throw "语义 Observer 在首个健康快照前退出 process_id=$observerProcessId diagnostic=$diagnostic"
        }
        if ((Test-Path -LiteralPath $paths.health) -and (Get-Item -LiteralPath $paths.health).Length -gt 0) {
            $observerInstance = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $observerProcessId"
            if ($null -eq $observerInstance) {
                throw "语义 Observer 在身份确认前退出 process_id=$observerProcessId"
            }
            if ($observerInstance.Name -ne 'powershell.exe' -or $observerInstance.CommandLine -notlike "*-EncodedCommand $encodedCommand*") {
                throw "语义 Observer PID 身份不匹配 process_id=$observerProcessId name=$($observerInstance.Name) command_line=$($observerInstance.CommandLine)"
            }
            $started = $true
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $started) {
        throw "语义 Observer 未在期限内写入首个健康快照 process_id=$observerProcessId"
    }
    $observerPath = $observerProcess.MainModule.FileName
    $state = [ordered]@{
        schema_version = '0.1.0'
        observer_process_id = $observerProcess.Id
        observer_started_at = $observerProcess.StartTime.ToUniversalTime().ToString('o')
        observer_path = $observerPath
        run_root = $run
        project_root = $projects
        identity_path = $identityFile
        session_id = $SessionId
        stop_signal_path = $paths.stop_signal
        health_output_path = $paths.health
        semantic_manifest_path = $paths.published_manifest
        poll_interval_seconds = $PollIntervalSeconds
    }
    [IO.File]::WriteAllText($paths.state, ($state | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $state | ConvertTo-Json -Compress
}
finally {
    if (-not $started) {
        if (-not (Test-Path -LiteralPath $paths.stop_signal)) {
            [IO.File]::WriteAllText($paths.stop_signal, 'stop', [Text.UTF8Encoding]::new($false))
        }
        Start-Sleep -Seconds 1
        $remainingObserver = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $observerProcessId"
        if ($null -ne $remainingObserver -and $remainingObserver.Name -eq 'powershell.exe' -and $remainingObserver.CommandLine -like "*-EncodedCommand $encodedCommand*") {
            Stop-Process -Id $observerProcessId -Force
        }
    }
}
