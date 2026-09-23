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

foreach ($inputPath in @($FullInputPath, $FilteredInputPath)) {
    if (-not (Test-Path -LiteralPath $inputPath)) {
        throw "未找到 NDJSON 输入文件 path=$inputPath"
    }
}

# 保持证据发布顺序并流式导出；分析端按时间字段或 source_sequence 排序。
$fullRowCount = 0
Get-Content -LiteralPath $FullInputPath -Encoding UTF8 |
    Where-Object { $_.Length -gt 0 } |
    ForEach-Object {
        $event = $_ | ConvertFrom-Json
        $occurrencesProperty = if ($null -eq $event.aggregation) { $null } else { $event.aggregation.PSObject.Properties['occurrences'] }
        $usesAggregateSummary = $null -eq $occurrencesProperty -or $null -eq $occurrencesProperty.Value
        $occurrences = if ($usesAggregateSummary) {
            @([pscustomobject]@{
                event_id = $event.event_id
                event_timestamp_unix_ms = $event.event_timestamp_unix_ms
                observed_at_unix_ms = $event.observed_at_unix_ms
                operation_id = $event.operation_id
                bytes_transferred = $event.result.bytes_transferred
            })
        }
        else {
            @($occurrencesProperty.Value)
        }
        foreach ($occurrence in $occurrences) {
            $fullRowCount += 1
            [pscustomobject]@{
                schema_version = $event.schema_version
                event_id = $occurrence.event_id
                source_sequence = [long](($occurrence.event_id -split ':')[-1])
                event_timestamp_unix_ms = $occurrence.event_timestamp_unix_ms
                observed_at_unix_ms = $occurrence.observed_at_unix_ms
                phase = $event.phase
                operation_id = $occurrence.operation_id
                session_id = $event.session.id
                pid = $event.process.pid
                parent_pid = $event.process.parent_pid
                unique_process_key = $event.process.unique_process_key
                process_instance_id = $event.process.process_instance_id
                process_session_id = $event.process.process_session_id
                user_sid = $event.process.user_sid
                image_name = $event.process.image_name
                image_path = $event.process.image_path
                command_line_observed = $event.process.command_line_observed
                command_line = $event.process.command_line
                exit_status = $event.process.exit_status
                action_kind = $event.action.kind
                resource_kind = $event.resource.kind
                resource_identifier = $event.resource.identifier
                destination_kind = if ($null -eq $event.destination) { $null } else { $event.destination.kind }
                destination_identifier = if ($null -eq $event.destination) { $null } else { $event.destination.identifier }
                result_status_code = $event.result.status_code
                result_success = $event.result.success
                bytes_transferred = $occurrence.bytes_transferred
                evidence_source = $event.evidence.source
                provider_id = $event.evidence.provider_id
                provider_event_id = $event.evidence.provider_event_id
                provider_opcode = $event.evidence.provider_opcode
                provider_event_version = $event.evidence.provider_event_version
                retention_class = if ($null -eq $event.retention) { $null } else { $event.retention.class }
                retention_reason = if ($null -eq $event.retention) { $null } else { $event.retention.reason }
                resource_category = if ($null -eq $event.retention) { $null } else { $event.retention.resource_category }
                occurrence_count = if ($usesAggregateSummary -and $null -ne $event.aggregation) { $event.aggregation.occurrence_count } else { 1 }
                first_event_timestamp_unix_ms = if ($usesAggregateSummary -and $null -ne $event.aggregation) { $event.aggregation.first_event_timestamp_unix_ms } else { $occurrence.event_timestamp_unix_ms }
                last_event_timestamp_unix_ms = if ($usesAggregateSummary -and $null -ne $event.aggregation) { $event.aggregation.last_event_timestamp_unix_ms } else { $occurrence.event_timestamp_unix_ms }
                total_bytes_transferred = if ($usesAggregateSummary -and $null -ne $event.aggregation) { $event.aggregation.total_bytes_transferred } else { $occurrence.bytes_transferred }
                aggregation_batch_event_id = $event.event_id
                aggregation_batch_occurrence_count = if ($null -eq $event.aggregation) { 1 } else { $event.aggregation.occurrence_count }
            }
        }
    } |
    Export-Csv -LiteralPath $FullCsvPath -NoTypeInformation -Encoding UTF8

$filteredRowCount = 0
$startCount = 0
$stopCount = 0
$candidateCount = 0
$verifiedCount = 0
$verifiedStartCount = 0
$verifiedStopCount = 0
$inheritedCount = 0
$fileActionCount = 0
$networkActionCount = 0
$invalidTimestampCount = 0
$imageNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
Get-Content -LiteralPath $FilteredInputPath -Encoding UTF8 |
    Where-Object { $_.Length -gt 0 } |
    ForEach-Object {
        $event = $_ | ConvertFrom-Json
        $filteredRowCount += 1
        if ($event.action.kind -eq "process_start") { $startCount += 1 }
        if ($event.action.kind -eq "process_stop") { $stopCount += 1 }
        if ($event.actor.identity_status -eq "candidate") { $candidateCount += 1 }
        if ($event.actor.identity_status -eq "verified") { $verifiedCount += 1 }
        if ($event.actor.identity_status -eq "inherited") { $inheritedCount += 1 }
        if ($event.actor.identity_status -eq "verified" -and $event.action.kind -eq "process_start") { $verifiedStartCount += 1 }
        if ($event.actor.identity_status -eq "verified" -and $event.action.kind -eq "process_stop") { $verifiedStopCount += 1 }
        if ($event.resource.kind -eq "file") { $fileActionCount += 1 }
        if ($event.resource.kind -eq "network") { $networkActionCount += 1 }
        if ($event.event_timestamp_unix_ms -gt $event.observed_at_unix_ms) { $invalidTimestampCount += 1 }
        $imageNames.Add([string]$event.process.image_name) | Out-Null
            [pscustomobject]@{
                schema_version = $event.schema_version
                event_id = $event.event_id
                source_sequence = [long](($event.event_id -split ':')[-1])
                event_timestamp_unix_ms = $event.event_timestamp_unix_ms
                observed_at_unix_ms = $event.observed_at_unix_ms
                phase = $event.phase
                operation_id = $event.operation_id
                session_id = $event.session.id
                agent_kind = $event.actor.agent_kind
                identity_status = $event.actor.identity_status
                root_process_instance_id = $event.actor.root_process_instance_id
                agent_executable_path = $event.actor.identity.executable_path
                agent_file_version = $event.actor.identity.file_version
                agent_sha256 = $event.actor.identity.sha256
                agent_signer_subject = $event.actor.identity.signer_subject
                confidence = $event.confidence
                pid = $event.process.pid
                parent_pid = $event.process.parent_pid
                unique_process_key = $event.process.unique_process_key
                process_instance_id = $event.process.process_instance_id
                process_session_id = $event.process.process_session_id
                user_sid = $event.process.user_sid
                image_name = $event.process.image_name
                image_path = $event.process.image_path
                command_line_observed = $event.process.command_line_observed
                exit_status = $event.process.exit_status
                action_kind = $event.action.kind
                resource_kind = $event.resource.kind
                resource_identifier = $event.resource.identifier
                destination_kind = if ($null -eq $event.destination) { $null } else { $event.destination.kind }
                destination_identifier = if ($null -eq $event.destination) { $null } else { $event.destination.identifier }
                result_status_code = $event.result.status_code
                result_success = $event.result.success
                bytes_transferred = $event.result.bytes_transferred
                observation_mode = $event.verdict.mode
                outcome = $event.verdict.outcome
                evidence_source = $event.evidence.source
                provider_id = $event.evidence.provider_id
                provider_event_id = $event.evidence.provider_event_id
                provider_opcode = $event.evidence.provider_opcode
                provider_event_version = $event.evidence.provider_event_version
                retention_class = if ($null -eq $event.retention) { $null } else { $event.retention.class }
                retention_reason = if ($null -eq $event.retention) { $null } else { $event.retention.reason }
                resource_category = if ($null -eq $event.retention) { $null } else { $event.retention.resource_category }
                occurrence_count = if ($null -eq $event.aggregation) { 1 } else { $event.aggregation.occurrence_count }
                first_event_timestamp_unix_ms = if ($null -eq $event.aggregation) { $event.event_timestamp_unix_ms } else { $event.aggregation.first_event_timestamp_unix_ms }
                last_event_timestamp_unix_ms = if ($null -eq $event.aggregation) { $event.event_timestamp_unix_ms } else { $event.aggregation.last_event_timestamp_unix_ms }
                total_bytes_transferred = if ($null -eq $event.aggregation) { $event.result.bytes_transferred } else { $event.aggregation.total_bytes_transferred }
            }
    } |
    Export-Csv -LiteralPath $FilteredCsvPath -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    full_rows = $fullRowCount
    filtered_rows = $filteredRowCount
    full_csv_path = $FullCsvPath
    filtered_csv_path = $FilteredCsvPath
    start_count = $startCount
    stop_count = $stopCount
    candidate_count = $candidateCount
    verified_count = $verifiedCount
    verified_start_count = $verifiedStartCount
    verified_stop_count = $verifiedStopCount
    inherited_count = $inheritedCount
    file_action_count = $fileActionCount
    network_action_count = $networkActionCount
    invalid_timestamp_count = $invalidTimestampCount
    image_names = @($imageNames | Sort-Object)
} | ConvertTo-Json -Compress
