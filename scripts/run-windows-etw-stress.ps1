[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1000, 100000)]
    [int]$Iterations,

    [Parameter(Mandatory = $true)]
    [ValidateRange(10, 60)]
    [int]$DurationSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
if (Test-Path -LiteralPath $evidence) {
    throw "性能证据目录已经存在，拒绝覆盖 path=$evidence"
}

$collectorPath = Join-Path $source 'target\debug\etw-collector.exe'
$exporterPath = Join-Path $source 'scripts\export-process-evidence-csv.ps1'
foreach ($path in @($collectorPath, $exporterPath, $env:ComSpec)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "找不到性能测试依赖 path=$path"
    }
}

New-Item -ItemType Directory -Path $evidence | Out-Null
$paths = [ordered]@{
    fixture = Join-Path $evidence 'WorkBuddy.exe'
    workload = Join-Path $evidence 'read-stress.ps1'
    payload = Join-Path $evidence 'agentreins-perf-payload.bin'
    high_cardinality_count = Join-Path $evidence 'high-cardinality-count.txt'
    identity = Join-Path $evidence 'fixture-identity.json'
    full_ndjson = Join-Path $evidence 'etw-stress.full.ndjson'
    filtered_ndjson = Join-Path $evidence 'etw-stress.filtered.ndjson'
    full_csv = Join-Path $evidence 'etw-stress.full.csv'
    filtered_csv = Join-Path $evidence 'etw-stress.filtered.csv'
    health_full_csv = Join-Path $evidence 'collector-health.full.csv'
    health_filtered_csv = Join-Path $evidence 'collector-health.filtered.csv'
    performance_full_csv = Join-Path $evidence 'collector-performance.full.csv'
    performance_filtered_csv = Join-Path $evidence 'collector-performance.filtered.csv'
    manifest = Join-Path $evidence 'manifest.csv'
    stdout = Join-Path $evidence 'collector-stdout.json'
    stderr = Join-Path $evidence 'collector-stderr.ndjson'
}

Copy-Item -LiteralPath $env:ComSpec -Destination $paths.fixture
[IO.File]::WriteAllBytes($paths.payload, [byte[]]::new(65536))
$workload = @'
param(
    [Parameter(Mandatory = $true)]
    [int]$Iterations,
    [Parameter(Mandatory = $true)]
    [string]$PayloadPath,
    [Parameter(Mandatory = $true)]
    [string]$HighCardinalityPathPrefix,
    [Parameter(Mandatory = $true)]
    [string]$HighCardinalityCountPath,
    [Parameter(Mandatory = $true)]
    [long]$StopAtUnixMs
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
for ($index = 0; $index -lt $Iterations; $index += 1) {
    [void][IO.File]::ReadAllBytes($PayloadPath)
}
$highCardinalityCount = 0
while ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -lt ($StopAtUnixMs - 2500)) {
    Start-Sleep -Milliseconds 20
}
while ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -lt $StopAtUnixMs) {
    $candidatePath = "$HighCardinalityPathPrefix-$highCardinalityCount.missing"
    try {
        $stream = [IO.File]::OpenRead($candidatePath)
        $stream.Dispose()
    }
    catch [IO.FileNotFoundException] {
    }
    $highCardinalityCount += 1
}
[IO.File]::WriteAllText($HighCardinalityCountPath, [string]$highCardinalityCount, [Text.UTF8Encoding]::new($false))
'@
[IO.File]::WriteAllText($paths.workload, $workload, [Text.UTF8Encoding]::new($false))
$identity = [pscustomobject]@{
    executable_path = $paths.fixture
    file_version = (Get-Item -LiteralPath $paths.fixture).VersionInfo.FileVersion
    sha256 = (Get-FileHash -LiteralPath $paths.fixture -Algorithm SHA256).Hash.ToLowerInvariant()
    signer_subject = 'LOCAL_PERFORMANCE_FIXTURE'
}
[IO.File]::WriteAllText($paths.identity, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

$collectorInfo = [Diagnostics.ProcessStartInfo]::new()
$collectorInfo.FileName = $collectorPath
$collectorInfo.Arguments = "--duration-seconds $DurationSeconds --full-output `"$($paths.full_ndjson)`" --filtered-output `"$($paths.filtered_ndjson)`" --identity `"$($paths.identity)`""
$collectorInfo.UseShellExecute = $false
$collectorInfo.CreateNoWindow = $true
$collectorInfo.RedirectStandardOutput = $true
$collectorInfo.RedirectStandardError = $true
$collectorStartedAt = [DateTimeOffset]::UtcNow
$collector = [Diagnostics.Process]::Start($collectorInfo)
$collectorStdoutTask = $collector.StandardOutput.ReadToEndAsync()
$collectorStderrTask = $collector.StandardError.ReadToEndAsync()
$fixture = $null
$peakWorkingSetBytes = 0L
$peakPrivateMemoryBytes = 0L

try {
    Start-Sleep -Seconds 2
    if ($collector.HasExited) {
        throw "ETW Collector 提前退出 exit_code=$($collector.ExitCode) stderr=$($collectorStderrTask.GetAwaiter().GetResult())"
    }
    $stopAtUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + (($DurationSeconds - 1) * 1000)
    $fixtureInfo = [Diagnostics.ProcessStartInfo]::new()
    $fixtureInfo.FileName = $paths.fixture
    $fixtureInfo.Arguments = "/d /c powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$($paths.workload)`" -Iterations $Iterations -PayloadPath `"$($paths.payload)`" -HighCardinalityPathPrefix `"$($paths.payload)`" -HighCardinalityCountPath `"$($paths.high_cardinality_count)`" -StopAtUnixMs $stopAtUnixMs"
    $fixtureInfo.UseShellExecute = $false
    $fixtureInfo.CreateNoWindow = $true
    $fixture = [Diagnostics.Process]::Start($fixtureInfo)
    $waitDeadline = [DateTimeOffset]::UtcNow.AddSeconds($DurationSeconds + 180)
    while (-not $fixture.HasExited -or -not $collector.HasExited) {
        if ([DateTimeOffset]::UtcNow -gt $waitDeadline) {
            throw "等待性能测试进程退出超时 collector_process_id=$($collector.Id) fixture_process_id=$($fixture.Id)"
        }
        if (-not $collector.HasExited) {
            $collector.Refresh()
            $peakWorkingSetBytes = [Math]::Max($peakWorkingSetBytes, $collector.WorkingSet64)
            $peakPrivateMemoryBytes = [Math]::Max($peakPrivateMemoryBytes, $collector.PrivateMemorySize64)
        }
        Start-Sleep -Milliseconds 50
    }
    if ($fixture.ExitCode -ne 0) {
        throw "I/O 突发夹具失败 exit_code=$($fixture.ExitCode)"
    }
}
finally {
    if ($null -ne $fixture -and -not $fixture.HasExited) {
        Stop-Process -Id $fixture.Id -Force
    }
    if (-not $collector.HasExited) {
        Stop-Process -Id $collector.Id -Force
        $traceName = "AgentReins-Gate0-$($collector.Id)"
        $stopOutput = & logman.exe stop $traceName -ets 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "清理 ETW 会话失败 trace_name=$traceName output=$stopOutput"
        }
    }
}

$collector.Refresh()
$peakWorkingSetBytes = [Math]::Max($peakWorkingSetBytes, $collector.WorkingSet64)
$peakPrivateMemoryBytes = [Math]::Max($peakPrivateMemoryBytes, $collector.PrivateMemorySize64)
$collectorFinishedAt = [DateTimeOffset]::UtcNow
$collectorWallMs = ($collectorFinishedAt - $collectorStartedAt).TotalMilliseconds
$collectorCpuMs = $collector.TotalProcessorTime.TotalMilliseconds
$collectorStdout = $collectorStdoutTask.GetAwaiter().GetResult()
$collectorStderr = $collectorStderrTask.GetAwaiter().GetResult()
[IO.File]::WriteAllText($paths.stdout, $collectorStdout, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($paths.stderr, $collectorStderr, [Text.UTF8Encoding]::new($false))
if ($collector.ExitCode -ne 0) {
    throw "ETW Collector 失败 exit_code=$($collector.ExitCode) stderr=$collectorStderr"
}

$summary = $collectorStdout | ConvertFrom-Json
$exportSummary = & $exporterPath `
    -FullInputPath $paths.full_ndjson `
    -FilteredInputPath $paths.filtered_ndjson `
    -FullCsvPath $paths.full_csv `
    -FilteredCsvPath $paths.filtered_csv |
    ConvertFrom-Json
$filteredEvents = @(Get-Content -LiteralPath $paths.filtered_ndjson -Encoding UTF8 | Where-Object { $_.Length -gt 0 } | ForEach-Object { $_ | ConvertFrom-Json })
$highCardinalityCount = [long][IO.File]::ReadAllText($paths.high_cardinality_count, [Text.Encoding]::UTF8)
$fullCsvRowCount = 0L
$invalidFullTimestampCount = 0L
$fullSortViolationCount = 0L
$previousFullTimestamp = -1L
$previousFullObservedAt = -1L
$previousFullSequence = -1L
$fullEventIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$highCardinalityResources = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$payloadLeaf = [IO.Path]::GetFileName($paths.payload)
$lastHighCardinalityEventUnixMs = 0L
Import-Csv -LiteralPath $paths.full_csv | ForEach-Object {
    $fullCsvRowCount += 1
    $eventTimestamp = [long]$_.event_timestamp_unix_ms
    $observedAt = [long]$_.observed_at_unix_ms
    $sourceSequence = [long]$_.source_sequence
    if ($eventTimestamp -gt $observedAt) {
        $invalidFullTimestampCount += 1
    }
    if (
        $eventTimestamp -lt $previousFullTimestamp -or
        ($eventTimestamp -eq $previousFullTimestamp -and $observedAt -lt $previousFullObservedAt) -or
        ($eventTimestamp -eq $previousFullTimestamp -and $observedAt -eq $previousFullObservedAt -and $sourceSequence -lt $previousFullSequence)
    ) {
        $fullSortViolationCount += 1
    }
    $previousFullTimestamp = $eventTimestamp
    $previousFullObservedAt = $observedAt
    $previousFullSequence = $sourceSequence
    $fullEventIds.Add([string]$_.event_id) | Out-Null
    if (
        ([string]$_.resource_identifier).IndexOf($payloadLeaf, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and
        ([string]$_.resource_identifier).EndsWith('.missing', [StringComparison]::OrdinalIgnoreCase)
    ) {
        $highCardinalityResources.Add([string]$_.resource_identifier) | Out-Null
        $lastHighCardinalityEventUnixMs = [Math]::Max($lastHighCardinalityEventUnixMs, $eventTimestamp)
    }
}
$eventsBySecond = @{}
foreach ($event in $filteredEvents) {
    $second = [string][Math]::Floor([double]$event.event_timestamp_unix_ms / 1000)
    $occurrenceCount = if ($null -eq $event.aggregation) { 1L } else { [long]$event.aggregation.occurrence_count }
    if (-not $eventsBySecond.ContainsKey($second)) {
        $eventsBySecond[$second] = 0L
    }
    $eventsBySecond[$second] += $occurrenceCount
}
$peakEventsPerSecond = [long](($eventsBySecond.Values | Measure-Object -Maximum).Maximum)
$logicalProcessorCount = [Environment]::ProcessorCount
$sessionStartedUnixMs = [long]([string]$summary.session_id).Substring('gate0-'.Length)
$stopTailDistanceMs = ($sessionStartedUnixMs + ($DurationSeconds * 1000)) - $lastHighCardinalityEventUnixMs
$capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
$health = [pscustomobject]@{
    captured_at = $capturedAt
    session_id = $summary.session_id
    provider_events_received = $summary.provider_events_received
    events_handled = $summary.events_handled
    parsed_events = $summary.parsed_events
    full_events_written = $summary.full_events_written
    filtered_events_written = $summary.filtered_events_written
    parse_failures = $summary.parse_failures
    write_failures = $summary.write_failures
    output_queue_capacity = $summary.output_queue_capacity
    peak_output_queue_depth = $summary.peak_output_queue_depth
    output_source_events = $summary.output_source_events
    output_batches_generated = $summary.output_batches_generated
    source_events_collapsed = $summary.source_events_collapsed
    output_batches_dropped = $summary.output_batches_dropped
    retained_detail_source_events = $summary.retained_detail_source_events
    retained_summary_source_events = $summary.retained_summary_source_events
    retained_review_source_events = $summary.retained_review_source_events
    discarded_source_events = $summary.discarded_source_events
    output_queue_wait_count = $summary.output_queue_wait_count
    output_queue_wait_ms = $summary.output_queue_wait_ms
    peak_aggregation_keys = $summary.peak_aggregation_keys
    events_lost = $summary.events_lost
    log_buffers_lost = $summary.log_buffers_lost
    realtime_buffers_lost = $summary.realtime_buffers_lost
    coverage_status = $summary.coverage_status
}
$health | Export-Csv -LiteralPath $paths.health_full_csv -NoTypeInformation -Encoding UTF8
$health |
    Select-Object captured_at,session_id,provider_events_received,events_handled,parsed_events,full_events_written,filtered_events_written,parse_failures,write_failures,output_queue_capacity,peak_output_queue_depth,output_source_events,output_batches_generated,source_events_collapsed,output_batches_dropped,retained_detail_source_events,retained_summary_source_events,retained_review_source_events,discarded_source_events,output_queue_wait_count,output_queue_wait_ms,peak_aggregation_keys,events_lost,log_buffers_lost,realtime_buffers_lost,coverage_status |
    Export-Csv -LiteralPath $paths.health_filtered_csv -NoTypeInformation -Encoding UTF8
$performance = [pscustomobject]@{
    captured_at = $capturedAt
    iterations = $Iterations
    duration_seconds = $DurationSeconds
    collector_wall_ms = [Math]::Round($collectorWallMs, 3)
    collector_cpu_ms = [Math]::Round($collectorCpuMs, 3)
    collector_cpu_percent_one_core = [Math]::Round(($collectorCpuMs / $collectorWallMs) * 100, 3)
    collector_cpu_percent_normalized = [Math]::Round(($collectorCpuMs / $collectorWallMs) * 100 / $logicalProcessorCount, 3)
    peak_working_set_mb = [Math]::Round($peakWorkingSetBytes / 1MB, 3)
    peak_private_memory_mb = [Math]::Round($peakPrivateMemoryBytes / 1MB, 3)
    peak_events_per_second = $peakEventsPerSecond
    full_output_bytes = (Get-Item -LiteralPath $paths.full_ndjson).Length
    filtered_output_bytes = (Get-Item -LiteralPath $paths.filtered_ndjson).Length
    high_cardinality_attempts = $highCardinalityCount
    captured_high_cardinality_resources = $highCardinalityResources.Count
    stop_tail_distance_ms = $stopTailDistanceMs
    full_csv_rows = $fullCsvRowCount
    unique_full_event_ids = $fullEventIds.Count
    invalid_full_timestamp_count = $invalidFullTimestampCount
    full_sort_violation_count = $fullSortViolationCount
}
$performance | Export-Csv -LiteralPath $paths.performance_full_csv -NoTypeInformation -Encoding UTF8
$performance |
    Select-Object captured_at,iterations,duration_seconds,collector_wall_ms,collector_cpu_ms,collector_cpu_percent_one_core,collector_cpu_percent_normalized,peak_working_set_mb,peak_private_memory_mb,peak_events_per_second,full_output_bytes,filtered_output_bytes,high_cardinality_attempts,captured_high_cardinality_resources,stop_tail_distance_ms,full_csv_rows,unique_full_event_ids,invalid_full_timestamp_count,full_sort_violation_count |
    Export-Csv -LiteralPath $paths.performance_filtered_csv -NoTypeInformation -Encoding UTF8
$manifest = @(
    [pscustomobject]@{dataset='etw-stress';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.full_csv);filtered_csv=(Split-Path -Leaf $paths.filtered_csv);full_rows=$exportSummary.full_rows;filtered_rows=$exportSummary.filtered_rows;source_host=$env:COMPUTERNAME;filter_rule='有效明细保留；背景活动按行为窗口聚合；重复或无法形成事实的事件丢弃'},
    [pscustomobject]@{dataset='collector-health';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.health_full_csv);filtered_csv=(Split-Path -Leaf $paths.health_filtered_csv);full_rows=1;filtered_rows=1;source_host=$env:COMPUTERNAME;filter_rule='过滤版移除本地输出路径'},
    [pscustomobject]@{dataset='collector-performance';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.performance_full_csv);filtered_csv=(Split-Path -Leaf $paths.performance_filtered_csv);full_rows=1;filtered_rows=1;source_host=$env:COMPUTERNAME;filter_rule='过滤版不包含测试路径'}
)
$manifest | Export-Csv -LiteralPath $paths.manifest -NoTypeInformation -Encoding UTF8

$result = [pscustomobject]@{
    status = 'completed'
    evidence_root = $evidence
    health = $health
    performance = $performance
}
$result | ConvertTo-Json -Depth 4 -Compress

if (
    $health.coverage_status -ne 'healthy' -or
    $health.events_lost -ne 0 -or
    $health.parse_failures -ne 0 -or
    $health.write_failures -ne 0 -or
    $health.output_batches_dropped -ne 0
) {
    throw "ETW 突发性能门禁失败 result=$($result | ConvertTo-Json -Depth 4 -Compress)"
}
if ($performance.high_cardinality_attempts -lt 4096) {
    throw "高基数测试负载不足 attempts=$($performance.high_cardinality_attempts) expected_at_least=4096"
}
if ($performance.captured_high_cardinality_resources -lt 4096) {
    throw "进入证据的高基数活动不足 captured=$($performance.captured_high_cardinality_resources) expected_at_least=4096"
}
if ([Math]::Abs($performance.stop_tail_distance_ms) -gt 500) {
    throw "高基数活动未持续到采集停止边界 distance_ms=$($performance.stop_tail_distance_ms) expected_absolute_at_most=500"
}
$accountedSourceEvents = [long]$summary.retained_detail_source_events + [long]$summary.retained_summary_source_events + [long]$summary.retained_review_source_events + [long]$summary.discarded_source_events
if (
    $accountedSourceEvents -ne $summary.output_source_events -or
    $performance.full_csv_rows -ne $summary.full_events_written -or
    $performance.unique_full_event_ids -ne $performance.full_csv_rows -or
    $performance.invalid_full_timestamp_count -ne 0 -or
    $performance.full_sort_violation_count -ne 0 -or
    $summary.output_batches_generated -ne $summary.full_events_written -or
    $exportSummary.filtered_rows -ne $summary.filtered_events_written
) {
    throw "完整证据一致性检查失败 result=$($result | ConvertTo-Json -Depth 4 -Compress) export=$($exportSummary | ConvertTo-Json -Compress)"
}
