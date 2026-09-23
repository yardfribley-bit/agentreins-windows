use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use evidence_correlator::{
    CorrelationStatus, McpProtocolIndex, McpProtocolRecord, OsEventIndex, SemanticEventIndex,
    correlate_native_evidence, correlate_semantic_evidence, correlation_session_id,
};
use native_contracts::{
    ActionKind, CausalEdge, EvidenceSource, NativeEvidence, ObservationEvent, ObservationLayer,
    ObservationStatus, OperationIdOrigin, SemanticContentKind, SemanticEvent,
};
use serde::{Deserialize, Serialize};

const MAX_CANDIDATE_EVIDENCE_IDS: usize = 20;

#[derive(Debug)]
struct Arguments {
    semantic_input: PathBuf,
    os_input: PathBuf,
    mcp_input: Option<PathBuf>,
    os_session_id: Option<String>,
    credential_catalog: PathBuf,
    full_output: PathBuf,
    filtered_output: PathBuf,
    window_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct CredentialEntry {
    label: String,
    kind: String,
    value: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
struct CredentialFinding {
    label: String,
    kind: String,
    fingerprint: String,
    secret_value: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct CorrelatedSemanticEvent {
    schema_version: String,
    event_id: String,
    event_timestamp_unix_ms: u64,
    session_id: String,
    process_id: u32,
    user_activity_id: String,
    agent_session_id: Option<String>,
    turn_id: Option<String>,
    tool_call_id: Option<String>,
    workspace_path: Option<String>,
    agent_id: Option<String>,
    provider_message_id: Option<String>,
    source_schema_profile: Option<String>,
    observation_layer: Option<ObservationLayer>,
    observation_status: ObservationStatus,
    content_kind: Option<SemanticContentKind>,
    action_kind: ActionKind,
    content: String,
    tool_name: Option<String>,
    operation_id: Option<String>,
    operation_id_origin: Option<OperationIdOrigin>,
    native_evidence: NativeEvidence,
    expected_os_action: Option<ActionKind>,
    evidence_source: EvidenceSource,
    source_record_id: String,
    parent_record_id: Option<String>,
    trace_id: Option<String>,
    model_id: Option<String>,
    request_model_id: Option<String>,
    request_model_name: Option<String>,
    conversation_request_id: Option<String>,
    record_status: Option<String>,
    error: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    request_count: Option<u64>,
    correlated_os_event_id: Option<String>,
    correlation_distance_ms: Option<u64>,
    correlation_basis: Option<String>,
    correlation_time_basis: Option<String>,
    correlation_os_session_id: Option<String>,
    correlation_confirmed: Option<bool>,
    correlation_status: CorrelationStatus,
    linked_os_event_count: usize,
    linked_os_event_ids: Vec<String>,
    linked_os_event_ids_truncated: bool,
    first_breakpoint: Option<String>,
    causal_edges: Vec<CausalEdge>,
    mcp_jsonrpc_request_id: Option<String>,
    mcp_process_id: Option<u32>,
    mcp_response_observed: bool,
    uncorrelated_reason: Option<String>,
    credential_findings: Vec<CredentialFinding>,
}

fn main() {
    if let Err(error) = run(std::env::args().collect()) {
        eprintln!(
            "{}",
            serde_json::json!({"level": "error", "message": error})
        );
        std::process::exit(1);
    }
}

fn run(raw_arguments: Vec<String>) -> Result<(), String> {
    let arguments = parse_arguments(&raw_arguments)?;
    let semantic_events = read_ndjson::<SemanticEvent>(&arguments.semantic_input)?;
    validate_semantic_sources(&semantic_events)?;
    let os_events = read_ndjson::<ObservationEvent>(&arguments.os_input)?;
    let os_event_index = OsEventIndex::new(&os_events);
    let mcp_protocol_records = arguments
        .mcp_input
        .as_deref()
        .map(read_ndjson::<McpProtocolRecord>)
        .transpose()?;
    let mcp_protocol_index = mcp_protocol_records
        .as_deref()
        .map(McpProtocolIndex::new)
        .transpose()?;
    let credentials = read_json::<Vec<CredentialEntry>>(&arguments.credential_catalog)?;
    let full_events = correlate_events(
        &semantic_events,
        &os_event_index,
        mcp_protocol_index.as_ref(),
        arguments.os_session_id.as_deref(),
        &credentials,
        arguments.window_ms,
        false,
    );
    let filtered_events = correlate_events(
        &semantic_events,
        &os_event_index,
        mcp_protocol_index.as_ref(),
        arguments.os_session_id.as_deref(),
        &credentials,
        arguments.window_ms,
        true,
    );
    write_ndjson(&arguments.full_output, &full_events)?;
    write_ndjson(&arguments.filtered_output, &filtered_events)?;
    println!(
        "{}",
        serde_json::json!({
            "semantic_events": semantic_events.len(),
            "os_events": os_events.len(),
            "mcp_protocol_records": mcp_protocol_records.as_ref().map(Vec::len).unwrap_or(0),
            "full_events_written": full_events.len(),
            "filtered_events_written": filtered_events.len(),
            "correlation_relevant_events": full_events.iter().filter(|event| event.expected_os_action.is_some() || event.mcp_jsonrpc_request_id.is_some()).count(),
            "confirmed_events": full_events.iter().filter(|event| (event.expected_os_action.is_some() || event.mcp_jsonrpc_request_id.is_some()) && event.correlation_status == CorrelationStatus::Confirmed).count(),
            "partial_events": full_events.iter().filter(|event| (event.expected_os_action.is_some() || event.mcp_jsonrpc_request_id.is_some()) && event.correlation_status == CorrelationStatus::Partial).count(),
            "unlinked_events": full_events.iter().filter(|event| (event.expected_os_action.is_some() || event.mcp_jsonrpc_request_id.is_some()) && event.correlation_status == CorrelationStatus::Unlinked).count(),
            "mcp_linked_events": full_events.iter().filter(|event| event.mcp_jsonrpc_request_id.is_some()).count(),
            "mcp_response_observed_events": full_events.iter().filter(|event| event.mcp_response_observed).count(),
            "credential_findings": full_events.iter().map(|event| event.credential_findings.len()).sum::<usize>(),
            "os_session_id_override": arguments.os_session_id,
        })
    );
    Ok(())
}

fn validate_semantic_sources(events: &[SemanticEvent]) -> Result<(), String> {
    if let Some(event) = events
        .iter()
        .find(|event| event.evidence_source == EvidenceSource::Etw)
    {
        return Err(format!(
            "语义输入不得声明为 ETW 证据 event_id={}",
            event.event_id
        ));
    }
    Ok(())
}

fn parse_arguments(raw_arguments: &[String]) -> Result<Arguments, String> {
    if raw_arguments.len() < 13
        || raw_arguments[1] != "--semantic-input"
        || raw_arguments[3] != "--os-input"
        || raw_arguments[5] != "--credential-catalog"
        || raw_arguments[7] != "--full-output"
        || raw_arguments[9] != "--filtered-output"
        || raw_arguments[11] != "--window-ms"
        || !(raw_arguments.len() - 13).is_multiple_of(2)
    {
        return Err(String::from(
            "参数错误，用法: evidence-correlator --semantic-input <path.ndjson> --os-input <path.ndjson> --credential-catalog <path.json> --full-output <path.ndjson> --filtered-output <path.ndjson> --window-ms <milliseconds> [--mcp-input <path.ndjson>] [--os-session-id <session-id>]",
        ));
    }
    let window_ms = raw_arguments[12].parse::<u64>().map_err(|error| {
        format!(
            "window-ms 必须是整数 value={} error={error}",
            raw_arguments[12]
        )
    })?;
    if !(1..=60_000).contains(&window_ms) {
        return Err(format!(
            "window-ms 超出允许范围 value={window_ms} expected=1..=60000"
        ));
    }
    let mut mcp_input = None;
    let mut os_session_id = None;
    for pair in raw_arguments[13..].as_chunks::<2>().0 {
        if pair[1].trim().is_empty() {
            return Err(format!("可选参数不得为空 name={}", pair[0]));
        }
        match pair[0].as_str() {
            "--mcp-input" if mcp_input.is_none() => {
                mcp_input = Some(PathBuf::from(&pair[1]));
            }
            "--os-session-id" if os_session_id.is_none() => {
                os_session_id = Some(pair[1].clone());
            }
            value => return Err(format!("未知或重复的可选参数 name={value}")),
        }
    }
    Ok(Arguments {
        semantic_input: PathBuf::from(&raw_arguments[2]),
        os_input: PathBuf::from(&raw_arguments[4]),
        mcp_input,
        os_session_id,
        credential_catalog: PathBuf::from(&raw_arguments[6]),
        full_output: PathBuf::from(&raw_arguments[8]),
        filtered_output: PathBuf::from(&raw_arguments[10]),
        window_ms,
    })
}

fn read_ndjson<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, String> {
    let file = File::open(path)
        .map_err(|error| format!("无法打开 NDJSON 输入 path={} error={error}", path.display()))?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(value) if value.trim().is_empty() => None,
            value => Some((index, value)),
        })
        .map(|(index, line)| {
            let content = line.map_err(|error| {
                format!(
                    "无法读取 NDJSON 输入 path={} line={} error={error}",
                    path.display(),
                    index + 1
                )
            })?;
            serde_json::from_str(&content).map_err(|error| {
                format!(
                    "无法解析 NDJSON 输入 path={} line={} error={error}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let file = File::open(path)
        .map_err(|error| format!("无法打开 JSON 输入 path={} error={error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("无法解析 JSON 输入 path={} error={error}", path.display()))
}

fn write_ndjson<T: Serialize>(path: &Path, values: &[T]) -> Result<(), String> {
    let file = File::create(path)
        .map_err(|error| format!("无法创建 NDJSON 输出 path={} error={error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for value in values {
        serde_json::to_writer(&mut writer, value).map_err(|error| {
            format!(
                "无法序列化 NDJSON 输出 path={} error={error}",
                path.display()
            )
        })?;
        writer.write_all(b"\n").map_err(|error| {
            format!("无法写入 NDJSON 输出 path={} error={error}", path.display())
        })?;
    }
    writer
        .flush()
        .map_err(|error| format!("无法刷新 NDJSON 输出 path={} error={error}", path.display()))
}

fn correlate_events(
    semantic_events: &[SemanticEvent],
    os_event_index: &OsEventIndex<'_>,
    mcp_protocol_index: Option<&McpProtocolIndex>,
    os_session_id: Option<&str>,
    credentials: &[CredentialEntry],
    window_ms: u64,
    redact_secrets: bool,
) -> Vec<CorrelatedSemanticEvent> {
    let semantic_event_index = SemanticEventIndex::new(semantic_events);
    semantic_events
        .iter()
        .map(|semantic_event| {
            let correlation = correlate_semantic_evidence(
                semantic_event,
                &semantic_event_index,
                os_event_index,
                mcp_protocol_index,
                os_session_id,
                window_ms,
            )
            .unwrap_or_else(|| {
                correlate_native_evidence(
                    semantic_event,
                    os_event_index,
                    mcp_protocol_index,
                    os_session_id,
                    window_ms,
                )
            });
            let findings = find_credentials(&semantic_event.content, credentials, redact_secrets);
            let content = if redact_secrets {
                redact_content(&semantic_event.content, credentials)
            } else {
                semantic_event.content.clone()
            };
            CorrelatedSemanticEvent {
                schema_version: semantic_event.schema_version.clone(),
                event_id: semantic_event.event_id.clone(),
                event_timestamp_unix_ms: semantic_event.event_timestamp_unix_ms,
                session_id: semantic_event.session_id.clone(),
                process_id: semantic_event.process_id,
                user_activity_id: semantic_event.user_activity_id.clone(),
                agent_session_id: semantic_event.agent_session_id.clone(),
                turn_id: semantic_event.turn_id.clone(),
                tool_call_id: semantic_event.tool_call_id.clone(),
                workspace_path: semantic_event.workspace_path.clone(),
                agent_id: semantic_event.agent_id.clone(),
                provider_message_id: semantic_event.provider_message_id.clone(),
                source_schema_profile: semantic_event.source_schema_profile.clone(),
                observation_layer: semantic_event.observation_layer,
                observation_status: semantic_event.observation_status,
                content_kind: semantic_event.content_kind,
                action_kind: semantic_event.action_kind,
                content,
                tool_name: semantic_event.tool_name.clone(),
                operation_id: semantic_event.operation_id.clone(),
                operation_id_origin: semantic_event.operation_id_origin,
                native_evidence: semantic_event.native_evidence.clone(),
                expected_os_action: semantic_event.expected_os_action,
                evidence_source: semantic_event.evidence_source,
                source_record_id: semantic_event.source_record_id.clone(),
                parent_record_id: semantic_event.parent_record_id.clone(),
                trace_id: semantic_event.trace_id.clone(),
                model_id: semantic_event.model_id.clone(),
                request_model_id: semantic_event.request_model_id.clone(),
                request_model_name: semantic_event.request_model_name.clone(),
                conversation_request_id: semantic_event.conversation_request_id.clone(),
                record_status: semantic_event.record_status.clone(),
                error: semantic_event.error.clone(),
                input_tokens: semantic_event.input_tokens,
                output_tokens: semantic_event.output_tokens,
                total_tokens: semantic_event.total_tokens,
                cached_tokens: semantic_event.cached_tokens,
                reasoning_tokens: semantic_event.reasoning_tokens,
                request_count: semantic_event.request_count,
                correlated_os_event_id: correlation.selected.map(|event| event.event_id.clone()),
                correlation_distance_ms: None,
                correlation_basis: correlation.basis.map(String::from),
                correlation_time_basis: correlation.time_basis.map(String::from),
                correlation_os_session_id: (semantic_event.expected_os_action.is_some()
                    || correlation.mcp_process_id.is_some())
                .then(|| correlation_session_id(semantic_event, os_session_id).to_owned()),
                correlation_confirmed: Some(correlation.status == CorrelationStatus::Confirmed),
                correlation_status: correlation.status,
                linked_os_event_count: correlation.linked_events.len(),
                linked_os_event_ids: correlation
                    .linked_events
                    .iter()
                    .take(MAX_CANDIDATE_EVIDENCE_IDS)
                    .map(|event| event.event_id.clone())
                    .collect(),
                linked_os_event_ids_truncated: correlation.linked_events.len()
                    > MAX_CANDIDATE_EVIDENCE_IDS,
                first_breakpoint: correlation.first_breakpoint.map(String::from),
                causal_edges: correlation.causal_edges,
                mcp_jsonrpc_request_id: correlation.mcp_jsonrpc_request_id,
                mcp_process_id: correlation.mcp_process_id,
                mcp_response_observed: correlation.mcp_response_observed,
                uncorrelated_reason: correlation.missing_reason.map(String::from),
                credential_findings: findings,
            }
        })
        .collect()
}

fn find_credentials(
    content: &str,
    credentials: &[CredentialEntry],
    redact_secrets: bool,
) -> Vec<CredentialFinding> {
    credentials
        .iter()
        .filter(|credential| !credential.value.is_empty() && content.contains(&credential.value))
        .map(|credential| CredentialFinding {
            label: credential.label.clone(),
            kind: credential.kind.clone(),
            fingerprint: credential.fingerprint.clone(),
            secret_value: (!redact_secrets).then(|| credential.value.clone()),
        })
        .collect()
}

fn redact_content(content: &str, credentials: &[CredentialEntry]) -> String {
    credentials
        .iter()
        .fold(String::from(content), |value, credential| {
            if credential.value.is_empty() {
                value
            } else {
                value.replace(
                    &credential.value,
                    &format!("<credential:{}>", credential.label),
                )
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{CredentialEntry, find_credentials, redact_content};
    use evidence_correlator::{
        CorrelationStatus, McpProtocolDirection, McpProtocolIndex, McpProtocolRecord, OsEventIndex,
        SemanticEventIndex, correlate_native_evidence, correlate_os_events,
        correlate_semantic_evidence,
    };
    use native_contracts::{
        Action, ActionKind, Actor, AgentIdentity, AgentKind, CausalEdgeKind, Confidence,
        Destination, EVENT_SCHEMA_VERSION, EventPhase, Evidence, EvidenceSource, IdentityStatus,
        NativeEvidence, NativeIdKind, NativeIdSource, NativeIdentifier, ObservationEvent,
        ObservationMode, ObservationOutcome, ObservationResult, ObservationStatus,
        OperationIdOrigin, Resource, ResourceKind, SemanticEvent, SessionRef, Verdict,
    };

    fn credential() -> CredentialEntry {
        CredentialEntry {
            label: String::from("测试令牌"),
            kind: String::from("api_token"),
            value: String::from("gate0-secret"),
            fingerprint: String::from("sha256:test"),
        }
    }

    fn semantic_event(operation_id: Option<&str>) -> SemanticEvent {
        SemanticEvent {
            schema_version: String::from(EVENT_SCHEMA_VERSION),
            event_id: String::from("semantic-1"),
            event_timestamp_unix_ms: 1_000,
            session_id: String::from("session-1"),
            process_id: 10,
            user_activity_id: String::from("activity-1"),
            agent_session_id: Some(String::from("agent-session-1")),
            turn_id: Some(String::from("activity-1")),
            tool_call_id: operation_id.map(String::from),
            workspace_path: Some(String::from("C:\\workspace")),
            agent_id: Some(String::from("workbuddy")),
            provider_message_id: None,
            source_schema_profile: Some(String::from("test")),
            observation_layer: None,
            observation_status: ObservationStatus::Partial,
            content_kind: None,
            action_kind: ActionKind::ToolCall,
            content: String::from("test"),
            tool_name: Some(String::from("PowerShell")),
            operation_id: operation_id.map(String::from),
            operation_id_origin: operation_id.map(|_| OperationIdOrigin::WorkBuddyToolCallId),
            native_evidence: NativeEvidence {
                identifiers: operation_id
                    .map(|value| {
                        vec![NativeIdentifier {
                            source: NativeIdSource::WorkBuddy,
                            kind: NativeIdKind::ProviderTrace,
                            scope: String::from("workbuddy-session:agent-session-1"),
                            value: String::from(value),
                        }]
                    })
                    .unwrap_or_default(),
                edges: Vec::new(),
            },
            expected_os_action: Some(ActionKind::ProcessStart),
            evidence_source: EvidenceSource::AgentSemantic,
            source_record_id: String::from("record-1"),
            parent_record_id: None,
            trace_id: operation_id.map(String::from),
            model_id: None,
            request_model_id: None,
            request_model_name: None,
            conversation_request_id: None,
            record_status: None,
            error: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cached_tokens: None,
            reasoning_tokens: None,
            request_count: None,
        }
    }

    fn os_event(event_id: &str, timestamp: u64, operation_id: Option<&str>) -> ObservationEvent {
        ObservationEvent {
            schema_version: String::from("0.4.0"),
            event_id: String::from(event_id),
            event_timestamp_unix_ms: timestamp,
            observed_at_unix_ms: timestamp,
            phase: EventPhase::Live,
            operation_id: operation_id.map(String::from),
            operation_id_origin: operation_id.map(|_| OperationIdOrigin::EtwIrp),
            native_evidence: NativeEvidence {
                identifiers: {
                    let mut identifiers = vec![NativeIdentifier {
                        source: NativeIdSource::Etw,
                        kind: NativeIdKind::Process,
                        scope: String::from("etw-session:session-1"),
                        value: String::from("10"),
                    }];
                    if let Some(value) = operation_id {
                        identifiers.push(NativeIdentifier {
                            source: NativeIdSource::Etw,
                            kind: NativeIdKind::Activity,
                            scope: String::from("etw-session:session-1"),
                            value: String::from(value),
                        });
                    }
                    identifiers
                },
                edges: Vec::new(),
            },
            actor: Actor {
                agent_kind: AgentKind::WorkBuddy,
                identity_status: IdentityStatus::Inherited,
                root_process_instance_id: String::from("root-1"),
                identity: AgentIdentity {
                    executable_path: String::from("WorkBuddy.exe"),
                    file_version: String::from("test"),
                    sha256: String::from("test"),
                    signer_subject: String::from("test"),
                },
            },
            session: SessionRef {
                id: String::from("session-1"),
            },
            process: native_contracts::ProcessRef {
                pid: 10,
                parent_pid: None,
                unique_process_key: None,
                process_instance_id: String::from("process-1"),
                process_session_id: None,
                user_sid: None,
                image_name: String::from("powershell.exe"),
                image_path: None,
                command_line_observed: false,
                command_line: None,
                exit_status: None,
            },
            action: Action {
                kind: ActionKind::ProcessStart,
            },
            resource: Resource {
                kind: ResourceKind::Process,
                identifier: String::from("powershell.exe"),
            },
            destination: None::<Destination>,
            result: ObservationResult {
                status_code: None,
                success: None,
                bytes_transferred: None,
            },
            verdict: Verdict {
                mode: ObservationMode::Observe,
                outcome: ObservationOutcome::Observed,
            },
            evidence: Evidence {
                source: EvidenceSource::Etw,
                provider_id: String::from("test"),
                provider_event_id: 1,
                provider_opcode: 1,
                provider_event_version: 1,
            },
            confidence: Confidence::High,
            retention: None,
            aggregation: None,
        }
    }

    fn file_event(event_id: &str, timestamp: u64, process_id: u32, path: &str) -> ObservationEvent {
        let mut event = os_event(event_id, timestamp, None);
        event.process.pid = process_id;
        event.process.image_name = String::from("WorkBuddy.exe");
        event.action.kind = ActionKind::FileRead;
        event.resource.kind = ResourceKind::File;
        event.resource.identifier = String::from(path);
        event
    }

    #[test]
    fn full_finding_keeps_credential_value() {
        let findings = find_credentials("token=gate0-secret", &[credential()], false);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].secret_value.as_deref(), Some("gate0-secret"));
    }

    #[test]
    fn filtered_finding_identifies_but_redacts_credential() {
        let credential = credential();
        let findings = find_credentials(
            "token=gate0-secret",
            std::slice::from_ref(&credential),
            true,
        );
        assert_eq!(findings[0].label, "测试令牌");
        assert_eq!(findings[0].secret_value, None);
        assert_eq!(
            redact_content("token=gate0-secret", &[credential]),
            "token=<credential:测试令牌>"
        );
    }

    #[test]
    fn exact_trace_and_activity_confirms_correlation() {
        let semantic = semantic_event(Some("op-1"));
        let events = [os_event("os-1", 1_001, Some("op-1"))];
        let index = OsEventIndex::new(&events);
        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Confirmed);
        assert_eq!(result.linked_events.len(), 1);
    }

    #[test]
    fn explicit_os_session_maps_rotated_observer_window() {
        let semantic = semantic_event(Some("op-1"));
        let mut event = os_event("os-1", 1_001, Some("op-1"));
        event.session.id = String::from("os-session-2");
        let events = [event];
        let index = OsEventIndex::new(&events);

        let unmapped = correlate_os_events(&semantic, &index, None, 5_000);
        let mapped = correlate_os_events(&semantic, &index, Some("os-session-2"), 5_000);

        assert_eq!(unmapped.status, CorrelationStatus::Partial);
        assert_eq!(
            unmapped.missing_reason,
            Some("os_session_action_events_missing")
        );
        assert_eq!(mapped.status, CorrelationStatus::Confirmed);
        assert_eq!(mapped.linked_events.len(), 1);
    }

    #[test]
    fn time_candidates_do_not_create_causal_links() {
        let semantic = semantic_event(Some("semantic-only"));
        let events = [os_event("os-1", 1_001, None), os_event("os-2", 1_002, None)];
        let index = OsEventIndex::new(&events);
        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Partial);
        assert!(result.linked_events.is_empty());
        assert_eq!(result.first_breakpoint, Some("etw_activity_id_missing"));
    }

    #[test]
    fn process_tool_excludes_unrelated_process_images() {
        let semantic = semantic_event(None);
        let mut unrelated = os_event("os-conhost", 1_001, None);
        unrelated.process.pid = 20;
        unrelated.process.image_name = String::from("conhost.exe");
        let mut powershell = os_event("os-powershell", 1_100, None);
        powershell.process.pid = 21;
        let mut root = os_event("os-root", 900, None);
        root.process.image_name = String::from("WorkBuddy.exe");
        let events = [root, unrelated, powershell];
        let index = OsEventIndex::new(&events);

        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Unlinked);
        assert!(result.linked_events.is_empty());
    }

    #[test]
    fn file_tool_keeps_only_matching_resource_candidates() {
        let mut semantic = semantic_event(None);
        semantic.tool_name = Some(String::from("Read"));
        semantic.expected_os_action = Some(ActionKind::FileRead);
        semantic.content =
            String::from(r#"{"file_path":"C:\\AgentReins-Lab\\fixtures\\read-only-input.txt"}"#);
        let events = [
            file_event(
                "os-unrelated",
                1_001,
                10,
                r"\Device\HarddiskVolume4\Users\AgentReinsTestUser\.workbuddy\logs\app.log",
            ),
            file_event(
                "os-target-1",
                1_050,
                20,
                r"\Device\HarddiskVolume4\AgentReins-Lab\fixtures\read-only-input.txt",
            ),
            file_event(
                "os-target-2",
                1_075,
                21,
                r"\Device\HarddiskVolume4\AgentReins-Lab\fixtures\read-only-input.txt",
            ),
        ];
        let index = OsEventIndex::new(&events);

        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Unlinked);
        assert!(result.linked_events.is_empty());
    }

    #[test]
    fn file_tool_does_not_fall_back_to_unrelated_resource() {
        let mut semantic = semantic_event(None);
        semantic.tool_name = Some(String::from("Read"));
        semantic.expected_os_action = Some(ActionKind::FileRead);
        semantic.content =
            String::from(r#"{"file_path":"C:\\AgentReins-Lab\\fixtures\\missing.txt"}"#);
        let events = [file_event(
            "os-unrelated",
            1_001,
            10,
            r"\Device\HarddiskVolume4\Users\AgentReinsTestUser\.workbuddy\logs\app.log",
        )];
        let index = OsEventIndex::new(&events);

        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Unlinked);
        assert!(result.linked_events.is_empty());
        assert_eq!(
            result.missing_reason,
            Some("workbuddy_provider_trace_id_missing")
        );
    }

    #[test]
    fn invalid_file_tool_arguments_are_reported_as_missing() {
        let mut semantic = semantic_event(None);
        semantic.tool_name = Some(String::from("Read"));
        semantic.expected_os_action = Some(ActionKind::FileRead);
        semantic.content = String::from("not-json");
        let events = [file_event(
            "os-target",
            1_001,
            10,
            r"\Device\HarddiskVolume4\AgentReins-Lab\fixtures\read-only-input.txt",
        )];
        let index = OsEventIndex::new(&events);

        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Unlinked);
        assert_eq!(
            result.missing_reason,
            Some("workbuddy_provider_trace_id_missing")
        );
    }

    #[test]
    fn generated_operation_id_does_not_confirm_without_native_activity() {
        let semantic = semantic_event(Some("same-operation"));
        let mut event = os_event("os-1", 1_000, None);
        event.operation_id = Some(String::from("same-operation"));
        event.operation_id_origin = Some(OperationIdOrigin::AgentReinsGenerated);
        let events = [event];
        let index = OsEventIndex::new(&events);
        let result = correlate_os_events(&semantic, &index, None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Partial);
        assert!(result.linked_events.is_empty());
    }

    #[test]
    fn native_workbuddy_metadata_links_mcp_request_response_and_process() {
        let mut semantic = semantic_event(Some("479bbd05bca44c0ca5a8e35cce56b407"));
        semantic.content_kind = Some(native_contracts::SemanticContentKind::ToolCall);
        semantic.expected_os_action = None;
        let records = [
            McpProtocolRecord {
                schema_version: String::from("1.0.0"),
                direction: McpProtocolDirection::Request,
                transport: String::from("stdio"),
                process_id: 10,
                parent_process_id: 5,
                working_directory: String::from("C:\\workspace"),
                raw_json: String::from(
                    r#"{"method":"tools/call","params":{"_meta":{"workbuddy.ai/conversationId":"agent-session-1","workbuddy.ai/requestId":"479bbd05bca44c0ca5a8e35cce56b407","workbuddy.ai/messageId":"record-1"}},"jsonrpc":"2.0","id":4}"#,
                ),
            },
            McpProtocolRecord {
                schema_version: String::from("1.0.0"),
                direction: McpProtocolDirection::Response,
                transport: String::from("stdio"),
                process_id: 10,
                parent_process_id: 5,
                working_directory: String::from("C:\\workspace"),
                raw_json: String::from(r#"{"jsonrpc":"2.0","id":4,"result":{}}"#),
            },
        ];
        let mcp_index = McpProtocolIndex::new(&records).expect("MCP 原生记录应可解析");
        let events = [os_event("process-start", 1_000, None)];
        let os_index = OsEventIndex::new(&events);

        let result = correlate_native_evidence(&semantic, &os_index, Some(&mcp_index), None, 5_000);

        assert_eq!(result.status, CorrelationStatus::Partial);
        assert_eq!(result.mcp_jsonrpc_request_id.as_deref(), Some("4"));
        assert_eq!(result.mcp_process_id, Some(10));
        assert!(result.mcp_response_observed);
        assert_eq!(
            result.first_breakpoint,
            Some("mcp_request_to_os_operation_native_id_missing")
        );
        assert_eq!(result.linked_events.len(), 1);
    }

    #[test]
    fn native_call_id_links_tool_result_to_mcp_request() {
        let mut tool_call = semantic_event(Some("479bbd05bca44c0ca5a8e35cce56b407"));
        tool_call.content_kind = Some(native_contracts::SemanticContentKind::ToolCall);
        tool_call.expected_os_action = None;
        tool_call
            .native_evidence
            .identifiers
            .push(NativeIdentifier {
                source: NativeIdSource::WorkBuddy,
                kind: NativeIdKind::Record,
                scope: String::from("workbuddy-session:agent-session-1"),
                value: String::from("record-1"),
            });
        let mut tool_result = tool_call.clone();
        tool_result.event_id = String::from("semantic-result-1");
        tool_result.source_record_id = String::from("record-2");
        tool_result.content_kind = Some(native_contracts::SemanticContentKind::ToolResult);
        tool_result.action_kind = ActionKind::ToolResult;
        tool_result
            .native_evidence
            .identifiers
            .retain(|identifier| identifier.kind != NativeIdKind::Record);
        tool_result
            .native_evidence
            .identifiers
            .push(NativeIdentifier {
                source: NativeIdSource::WorkBuddy,
                kind: NativeIdKind::Record,
                scope: String::from("workbuddy-session:agent-session-1"),
                value: String::from("record-2"),
            });
        let semantic_events = [tool_call, tool_result];
        let semantic_index = SemanticEventIndex::new(&semantic_events);
        let records = [
            McpProtocolRecord {
                schema_version: String::from("1.0.0"),
                direction: McpProtocolDirection::Request,
                transport: String::from("stdio"),
                process_id: 10,
                parent_process_id: 5,
                working_directory: String::from("C:\\workspace"),
                raw_json: String::from(
                    r#"{"method":"tools/call","params":{"_meta":{"workbuddy.ai/conversationId":"agent-session-1","workbuddy.ai/requestId":"479bbd05bca44c0ca5a8e35cce56b407","workbuddy.ai/messageId":"record-1"}},"jsonrpc":"2.0","id":4}"#,
                ),
            },
            McpProtocolRecord {
                schema_version: String::from("1.0.0"),
                direction: McpProtocolDirection::Response,
                transport: String::from("stdio"),
                process_id: 10,
                parent_process_id: 5,
                working_directory: String::from("C:\\workspace"),
                raw_json: String::from(r#"{"jsonrpc":"2.0","id":4,"result":{}}"#),
            },
        ];
        let mcp_index = McpProtocolIndex::new(&records).expect("MCP 原生记录应可解析");
        let events = [os_event("process-start", 1_000, None)];
        let os_index = OsEventIndex::new(&events);

        let result = correlate_semantic_evidence(
            &semantic_events[1],
            &semantic_index,
            &os_index,
            Some(&mcp_index),
            None,
            5_000,
        )
        .expect("ToolResult 应参与原生关联");

        assert_eq!(result.status, CorrelationStatus::Partial);
        assert_eq!(result.mcp_jsonrpc_request_id.as_deref(), Some("4"));
        assert!(result.causal_edges.iter().any(|edge| {
            edge.kind == CausalEdgeKind::RequestResponse
                && edge.from.value == "record-1"
                && edge.to.value == "record-2"
        }));
    }
}
