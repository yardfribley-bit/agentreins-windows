param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$exporterPath = Join-Path $SourceRoot "scripts\export-process-evidence-csv.ps1"
if (-not (Test-Path -LiteralPath $exporterPath)) {
    throw "未找到进程证据导出脚本 path=$exporterPath"
}
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null

$fixtureProcessPath = Join-Path $EvidenceRoot "fixture-process.ndjson"
$workBuddyProcessPath = Join-Path $EvidenceRoot "workbuddy-real-process.ndjson"
& $exporterPath `
    -FullInputPath $fixtureProcessPath `
    -FilteredInputPath $fixtureProcessPath `
    -FullCsvPath (Join-Path $OutputRoot "fixture-process.full.csv") `
    -FilteredCsvPath (Join-Path $OutputRoot "fixture-process.filtered.csv") | Out-Null
& $exporterPath `
    -FullInputPath $workBuddyProcessPath `
    -FilteredInputPath $workBuddyProcessPath `
    -FullCsvPath (Join-Path $OutputRoot "workbuddy-real-process.full.csv") `
    -FilteredCsvPath (Join-Path $OutputRoot "workbuddy-real-process.filtered.csv") | Out-Null

$fixtureHealth = Get-Content -LiteralPath (Join-Path $EvidenceRoot "collector-stdout.json") -Raw |
    ConvertFrom-Json
$workBuddyHealth = Get-Content -LiteralPath (Join-Path $EvidenceRoot "workbuddy-real-collector-stdout.json") -Raw |
    ConvertFrom-Json
$healthRows = @(
    [pscustomobject]@{
        dataset = "fixture-process"
        session_id = $fixtureHealth.session_id
        provider_events_received = $fixtureHealth.provider_events_received
        persisted_events = $fixtureHealth.events_written
        lost_events_available = $fixtureHealth.lost_events_available
        output_path = $fixtureHealth.output_path
        stderr_bytes = (Get-Item -LiteralPath (Join-Path $EvidenceRoot "collector-stderr.ndjson")).Length
    },
    [pscustomobject]@{
        dataset = "workbuddy-real-process"
        session_id = $workBuddyHealth.session_id
        provider_events_received = $workBuddyHealth.provider_events_received
        persisted_events = $workBuddyHealth.events_written
        lost_events_available = $workBuddyHealth.lost_events_available
        output_path = $workBuddyHealth.output_path
        stderr_bytes = (Get-Item -LiteralPath (Join-Path $EvidenceRoot "workbuddy-real-collector-stderr.ndjson")).Length
    }
)
$healthRows |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "collector-health.full.csv") -NoTypeInformation -Encoding UTF8
$healthRows |
    Select-Object dataset,session_id,provider_events_received,persisted_events,lost_events_available,stderr_bytes |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "collector-health.filtered.csv") -NoTypeInformation -Encoding UTF8

$baseline = Get-Content -LiteralPath (Join-Path $EvidenceRoot "workbuddy-real-baseline.json") -Raw |
    ConvertFrom-Json
$baselineFull = [pscustomobject]@{
    captured_at = $baseline.captured_at
    workbuddy_version = $baseline.workbuddy_version
    workbuddy_sha256 = $baseline.workbuddy_sha256
    signer_subject = $baseline.signer_subject
    interactive_user = $baseline.interactive_user
    interactive_session_id = $baseline.interactive_session_id
    launched_process_ids = $baseline.launched_process_ids -join ";"
    exit_method = $baseline.exit_method
    sample_seconds = $baseline.resource_sample.sample_seconds
    process_count = $baseline.resource_sample.process_count
    working_set_mb = $baseline.resource_sample.working_set_mb
    private_memory_mb = $baseline.resource_sample.private_memory_mb
    cpu_percent_normalized = $baseline.resource_sample.cpu_percent_normalized
    established_connections = $baseline.resource_sample.established_connections
    loopback_listeners = $baseline.resource_sample.loopback_listeners
    event_count = $baseline.event_count
    start_count = $baseline.start_count
    stop_count = $baseline.stop_count
    candidate_count = $baseline.candidate_count
    inherited_count = $baseline.inherited_count
    image_names = $baseline.image_names -join ";"
    stderr_bytes = $baseline.stderr_bytes
    remaining_processes = $baseline.remaining_processes
    remaining_trace_sessions = $baseline.remaining_trace_sessions
    remaining_scheduled_tasks = $baseline.remaining_scheduled_tasks
}
$baselineFull |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-real-baseline.full.csv") -NoTypeInformation -Encoding UTF8
$baselineFull |
    Select-Object captured_at,workbuddy_version,signer_subject,exit_method,sample_seconds,process_count,working_set_mb,private_memory_mb,cpu_percent_normalized,established_connections,loopback_listeners,event_count,start_count,stop_count,candidate_count,inherited_count,stderr_bytes,remaining_processes,remaining_trace_sessions,remaining_scheduled_tasks |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-real-baseline.filtered.csv") -NoTypeInformation -Encoding UTF8

$idle = Get-Content -LiteralPath (Join-Path $EvidenceRoot "workbuddy-configured-idle.json") -Raw |
    ConvertFrom-Json
$idle |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-configured-idle.full.csv") -NoTypeInformation -Encoding UTF8
$idle |
    Select-Object captured_at,sample_seconds,process_count,working_set_mb,private_memory_mb,cpu_percent_normalized,handle_count,thread_count,established_connections,listen_connections,loopback_listeners |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-configured-idle.filtered.csv") -NoTypeInformation -Encoding UTF8

$close = Get-Content -LiteralPath (Join-Path $EvidenceRoot "workbuddy-pre-baseline-close.json") -Raw |
    ConvertFrom-Json
$close |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-pre-baseline-close.full.csv") -NoTypeInformation -Encoding UTF8
$close |
    Select-Object captured_at,initial_process_count,close_requests_accepted,forced_stop_used,remaining_process_count |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "workbuddy-pre-baseline-close.filtered.csv") -NoTypeInformation -Encoding UTF8

$manifestRows = @(
    [pscustomobject]@{
        dataset = "fixture-process"
        captured_at = "2026-09-11"
        full_csv = "fixture-process.full.csv"
        filtered_csv = "fixture-process.filtered.csv"
        full_rows = 4
        filtered_rows = 4
        prefilter_full_available = $false
        unrecoverable_prefilter_events = 203
        source_host = "DESKTOP-TGVDKR8"
        note = "历史采集只持久化 Agent 归因事件"
    },
    [pscustomobject]@{
        dataset = "workbuddy-real-process"
        captured_at = $baseline.captured_at
        full_csv = "workbuddy-real-process.full.csv"
        filtered_csv = "workbuddy-real-process.filtered.csv"
        full_rows = 110
        filtered_rows = 110
        prefilter_full_available = $false
        unrecoverable_prefilter_events = 350
        source_host = "DESKTOP-TGVDKR8"
        note = "历史采集只持久化 Agent 归因事件"
    },
    [pscustomobject]@{
        dataset = "collector-health"
        captured_at = "2026-09-11"
        full_csv = "collector-health.full.csv"
        filtered_csv = "collector-health.filtered.csv"
        full_rows = 2
        filtered_rows = 2
        prefilter_full_available = "not_applicable"
        unrecoverable_prefilter_events = 0
        source_host = "DESKTOP-TGVDKR8"
        note = "采集器会话摘要"
    },
    [pscustomobject]@{
        dataset = "workbuddy-real-baseline"
        captured_at = $baseline.captured_at
        full_csv = "workbuddy-real-baseline.full.csv"
        filtered_csv = "workbuddy-real-baseline.filtered.csv"
        full_rows = 1
        filtered_rows = 1
        prefilter_full_available = "not_applicable"
        unrecoverable_prefilter_events = 0
        source_host = "DESKTOP-TGVDKR8"
        note = "启动阶段资源基线"
    },
    [pscustomobject]@{
        dataset = "workbuddy-configured-idle"
        captured_at = $idle.captured_at
        full_csv = "workbuddy-configured-idle.full.csv"
        filtered_csv = "workbuddy-configured-idle.filtered.csv"
        full_rows = 1
        filtered_rows = 1
        prefilter_full_available = "not_applicable"
        unrecoverable_prefilter_events = 0
        source_host = "DESKTOP-TGVDKR8"
        note = "配置后空闲资源样本"
    },
    [pscustomobject]@{
        dataset = "workbuddy-pre-baseline-close"
        captured_at = $close.captured_at
        full_csv = "workbuddy-pre-baseline-close.full.csv"
        filtered_csv = "workbuddy-pre-baseline-close.filtered.csv"
        full_rows = 1
        filtered_rows = 1
        prefilter_full_available = "not_applicable"
        unrecoverable_prefilter_events = 0
        source_host = "DESKTOP-TGVDKR8"
        note = "基线前关闭状态"
    }
)
$manifestRows |
    Export-Csv -LiteralPath (Join-Path $OutputRoot "manifest.csv") -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    output_root = $OutputRoot
    csv_files = @(Get-ChildItem -LiteralPath $OutputRoot -Filter "*.csv" -File).Count
    manifest_rows = $manifestRows.Count
} | ConvertTo-Json -Compress
