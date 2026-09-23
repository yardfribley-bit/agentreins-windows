param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$collectorPath = Join-Path $SourceRoot "target\debug\etw-collector.exe"
$correlatorPath = Join-Path $SourceRoot "target\debug\evidence-correlator.exe"
$fixturePath = Join-Path $SourceRoot "target\debug\WorkBuddy.exe"
$processExporterPath = Join-Path $SourceRoot "scripts\export-process-evidence-csv.ps1"
$correlationExporterPath = Join-Path $SourceRoot "scripts\export-correlated-evidence-csv.ps1"
foreach ($requiredPath in @($collectorPath, $correlatorPath, $fixturePath, $processExporterPath, $correlationExporterPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "未找到烟测依赖 path=$requiredPath"
    }
}
if (@(Get-Process | Where-Object { $_.ProcessName -eq "WorkBuddy" }).Count -ne 0) {
    throw "发现已运行的 WorkBuddy 进程；为避免混入真实客户端，拒绝执行测试"
}

New-Item -ItemType Directory -Path $EvidenceRoot -Force | Out-Null
$paths = [ordered]@{
    identity = Join-Path $EvidenceRoot "fixture-identity.json"
    full_ndjson = Join-Path $EvidenceRoot "fixture-observation.full.ndjson"
    filtered_ndjson = Join-Path $EvidenceRoot "fixture-observation.filtered.ndjson"
    full_csv = Join-Path $EvidenceRoot "fixture-observation.full.csv"
    filtered_csv = Join-Path $EvidenceRoot "fixture-observation.filtered.csv"
    semantic_input = Join-Path $EvidenceRoot "synthetic-semantic.input.ndjson"
    credential_catalog = Join-Path $EvidenceRoot "synthetic-credential-catalog.json"
    correlation_full_ndjson = Join-Path $EvidenceRoot "synthetic-correlation.full.ndjson"
    correlation_filtered_ndjson = Join-Path $EvidenceRoot "synthetic-correlation.filtered.ndjson"
    correlation_full_csv = Join-Path $EvidenceRoot "synthetic-correlation.full.csv"
    correlation_filtered_csv = Join-Path $EvidenceRoot "synthetic-correlation.filtered.csv"
    health_full_csv = Join-Path $EvidenceRoot "collector-health.full.csv"
    health_filtered_csv = Join-Path $EvidenceRoot "collector-health.filtered.csv"
    manifest = Join-Path $EvidenceRoot "manifest.csv"
    collector_stdout = Join-Path $EvidenceRoot "collector-stdout.json"
    collector_stderr = Join-Path $EvidenceRoot "collector-stderr.ndjson"
    correlator_stdout = Join-Path $EvidenceRoot "correlator-stdout.json"
    correlator_stderr = Join-Path $EvidenceRoot "correlator-stderr.ndjson"
}
foreach ($artifactPath in $paths.Values) {
    if (Test-Path -LiteralPath $artifactPath) {
        Remove-Item -LiteralPath $artifactPath
    }
}

$fixtureIdentity = [pscustomobject]@{
    executable_path = $fixturePath
    file_version = "0.1.0"
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $fixturePath).Hash.ToLowerInvariant()
    signer_subject = "UNSIGNED_TEST_FIXTURE"
}
[IO.File]::WriteAllText(
    $paths.identity,
    ($fixtureIdentity | ConvertTo-Json),
    [Text.UTF8Encoding]::new($false)
)

$collector = $null
$fixtureExitCode = $null
try {
    $collectorArguments = "--duration-seconds 8 --full-output $($paths.full_ndjson) --filtered-output $($paths.filtered_ndjson) --identity $($paths.identity)"
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $collectorPath
    $startInfo.Arguments = $collectorArguments
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $collector = [System.Diagnostics.Process]::Start($startInfo)
    Start-Sleep -Seconds 2
    & $fixturePath
    $fixtureExitCode = $LASTEXITCODE
    if (-not $collector.WaitForExit(15000)) {
        throw "等待 ETW Collector 退出超时 process_id=$($collector.Id)"
    }
    [IO.File]::WriteAllText($paths.collector_stdout, $collector.StandardOutput.ReadToEnd(), [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($paths.collector_stderr, $collector.StandardError.ReadToEnd(), [Text.UTF8Encoding]::new($false))
}
finally {
    if ($null -ne $collector -and -not $collector.HasExited) {
        Stop-Process -Id $collector.Id -Force
        $traceName = "AgentReins-Gate0-$($collector.Id)"
        $stopOutput = & logman.exe stop $traceName -ets 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "清理 ETW 会话失败 trace_name=$traceName output=$stopOutput"
        }
    }
}

if ($collector.ExitCode -ne 0) {
    $errorOutput = Get-Content -LiteralPath $paths.collector_stderr -Raw -Encoding UTF8
    throw "ETW Collector 失败 exit_code=$($collector.ExitCode) stderr=$errorOutput"
}
if ($fixtureExitCode -ne 0) {
    throw "WorkBuddy 测试夹具失败 exit_code=$fixtureExitCode"
}

$fullEvents = @(Get-Content -LiteralPath $paths.full_ndjson -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })
$filteredEvents = @(Get-Content -LiteralPath $paths.filtered_ndjson -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })
$exportSummary = & $processExporterPath -FullInputPath $paths.full_ndjson -FilteredInputPath $paths.filtered_ndjson -FullCsvPath $paths.full_csv -FilteredCsvPath $paths.filtered_csv |
    ConvertFrom-Json
$collectorSummary = Get-Content -LiteralPath $paths.collector_stdout -Raw -Encoding UTF8 | ConvertFrom-Json
$rootStarts = @($filteredEvents | Where-Object {
    $_.action.kind -eq "process_start" -and
    $_.phase -eq "live" -and
    $_.actor.identity_status -eq "verified"
})
$rootStops = @($filteredEvents | Where-Object {
    $_.action.kind -eq "process_stop" -and
    $_.phase -eq "live" -and
    $_.actor.identity_status -eq "verified"
})
if ($rootStarts.Count -ne 1 -or $rootStops.Count -ne 1) {
    throw "测试夹具进程生命周期数量不符合预期 starts=$($rootStarts.Count) stops=$($rootStops.Count)"
}
$rootStart = $rootStarts[0]
$rootStop = $rootStops[0]
if ($rootStart.process.process_instance_id -ne $rootStop.process.process_instance_id) {
    throw "进程启动/退出实例标识不一致 start=$($rootStart.process.process_instance_id) stop=$($rootStop.process.process_instance_id)"
}

$fakeCredential = "agentreins_gate0_fake_token_9f0c"
$credentialFingerprint = "sha256:$((Get-FileHash -Algorithm SHA256 -LiteralPath $fixturePath).Hash.Substring(0, 16).ToLowerInvariant())"
$credentialCatalog = @(
    [pscustomobject]@{
        label = "Gate0测试令牌"
        kind = "api_token"
        value = $fakeCredential
        fingerprint = $credentialFingerprint
    }
)
[IO.File]::WriteAllText(
    $paths.credential_catalog,
    (ConvertTo-Json -InputObject $credentialCatalog),
    [Text.UTF8Encoding]::new($false)
)
$referenceEvent = @($filteredEvents | Where-Object { $_.process.pid -eq $rootStart.process.pid })[0]
$fileActionEvent = @($filteredEvents | Where-Object {
    $_.resource.kind -eq "file" -and
    $_.result.success -eq $true -and
    -not [string]::IsNullOrWhiteSpace([string]$_.operation_id)
})[0]
$fileResultEvent = $fileActionEvent
$semanticEvents = @(
    [pscustomobject]@{schema_version="0.6.0";event_id="synthetic:1";event_timestamp_unix_ms=$referenceEvent.event_timestamp_unix_ms;session_id=$referenceEvent.session.id;process_id=$referenceEvent.process.pid;action_kind="user_input";content="请使用测试令牌 $fakeCredential 完成文件检查";tool_name=$null;operation_id=$null;expected_os_action=$null;evidence_source="synthetic_fixture";source_record_id="synthetic-source-1";parent_record_id=$null;trace_id="synthetic-trace-1";model_id="synthetic-model"},
    [pscustomobject]@{schema_version="0.6.0";event_id="synthetic:2";event_timestamp_unix_ms=($referenceEvent.event_timestamp_unix_ms + 1);session_id=$referenceEvent.session.id;process_id=$referenceEvent.process.pid;action_kind="llm_request";content="模型请求携带测试令牌 $fakeCredential";tool_name=$null;operation_id="semantic-op-1";operation_id_origin="agent_reins_generated";expected_os_action=$null;evidence_source="synthetic_fixture";source_record_id="synthetic-source-2";parent_record_id="synthetic-source-1";trace_id="synthetic-trace-1";model_id="synthetic-model"},
    [pscustomobject]@{schema_version="0.6.0";event_id="synthetic:3";event_timestamp_unix_ms=$fileActionEvent.event_timestamp_unix_ms;session_id=$referenceEvent.session.id;process_id=$referenceEvent.process.pid;action_kind="tool_call";content="访问测试文件";tool_name="fixture_file_io";operation_id=$fileActionEvent.operation_id;operation_id_origin="agent_reins_generated";expected_os_action=$fileActionEvent.action.kind;evidence_source="synthetic_fixture";source_record_id="synthetic-source-3";parent_record_id="synthetic-source-2";trace_id="synthetic-trace-1";model_id="synthetic-model"},
    [pscustomobject]@{schema_version="0.6.0";event_id="synthetic:4";event_timestamp_unix_ms=$fileResultEvent.event_timestamp_unix_ms;session_id=$referenceEvent.session.id;process_id=$referenceEvent.process.pid;action_kind="tool_result";content="测试工具执行成功";tool_name="fixture_file_io";operation_id=$fileActionEvent.operation_id;operation_id_origin="agent_reins_generated";expected_os_action=$fileActionEvent.action.kind;evidence_source="synthetic_fixture";source_record_id="synthetic-source-4";parent_record_id="synthetic-source-3";trace_id="synthetic-trace-1";model_id="synthetic-model"},
    [pscustomobject]@{schema_version="0.6.0";event_id="synthetic:5";event_timestamp_unix_ms=$rootStop.event_timestamp_unix_ms;session_id=$referenceEvent.session.id;process_id=$referenceEvent.process.pid;action_kind="llm_result";content="任务已完成";tool_name=$null;operation_id="semantic-op-1";operation_id_origin="agent_reins_generated";expected_os_action=$null;evidence_source="synthetic_fixture";source_record_id="synthetic-source-5";parent_record_id="synthetic-source-4";trace_id="synthetic-trace-1";model_id="synthetic-model"}
)
$semanticLines = @($semanticEvents | ForEach-Object { $_ | ConvertTo-Json -Compress })
[IO.File]::WriteAllLines($paths.semantic_input, $semanticLines, [Text.UTF8Encoding]::new($false))

$correlatorArguments = "--semantic-input $($paths.semantic_input) --os-input $($paths.filtered_ndjson) --credential-catalog $($paths.credential_catalog) --full-output $($paths.correlation_full_ndjson) --filtered-output $($paths.correlation_filtered_ndjson) --window-ms 5000"
$correlatorInfo = [System.Diagnostics.ProcessStartInfo]::new()
$correlatorInfo.FileName = $correlatorPath
$correlatorInfo.Arguments = $correlatorArguments
$correlatorInfo.UseShellExecute = $false
$correlatorInfo.RedirectStandardOutput = $true
$correlatorInfo.RedirectStandardError = $true
$correlator = [System.Diagnostics.Process]::Start($correlatorInfo)
$correlator.WaitForExit()
[IO.File]::WriteAllText($paths.correlator_stdout, $correlator.StandardOutput.ReadToEnd(), [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($paths.correlator_stderr, $correlator.StandardError.ReadToEnd(), [Text.UTF8Encoding]::new($false))
if ($correlator.ExitCode -ne 0) {
    throw "证据关联器失败 exit_code=$($correlator.ExitCode) stderr=$(Get-Content -LiteralPath $paths.correlator_stderr -Raw -Encoding UTF8)"
}
& $correlationExporterPath -FullInputPath $paths.correlation_full_ndjson -FilteredInputPath $paths.correlation_filtered_ndjson -FullCsvPath $paths.correlation_full_csv -FilteredCsvPath $paths.correlation_filtered_csv | Out-Null
$correlationFullEvents = @(Get-Content -LiteralPath $paths.correlation_full_ndjson -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })
$correlationFilteredEvents = @(Get-Content -LiteralPath $paths.correlation_filtered_ndjson -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })

$capturedAt = (Get-Date).ToString("o")
$healthFull = [pscustomobject]@{
    captured_at = $capturedAt
    session_id = $collectorSummary.session_id
    full_output_path = $collectorSummary.full_output_path
    filtered_output_path = $collectorSummary.filtered_output_path
    provider_events_received = $collectorSummary.provider_events_received
    events_handled = $collectorSummary.events_handled
    parsed_events = $collectorSummary.parsed_events
    process_events_parsed = $collectorSummary.process_events_parsed
    thread_events_parsed = $collectorSummary.thread_events_parsed
    file_events_parsed = $collectorSummary.file_events_parsed
    network_events_parsed = $collectorSummary.network_events_parsed
    full_events_written = $collectorSummary.full_events_written
    filtered_events_written = $collectorSummary.filtered_events_written
    parse_failures = $collectorSummary.parse_failures
    write_failures = $collectorSummary.write_failures
    output_queue_capacity = $collectorSummary.output_queue_capacity
    peak_output_queue_depth = $collectorSummary.peak_output_queue_depth
    output_source_events = $collectorSummary.output_source_events
    output_batches_generated = $collectorSummary.output_batches_generated
    source_events_collapsed = $collectorSummary.source_events_collapsed
    output_batches_dropped = $collectorSummary.output_batches_dropped
    retained_detail_source_events = $collectorSummary.retained_detail_source_events
    retained_summary_source_events = $collectorSummary.retained_summary_source_events
    retained_review_source_events = $collectorSummary.retained_review_source_events
    discarded_source_events = $collectorSummary.discarded_source_events
    output_queue_wait_count = $collectorSummary.output_queue_wait_count
    output_queue_wait_ms = $collectorSummary.output_queue_wait_ms
    peak_aggregation_keys = $collectorSummary.peak_aggregation_keys
    identity_collision_count = $collectorSummary.identity_collision_count
    events_lost = $collectorSummary.events_lost
    log_buffers_lost = $collectorSummary.log_buffers_lost
    realtime_buffers_lost = $collectorSummary.realtime_buffers_lost
    buffers_written = $collectorSummary.buffers_written
    coverage_status = $collectorSummary.coverage_status
}
$healthFull | Export-Csv -LiteralPath $paths.health_full_csv -NoTypeInformation -Encoding UTF8
$healthFull |
    Select-Object captured_at,session_id,provider_events_received,events_handled,parsed_events,process_events_parsed,thread_events_parsed,file_events_parsed,network_events_parsed,full_events_written,filtered_events_written,parse_failures,write_failures,output_queue_capacity,peak_output_queue_depth,output_source_events,output_batches_generated,source_events_collapsed,output_batches_dropped,retained_detail_source_events,retained_summary_source_events,retained_review_source_events,discarded_source_events,output_queue_wait_count,output_queue_wait_ms,peak_aggregation_keys,identity_collision_count,events_lost,log_buffers_lost,realtime_buffers_lost,buffers_written,coverage_status |
    Export-Csv -LiteralPath $paths.health_filtered_csv -NoTypeInformation -Encoding UTF8

$manifestRows = @(
    [pscustomobject]@{dataset="fixture-observation";captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.full_csv);filtered_csv=(Split-Path -Leaf $paths.filtered_csv);full_rows=$exportSummary.full_rows;filtered_rows=$exportSummary.filtered_rows;prefilter_full_available=$true;source_host=$env:COMPUTERNAME;filter_rule="WorkBuddy实例归因；有效明细保留；背景活动聚合；重复或无事实事件丢弃"},
    [pscustomobject]@{dataset="synthetic-correlation";captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.correlation_full_csv);filtered_csv=(Split-Path -Leaf $paths.correlation_filtered_csv);full_rows=$correlationFullEvents.Count;filtered_rows=$correlationFilteredEvents.Count;prefilter_full_available=$true;source_host=$env:COMPUTERNAME;filter_rule="过滤版保留凭据标签与指纹并移除凭据正文"},
    [pscustomobject]@{dataset="collector-health";captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.health_full_csv);filtered_csv=(Split-Path -Leaf $paths.health_filtered_csv);full_rows=1;filtered_rows=1;prefilter_full_available="not_applicable";source_host=$env:COMPUTERNAME;filter_rule="过滤版移除本地输出路径"}
)
$manifestRows | Export-Csv -LiteralPath $paths.manifest -NoTypeInformation -Encoding UTF8

$result = [pscustomobject]@{
    collector_exit_code = $collector.ExitCode
    fixture_exit_code = $fixtureExitCode
    full_event_count = $exportSummary.full_rows
    full_batch_count = $fullEvents.Count
    filtered_event_count = $filteredEvents.Count
    live_start_count = @($filteredEvents | Where-Object { $_.action.kind -eq "process_start" -and $_.phase -eq "live" }).Count
    live_stop_count = @($filteredEvents | Where-Object { $_.action.kind -eq "process_stop" -and $_.phase -eq "live" }).Count
    rundown_count = @($fullEvents | Where-Object { $_.phase -eq "rundown" }).Count
    file_action_count = @($filteredEvents | Where-Object { $_.resource.kind -eq "file" }).Count
    successful_file_result_count = @($filteredEvents | Where-Object { $_.resource.kind -eq "file" -and $_.result.success -eq $true }).Count
    retained_file_completion_count = @($filteredEvents | Where-Object { $_.action.kind -eq "file_operation_result" }).Count
    missing_retention_reason_count = @($filteredEvents | Where-Object { $null -eq $_.retention -or [string]::IsNullOrWhiteSpace([string]$_.retention.reason) }).Count
    network_action_count = @($filteredEvents | Where-Object { $_.resource.kind -eq "network" }).Count
    network_events_missing_operation_id = @($filteredEvents | Where-Object { $_.resource.kind -eq "network" -and [string]::IsNullOrWhiteSpace([string]$_.operation_id) }).Count
    verified_count = @($filteredEvents | Where-Object { $_.actor.identity_status -eq "verified" }).Count
    inherited_count = @($filteredEvents | Where-Object { $_.actor.identity_status -eq "inherited" }).Count
    invalid_timestamp_count = @($filteredEvents | Where-Object { $_.event_timestamp_unix_ms -gt $_.observed_at_unix_ms }).Count
    stable_process_instance = $rootStart.process.process_instance_id -eq $rootStop.process.process_instance_id
    coverage_status = $collectorSummary.coverage_status
    events_lost = $collectorSummary.events_lost
    parse_failures = $collectorSummary.parse_failures
    write_failures = $collectorSummary.write_failures
    output_batches_dropped = $collectorSummary.output_batches_dropped
    identity_collision_count = $collectorSummary.identity_collision_count
    semantic_event_count = $correlationFullEvents.Count
    correlated_semantic_count = @($correlationFullEvents | Where-Object { $null -ne $_.correlated_os_event_id }).Count
    confirmed_semantic_count = @($correlationFullEvents | Where-Object { $_.correlation_confirmed -eq $true }).Count
    partial_semantic_count = @($correlationFullEvents | Where-Object { $null -ne $_.expected_os_action -and $_.correlation_status -eq 'partial' }).Count
    credential_finding_count = @($correlationFullEvents | ForEach-Object { $_.credential_findings }).Count
    filtered_secret_occurrences = @($correlationFilteredEvents | Where-Object { $_.content -like "*$fakeCredential*" }).Count
    filtered_credential_label_count = @($correlationFilteredEvents | ForEach-Object { $_.credential_findings } | Where-Object { $_.label -eq "Gate0测试令牌" }).Count
    generated_operation_id_created_link = @($correlationFullEvents | Where-Object { $_.event_id -in @("synthetic:3", "synthetic:4") -and $null -ne $_.correlated_os_event_id }).Count -gt 0
    full_csv_rows = @(Import-Csv -LiteralPath $paths.full_csv).Count
    filtered_csv_rows = @(Import-Csv -LiteralPath $paths.filtered_csv).Count
    semantic_full_csv_rows = @(Import-Csv -LiteralPath $paths.correlation_full_csv).Count
    semantic_filtered_csv_rows = @(Import-Csv -LiteralPath $paths.correlation_filtered_csv).Count
    stderr_bytes = (Get-Item -LiteralPath $paths.collector_stderr).Length + (Get-Item -LiteralPath $paths.correlator_stderr).Length
    remaining_trace_sessions = @(& logman.exe query -ets | Select-String "AgentReins-Gate0").Count
}
$result | ConvertTo-Json -Compress

if ($result.live_start_count -lt 2 -or $result.live_stop_count -lt 2) {
    throw "进程生命周期或阶段语义不完整 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.file_action_count -lt 1 -or $result.successful_file_result_count -lt 1 -or $result.network_action_count -lt 1 -or $result.network_events_missing_operation_id -ne 0) {
    throw "文件/网络/结果观测不完整 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.retained_file_completion_count -ne 0 -or $result.missing_retention_reason_count -ne 0) {
    throw "证据保留规则验证失败 result=$($result | ConvertTo-Json -Compress)"
}
if (-not $result.stable_process_instance -or $result.verified_count -lt 1 -or $result.inherited_count -lt 1 -or $result.invalid_timestamp_count -ne 0) {
    throw "身份、实例或时间验证失败 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.coverage_status -ne "healthy" -or $result.events_lost -ne 0 -or $result.parse_failures -ne 0 -or $result.write_failures -ne 0 -or $result.output_batches_dropped -ne 0 -or $result.identity_collision_count -ne 0) {
    throw "ETW 覆盖健康检查失败 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.semantic_event_count -ne 5 -or $result.correlated_semantic_count -ne 0 -or $result.confirmed_semantic_count -ne 0 -or $result.partial_semantic_count -ne 2 -or $result.generated_operation_id_created_link -or $result.credential_finding_count -ne 2 -or $result.filtered_secret_occurrences -ne 0 -or $result.filtered_credential_label_count -ne 2) {
    throw "语义/凭据关联验证失败 result=$($result | ConvertTo-Json -Compress)"
}
$accountedSourceEvents = [long]$collectorSummary.retained_detail_source_events + [long]$collectorSummary.retained_summary_source_events + [long]$collectorSummary.retained_review_source_events + [long]$collectorSummary.discarded_source_events
if ($accountedSourceEvents -ne $collectorSummary.output_source_events -or $result.full_csv_rows -ne $result.full_event_count -or $result.full_batch_count -ne $collectorSummary.full_events_written -or $collectorSummary.output_batches_generated -ne $collectorSummary.full_events_written -or $result.filtered_csv_rows -ne $result.filtered_event_count -or $result.semantic_full_csv_rows -ne 5 -or $result.semantic_filtered_csv_rows -ne 5) {
    throw "CSV 行数校验失败 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.stderr_bytes -ne 0 -or $result.remaining_trace_sessions -ne 0) {
    throw "采集器存在警告或残留会话 result=$($result | ConvertTo-Json -Compress)"
}
