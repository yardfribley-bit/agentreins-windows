[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$fixturePath = Join-Path $source 'target\debug\WorkBuddy.exe'
$collectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
$startScript = Join-Path $source 'scripts\start-workbuddy-semantic-observer.ps1'
$recoverScript = Join-Path $source 'scripts\recover-workbuddy-semantic-observer.ps1'
$stopScript = Join-Path $source 'scripts\stop-workbuddy-semantic-observer.ps1'
foreach ($requiredPath in @($fixturePath, $collectorPath, $startScript, $recoverScript, $stopScript)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "语义 Observer 恢复烟测依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "语义 Observer 恢复烟测目录已存在，拒绝覆盖 path=$evidence"
}

$projectRoot = Join-Path $evidence 'projects'
$identityPath = Join-Path $evidence 'fixture-identity.json'
$interruptedRun = Join-Path $evidence 'interrupted-observer'
$recoveryRun = Join-Path $evidence 'recovery-observer'
New-Item -ItemType Directory -Path $projectRoot | Out-Null
$identity = [ordered]@{
    executable_path = $fixturePath
    file_version = '0.1.0'
    sha256 = (Get-FileHash -LiteralPath $fixturePath -Algorithm SHA256).Hash.ToLowerInvariant()
    signer_subject = 'UNSIGNED_TEST_FIXTURE'
}
[IO.File]::WriteAllText($identityPath, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

$started = & $startScript -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectRoot -RunRoot $interruptedRun -IdentityPath $identityPath -SessionId 'semantic-recovery-smoke' -PollIntervalSeconds 1 | ConvertFrom-Json
$recovered = $null
$stopped = $null
try {
    Stop-Process -Id $started.observer_process_id -Force
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ($null -ne (Get-Process -Id $started.observer_process_id -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if ($null -ne (Get-Process -Id $started.observer_process_id -ErrorAction SilentlyContinue)) {
        throw "隔离语义 Observer 未在强制中断后退出 process_id=$($started.observer_process_id)"
    }

    $recovered = & $recoverScript -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectRoot -InterruptedRunRoot $interruptedRun -RecoveryRunRoot $recoveryRun -IdentityPath $identityPath -SessionId 'semantic-recovery-smoke' -PollIntervalSeconds 1 | ConvertFrom-Json
    $stopped = & $stopScript -RunRoot $recoveryRun | ConvertFrom-Json
}
finally {
    if ($null -ne $recovered -and $null -eq $stopped) {
        $recoveryProcess = Get-Process -Id $recovered.recovery_process_id -ErrorAction SilentlyContinue
        if ($null -ne $recoveryProcess) {
            Stop-Process -Id $recoveryProcess.Id -Force
        }
    }
}

$interruptionPath = Join-Path $interruptedRun 'semantic-observer-interruption.json'
$recoveryRecordPath = Join-Path $recoveryRun 'semantic-observer-recovery.json'
$interruption = Get-Content -LiteralPath $interruptionPath -Raw -Encoding UTF8 | ConvertFrom-Json
$recoveryRecord = Get-Content -LiteralPath $recoveryRecordPath -Raw -Encoding UTF8 | ConvertFrom-Json
$result = [ordered]@{
    interrupted_process_id = $started.observer_process_id
    interruption_status = $interruption.status
    interruption_reason = $interruption.reason
    interruption_record_exists = Test-Path -LiteralPath $interruptionPath -PathType Leaf
    recovery_process_id = $recovered.recovery_process_id
    recovery_record_status = $recoveryRecord.status
    recovery_record_exists = Test-Path -LiteralPath $recoveryRecordPath -PathType Leaf
    recovery_exit_code = $stopped.exit_code
    recovery_stop_reason = $stopped.stop_reason
}
if ($result.interruption_status -ne 'incomplete' -or $result.interruption_reason -ne 'observer_process_missing' -or -not $result.interruption_record_exists) {
    throw "异常中断识别不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.recovery_record_status -ne 'running' -or -not $result.recovery_record_exists -or $result.recovery_exit_code -ne 0 -or $result.recovery_stop_reason -ne 'stop_signal') {
    throw "语义 Observer 恢复生命周期不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
$result | ConvertTo-Json -Compress
