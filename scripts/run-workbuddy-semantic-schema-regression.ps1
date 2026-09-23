[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 120)]
    [int]$CollectorTimeoutSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [IO.Path]::GetFullPath($SourceRoot)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$collectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
foreach ($requiredPath in @($projects, $collectorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "语义格式回归依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "语义格式回归目录已存在，拒绝覆盖 path=$evidence"
}
New-Item -ItemType Directory -Path $evidence | Out-Null

$utf8 = [Text.UTF8Encoding]::new($false)
$expectedSourceSchemaProfile = 'workbuddy-private-jsonl-observed-v1'
$projectFiles = @(Get-ChildItem -LiteralPath $projects -Recurse -File -Filter '*.jsonl' | Sort-Object FullName)
if ($projectFiles.Count -eq 0) {
    throw "WorkBuddy 项目目录中没有 JSONL 文件 project_root=$projects"
}

$events = [Collections.Generic.List[object]]::new()
$recordCount = 0L
$parsedFileCount = 0
foreach ($projectFile in $projectFiles) {
    if ($projectFile.Length -eq 0) {
        continue
    }
    $parsedFileCount += 1
    $outputPath = Join-Path $evidence "$parsedFileCount.ndjson"
    $arguments = '--input "{0}" --since-unix-ms 0 --until-unix-ms 9999999999999 --session-id "schema-regression" --process-id 1 --output "{1}"' -f $projectFile.FullName, $outputPath
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $collectorPath
    $startInfo.Arguments = $arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $collector = [Diagnostics.Process]::Start($startInfo)
    if (-not $collector.WaitForExit($CollectorTimeoutSeconds * 1000)) {
        $collector.Kill()
        throw "WorkBuddy 私有格式回归超时 path=$($projectFile.FullName) timeout_seconds=$CollectorTimeoutSeconds"
    }
    $stdout = $collector.StandardOutput.ReadToEnd()
    $stderr = $collector.StandardError.ReadToEnd()
    if ($collector.ExitCode -ne 0) {
        throw "WorkBuddy 私有格式回归失败 path=$($projectFile.FullName) exit_code=$($collector.ExitCode) stdout=$stdout stderr=$stderr"
    }
    $collectorSummary = $stdout | ConvertFrom-Json
    $recordCount += [long]$collectorSummary.records_in_window
    foreach ($line in [IO.File]::ReadLines($outputPath, $utf8)) {
        if (-not [string]::IsNullOrWhiteSpace($line)) {
            $events.Add(($line | ConvertFrom-Json))
        }
    }
}

$eventIds = @($events | ForEach-Object { [string]$_.event_id })
$toolEvents = @($events | Where-Object { $_.content_kind -in @('available_tool', 'tool_call', 'tool_result') })
$summary = [ordered]@{
    schema_version = '0.1.0'
    source_schema_profiles = @($events | ForEach-Object { [string]$_.source_schema_profile } | Sort-Object -Unique)
    project_files_found = $projectFiles.Count
    project_files_parsed = $parsedFileCount
    source_records = $recordCount
    semantic_events = $events.Count
    unique_event_ids = @($eventIds | Sort-Object -Unique).Count
    native_agent_sessions = @($events | ForEach-Object { [string]$_.agent_session_id } | Where-Object { $_ } | Sort-Object -Unique).Count
    events_missing_native_session = @($events | Where-Object { [string]::IsNullOrWhiteSpace([string]$_.agent_session_id) }).Count
    events_missing_workspace = @($events | Where-Object { [string]::IsNullOrWhiteSpace([string]$_.workspace_path) }).Count
    tool_events = $toolEvents.Count
    tool_events_missing_call_id = @($toolEvents | Where-Object { [string]::IsNullOrWhiteSpace([string]$_.tool_call_id) }).Count
    completed_at = [DateTimeOffset]::UtcNow.ToString('o')
}
if ($summary.semantic_events -ne $summary.unique_event_ids) {
    throw "语义格式回归产生重复事件 ID events=$($summary.semantic_events) unique_event_ids=$($summary.unique_event_ids)"
}
if ($summary.source_schema_profiles.Count -ne 1 -or $summary.source_schema_profiles[0] -ne $expectedSourceSchemaProfile) {
    throw "语义格式回归来源格式标识不符合预期 expected=$expectedSourceSchemaProfile actual=$($summary.source_schema_profiles -join ',')"
}
if ($summary.native_agent_sessions -eq 0 -or $summary.events_missing_native_session -ne 0) {
    throw "语义格式回归缺少原生会话身份 sessions=$($summary.native_agent_sessions) missing=$($summary.events_missing_native_session)"
}
if ($summary.events_missing_workspace -ne 0) {
    throw "语义格式回归存在缺少工作空间的事件 missing=$($summary.events_missing_workspace)"
}
if ($summary.tool_events_missing_call_id -ne 0) {
    throw "语义格式回归存在缺少 callId 的工具事件 missing=$($summary.tool_events_missing_call_id)"
}

$summaryPath = Join-Path $evidence 'schema-regression-summary.json'
[IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 5), $utf8)
$summary | ConvertTo-Json -Depth 5 -Compress
