[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$WorkBuddyPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedSha256,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedSignerThumbprint,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TokenFile,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialLabel,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialKind,

    [Parameter(Mandatory = $true)]
    [ValidateRange(30, 3600)]
    [int]$DurationSeconds,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 60000)]
    [int]$CorrelationWindowMs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$workBuddy = [IO.Path]::GetFullPath($WorkBuddyPath)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$tokenPath = [IO.Path]::GetFullPath($TokenFile)

foreach ($path in @($source, $workBuddy, $projects, $tokenPath)) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "找不到观测所需路径 path=$path"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "证据目录已经存在，拒绝覆盖 path=$evidence"
}

$collectorPath = Join-Path $source 'target\debug\etw-collector.exe'
$semanticCollectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
$correlatorPath = Join-Path $source 'target\debug\evidence-correlator.exe'
$processExporterPath = Join-Path $source 'scripts\export-process-evidence-csv.ps1'
$correlationExporterPath = Join-Path $source 'scripts\export-correlated-evidence-csv.ps1'
$contextExporterPath = Join-Path $source 'scripts\export-workbuddy-context-snapshot.ps1'
$reportGeneratorPath = Join-Path $source 'scripts\new-workbuddy-observation-report.ps1'
foreach ($path in @($collectorPath, $semanticCollectorPath, $correlatorPath, $processExporterPath, $correlationExporterPath, $contextExporterPath, $reportGeneratorPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "找不到已构建的观测组件 path=$path"
    }
}

$actualSha256 = (Get-FileHash -LiteralPath $workBuddy -Algorithm SHA256).Hash
if ($actualSha256 -ne $ExpectedSha256) {
    throw "WorkBuddy 哈希不匹配 path=$workBuddy expected=$ExpectedSha256 actual=$actualSha256"
}
$signature = Get-AuthenticodeSignature -LiteralPath $workBuddy
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $ExpectedSignerThumbprint) {
    throw "WorkBuddy 签名不匹配 path=$workBuddy status=$($signature.Status) thumbprint=$($signature.SignerCertificate.Thumbprint)"
}

$workBuddyProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'WorkBuddy.exe'")
$workBuddyProcessIds = @($workBuddyProcesses.ProcessId)
$rootProcesses = @($workBuddyProcesses | Where-Object {
    $_.ExecutablePath -eq $workBuddy -and $workBuddyProcessIds -notcontains $_.ParentProcessId
})
if ($rootProcesses.Count -ne 1) {
    throw "无法唯一识别 WorkBuddy 根进程 path=$workBuddy candidate_count=$($rootProcesses.Count) process_ids=$($rootProcesses.ProcessId -join ',')"
}
$rootProcessId = [uint32]$rootProcesses[0].ProcessId

$token = (Get-Content -LiteralPath $tokenPath -Raw).TrimEnd()
if ($token.Length -eq 0) {
    throw "测试凭据文件为空 path=$tokenPath"
}

New-Item -ItemType Directory -Path $evidence | Out-Null
$paths = [ordered]@{
    identity = Join-Path $evidence 'workbuddy-identity.json'
    credential_catalog = Join-Path $evidence 'credential-catalog.json'
    os_full_ndjson = Join-Path $evidence 'workbuddy-os.full.ndjson'
    os_filtered_ndjson = Join-Path $evidence 'workbuddy-os.filtered.ndjson'
    os_full_csv = Join-Path $evidence 'workbuddy-os.full.csv'
    os_filtered_csv = Join-Path $evidence 'workbuddy-os.filtered.csv'
    semantic_ndjson = Join-Path $evidence 'workbuddy-semantic.full.ndjson'
    correlation_full_ndjson = Join-Path $evidence 'workbuddy-correlation.full.ndjson'
    correlation_filtered_ndjson = Join-Path $evidence 'workbuddy-correlation.filtered.ndjson'
    correlation_full_csv = Join-Path $evidence 'workbuddy-correlation.full.csv'
    correlation_filtered_csv = Join-Path $evidence 'workbuddy-correlation.filtered.csv'
    context_full_json = Join-Path $evidence 'workbuddy-context.full.json'
    context_filtered_json = Join-Path $evidence 'workbuddy-context.filtered.json'
    context_full_csv = Join-Path $evidence 'workbuddy-context.full.csv'
    context_filtered_csv = Join-Path $evidence 'workbuddy-context.filtered.csv'
    report_full_html = Join-Path $evidence 'workbuddy-observation-report.full.html'
    report_filtered_html = Join-Path $evidence 'workbuddy-observation-report.filtered.html'
    collector_stdout = Join-Path $evidence 'collector-stdout.json'
    collector_stderr = Join-Path $evidence 'collector-stderr.ndjson'
    ready = Join-Path $evidence 'capture-ready.json'
    stop_signal = Join-Path $evidence 'capture-stop.requested'
    correlator_stdout = Join-Path $evidence 'correlator-stdout.json'
    correlator_stderr = Join-Path $evidence 'correlator-stderr.ndjson'
    health_full_csv = Join-Path $evidence 'collector-health.full.csv'
    health_filtered_csv = Join-Path $evidence 'collector-health.filtered.csv'
    manifest = Join-Path $evidence 'manifest.csv'
}

$identity = [pscustomobject]@{
    executable_path = $workBuddy
    file_version = (Get-Item -LiteralPath $workBuddy).VersionInfo.FileVersion
    sha256 = $actualSha256.ToLowerInvariant()
    signer_subject = $signature.SignerCertificate.Subject
}
[IO.File]::WriteAllText($paths.identity, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$credentialCatalog = @(
    [pscustomobject]@{
        label = $CredentialLabel
        kind = $CredentialKind
        value = $token
        fingerprint = "sha256:$((Get-FileHash -LiteralPath $tokenPath -Algorithm SHA256).Hash.ToLowerInvariant())"
    }
)
[IO.File]::WriteAllText($paths.credential_catalog, (ConvertTo-Json -InputObject $credentialCatalog), [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($paths.semantic_ndjson, '', [Text.UTF8Encoding]::new($false))
$workBuddyHome = Split-Path -Parent $projects
$contextSummary = & $contextExporterPath `
    -WorkBuddyHome $workBuddyHome `
    -ProjectRoot $projects `
    -FullOutputPath $paths.context_full_json `
    -FilteredOutputPath $paths.context_filtered_json `
    -FullCsvPath $paths.context_full_csv `
    -FilteredCsvPath $paths.context_filtered_csv |
    ConvertFrom-Json

$startedAt = [DateTimeOffset]::UtcNow
$startedAtUnixMs = $startedAt.ToUnixTimeMilliseconds()
$deadlineAt = $startedAt.AddSeconds($DurationSeconds)
[long]$endedAtUnixMs = 0
[DateTimeOffset]$endedAt = [DateTimeOffset]::MinValue
$collectorInfo = [Diagnostics.ProcessStartInfo]::new()
$collectorInfo.FileName = $collectorPath
$collectorInfo.Arguments = "--duration-seconds $DurationSeconds --full-output `"$($paths.os_full_ndjson)`" --filtered-output `"$($paths.os_filtered_ndjson)`" --identity `"$($paths.identity)`" --stop-signal `"$($paths.stop_signal)`""
$collectorInfo.UseShellExecute = $false
$collectorInfo.CreateNoWindow = $true
$collectorInfo.RedirectStandardOutput = $true
$collectorInfo.RedirectStandardError = $true
$collector = [Diagnostics.Process]::Start($collectorInfo)

try {
    Start-Sleep -Seconds 2
    if ($collector.HasExited) {
        $collector.WaitForExit()
        $stderr = $collector.StandardError.ReadToEnd()
        throw "ETW Collector 提前退出 exit_code=$($collector.ExitCode) stderr=$stderr"
    }
    $ready = [pscustomobject]@{
        status = 'ready'
        started_at = $startedAt.ToString('o')
        deadline_at = $deadlineAt.ToString('o')
        duration_seconds = $DurationSeconds
        workbuddy_root_process_id = $rootProcessId
        evidence_root = $evidence
        stop_signal_path = $paths.stop_signal
    }
    [IO.File]::WriteAllText($paths.ready, ($ready | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $ready | ConvertTo-Json -Compress | Write-Output

    $collector.WaitForExit()
    $endedAt = [DateTimeOffset]::UtcNow
    $endedAtUnixMs = $endedAt.ToUnixTimeMilliseconds()
}
finally {
    if (-not $collector.HasExited) {
        Stop-Process -Id $collector.Id -Force
        $traceName = "AgentReins-Gate0-$($collector.Id)"
        $stopOutput = & logman.exe stop $traceName -ets 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "清理 ETW 会话失败 trace_name=$traceName output=$stopOutput"
        }
    }
}

$collectorStdout = $collector.StandardOutput.ReadToEnd()
$collectorStderr = $collector.StandardError.ReadToEnd()
[IO.File]::WriteAllText($paths.collector_stdout, $collectorStdout, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($paths.collector_stderr, $collectorStderr, [Text.UTF8Encoding]::new($false))
if ($collector.ExitCode -ne 0) {
    throw "ETW Collector 失败 exit_code=$($collector.ExitCode) stderr=$collectorStderr"
}
$collectorSummary = $collectorStdout | ConvertFrom-Json
$projectFiles = @(Get-ChildItem -LiteralPath $projects -Recurse -File -Filter '*.jsonl' | Where-Object {
    $_.LastWriteTimeUtc -ge $startedAt.UtcDateTime
})
if ($projectFiles.Count -eq 0) {
    throw "观测时间窗内没有更新的 WorkBuddy 项目会话 project_root=$projects started_at=$($startedAt.ToString('o'))"
}

$semanticEventCount = 0
for ($index = 0; $index -lt $projectFiles.Count; $index += 1) {
    $temporaryOutput = Join-Path $evidence "semantic-$index.ndjson"
    $semanticOutput = & $semanticCollectorPath `
        --input $projectFiles[$index].FullName `
        --since-unix-ms $startedAtUnixMs `
        --until-unix-ms $endedAtUnixMs `
        --session-id $collectorSummary.session_id `
        --process-id $rootProcessId `
        --output $temporaryOutput
    if ($LASTEXITCODE -ne 0) {
        throw "WorkBuddy 语义导入失败 path=$($projectFiles[$index].FullName) output=$semanticOutput exit_code=$LASTEXITCODE"
    }
    $semanticSummary = $semanticOutput | ConvertFrom-Json
    $semanticEventCount += $semanticSummary.semantic_events_written
    [IO.File]::AppendAllText(
        $paths.semantic_ndjson,
        [IO.File]::ReadAllText($temporaryOutput),
        [Text.UTF8Encoding]::new($false)
    )
    Remove-Item -LiteralPath $temporaryOutput
}

$correlatorInfo = [Diagnostics.ProcessStartInfo]::new()
$correlatorInfo.FileName = $correlatorPath
$correlatorInfo.Arguments = "--semantic-input `"$($paths.semantic_ndjson)`" --os-input `"$($paths.os_filtered_ndjson)`" --credential-catalog `"$($paths.credential_catalog)`" --full-output `"$($paths.correlation_full_ndjson)`" --filtered-output `"$($paths.correlation_filtered_ndjson)`" --window-ms $CorrelationWindowMs"
$correlatorInfo.UseShellExecute = $false
$correlatorInfo.CreateNoWindow = $true
$correlatorInfo.RedirectStandardOutput = $true
$correlatorInfo.RedirectStandardError = $true
$correlator = [Diagnostics.Process]::Start($correlatorInfo)
$correlator.WaitForExit()
$correlatorStdout = $correlator.StandardOutput.ReadToEnd()
$correlatorStderr = $correlator.StandardError.ReadToEnd()
[IO.File]::WriteAllText($paths.correlator_stdout, $correlatorStdout, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($paths.correlator_stderr, $correlatorStderr, [Text.UTF8Encoding]::new($false))
if ($correlator.ExitCode -ne 0) {
    throw "证据关联器失败 exit_code=$($correlator.ExitCode) stderr=$correlatorStderr"
}

$processSummary = & $processExporterPath `
    -FullInputPath $paths.os_full_ndjson `
    -FilteredInputPath $paths.os_filtered_ndjson `
    -FullCsvPath $paths.os_full_csv `
    -FilteredCsvPath $paths.os_filtered_csv |
    ConvertFrom-Json
$correlationSummary = & $correlationExporterPath `
    -FullInputPath $paths.correlation_full_ndjson `
    -FilteredInputPath $paths.correlation_filtered_ndjson `
    -FullCsvPath $paths.correlation_full_csv `
    -FilteredCsvPath $paths.correlation_filtered_csv |
    ConvertFrom-Json
$correlatorSummary = $correlatorStdout | ConvertFrom-Json

$fullReportSummary = & $reportGeneratorPath `
    -OsInputPath $paths.os_full_ndjson `
    -CorrelationInputPath $paths.correlation_full_ndjson `
    -ContextSnapshotPath $paths.context_full_json `
    -CollectorSummaryPath $paths.collector_stdout `
    -RunId (Split-Path -Leaf $evidence) `
    -OutputPath $paths.report_full_html |
    ConvertFrom-Json
$filteredReportSummary = & $reportGeneratorPath `
    -OsInputPath $paths.os_filtered_ndjson `
    -CorrelationInputPath $paths.correlation_filtered_ndjson `
    -ContextSnapshotPath $paths.context_filtered_json `
    -CollectorSummaryPath $paths.collector_stdout `
    -RunId (Split-Path -Leaf $evidence) `
    -OutputPath $paths.report_filtered_html |
    ConvertFrom-Json

$healthFull = [pscustomobject]@{
    captured_at = [DateTimeOffset]::UtcNow.ToString('o')
    session_id = $collectorSummary.session_id
    provider_events_received = $collectorSummary.provider_events_received
    parsed_events = $collectorSummary.parsed_events
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
    events_lost = $collectorSummary.events_lost
    log_buffers_lost = $collectorSummary.log_buffers_lost
    realtime_buffers_lost = $collectorSummary.realtime_buffers_lost
    stop_reason = $collectorSummary.stop_reason
    coverage_status = $collectorSummary.coverage_status
}
$healthFull | Export-Csv -LiteralPath $paths.health_full_csv -NoTypeInformation -Encoding UTF8
$healthFull |
    Select-Object captured_at,session_id,provider_events_received,parsed_events,full_events_written,filtered_events_written,parse_failures,write_failures,output_queue_capacity,peak_output_queue_depth,output_source_events,output_batches_generated,source_events_collapsed,output_batches_dropped,retained_detail_source_events,retained_summary_source_events,retained_review_source_events,discarded_source_events,output_queue_wait_count,output_queue_wait_ms,peak_aggregation_keys,events_lost,log_buffers_lost,realtime_buffers_lost,stop_reason,coverage_status |
    Export-Csv -LiteralPath $paths.health_filtered_csv -NoTypeInformation -Encoding UTF8

$capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
$manifestRows = @(
    [pscustomobject]@{dataset='workbuddy-os';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.os_full_csv);filtered_csv=(Split-Path -Leaf $paths.os_filtered_csv);full_rows=$processSummary.full_rows;filtered_rows=$processSummary.filtered_rows;source_host=$env:COMPUTERNAME;filter_rule='完整与过滤版均只保留可分析证据；高频背景活动按行为窗口聚合，过滤版移除命令行'},
    [pscustomobject]@{dataset='workbuddy-correlation';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.correlation_full_csv);filtered_csv=(Split-Path -Leaf $paths.correlation_filtered_csv);full_rows=$correlationSummary.full_rows;filtered_rows=$correlationSummary.filtered_rows;source_host=$env:COMPUTERNAME;filter_rule='过滤版保留凭据身份与指纹并移除凭据正文'},
    [pscustomobject]@{dataset='workbuddy-context';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.context_full_csv);filtered_csv=(Split-Path -Leaf $paths.context_filtered_csv);full_rows=$contextSummary.artifact_count;filtered_rows=$contextSummary.artifact_count;source_host=$env:COMPUTERNAME;filter_rule='过滤版保留配置身份与哈希并移除配置正文'},
    [pscustomobject]@{dataset='workbuddy-observation-report';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.report_full_html);filtered_csv=(Split-Path -Leaf $paths.report_filtered_html);full_rows=$fullReportSummary.user_activity_count;filtered_rows=$filteredReportSummary.user_activity_count;source_host=$env:COMPUTERNAME;filter_rule='过滤版使用过滤后的语义、上下文和 OS 证据'},
    [pscustomobject]@{dataset='collector-health';captured_at=$capturedAt;full_csv=(Split-Path -Leaf $paths.health_full_csv);filtered_csv=(Split-Path -Leaf $paths.health_filtered_csv);full_rows=1;filtered_rows=1;source_host=$env:COMPUTERNAME;filter_rule='过滤版移除本地输出路径'}
)
$manifestRows | Export-Csv -LiteralPath $paths.manifest -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    status = 'completed'
    evidence_root = $evidence
    session_id = $collectorSummary.session_id
    started_at = $startedAt.ToString('o')
    ended_at = $endedAt.ToString('o')
    deadline_at = $deadlineAt.ToString('o')
    stop_reason = $collectorSummary.stop_reason
    project_files = @($projectFiles.FullName)
    semantic_event_count = $semanticEventCount
    os_full_event_count = $processSummary.full_rows
    os_filtered_event_count = $processSummary.filtered_rows
    correlated_event_count = $correlatorSummary.correlated_events
    credential_finding_count = $correlatorSummary.credential_findings
    user_activity_count = $fullReportSummary.user_activity_count
    context_artifact_count = $contextSummary.artifact_count
    full_report_path = $paths.report_full_html
    filtered_report_path = $paths.report_filtered_html
    coverage_status = $collectorSummary.coverage_status
} | ConvertTo-Json -Depth 3
