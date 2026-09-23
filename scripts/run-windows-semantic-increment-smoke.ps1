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
if (Test-Path -LiteralPath $evidence) {
    throw "语义增量烟测目录已存在，拒绝覆盖 path=$evidence"
}

$projectRoot = Join-Path $evidence 'projects\workspace'
$projectFile = Join-Path $projectRoot 'conversation.jsonl'
$outputPath = Join-Path $evidence 'semantic.full.ndjson'
$statePath = Join-Path $evidence 'semantic-state.json'
$publishedManifestPath = Join-Path $evidence 'semantic-evidence-manifest.json'
$updaterPath = Join-Path $source 'scripts\update-workbuddy-semantic-evidence.ps1'
$collectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
foreach ($requiredPath in @($updaterPath, $collectorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "找不到语义增量烟测依赖 path=$requiredPath"
    }
}

New-Item -ItemType Directory -Path $projectRoot | Out-Null
$utf8 = [Text.UTF8Encoding]::new($false)
$firstRecords = @(
    '{"id":"u1","timestamp":100,"type":"message","role":"user","content":[{"type":"input_text","text":"请查询天气"}]}',
    '{"id":"t1","parentId":"u1","timestamp":110,"type":"function_call","name":"PowerShell","callId":"call-1","arguments":"{\"command\":\"weather\"}"}',
    '{"id":"r1","parentId":"t1","timestamp":120,"type":"function_call_result","name":"PowerShell","callId":"call-1","output":{"type":"text","text":"晴"}}'
)
[IO.File]::WriteAllLines($projectFile, $firstRecords, $utf8)

$projectParent = Split-Path -Parent $projectRoot
$first = & $updaterPath -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectParent -StatePath $statePath -OutputPath $outputPath -PublishedManifestPath $publishedManifestPath -SessionId 'semantic-smoke' -WorkBuddyProcessId 42 -SinceUnixMs 0 -UntilUnixMs 1000 | ConvertFrom-Json
$second = & $updaterPath -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectParent -StatePath $statePath -OutputPath $outputPath -PublishedManifestPath $publishedManifestPath -SessionId 'semantic-smoke' -WorkBuddyProcessId 42 -SinceUnixMs 0 -UntilUnixMs 1000 | ConvertFrom-Json
[IO.File]::AppendAllLines($projectFile, [string[]]@(
    '{"id":"a1","parentId":"r1","timestamp":130,"type":"message","role":"assistant","content":[{"type":"output_text","text":"北京晴"}]}',
    '{"id":"shared","parentId":"u1","timestamp":140,"type":"function_call","name":"Read","callId":"parallel-1","arguments":"{\"file_path\":\"C:\\\\workspace\\\\a.txt\"}"}',
    '{"id":"shared","parentId":"u1","timestamp":140,"type":"function_call","name":"Read","callId":"parallel-2","arguments":"{\"file_path\":\"C:\\\\workspace\\\\b.txt\"}"}'
), $utf8)
$third = & $updaterPath -SourceRoot $source -CollectorPath $collectorPath -ProjectRoot $projectParent -StatePath $statePath -OutputPath $outputPath -PublishedManifestPath $publishedManifestPath -SessionId 'semantic-smoke' -WorkBuddyProcessId 42 -SinceUnixMs 0 -UntilUnixMs 1000 | ConvertFrom-Json

$events = @([IO.File]::ReadLines($outputPath, $utf8) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
$uniqueEventIds = @($events.event_id | Sort-Object -Unique)
$activityIds = @($events.user_activity_id | Sort-Object -Unique)
$publishedManifest = Get-Content -LiteralPath $publishedManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($first.new_events_written -ne 5 -or $second.new_events_written -ne 0 -or $third.new_events_written -ne 6) {
    throw "增量写入数量不符合预期 first=$($first.new_events_written) second=$($second.new_events_written) third=$($third.new_events_written)"
}
if ($first.project_files_processed -ne 1 -or $second.project_files_processed -ne 0 -or $second.project_files_unchanged -ne 1 -or $third.project_files_processed -ne 1) {
    throw "文件增量检查不符合预期 first_processed=$($first.project_files_processed) second_processed=$($second.project_files_processed) second_unchanged=$($second.project_files_unchanged) third_processed=$($third.project_files_processed)"
}
if ($events.Count -ne 11 -or $uniqueEventIds.Count -ne 11 -or $activityIds.Count -ne 1 -or $activityIds[0] -ne 'u1') {
    throw "语义去重或自然任务边界不符合预期 events=$($events.Count) unique_events=$($uniqueEventIds.Count) activities=$($activityIds -join ',')"
}
if ($first.unattributed_event_count -ne 0 -or $second.unattributed_event_count -ne 0 -or $third.unattributed_event_count -ne 0) {
    throw "已进入自然任务边界的事件不应计为未归因 first=$($first.unattributed_event_count) second=$($second.unattributed_event_count) third=$($third.unattributed_event_count)"
}
if ($publishedManifest.semantic_events -ne $events.Count -or $publishedManifest.published_bytes -ne (Get-Item -LiteralPath $outputPath).Length) {
    throw "语义发布 manifest 与输出不闭合 manifest_events=$($publishedManifest.semantic_events) actual_events=$($events.Count) published_bytes=$($publishedManifest.published_bytes) actual_bytes=$((Get-Item -LiteralPath $outputPath).Length)"
}

[pscustomobject]@{
    first_new_events = $first.new_events_written
    repeated_new_events = $second.new_events_written
    appended_new_events = $third.new_events_written
    final_events = $events.Count
    unique_event_ids = $uniqueEventIds.Count
    unattributed_event_count = $third.unattributed_event_count
    user_activity_ids = $activityIds
    published_bytes = [long]$publishedManifest.published_bytes
} | ConvertTo-Json -Compress
