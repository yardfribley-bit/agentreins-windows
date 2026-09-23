use std::collections::{HashMap, HashSet};

use native_contracts::{
    ActionKind, CausalEdge, CausalEdgeKind, NativeIdKind, NativeIdSource, NativeIdentifier,
    ObservationEvent, SemanticContentKind, SemanticEvent,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationStatus {
    Confirmed,
    Partial,
    Unlinked,
}

pub struct CorrelationResult<'a> {
    pub selected: Option<&'a ObservationEvent>,
    pub linked_events: Vec<&'a ObservationEvent>,
    pub status: CorrelationStatus,
    pub basis: Option<&'static str>,
    pub time_basis: Option<&'static str>,
    pub missing_reason: Option<&'static str>,
    pub first_breakpoint: Option<&'static str>,
    pub causal_edges: Vec<CausalEdge>,
    pub mcp_jsonrpc_request_id: Option<String>,
    pub mcp_process_id: Option<u32>,
    pub mcp_response_observed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpProtocolDirection {
    Request,
    Response,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpProtocolRecord {
    pub schema_version: String,
    pub direction: McpProtocolDirection,
    pub transport: String,
    pub process_id: u32,
    pub parent_process_id: u32,
    pub working_directory: String,
    pub raw_json: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(untagged)]
enum JsonRpcId {
    Text(String),
    Number(i64),
}

impl JsonRpcId {
    fn value(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct JsonRpcMessage {
    id: Option<JsonRpcId>,
    method: Option<String>,
    params: Option<JsonRpcParams>,
}

#[derive(Deserialize)]
struct JsonRpcParams {
    #[serde(rename = "_meta")]
    metadata: Option<WorkBuddyMcpMetadata>,
}

#[derive(Deserialize)]
struct WorkBuddyMcpMetadata {
    #[serde(rename = "workbuddy.ai/conversationId")]
    conversation_id: Option<String>,
    #[serde(rename = "workbuddy.ai/requestId")]
    request_id: Option<String>,
    #[serde(rename = "workbuddy.ai/messageId")]
    message_id: Option<String>,
}

struct McpRequestLink {
    json_rpc_id: String,
    process_id: u32,
    conversation_id: String,
    request_id: String,
    message_id: String,
    response_observed: bool,
}

pub struct McpProtocolIndex {
    requests: Vec<McpRequestLink>,
}

pub struct SemanticEventIndex<'a> {
    tool_calls: HashMap<(&'a str, &'a str), Vec<&'a SemanticEvent>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProcessRootAssociation {
    pub session_id: String,
    pub process_id: u32,
    pub root_process_instance_id: String,
}

pub struct OsEventIndex<'a> {
    events_by_session_action: HashMap<(&'a str, ActionKind), Vec<&'a ObservationEvent>>,
    events_by_session_pid: HashMap<(&'a str, u32), Vec<&'a ObservationEvent>>,
    process_starts_by_session_pid: HashMap<(&'a str, u32), Vec<&'a ObservationEvent>>,
}

impl<'a> OsEventIndex<'a> {
    pub fn new(events: &'a [ObservationEvent]) -> Self {
        Self::from_events(events)
    }

    pub fn with_process_roots(
        events: &'a [ObservationEvent],
        _process_roots: &'a [ProcessRootAssociation],
    ) -> Self {
        Self::from_events(events)
    }

    fn from_events(events: &'a [ObservationEvent]) -> Self {
        let mut events_by_session_action = HashMap::new();
        let mut events_by_session_pid = HashMap::new();
        let mut process_starts_by_session_pid = HashMap::new();
        for event in events {
            events_by_session_action
                .entry((event.session.id.as_str(), event.action.kind))
                .or_insert_with(Vec::new)
                .push(event);
            events_by_session_pid
                .entry((event.session.id.as_str(), event.process.pid))
                .or_insert_with(Vec::new)
                .push(event);
            if event.action.kind == ActionKind::ProcessStart {
                process_starts_by_session_pid
                    .entry((event.session.id.as_str(), event.process.pid))
                    .or_insert_with(Vec::new)
                    .push(event);
            }
        }
        Self {
            events_by_session_action,
            events_by_session_pid,
            process_starts_by_session_pid,
        }
    }
}

impl McpProtocolIndex {
    pub fn new(records: &[McpProtocolRecord]) -> Result<Self, String> {
        let mut responses = HashSet::new();
        for record in records {
            let message = parse_mcp_message(record)?;
            if record.direction == McpProtocolDirection::Response
                && let Some(id) = message.id
            {
                responses.insert((record.process_id, id.value()));
            }
        }
        let mut requests = Vec::new();
        for record in records {
            let message = parse_mcp_message(record)?;
            if record.direction != McpProtocolDirection::Request
                || message.method.as_deref() != Some("tools/call")
            {
                continue;
            }
            let json_rpc_id = message
                .id
                .ok_or_else(|| String::from("MCP tools/call 缺少 JSON-RPC id"))?
                .value();
            let metadata = message
                .params
                .and_then(|value| value.metadata)
                .ok_or_else(|| format!("MCP tools/call 缺少 WorkBuddy _meta id={json_rpc_id}"))?;
            let conversation_id = required_mcp_metadata(
                metadata.conversation_id,
                "workbuddy.ai/conversationId",
                &json_rpc_id,
            )?;
            let request_id =
                required_mcp_metadata(metadata.request_id, "workbuddy.ai/requestId", &json_rpc_id)?;
            let message_id =
                required_mcp_metadata(metadata.message_id, "workbuddy.ai/messageId", &json_rpc_id)?;
            requests.push(McpRequestLink {
                response_observed: responses.contains(&(record.process_id, json_rpc_id.clone())),
                json_rpc_id,
                process_id: record.process_id,
                conversation_id,
                request_id,
                message_id,
            });
        }
        Ok(Self { requests })
    }

    pub fn process_ids(&self) -> HashSet<u32> {
        self.requests
            .iter()
            .map(|request| request.process_id)
            .collect()
    }
}

impl<'a> SemanticEventIndex<'a> {
    pub fn new(events: &'a [SemanticEvent]) -> Self {
        let mut tool_calls = HashMap::new();
        for event in events
            .iter()
            .filter(|event| event.content_kind == Some(SemanticContentKind::ToolCall))
        {
            let (Some(session_id), Some(call_id)) = (
                event.agent_session_id.as_deref(),
                event.tool_call_id.as_deref(),
            ) else {
                continue;
            };
            tool_calls
                .entry((session_id, call_id))
                .or_insert_with(Vec::new)
                .push(event);
        }
        Self { tool_calls }
    }
}

fn parse_mcp_message(record: &McpProtocolRecord) -> Result<JsonRpcMessage, String> {
    serde_json::from_str(&record.raw_json).map_err(|error| {
        format!(
            "无法解析 MCP 原始 JSON-RPC direction={:?} process_id={} error={error}",
            record.direction, record.process_id
        )
    })
}

fn required_mcp_metadata(
    value: Option<String>,
    field: &str,
    json_rpc_id: &str,
) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("MCP tools/call 缺少原生字段 field={field} id={json_rpc_id}"))
}

pub fn correlate_native_evidence<'a>(
    semantic_event: &SemanticEvent,
    os_event_index: &OsEventIndex<'a>,
    mcp_protocol_index: Option<&McpProtocolIndex>,
    os_session_id: Option<&str>,
    window_ms: u64,
) -> CorrelationResult<'a> {
    if let Some(index) = mcp_protocol_index
        && let Some(result) =
            correlate_mcp_evidence(semantic_event, os_event_index, index, os_session_id)
    {
        return result;
    }
    correlate_os_events(semantic_event, os_event_index, os_session_id, window_ms)
}

pub fn correlate_semantic_evidence<'a>(
    semantic_event: &SemanticEvent,
    semantic_event_index: &SemanticEventIndex<'_>,
    os_event_index: &OsEventIndex<'a>,
    mcp_protocol_index: Option<&McpProtocolIndex>,
    os_session_id: Option<&str>,
    window_ms: u64,
) -> Option<CorrelationResult<'a>> {
    if semantic_event.content_kind == Some(SemanticContentKind::ToolResult) {
        let correlation = correlate_tool_result_evidence(
            semantic_event,
            semantic_event_index,
            os_event_index,
            mcp_protocol_index,
            os_session_id,
            window_ms,
        );
        return (correlation.first_breakpoint != Some("semantic_event_has_no_expected_os_action"))
            .then_some(correlation);
    }
    if semantic_event.content_kind == Some(SemanticContentKind::ToolCall) {
        let correlation = correlate_native_evidence(
            semantic_event,
            os_event_index,
            mcp_protocol_index,
            os_session_id,
            window_ms,
        );
        return (correlation.first_breakpoint != Some("semantic_event_has_no_expected_os_action"))
            .then_some(correlation);
    }
    if semantic_event.expected_os_action.is_some() {
        return Some(correlate_native_evidence(
            semantic_event,
            os_event_index,
            mcp_protocol_index,
            os_session_id,
            window_ms,
        ));
    }
    None
}

fn correlate_tool_result_evidence<'a>(
    semantic_event: &SemanticEvent,
    semantic_event_index: &SemanticEventIndex<'_>,
    os_event_index: &OsEventIndex<'a>,
    mcp_protocol_index: Option<&McpProtocolIndex>,
    os_session_id: Option<&str>,
    window_ms: u64,
) -> CorrelationResult<'a> {
    let (Some(session_id), Some(call_id)) = (
        semantic_event.agent_session_id.as_deref(),
        semantic_event.tool_call_id.as_deref(),
    ) else {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "tool_result_native_tool_call_id_missing",
            semantic_event.native_evidence.edges.clone(),
        );
    };
    let Some(tool_calls) = semantic_event_index.tool_calls.get(&(session_id, call_id)) else {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "tool_result_matching_tool_call_missing",
            semantic_event.native_evidence.edges.clone(),
        );
    };
    if tool_calls.len() != 1 {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "multiple_tool_calls_for_native_call_id",
            semantic_event.native_evidence.edges.clone(),
        );
    }

    let tool_call = tool_calls[0];
    let mut correlation = correlate_native_evidence(
        tool_call,
        os_event_index,
        mcp_protocol_index,
        os_session_id,
        window_ms,
    );
    correlation
        .causal_edges
        .extend(semantic_event.native_evidence.edges.clone());
    if let (Some(call_record), Some(result_record)) = (
        workbuddy_record_identifier(tool_call),
        workbuddy_record_identifier(semantic_event),
    ) {
        correlation.causal_edges.push(CausalEdge {
            kind: CausalEdgeKind::RequestResponse,
            from: call_record,
            to: result_record,
            evidence_fields: vec![
                String::from("function_call.callId"),
                String::from("function_call_result.callId"),
            ],
        });
    }
    correlation.basis =
        Some("workbuddy_tool_result_call_id_equals_tool_call_call_id_then_native_tool_graph");
    correlation
}

fn correlate_mcp_evidence<'a>(
    semantic_event: &SemanticEvent,
    os_event_index: &OsEventIndex<'a>,
    mcp_protocol_index: &McpProtocolIndex,
    os_session_id: Option<&str>,
) -> Option<CorrelationResult<'a>> {
    if semantic_event.content_kind != Some(SemanticContentKind::ToolCall) {
        return None;
    }
    let workbuddy_trace = workbuddy_trace_identifier(semantic_event)?;
    let trace_value = normalize_native_id(&workbuddy_trace.value)?;
    let candidates = mcp_protocol_index
        .requests
        .iter()
        .filter(|request| {
            request.request_id.as_str() == workbuddy_trace.value.as_str()
                && request.message_id == semantic_event.source_record_id
                && semantic_event.agent_session_id.as_deref()
                    == Some(request.conversation_id.as_str())
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() > 1 {
        return Some(mcp_result_without_os_link(
            "multiple_mcp_requests_for_workbuddy_native_ids",
            semantic_event.native_evidence.edges.clone(),
            None,
        ));
    }
    let request = candidates[0];
    let scope = format!("workbuddy-session:{}", request.conversation_id);
    let mcp_scope = format!("mcp-process:{}", request.process_id);
    let embedded_trace = NativeIdentifier {
        source: NativeIdSource::WorkBuddy,
        kind: NativeIdKind::ProviderTrace,
        scope: scope.clone(),
        value: request.request_id.clone(),
    };
    let semantic_record = semantic_event
        .native_evidence
        .identifiers
        .iter()
        .find(|identifier| {
            identifier.source == NativeIdSource::WorkBuddy
                && identifier.kind == NativeIdKind::Record
                && identifier.value == semantic_event.source_record_id
        })
        .cloned()
        .unwrap_or_else(|| NativeIdentifier {
            source: NativeIdSource::WorkBuddy,
            kind: NativeIdKind::Record,
            scope: scope.clone(),
            value: semantic_event.source_record_id.clone(),
        });
    let embedded_record = NativeIdentifier {
        source: NativeIdSource::WorkBuddy,
        kind: NativeIdKind::Record,
        scope,
        value: request.message_id.clone(),
    };
    let json_rpc_request = NativeIdentifier {
        source: NativeIdSource::McpJsonRpc,
        kind: NativeIdKind::JsonRpcRequest,
        scope: mcp_scope.clone(),
        value: request.json_rpc_id.clone(),
    };
    let mcp_process = NativeIdentifier {
        source: NativeIdSource::McpJsonRpc,
        kind: NativeIdKind::Process,
        scope: String::from("windows-process-id"),
        value: request.process_id.to_string(),
    };
    let mut causal_edges = semantic_event.native_evidence.edges.clone();
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::SameNativeId,
        from: workbuddy_trace,
        to: embedded_trace.clone(),
        evidence_fields: vec![
            String::from("providerData.traceId"),
            String::from("params._meta.workbuddy.ai/requestId"),
        ],
    });
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::SameNativeId,
        from: semantic_record,
        to: embedded_record.clone(),
        evidence_fields: vec![
            String::from("id"),
            String::from("params._meta.workbuddy.ai/messageId"),
        ],
    });
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::CoRecordedMapping,
        from: embedded_trace,
        to: json_rpc_request.clone(),
        evidence_fields: vec![
            String::from("params._meta.workbuddy.ai/requestId"),
            String::from("id"),
        ],
    });
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::CoRecordedMapping,
        from: embedded_record,
        to: json_rpc_request.clone(),
        evidence_fields: vec![
            String::from("params._meta.workbuddy.ai/messageId"),
            String::from("id"),
        ],
    });
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::RuntimeOwnership,
        from: json_rpc_request.clone(),
        to: mcp_process.clone(),
        evidence_fields: vec![String::from("id"), String::from("process_id")],
    });
    if request.response_observed {
        causal_edges.push(CausalEdge {
            kind: CausalEdgeKind::RequestResponse,
            from: json_rpc_request.clone(),
            to: NativeIdentifier {
                source: NativeIdSource::McpJsonRpc,
                kind: NativeIdKind::JsonRpcResponse,
                scope: mcp_scope,
                value: request.json_rpc_id.clone(),
            },
            evidence_fields: vec![String::from("request.id"), String::from("response.id")],
        });
    } else {
        return Some(mcp_result_without_os_link(
            "mcp_jsonrpc_response_missing",
            causal_edges,
            Some(request),
        ));
    }
    let session_id = correlation_session_id(semantic_event, os_session_id);
    let Some(process_starts) = os_event_index
        .process_starts_by_session_pid
        .get(&(session_id, request.process_id))
    else {
        return Some(mcp_result_without_os_link(
            "mcp_process_id_missing_in_etw",
            causal_edges,
            Some(request),
        ));
    };
    if process_starts.len() != 1 {
        return Some(mcp_result_without_os_link(
            "mcp_process_pid_reused_without_process_start_key",
            causal_edges,
            Some(request),
        ));
    }
    let process_start = process_starts[0];
    let etw_process = process_start
        .native_evidence
        .identifiers
        .iter()
        .find(|identifier| {
            identifier.source == NativeIdSource::Etw
                && identifier.kind == NativeIdKind::Process
                && identifier.value == request.process_id.to_string()
        })
        .cloned();
    let Some(etw_process) = etw_process else {
        return Some(mcp_result_without_os_link(
            "etw_process_identifier_missing",
            causal_edges,
            Some(request),
        ));
    };
    causal_edges.extend(process_start.native_evidence.edges.clone());
    causal_edges.push(CausalEdge {
        kind: CausalEdgeKind::SameNativeId,
        from: mcp_process,
        to: etw_process,
        evidence_fields: vec![String::from("process_id"), String::from("ProcessId")],
    });
    let process_events = os_event_index
        .events_by_session_pid
        .get(&(session_id, request.process_id))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let linked_events = process_events
        .iter()
        .copied()
        .filter(|event| {
            etw_activity_identifiers(event).iter().any(|identifier| {
                normalize_native_id(&identifier.value).as_deref() == Some(trace_value.as_str())
            })
        })
        .collect::<Vec<_>>();
    if linked_events.is_empty() {
        return Some(CorrelationResult {
            selected: None,
            linked_events: vec![process_start],
            status: CorrelationStatus::Partial,
            basis: Some("workbuddy_native_ids_equal_mcp_meta_and_process_id_equals_etw"),
            time_basis: None,
            missing_reason: Some("mcp_request_to_os_operation_native_id_missing"),
            first_breakpoint: Some("mcp_request_to_os_operation_native_id_missing"),
            causal_edges,
            mcp_jsonrpc_request_id: Some(request.json_rpc_id.clone()),
            mcp_process_id: Some(request.process_id),
            mcp_response_observed: true,
        });
    }
    for event in &linked_events {
        causal_edges.extend(event.native_evidence.edges.clone());
    }
    Some(CorrelationResult {
        selected: (linked_events.len() == 1).then(|| linked_events[0]),
        linked_events,
        status: CorrelationStatus::Confirmed,
        basis: Some("workbuddy_request_id_equals_mcp_meta_and_etw_activity_id"),
        time_basis: None,
        missing_reason: None,
        first_breakpoint: None,
        causal_edges,
        mcp_jsonrpc_request_id: Some(request.json_rpc_id.clone()),
        mcp_process_id: Some(request.process_id),
        mcp_response_observed: true,
    })
}

fn mcp_result_without_os_link<'a>(
    breakpoint: &'static str,
    causal_edges: Vec<CausalEdge>,
    request: Option<&McpRequestLink>,
) -> CorrelationResult<'a> {
    CorrelationResult {
        selected: None,
        linked_events: Vec::new(),
        status: CorrelationStatus::Partial,
        basis: request.map(|_| "workbuddy_native_ids_equal_mcp_meta"),
        time_basis: None,
        missing_reason: Some(breakpoint),
        first_breakpoint: Some(breakpoint),
        causal_edges,
        mcp_jsonrpc_request_id: request.map(|value| value.json_rpc_id.clone()),
        mcp_process_id: request.map(|value| value.process_id),
        mcp_response_observed: request.is_some_and(|value| value.response_observed),
    }
}

pub fn correlate_os_events<'a>(
    semantic_event: &SemanticEvent,
    os_event_index: &OsEventIndex<'a>,
    os_session_id: Option<&str>,
    _window_ms: u64,
) -> CorrelationResult<'a> {
    let semantic_edges = semantic_event.native_evidence.edges.clone();
    let Some(expected_action) = semantic_event.expected_os_action else {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "semantic_event_has_no_expected_os_action",
            semantic_edges,
        );
    };
    let Some(workbuddy_trace_id) = workbuddy_trace_identifier(semantic_event) else {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "workbuddy_provider_trace_id_missing",
            semantic_edges,
        );
    };
    let correlation_session_id = correlation_session_id(semantic_event, os_session_id);
    let Some(indexed_events) = os_event_index
        .events_by_session_action
        .get(&(correlation_session_id, expected_action))
    else {
        return result_without_link(
            CorrelationStatus::Partial,
            "os_session_action_events_missing",
            semantic_edges,
        );
    };
    let workbuddy_trace_value = normalize_native_id(&workbuddy_trace_id.value);
    if workbuddy_trace_value.is_none() {
        return result_without_link(
            CorrelationStatus::Unlinked,
            "workbuddy_provider_trace_id_zero_or_invalid",
            semantic_edges,
        );
    }
    let linked_events = indexed_events
        .iter()
        .copied()
        .filter(|event| {
            etw_activity_identifiers(event)
                .iter()
                .any(|identifier| normalize_native_id(&identifier.value) == workbuddy_trace_value)
        })
        .collect::<Vec<_>>();
    if linked_events.is_empty() {
        let breakpoint = if indexed_events
            .iter()
            .any(|event| !etw_activity_identifiers(event).is_empty())
        {
            "workbuddy_trace_id_does_not_equal_etw_activity_id"
        } else {
            "etw_activity_id_missing"
        };
        return result_without_link(CorrelationStatus::Partial, breakpoint, semantic_edges);
    }
    let mut causal_edges = semantic_edges;
    for event in &linked_events {
        causal_edges.extend(event.native_evidence.edges.clone());
        let Some(activity_id) = etw_activity_identifiers(event)
            .into_iter()
            .find(|identifier| normalize_native_id(&identifier.value) == workbuddy_trace_value)
        else {
            continue;
        };
        causal_edges.push(CausalEdge {
            kind: CausalEdgeKind::SameNativeId,
            from: workbuddy_trace_id.clone(),
            to: activity_id,
            evidence_fields: vec![
                String::from("providerData.traceId"),
                String::from("EventHeader.ActivityId"),
            ],
        });
    }
    CorrelationResult {
        selected: (linked_events.len() == 1).then(|| linked_events[0]),
        linked_events,
        status: CorrelationStatus::Confirmed,
        basis: Some("workbuddy_trace_id_equals_etw_activity_id"),
        time_basis: None,
        missing_reason: None,
        first_breakpoint: None,
        causal_edges,
        mcp_jsonrpc_request_id: None,
        mcp_process_id: None,
        mcp_response_observed: false,
    }
}

pub fn correlation_session_id<'a>(
    semantic_event: &'a SemanticEvent,
    os_session_id: Option<&'a str>,
) -> &'a str {
    os_session_id.unwrap_or(semantic_event.session_id.as_str())
}

fn result_without_link<'a>(
    status: CorrelationStatus,
    breakpoint: &'static str,
    causal_edges: Vec<CausalEdge>,
) -> CorrelationResult<'a> {
    CorrelationResult {
        selected: None,
        linked_events: Vec::new(),
        status,
        basis: None,
        time_basis: None,
        missing_reason: Some(breakpoint),
        first_breakpoint: Some(breakpoint),
        causal_edges,
        mcp_jsonrpc_request_id: None,
        mcp_process_id: None,
        mcp_response_observed: false,
    }
}

fn workbuddy_trace_identifier(semantic_event: &SemanticEvent) -> Option<NativeIdentifier> {
    semantic_event
        .native_evidence
        .identifiers
        .iter()
        .find(|identifier| {
            identifier.source == NativeIdSource::WorkBuddy
                && identifier.kind == NativeIdKind::ProviderTrace
        })
        .cloned()
        .or_else(|| {
            semantic_event
                .trace_id
                .as_deref()
                .map(|value| NativeIdentifier {
                    source: NativeIdSource::WorkBuddy,
                    kind: NativeIdKind::ProviderTrace,
                    scope: semantic_event
                        .agent_session_id
                        .as_deref()
                        .map(|session_id| format!("workbuddy-session:{session_id}"))
                        .unwrap_or_default(),
                    value: String::from(value),
                })
        })
}

fn workbuddy_record_identifier(semantic_event: &SemanticEvent) -> Option<NativeIdentifier> {
    semantic_event
        .native_evidence
        .identifiers
        .iter()
        .find(|identifier| {
            identifier.source == NativeIdSource::WorkBuddy
                && identifier.kind == NativeIdKind::Record
                && identifier.value == semantic_event.source_record_id
        })
        .cloned()
}

fn etw_activity_identifiers(event: &ObservationEvent) -> Vec<NativeIdentifier> {
    event
        .native_evidence
        .identifiers
        .iter()
        .filter(|identifier| {
            identifier.source == NativeIdSource::Etw
                && identifier.kind == NativeIdKind::Activity
                && normalize_native_id(&identifier.value).is_some()
        })
        .cloned()
        .collect()
}

fn normalize_native_id(value: &str) -> Option<String> {
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    (!normalized.is_empty() && normalized.chars().any(|character| character != '0'))
        .then_some(normalized)
}
