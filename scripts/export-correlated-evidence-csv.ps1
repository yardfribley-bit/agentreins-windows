param(
    [Parameter(Mandatory = $true)]
    [string]$FullInputPath,
    [Parameter(Mandatory = $true)]
    [string]$FilteredInputPath,
    [Parameter(Mandatory = $true)]
    [string]$FullCsvPath,
    [Parameter(Mandatory = $true)]
    [string]$FilteredCsvPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Convert-CorrelatedEvent {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Event,
        [Parameter(Mandatory = $true)]
        [bool]$IncludeSecretValue
    )

    $findings = @($Event.credential_findings)
    [pscustomobject]@{
        schema_version = $Event.schema_version
        event_id = $Event.event_id
        event_timestamp_unix_ms = $Event.event_timestamp_unix_ms
        session_id = $Event.session_id
        process_id = $Event.process_id
        user_activity_id = $Event.user_activity_id
        agent_session_id = $Event.agent_session_id
        turn_id = $Event.turn_id
        tool_call_id = $Event.tool_call_id
        workspace_path = $Event.workspace_path
        agent_id = $Event.agent_id
        provider_message_id = $Event.provider_message_id
        source_schema_profile = $Event.source_schema_profile
        observation_layer = $Event.observation_layer
        observation_status = $Event.observation_status
        content_kind = $Event.content_kind
        action_kind = $Event.action_kind
        content = $Event.content
        tool_name = $Event.tool_name
        operation_id = $Event.operation_id
        operation_id_origin = $Event.operation_id_origin
        expected_os_action = $Event.expected_os_action
        evidence_source = $Event.evidence_source
        source_record_id = $Event.source_record_id
        parent_record_id = $Event.parent_record_id
        trace_id = $Event.trace_id
        model_id = $Event.model_id
        request_model_id = $Event.request_model_id
        request_model_name = $Event.request_model_name
        conversation_request_id = $Event.conversation_request_id
        record_status = $Event.record_status
        error = $Event.error
        input_tokens = $Event.input_tokens
        output_tokens = $Event.output_tokens
        total_tokens = $Event.total_tokens
        cached_tokens = $Event.cached_tokens
        reasoning_tokens = $Event.reasoning_tokens
        request_count = $Event.request_count
        correlated_os_event_id = $Event.correlated_os_event_id
        correlation_distance_ms = $Event.correlation_distance_ms
        correlation_basis = $Event.correlation_basis
        correlation_time_basis = $Event.correlation_time_basis
        correlation_os_session_id = $Event.correlation_os_session_id
        correlation_confirmed = $Event.correlation_confirmed
        correlation_status = $Event.correlation_status
        linked_os_event_count = $Event.linked_os_event_count
        linked_os_event_ids = (@($Event.linked_os_event_ids) -join ";")
        linked_os_event_ids_truncated = $Event.linked_os_event_ids_truncated
        first_breakpoint = $Event.first_breakpoint
        causal_edges = (@($Event.causal_edges) | ConvertTo-Json -Compress -Depth 8)
        uncorrelated_reason = $Event.uncorrelated_reason
        credential_count = $findings.Count
        credential_labels = (@($findings | ForEach-Object { $_.label }) -join ";")
        credential_kinds = (@($findings | ForEach-Object { $_.kind }) -join ";")
        credential_fingerprints = (@($findings | ForEach-Object { $_.fingerprint }) -join ";")
        credential_values = if ($IncludeSecretValue) { @($findings | ForEach-Object { $_.secret_value }) -join ";" } else { $null }
    }
}

foreach ($inputPath in @($FullInputPath, $FilteredInputPath)) {
    if (-not (Test-Path -LiteralPath $inputPath)) {
        throw "未找到关联 NDJSON 输入文件 path=$inputPath"
    }
}

$fullRows = @(
    Get-Content -LiteralPath $FullInputPath -Encoding UTF8 |
        Where-Object { $_.Length -gt 0 } |
        ForEach-Object { Convert-CorrelatedEvent -Event ($_ | ConvertFrom-Json) -IncludeSecretValue $true }
)
$filteredRows = @(
    Get-Content -LiteralPath $FilteredInputPath -Encoding UTF8 |
        Where-Object { $_.Length -gt 0 } |
        ForEach-Object { Convert-CorrelatedEvent -Event ($_ | ConvertFrom-Json) -IncludeSecretValue $false }
)
$fullRows | Export-Csv -LiteralPath $FullCsvPath -NoTypeInformation -Encoding UTF8
$filteredRows | Export-Csv -LiteralPath $FilteredCsvPath -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    full_rows = $fullRows.Count
    filtered_rows = $filteredRows.Count
    full_csv_path = $FullCsvPath
    filtered_csv_path = $FilteredCsvPath
} | ConvertTo-Json -Compress
