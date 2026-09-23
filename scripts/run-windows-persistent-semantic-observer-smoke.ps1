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

function New-MessageRecord {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Id,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$ParentId,
        [Parameter(Mandatory = $true)]
        [long]$Timestamp,
        [Parameter(Mandatory = $true)]
        [string]$Role,
        [Parameter(Mandatory = $true)]
        [string]$BlockType,
        [Parameter(Mandatory = $true)]
        [string]$Text
    )

    $record = [ordered]@{
        id = $Id
        timestamp = $Timestamp
        type = 'message'
        role = $Role
        content = @([ordered]@{ type = $BlockType; text = $Text })
    }
    if (-not [string]::IsNullOrEmpty($ParentId)) {
        $record.parentId = $ParentId
    }
    $record | ConvertTo-Json -Depth 5 -Compress
}

function New-ToolRecords {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ActivityId,
        [Parameter(Mandatory = $true)]
        [string]$Suffix,
        [Parameter(Mandatory = $true)]
        [long]$Timestamp
    )

    $toolId = "tool-$Suffix"
    $callId = "call-$Suffix"
    @(
        ([ordered]@{
            id = $toolId
            parentId = $ActivityId
            timestamp = $Timestamp
            type = 'function_call'
            name = 'PowerShell'
            callId = $callId
            arguments = '{"command":"ipconfig"}'
        } | ConvertTo-Json -Compress),
        ([ordered]@{
            id = "result-$Suffix"
            parentId = $toolId
            timestamp = $Timestamp + 1
            type = 'function_call_result'
            name = 'PowerShell'
            callId = $callId
            output = [ordered]@{ type = 'text'; text = 'ok' }
        } | ConvertTo-Json -Depth 5 -Compress)
    )
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$fixturePath = Join-Path $source 'target\debug\WorkBuddy.exe'
$collectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
$startScript = Join-Path $source 'scripts\start-workbuddy-semantic-observer.ps1'
$stopScript = Join-Path $source 'scripts\stop-workbuddy-semantic-observer.ps1'
foreach ($requiredPath in @($fixturePath, $collectorPath, $startScript, $stopScript)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "常驻语义烟测依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "常驻语义烟测目录已存在，拒绝覆盖 path=$evidence"
}

$projectRoot = Join-Path $evidence 'projects'
$projectFile = Join-Path $projectRoot 'conversation.jsonl'
$identityPath = Join-Path $evidence 'fixture-identity.json'
$runRoot = Join-Path $evidence 'observer'
New-Item -ItemType Directory -Path $projectRoot | Out-Null
$identity = [ordered]@{
    executable_path = $fixturePath
    file_version = '0.1.0'
    sha256 = (Get-FileHash -LiteralPath $fixturePath -Algorithm SHA256).Hash.ToLowerInvariant()
    signer_subject = 'UNSIGNED_TEST_FIXTURE'
}
[IO.File]::WriteAllText($identityPath, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

$started = & $startScript -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectRoot -RunRoot $runRoot -IdentityPath $identityPath -SessionId 'persistent-semantic-smoke' -PollIntervalSeconds 1 | ConvertFrom-Json
$firstProcess = $null
$secondProcess = $null
$stopped = $null
try {
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(20)
    $ready = $false
    while ([DateTime]::UtcNow -lt $readyDeadline) {
        $healthPath = Join-Path $runRoot 'semantic-observer-health.ndjson'
        if (Test-Path -LiteralPath $healthPath -PathType Leaf) {
            $lastHealth = Get-Content -LiteralPath $healthPath -Tail 1 -Encoding UTF8 | ConvertFrom-Json
            if ($lastHealth.status -eq 'idle') {
                $ready = $true
                break
            }
            if ($lastHealth.status -eq 'failed') {
                throw "常驻语义 Observer 在就绪前失败 error=$($lastHealth.error)"
            }
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) {
        throw '常驻语义 Observer 未在期限内进入 idle'
    }
    $firstProcess = Start-Process -FilePath $fixturePath -PassThru
    $firstTimestamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $firstRecords = [Collections.Generic.List[string]]::new()
    $firstRecords.Add((New-MessageRecord -Id 'user-1' -ParentId '' -Timestamp $firstTimestamp -Role 'user' -BlockType 'input_text' -Text '请执行第一项任务'))
    foreach ($line in (New-ToolRecords -ActivityId 'user-1' -Suffix '1' -Timestamp ($firstTimestamp + 1))) {
        $firstRecords.Add($line)
    }
    $firstRecords.Add((New-MessageRecord -Id 'assistant-1' -ParentId 'result-1' -Timestamp ($firstTimestamp + 3) -Role 'assistant' -BlockType 'output_text' -Text '第一项任务完成'))
    [IO.File]::WriteAllLines($projectFile, $firstRecords, [Text.UTF8Encoding]::new($false))
    $firstProcess.WaitForExit()

    $secondProcess = Start-Process -FilePath $fixturePath -PassThru
    $secondTimestamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $secondRecords = [Collections.Generic.List[string]]::new()
    $secondRecords.Add((New-MessageRecord -Id 'user-2' -ParentId '' -Timestamp $secondTimestamp -Role 'user' -BlockType 'input_text' -Text '请执行第二项任务'))
    foreach ($line in (New-ToolRecords -ActivityId 'user-2' -Suffix '2' -Timestamp ($secondTimestamp + 1))) {
        $secondRecords.Add($line)
    }
    $secondRecords.Add((New-MessageRecord -Id 'assistant-2' -ParentId 'result-2' -Timestamp ($secondTimestamp + 3) -Role 'assistant' -BlockType 'output_text' -Text '第二项任务完成'))
    [IO.File]::AppendAllLines($projectFile, $secondRecords, [Text.UTF8Encoding]::new($false))
    $secondProcess.WaitForExit()

    $outputPath = Join-Path $runRoot 'workbuddy-semantic.full.ndjson'
    $publishedManifestPath = Join-Path $runRoot 'semantic-evidence-manifest.json'
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    $events = @()
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path -LiteralPath $outputPath -PathType Leaf) {
            $events = @([IO.File]::ReadLines($outputPath, [Text.UTF8Encoding]::new($false)) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
            if (@($events.user_activity_id | Sort-Object -Unique).Count -eq 2) {
                break
            }
        }
        Start-Sleep -Milliseconds 200
    }
}
finally {
    foreach ($fixtureProcess in @($firstProcess, $secondProcess)) {
        if ($null -ne $fixtureProcess -and -not $fixtureProcess.HasExited) {
            Stop-Process -Id $fixtureProcess.Id -Force
        }
    }
    $stopped = & $stopScript -RunRoot $runRoot | ConvertFrom-Json
}

$health = @(Get-Content -LiteralPath (Join-Path $runRoot 'semantic-observer-health.ndjson') -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })
$eventIds = @($events.event_id | Sort-Object -Unique)
$activityIds = @($events.user_activity_id | Sort-Object -Unique)
$processIds = @($events.process_id | Sort-Object -Unique)
$publishedManifest = Get-Content -LiteralPath (Join-Path $runRoot 'semantic-evidence-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$result = [ordered]@{
    observer_process_id = $started.observer_process_id
    observer_exit_code = $stopped.exit_code
    stop_reason = $stopped.stop_reason
    poll_count = $stopped.poll_count
    sync_count = $stopped.sync_count
    semantic_events = $events.Count
    unique_event_ids = $eventIds.Count
    user_activity_ids = $activityIds
    attributed_process_ids = $processIds
    expected_process_ids = @($firstProcess.Id, $secondProcess.Id)
    idle_health_count = @($health | Where-Object { $_.status -eq 'idle' }).Count
    healthy_sync_count = @($health | Where-Object { $_.status -eq 'running' -and $_.project_changed }).Count
    degraded_health_count = @($health | Where-Object { $_.coverage_status -eq 'degraded' }).Count
    unattributed_event_count = [long]$health[-1].unattributed_event_count
    published_semantic_events = [long]$publishedManifest.semantic_events
    published_bytes = [long]$publishedManifest.published_bytes
}
if ($result.observer_exit_code -ne 0 -or $result.stop_reason -ne 'stop_signal') {
    throw "常驻语义 Observer 生命周期不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.semantic_events -ne 14 -or $result.unique_event_ids -ne 14 -or $result.user_activity_ids.Count -ne 2) {
    throw "常驻语义增量或自然任务边界不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.attributed_process_ids.Count -ne 2 -or @($result.expected_process_ids | Where-Object { $_ -notin $result.attributed_process_ids }).Count -ne 0) {
    throw "WorkBuddy 重启后的进程归因不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.idle_health_count -lt 1 -or $result.healthy_sync_count -lt 2 -or $result.degraded_health_count -ne 0 -or $result.unattributed_event_count -ne 0) {
    throw "常驻语义 Observer 健康状态不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.published_semantic_events -ne $result.semantic_events -or $result.published_bytes -ne (Get-Item -LiteralPath $outputPath).Length) {
    throw "常驻语义发布边界不闭合 result=$($result | ConvertTo-Json -Compress) actual_bytes=$((Get-Item -LiteralPath $outputPath).Length)"
}
$result | ConvertTo-Json -Compress
