use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use native_contracts::{
    ActionKind, CausalEdge, CausalEdgeKind, EVENT_SCHEMA_VERSION, EvidenceSource, NativeEvidence,
    NativeIdKind, NativeIdSource, NativeIdentifier, ObservationLayer, ObservationStatus,
    OperationIdOrigin, SemanticContentKind, SemanticEvent,
};
use serde::Deserialize;

const SOURCE_SCHEMA_PROFILE: &str = "workbuddy-private-jsonl-observed-v1";
const KNOWN_TOP_LEVEL_FIELDS: &[&str] = &[
    "__codebuddyLocal",
    "aiTitle",
    "arguments",
    "callId",
    "content",
    "cwd",
    "id",
    "isSnapshotUpdate",
    "message",
    "name",
    "output",
    "parentId",
    "providerData",
    "rawContent",
    "role",
    "sessionId",
    "snapshot",
    "status",
    "timestamp",
    "type",
];
const KNOWN_PROVIDER_FIELDS: &[&str] = &[
    "agent",
    "argumentsDisplayText",
    "conversationRequestId",
    "error",
    "isMeta",
    "messageId",
    "model",
    "rawUsage",
    "reasoning",
    "requestModelId",
    "requestModelName",
    "skipRun",
    "startsNewUserRequest",
    "toolResult",
    "traceId",
    "usage",
];

struct Arguments {
    input: PathBuf,
    since_unix_ms: u64,
    until_unix_ms: u64,
    session_id: String,
    process_id: u32,
    output: PathBuf,
}

struct EventContext<'a> {
    session_id: &'a str,
    process_id: u32,
}

struct EventDetails {
    event_suffix: &'static str,
    user_activity_id: String,
    observation_layer: ObservationLayer,
    observation_status: ObservationStatus,
    content_kind: SemanticContentKind,
    action_kind: ActionKind,
    content: String,
    tool_name: Option<String>,
    operation_id: Option<String>,
    expected_os_action: Option<ActionKind>,
}

#[derive(Clone, Debug, Deserialize)]
struct WorkBuddyRecord {
    id: String,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    timestamp: u64,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "type")]
    record_type: String,
    role: Option<String>,
    content: Option<Vec<ContentBlock>>,
    #[serde(rename = "rawContent")]
    raw_content: Option<Vec<ContentBlock>>,
    name: Option<String>,
    status: Option<String>,
    #[serde(rename = "callId")]
    call_id: Option<String>,
    arguments: Option<String>,
    output: Option<ToolOutput>,
    #[serde(rename = "providerData")]
    provider_data: Option<ProviderData>,
}

#[derive(Clone, Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
    text: Option<String>,
    name: Option<String>,
    path: Option<String>,
    url: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ToolOutput {
    Single(ContentBlock),
    Multiple(Vec<ContentBlock>),
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderData {
    agent: Option<String>,
    model: Option<String>,
    #[serde(rename = "traceId")]
    trace_id: Option<String>,
    #[serde(rename = "requestModelId")]
    request_model_id: Option<String>,
    #[serde(rename = "requestModelName")]
    request_model_name: Option<String>,
    #[serde(rename = "conversationRequestId")]
    conversation_request_id: Option<String>,
    #[serde(rename = "messageId")]
    message_id: Option<String>,
    reasoning: Option<String>,
    error: Option<ProviderError>,
    #[serde(rename = "rawUsage")]
    raw_usage: Option<TokenUsage>,
    usage: Option<ProviderUsage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ProviderError {
    Message(String),
    Details(ProviderErrorDetails),
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderErrorDetails {
    message: Option<String>,
    code: Option<ProviderErrorCode>,
    #[serde(rename = "type")]
    error_type: Option<String>,
    #[serde(rename = "requestID")]
    request_id: Option<String>,
    status: Option<u16>,
    #[serde(rename = "isNetworkError")]
    is_network_error: Option<bool>,
    #[serde(rename = "isStreamTimeout")]
    is_stream_timeout: Option<bool>,
    #[serde(rename = "isRetryable")]
    is_retryable: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ProviderErrorCode {
    Number(i64),
    Text(String),
}

#[derive(Clone, Debug, Deserialize)]
struct TokenUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    prompt_cache_hit_tokens: Option<u64>,
    completion_thinking_tokens: Option<u64>,
    prompt_tokens_details: Option<TokenDetails>,
    completion_tokens_details: Option<TokenDetails>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderUsage {
    #[serde(rename = "inputTokens")]
    input_tokens: Option<u64>,
    #[serde(rename = "outputTokens")]
    output_tokens: Option<u64>,
    #[serde(rename = "totalTokens")]
    total_tokens: Option<u64>,
    requests: Option<u64>,
    #[serde(rename = "inputTokensDetails")]
    input_token_details: Option<Vec<TokenDetails>>,
    #[serde(rename = "outputTokensDetails")]
    output_token_details: Option<Vec<TokenDetails>>,
}

#[derive(Clone, Debug, Deserialize)]
struct TokenDetails {
    cached_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
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
    let records = read_records(&arguments.input)?;
    let eligible_records =
        records_in_window(records, arguments.since_unix_ms, arguments.until_unix_ms);
    let events = collect_events(
        &eligible_records,
        &arguments.session_id,
        arguments.process_id,
    )?;
    write_events(&arguments.output, &events)?;
    println!(
        "{}",
        serde_json::json!({
            "input_path": arguments.input,
            "since_unix_ms": arguments.since_unix_ms,
            "until_unix_ms": arguments.until_unix_ms,
            "records_in_window": eligible_records.len(),
            "semantic_events_written": events.len(),
            "output_path": arguments.output,
        })
    );
    Ok(())
}

fn parse_arguments(raw_arguments: &[String]) -> Result<Arguments, String> {
    if raw_arguments.len() != 13
        || raw_arguments[1] != "--input"
        || raw_arguments[3] != "--since-unix-ms"
        || raw_arguments[5] != "--until-unix-ms"
        || raw_arguments[7] != "--session-id"
        || raw_arguments[9] != "--process-id"
        || raw_arguments[11] != "--output"
    {
        return Err(String::from(
            "参数错误，用法: workbuddy-semantic-collector --input <project.jsonl> --since-unix-ms <milliseconds> --until-unix-ms <milliseconds> --session-id <id> --process-id <pid> --output <path.ndjson>",
        ));
    }
    let since_unix_ms = raw_arguments[4].parse::<u64>().map_err(|error| {
        format!(
            "since-unix-ms 必须是整数 value={} error={error}",
            raw_arguments[4]
        )
    })?;
    let until_unix_ms = raw_arguments[6].parse::<u64>().map_err(|error| {
        format!(
            "until-unix-ms 必须是整数 value={} error={error}",
            raw_arguments[6]
        )
    })?;
    if since_unix_ms > until_unix_ms {
        return Err(format!(
            "时间窗起点不得晚于终点 since_unix_ms={since_unix_ms} until_unix_ms={until_unix_ms}"
        ));
    }
    let process_id = raw_arguments[10].parse::<u32>().map_err(|error| {
        format!(
            "process-id 必须是整数 value={} error={error}",
            raw_arguments[10]
        )
    })?;
    if process_id == 0 {
        return Err(String::from("process-id 不得为 0"));
    }
    if raw_arguments[8].is_empty() {
        return Err(String::from("session-id 不得为空"));
    }
    Ok(Arguments {
        input: PathBuf::from(&raw_arguments[2]),
        since_unix_ms,
        until_unix_ms,
        session_id: raw_arguments[8].clone(),
        process_id,
        output: PathBuf::from(&raw_arguments[12]),
    })
}

fn records_in_window(
    records: Vec<WorkBuddyRecord>,
    since_unix_ms: u64,
    until_unix_ms: u64,
) -> Vec<WorkBuddyRecord> {
    records
        .into_iter()
        .filter(|record| record.timestamp >= since_unix_ms && record.timestamp <= until_unix_ms)
        .collect::<Vec<_>>()
}

fn read_records(path: &Path) -> Result<Vec<WorkBuddyRecord>, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "无法打开 WorkBuddy 项目会话 path={} error={error}",
            path.display()
        )
    })?;
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
                    "无法读取 WorkBuddy 项目会话 path={} line={} error={error}",
                    path.display(),
                    index + 1
                )
            })?;
            let value = serde_json::from_str::<serde_json::Value>(&content).map_err(|error| {
                format!(
                    "无法解析 WorkBuddy 项目会话 path={} line={} error={error}",
                    path.display(),
                    index + 1
                )
            })?;
            validate_record_schema(&value, path, index + 1)?;
            serde_json::from_value(value).map_err(|error| {
                format!(
                    "WorkBuddy 项目会话字段类型不兼容 profile={} path={} line={} error={error}",
                    SOURCE_SCHEMA_PROFILE,
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

fn validate_record_schema(
    value: &serde_json::Value,
    path: &Path,
    line_number: usize,
) -> Result<(), String> {
    let record = value.as_object().ok_or_else(|| {
        format!(
            "WorkBuddy 项目会话记录不是对象 profile={} path={} line={line_number}",
            SOURCE_SCHEMA_PROFILE,
            path.display()
        )
    })?;
    let unknown_fields = record
        .keys()
        .filter(|field| !KNOWN_TOP_LEVEL_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown_fields.is_empty() {
        return Err(format!(
            "WorkBuddy 私有格式出现未知顶层字段 profile={} path={} line={} fields={}",
            SOURCE_SCHEMA_PROFILE,
            path.display(),
            line_number,
            unknown_fields.join(",")
        ));
    }
    let Some(provider_data) = record.get("providerData") else {
        return Ok(());
    };
    let provider = provider_data.as_object().ok_or_else(|| {
        format!(
            "WorkBuddy providerData 不是对象 profile={} path={} line={line_number}",
            SOURCE_SCHEMA_PROFILE,
            path.display()
        )
    })?;
    let unknown_provider_fields = provider
        .keys()
        .filter(|field| !KNOWN_PROVIDER_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown_provider_fields.is_empty() {
        return Err(format!(
            "WorkBuddy 私有格式出现未知 providerData 字段 profile={} path={} line={} fields={}",
            SOURCE_SCHEMA_PROFILE,
            path.display(),
            line_number,
            unknown_provider_fields.join(",")
        ));
    }
    Ok(())
}

fn collect_events(
    records: &[WorkBuddyRecord],
    session_id: &str,
    process_id: u32,
) -> Result<Vec<SemanticEvent>, String> {
    let mut events = Vec::new();
    let final_assistant_ids = final_assistant_record_ids(records);
    let mut user_activity_id = format!("workbuddy:unattributed:{session_id}");
    let context = EventContext {
        session_id,
        process_id,
    };
    for record in records {
        if record.record_type == "message" && record.role.as_deref() == Some("user") {
            user_activity_id = record.id.clone();
        }
        match record.record_type.as_str() {
            "message" => collect_message_events(
                record,
                &context,
                &user_activity_id,
                final_assistant_ids.contains(&record.id),
                &mut events,
            )?,
            "function_call" => {
                let name = required(record.name.as_deref(), record, "name")?;
                let arguments = required(record.arguments.as_deref(), record, "arguments")?;
                let call_id = required(record.call_id.as_deref(), record, "callId")?;
                events.push(build_event(
                    record,
                    &context,
                    EventDetails {
                        event_suffix: "available-tool-observed",
                        user_activity_id: user_activity_id.clone(),
                        observation_layer: ObservationLayer::AvailableToolsMcp,
                        observation_status: ObservationStatus::Partial,
                        content_kind: SemanticContentKind::AvailableTool,
                        action_kind: ActionKind::ToolCall,
                        content: String::from(name),
                        tool_name: Some(String::from(name)),
                        operation_id: Some(String::from(call_id)),
                        expected_os_action: None,
                    },
                ));
                events.push(build_event(
                    record,
                    &context,
                    EventDetails {
                        event_suffix: "tool-call",
                        user_activity_id: user_activity_id.clone(),
                        observation_layer: ObservationLayer::ToolMcpCalls,
                        observation_status: ObservationStatus::Observed,
                        content_kind: SemanticContentKind::ToolCall,
                        action_kind: ActionKind::ToolCall,
                        content: String::from(arguments),
                        tool_name: Some(String::from(name)),
                        operation_id: Some(String::from(call_id)),
                        expected_os_action: expected_os_action(name),
                    },
                ));
            }
            "function_call_result" => {
                let name = required(record.name.as_deref(), record, "name")?;
                let call_id = required(record.call_id.as_deref(), record, "callId")?;
                let output = required_output(record)?;
                if name == "AskUserQuestion" {
                    events.push(build_event(
                        record,
                        &context,
                        EventDetails {
                            event_suffix: "user-response",
                            user_activity_id: user_activity_id.clone(),
                            observation_layer: ObservationLayer::UserPrompt,
                            observation_status: ObservationStatus::Observed,
                            content_kind: SemanticContentKind::RawUserPrompt,
                            action_kind: ActionKind::UserInput,
                            content: output.clone(),
                            tool_name: Some(String::from(name)),
                            operation_id: Some(String::from(call_id)),
                            expected_os_action: None,
                        },
                    ));
                }
                events.push(build_event(
                    record,
                    &context,
                    EventDetails {
                        event_suffix: "tool-result",
                        user_activity_id: user_activity_id.clone(),
                        observation_layer: ObservationLayer::ToolResult,
                        observation_status: ObservationStatus::Observed,
                        content_kind: SemanticContentKind::ToolResult,
                        action_kind: ActionKind::ToolResult,
                        content: output,
                        tool_name: Some(String::from(name)),
                        operation_id: Some(String::from(call_id)),
                        expected_os_action: None,
                    },
                ));
            }
            "reasoning" => {
                if let Some(content) = reasoning_content(record) {
                    events.push(build_event(
                        record,
                        &context,
                        EventDetails {
                            event_suffix: "llm-response-reasoning",
                            user_activity_id: user_activity_id.clone(),
                            observation_layer: ObservationLayer::LlmRequestResponse,
                            observation_status: ObservationStatus::Partial,
                            content_kind: SemanticContentKind::LlmResponse,
                            action_kind: ActionKind::LlmResult,
                            content,
                            tool_name: None,
                            operation_id: record_trace_id(record),
                            expected_os_action: None,
                        },
                    ));
                }
            }
            "file-history-snapshot" | "ai-title" => {}
            record_type => {
                return Err(format!(
                    "发现未支持的 WorkBuddy 记录类型 record_id={} type={record_type}",
                    record.id
                ));
            }
        }
    }
    Ok(events)
}

fn collect_message_events(
    record: &WorkBuddyRecord,
    context: &EventContext<'_>,
    user_activity_id: &str,
    is_final: bool,
    events: &mut Vec<SemanticEvent>,
) -> Result<(), String> {
    let role = required(record.role.as_deref(), record, "role")?;
    match role {
        "user" => {
            let content = required_text_content(record)?;
            let (user_query, user_query_status) = extract_user_query(&content, record)?;
            events.push(build_event(
                record,
                context,
                EventDetails {
                    event_suffix: "user-input",
                    user_activity_id: String::from(user_activity_id),
                    observation_layer: ObservationLayer::UserPrompt,
                    observation_status: user_query_status,
                    content_kind: SemanticContentKind::RawUserPrompt,
                    action_kind: ActionKind::UserInput,
                    content: user_query,
                    tool_name: None,
                    operation_id: record_trace_id(record),
                    expected_os_action: None,
                },
            ));
            collect_prompt_context_events(record, context, user_activity_id, &content, events);
            events.push(build_event(
                record,
                context,
                EventDetails {
                    event_suffix: "llm-request",
                    user_activity_id: String::from(user_activity_id),
                    observation_layer: ObservationLayer::LlmRequestResponse,
                    observation_status: ObservationStatus::Partial,
                    content_kind: SemanticContentKind::LlmRequest,
                    action_kind: ActionKind::LlmRequest,
                    content,
                    tool_name: None,
                    operation_id: record_trace_id(record),
                    expected_os_action: None,
                },
            ));
        }
        "assistant" => {
            let content = optional_text_content(record)
                .or_else(|| provider_error(record))
                .ok_or_else(|| {
                    format!(
                        "WorkBuddy assistant 消息缺少正文和错误 record_id={}",
                        record.id
                    )
                })?;
            let has_error = provider_error(record).is_some();
            events.push(build_event(
                record,
                context,
                EventDetails {
                    event_suffix: "llm-response",
                    user_activity_id: String::from(user_activity_id),
                    observation_layer: ObservationLayer::LlmRequestResponse,
                    observation_status: ObservationStatus::Partial,
                    content_kind: if has_error {
                        SemanticContentKind::Error
                    } else {
                        SemanticContentKind::LlmResponse
                    },
                    action_kind: ActionKind::LlmResult,
                    content: content.clone(),
                    tool_name: None,
                    operation_id: record_trace_id(record),
                    expected_os_action: None,
                },
            ));
            if is_final {
                events.push(build_event(
                    record,
                    context,
                    EventDetails {
                        event_suffix: "llm-final-result",
                        user_activity_id: String::from(user_activity_id),
                        observation_layer: ObservationLayer::LlmFinalResult,
                        observation_status: if has_error {
                            ObservationStatus::Partial
                        } else {
                            ObservationStatus::Observed
                        },
                        content_kind: SemanticContentKind::FinalResult,
                        action_kind: ActionKind::LlmResult,
                        content,
                        tool_name: None,
                        operation_id: record_trace_id(record),
                        expected_os_action: None,
                    },
                ));
            }
        }
        value => {
            return Err(format!(
                "发现未支持的 WorkBuddy 消息角色 record_id={} role={value}",
                record.id
            ));
        }
    }
    Ok(())
}

fn final_assistant_record_ids(records: &[WorkBuddyRecord]) -> HashSet<String> {
    let mut final_ids = HashSet::new();
    let mut last_assistant_id: Option<&str> = None;
    for record in records {
        if record.record_type != "message" {
            continue;
        }
        match record.role.as_deref() {
            Some("user") => {
                if let Some(record_id) = last_assistant_id.take() {
                    final_ids.insert(String::from(record_id));
                }
            }
            Some("assistant") => last_assistant_id = Some(&record.id),
            _ => {}
        }
    }
    if let Some(record_id) = last_assistant_id {
        final_ids.insert(String::from(record_id));
    }
    final_ids
}

fn collect_prompt_context_events(
    record: &WorkBuddyRecord,
    context: &EventContext<'_>,
    user_activity_id: &str,
    content: &str,
    events: &mut Vec<SemanticEvent>,
) {
    if let Some(envelope) = extract_prompt_envelope(content) {
        events.push(build_event(
            record,
            context,
            EventDetails {
                event_suffix: "prompt-envelope",
                user_activity_id: String::from(user_activity_id),
                observation_layer: ObservationLayer::BaseSystemInstructions,
                observation_status: ObservationStatus::Partial,
                content_kind: SemanticContentKind::PromptEnvelope,
                action_kind: ActionKind::LlmRequest,
                content: envelope,
                tool_name: None,
                operation_id: record_trace_id(record),
                expected_os_action: None,
            },
        ));
    }
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "product_identity",
        "base-system-product-identity",
        ObservationLayer::BaseSystemInstructions,
        SemanticContentKind::BaseSystemInstruction,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "user_info",
        "base-system-user-info",
        ObservationLayer::BaseSystemInstructions,
        SemanticContentKind::BaseSystemInstruction,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "current_time",
        "base-system-current-time",
        ObservationLayer::BaseSystemInstructions,
        SemanticContentKind::RuntimeContext,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "identity_context",
        "agent-identity-policy",
        ObservationLayer::SkillsAgentPolicy,
        SemanticContentKind::AgentPolicy,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "memory_and_skills_reminder",
        "memory-skill-policy",
        ObservationLayer::SkillsAgentPolicy,
        SemanticContentKind::SkillPolicy,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "project_layout",
        "project-layout-context",
        ObservationLayer::LlmRequestResponse,
        SemanticContentKind::ProjectContext,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "project_context",
        "project-runtime-context",
        ObservationLayer::LlmRequestResponse,
        SemanticContentKind::ProjectContext,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "additional_data",
        "additional-request-context",
        ObservationLayer::LlmRequestResponse,
        SemanticContentKind::AdditionalContext,
        events,
    );
    collect_tagged_context(
        record,
        context,
        user_activity_id,
        content,
        "connector-status",
        "available-mcp-status",
        ObservationLayer::AvailableToolsMcp,
        SemanticContentKind::AvailableMcp,
        events,
    );

    let attachments = record
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|block| {
            !matches!(
                block.block_type.as_deref(),
                Some("input_text" | "output_text" | "reasoning_text")
            )
        })
        .map(attachment_description)
        .collect::<Vec<_>>();
    if !attachments.is_empty() {
        events.push(build_event(
            record,
            context,
            EventDetails {
                event_suffix: "attachments",
                user_activity_id: String::from(user_activity_id),
                observation_layer: ObservationLayer::UserPrompt,
                observation_status: ObservationStatus::Partial,
                content_kind: SemanticContentKind::Attachment,
                action_kind: ActionKind::UserInput,
                content: attachments.join("\n"),
                tool_name: None,
                operation_id: record_trace_id(record),
                expected_os_action: None,
            },
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_tagged_context(
    record: &WorkBuddyRecord,
    context: &EventContext<'_>,
    user_activity_id: &str,
    content: &str,
    tag: &str,
    event_suffix: &'static str,
    observation_layer: ObservationLayer,
    content_kind: SemanticContentKind,
    events: &mut Vec<SemanticEvent>,
) {
    let Some(value) = extract_tag_content(content, tag) else {
        return;
    };
    events.push(build_event(
        record,
        context,
        EventDetails {
            event_suffix,
            user_activity_id: String::from(user_activity_id),
            observation_layer,
            observation_status: ObservationStatus::Partial,
            content_kind,
            action_kind: ActionKind::LlmRequest,
            content: value,
            tool_name: None,
            operation_id: record_trace_id(record),
            expected_os_action: None,
        },
    ));
}

fn extract_prompt_envelope(content: &str) -> Option<String> {
    const START: &str = "<user_query>";
    const END: &str = "</user_query>";
    let start = content.find(START)?;
    let end = content[start + START.len()..].find(END)? + start + START.len();
    let suffix_start = end + END.len();
    Some(format!("{}{}", &content[..start], &content[suffix_start..]))
}

fn extract_tag_content(content: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = content.find(&start_tag)? + start_tag.len();
    let end = content[start..].find(&end_tag)? + start;
    Some(String::from(content[start..end].trim()))
}

fn attachment_description(block: &ContentBlock) -> String {
    format!(
        "type={} name={} path={} url={} mime_type={}",
        block.block_type.as_deref().unwrap_or("unknown"),
        block.name.as_deref().unwrap_or("unknown"),
        block.path.as_deref().unwrap_or("unknown"),
        block.url.as_deref().unwrap_or("unknown"),
        block.mime_type.as_deref().unwrap_or("unknown")
    )
}

fn reasoning_content(record: &WorkBuddyRecord) -> Option<String> {
    content_blocks_text(record.raw_content.as_deref().unwrap_or_default())
        .or_else(|| content_blocks_text(record.content.as_deref().unwrap_or_default()))
        .or_else(|| {
            record
                .provider_data
                .as_ref()
                .and_then(|value| value.reasoning.clone())
        })
}

fn provider_error(record: &WorkBuddyRecord) -> Option<String> {
    record
        .provider_data
        .as_ref()
        .and_then(|value| value.error.as_ref())
        .map(|error| match error {
            ProviderError::Message(message) => message.clone(),
            ProviderError::Details(details) => format!(
                "type={} code={} status={} request_id={} network={} stream_timeout={} retryable={} message={}",
                details.error_type.as_deref().unwrap_or("unknown"),
                details
                    .code
                    .as_ref()
                    .map(provider_error_code)
                    .unwrap_or_else(|| String::from("unknown")),
                details
                    .status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| String::from("unknown")),
                details.request_id.as_deref().unwrap_or("unknown"),
                details
                    .is_network_error
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| String::from("unknown")),
                details
                    .is_stream_timeout
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| String::from("unknown")),
                details
                    .is_retryable
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| String::from("unknown")),
                details.message.as_deref().unwrap_or("unknown")
            ),
        })
}

fn provider_error_code(code: &ProviderErrorCode) -> String {
    match code {
        ProviderErrorCode::Number(value) => value.to_string(),
        ProviderErrorCode::Text(value) => value.clone(),
    }
}

fn build_event(
    record: &WorkBuddyRecord,
    context: &EventContext<'_>,
    details: EventDetails,
) -> SemanticEvent {
    let turn_id = (!details
        .user_activity_id
        .starts_with("workbuddy:unattributed:"))
    .then(|| details.user_activity_id.clone());
    SemanticEvent {
        schema_version: String::from(EVENT_SCHEMA_VERSION),
        event_id: format!(
            "workbuddy:{}:{}:{}:{}:{}:{}",
            record.session_id.as_deref().unwrap_or("no-agent-session"),
            record.id,
            record.record_type,
            record.call_id.as_deref().unwrap_or("no-call"),
            record.timestamp,
            details.event_suffix
        ),
        event_timestamp_unix_ms: record.timestamp,
        session_id: String::from(context.session_id),
        process_id: context.process_id,
        user_activity_id: details.user_activity_id,
        agent_session_id: record.session_id.clone(),
        turn_id,
        tool_call_id: record.call_id.clone(),
        workspace_path: record.cwd.clone(),
        agent_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.agent.clone()),
        provider_message_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.message_id.clone()),
        source_schema_profile: Some(String::from(SOURCE_SCHEMA_PROFILE)),
        observation_layer: Some(details.observation_layer),
        observation_status: details.observation_status,
        content_kind: Some(details.content_kind),
        action_kind: details.action_kind,
        content: details.content,
        tool_name: details.tool_name,
        operation_id_origin: operation_id_origin(record, details.operation_id.as_deref()),
        operation_id: details.operation_id,
        native_evidence: native_evidence(record),
        expected_os_action: details.expected_os_action,
        evidence_source: EvidenceSource::AgentSemantic,
        source_record_id: record.id.clone(),
        parent_record_id: record.parent_id.clone(),
        trace_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.trace_id.clone()),
        model_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.model.clone()),
        request_model_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.request_model_id.clone()),
        request_model_name: record
            .provider_data
            .as_ref()
            .and_then(|value| value.request_model_name.clone()),
        conversation_request_id: record
            .provider_data
            .as_ref()
            .and_then(|value| value.conversation_request_id.clone()),
        record_status: record.status.clone(),
        error: provider_error(record),
        input_tokens: input_tokens(record),
        output_tokens: output_tokens(record),
        total_tokens: total_tokens(record),
        cached_tokens: cached_tokens(record),
        reasoning_tokens: reasoning_tokens(record),
        request_count: record
            .provider_data
            .as_ref()
            .and_then(|value| value.usage.as_ref())
            .and_then(|value| value.requests),
    }
}

fn operation_id_origin(
    record: &WorkBuddyRecord,
    operation_id: Option<&str>,
) -> Option<OperationIdOrigin> {
    let value = operation_id?;
    if record.call_id.as_deref() == Some(value) {
        return Some(OperationIdOrigin::WorkBuddyToolCallId);
    }
    if record
        .provider_data
        .as_ref()
        .and_then(|provider| provider.trace_id.as_deref())
        == Some(value)
    {
        return Some(OperationIdOrigin::WorkBuddyProviderTraceId);
    }
    None
}

fn native_evidence(record: &WorkBuddyRecord) -> NativeEvidence {
    let Some(session_id) = record.session_id.as_deref() else {
        return NativeEvidence::default();
    };
    let scope = format!("workbuddy-session:{session_id}");
    let record_id = native_identifier(NativeIdKind::Record, &scope, &record.id);
    let mut identifiers = vec![
        native_identifier(NativeIdKind::AgentSession, &scope, session_id),
        record_id.clone(),
    ];
    let mut edges = Vec::new();
    if let Some(parent_id) = record.parent_id.as_deref() {
        let parent = native_identifier(NativeIdKind::Record, &scope, parent_id);
        identifiers.push(parent.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::ExplicitParent,
            from: record_id.clone(),
            to: parent,
            evidence_fields: vec![String::from("id"), String::from("parentId")],
        });
    }
    let call_id = record.call_id.as_deref().map(|value| {
        let identifier = native_identifier(NativeIdKind::ToolCall, &scope, value);
        identifiers.push(identifier.clone());
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: record_id.clone(),
            to: identifier.clone(),
            evidence_fields: vec![String::from("id"), String::from("callId")],
        });
        identifier
    });
    let trace_id = record
        .provider_data
        .as_ref()
        .and_then(|provider| provider.trace_id.as_deref())
        .map(|value| {
            let identifier = native_identifier(NativeIdKind::ProviderTrace, &scope, value);
            identifiers.push(identifier.clone());
            identifier
        });
    let conversation_request_id = record
        .provider_data
        .as_ref()
        .and_then(|provider| provider.conversation_request_id.as_deref())
        .map(|value| {
            let identifier = native_identifier(NativeIdKind::ConversationRequest, &scope, value);
            identifiers.push(identifier.clone());
            identifier
        });
    if let (Some(call_id), Some(trace_id)) = (&call_id, &trace_id) {
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: call_id.clone(),
            to: trace_id.clone(),
            evidence_fields: vec![String::from("callId"), String::from("providerData.traceId")],
        });
    }
    if let (Some(trace_id), Some(conversation_request_id)) = (&trace_id, &conversation_request_id) {
        edges.push(CausalEdge {
            kind: CausalEdgeKind::CoRecordedMapping,
            from: trace_id.clone(),
            to: conversation_request_id.clone(),
            evidence_fields: vec![
                String::from("providerData.traceId"),
                String::from("providerData.conversationRequestId"),
            ],
        });
    }
    NativeEvidence { identifiers, edges }
}

fn native_identifier(kind: NativeIdKind, scope: &str, value: &str) -> NativeIdentifier {
    NativeIdentifier {
        source: NativeIdSource::WorkBuddy,
        kind,
        scope: String::from(scope),
        value: String::from(value),
    }
}

fn input_tokens(record: &WorkBuddyRecord) -> Option<u64> {
    let provider = record.provider_data.as_ref()?;
    provider
        .raw_usage
        .as_ref()
        .and_then(|usage| usage.prompt_tokens)
        .or_else(|| provider.usage.as_ref().and_then(|usage| usage.input_tokens))
}

fn output_tokens(record: &WorkBuddyRecord) -> Option<u64> {
    let provider = record.provider_data.as_ref()?;
    provider
        .raw_usage
        .as_ref()
        .and_then(|usage| usage.completion_tokens)
        .or_else(|| {
            provider
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens)
        })
}

fn total_tokens(record: &WorkBuddyRecord) -> Option<u64> {
    let provider = record.provider_data.as_ref()?;
    provider
        .raw_usage
        .as_ref()
        .and_then(|usage| usage.total_tokens)
        .or_else(|| provider.usage.as_ref().and_then(|usage| usage.total_tokens))
}

fn cached_tokens(record: &WorkBuddyRecord) -> Option<u64> {
    let provider = record.provider_data.as_ref()?;
    let raw = provider.raw_usage.as_ref();
    let normalized = provider.usage.as_ref();
    [
        raw.and_then(|usage| usage.cached_tokens),
        raw.and_then(|usage| usage.cache_read_input_tokens),
        raw.and_then(|usage| usage.prompt_cache_hit_tokens),
        raw.and_then(|usage| usage.prompt_tokens_details.as_ref())
            .and_then(|details| details.cached_tokens),
        normalized
            .and_then(|usage| usage.input_token_details.as_deref())
            .and_then(max_cached_tokens),
    ]
    .into_iter()
    .flatten()
    .max()
}

fn reasoning_tokens(record: &WorkBuddyRecord) -> Option<u64> {
    let provider = record.provider_data.as_ref()?;
    let raw = provider.raw_usage.as_ref();
    let normalized = provider.usage.as_ref();
    [
        raw.and_then(|usage| usage.completion_thinking_tokens),
        raw.and_then(|usage| usage.completion_tokens_details.as_ref())
            .and_then(|details| details.reasoning_tokens),
        normalized
            .and_then(|usage| usage.output_token_details.as_deref())
            .and_then(max_reasoning_tokens),
    ]
    .into_iter()
    .flatten()
    .max()
}

fn max_cached_tokens(details: &[TokenDetails]) -> Option<u64> {
    details.iter().filter_map(|value| value.cached_tokens).max()
}

fn max_reasoning_tokens(details: &[TokenDetails]) -> Option<u64> {
    details
        .iter()
        .filter_map(|value| value.reasoning_tokens)
        .max()
}

fn required<'a>(
    value: Option<&'a str>,
    record: &WorkBuddyRecord,
    field: &str,
) -> Result<&'a str, String> {
    value.ok_or_else(|| {
        format!(
            "WorkBuddy 语义记录缺少字段 record_id={} type={} field={field}",
            record.id, record.record_type
        )
    })
}

fn required_text_content(record: &WorkBuddyRecord) -> Result<String, String> {
    let blocks = record.content.as_ref().ok_or_else(|| {
        format!(
            "WorkBuddy 消息缺少 content record_id={} role={}",
            record.id,
            record.role.as_deref().unwrap_or("missing")
        )
    })?;
    if blocks.is_empty() {
        return Err(format!(
            "WorkBuddy 消息 content 为空 record_id={}",
            record.id
        ));
    }
    content_blocks_text(blocks).ok_or_else(|| {
        format!(
            "WorkBuddy 消息没有可读取的文本 content record_id={} role={}",
            record.id,
            record.role.as_deref().unwrap_or("missing")
        )
    })
}

fn optional_text_content(record: &WorkBuddyRecord) -> Option<String> {
    content_blocks_text(record.content.as_deref().unwrap_or_default())
}

fn required_output(record: &WorkBuddyRecord) -> Result<String, String> {
    let output = record.output.as_ref().ok_or_else(|| {
        format!(
            "WorkBuddy ToolResult 缺少 output record_id={} tool={}",
            record.id,
            record.name.as_deref().unwrap_or("missing")
        )
    })?;
    let text = match output {
        ToolOutput::Single(block) => content_blocks_text(std::slice::from_ref(block)),
        ToolOutput::Multiple(blocks) => content_blocks_text(blocks),
    };
    let Some(text) = text else {
        return Err(format!(
            "WorkBuddy ToolResult output 为空 record_id={} tool={}",
            record.id,
            record.name.as_deref().unwrap_or("missing")
        ));
    };
    Ok(text)
}

fn content_blocks_text(blocks: &[ContentBlock]) -> Option<String> {
    let content = blocks
        .iter()
        .filter_map(|block| block.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    (!content.is_empty()).then_some(content)
}

fn extract_user_query(
    content: &str,
    record: &WorkBuddyRecord,
) -> Result<(String, ObservationStatus), String> {
    const START: &str = "<user_query>";
    const END: &str = "</user_query>";
    let start_position = content.find(START);
    let end_position = content.find(END);
    if start_position.is_none() && end_position.is_none() {
        return Ok((String::from(content), ObservationStatus::Partial));
    }
    let start = start_position.ok_or_else(|| {
        format!(
            "WorkBuddy 用户消息缺少 user_query 起始标签 record_id={}",
            record.id
        )
    })? + START.len();
    let end = content[start..].find(END).ok_or_else(|| {
        format!(
            "WorkBuddy 用户消息缺少 user_query 结束标签 record_id={}",
            record.id
        )
    })? + start;
    Ok((
        String::from(&content[start..end]),
        ObservationStatus::Observed,
    ))
}

fn record_trace_id(record: &WorkBuddyRecord) -> Option<String> {
    record
        .provider_data
        .as_ref()
        .and_then(|value| value.trace_id.clone())
}

fn expected_os_action(tool_name: &str) -> Option<ActionKind> {
    match tool_name {
        "Read" => Some(ActionKind::FileRead),
        "Write" | "Edit" => Some(ActionKind::FileWrite),
        "Bash" | "Cmd" | "Shell" | "PowerShell" => Some(ActionKind::ProcessStart),
        "WebSearch" => Some(ActionKind::NetworkConnect),
        _ => None,
    }
}

fn write_events(path: &Path, events: &[SemanticEvent]) -> Result<(), String> {
    let file = File::create(path).map_err(|error| {
        format!(
            "无法创建 WorkBuddy 语义输出 path={} error={error}",
            path.display()
        )
    })?;
    let mut writer = BufWriter::new(file);
    for event in events {
        serde_json::to_writer(&mut writer, event).map_err(|error| {
            format!(
                "无法序列化 WorkBuddy 语义事件 path={} error={error}",
                path.display()
            )
        })?;
        writer.write_all(b"\n").map_err(|error| {
            format!(
                "无法写入 WorkBuddy 语义事件 path={} error={error}",
                path.display()
            )
        })?;
    }
    writer.flush().map_err(|error| {
        format!(
            "无法刷新 WorkBuddy 语义输出 path={} error={error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::{WorkBuddyRecord, collect_events, records_in_window, validate_record_schema};
    use native_contracts::{ActionKind, ObservationLayer, SemanticContentKind};

    #[test]
    fn converts_prompt_user_response_tools_and_final_result() {
        let lines = [
            r#"{"id":"u1","timestamp":100,"type":"message","role":"user","content":[{"type":"input_text","text":"context<user_query>查天气</user_query>"}]}"#,
            r#"{"id":"t1","parentId":"u1","timestamp":110,"type":"function_call","name":"AskUserQuestion","callId":"call-1","arguments":"{}"}"#,
            r#"{"id":"r1","parentId":"t1","timestamp":120,"type":"function_call_result","name":"AskUserQuestion","callId":"call-1","output":{"type":"text","text":"上海"}}"#,
            r#"{"id":"a1","parentId":"r1","timestamp":130,"type":"message","role":"assistant","content":[{"type":"output_text","text":"多云"}]}"#,
        ];
        let records = lines
            .iter()
            .map(|line| serde_json::from_str::<WorkBuddyRecord>(line).expect("测试记录应可解析"))
            .collect::<Vec<_>>();
        let events = collect_events(&records, "session-1", 42).expect("测试记录应可转换");

        assert_eq!(events.len(), 9);
        assert_eq!(events[0].action_kind, ActionKind::UserInput);
        assert_eq!(events[0].content, "查天气");
        assert_eq!(
            events[1].observation_layer,
            Some(ObservationLayer::BaseSystemInstructions)
        );
        assert_eq!(events[2].action_kind, ActionKind::LlmRequest);
        assert_eq!(
            events[3].observation_layer,
            Some(ObservationLayer::AvailableToolsMcp)
        );
        assert_eq!(events[4].action_kind, ActionKind::ToolCall);
        assert_eq!(events[5].action_kind, ActionKind::UserInput);
        assert_eq!(events[6].action_kind, ActionKind::ToolResult);
        assert_eq!(events[6].expected_os_action, None);
        assert_eq!(events[7].action_kind, ActionKind::LlmResult);
        assert_eq!(
            events[8].observation_layer,
            Some(ObservationLayer::LlmFinalResult)
        );
        assert_eq!(events[0].user_activity_id, "u1");
        assert_eq!(events[8].user_activity_id, "u1");
    }

    #[test]
    fn excludes_records_after_observation_window() {
        let lines = [
            r#"{"id":"before","timestamp":99,"type":"message","role":"user"}"#,
            r#"{"id":"start","timestamp":100,"type":"message","role":"user"}"#,
            r#"{"id":"end","timestamp":200,"type":"message","role":"user"}"#,
            r#"{"id":"after","timestamp":201,"type":"message","role":"user"}"#,
        ];
        let records = lines
            .iter()
            .map(|line| serde_json::from_str::<WorkBuddyRecord>(line).expect("测试记录应可解析"))
            .collect::<Vec<_>>();

        let eligible_records = records_in_window(records, 100, 200);

        assert_eq!(eligible_records.len(), 2);
        assert_eq!(eligible_records[0].id, "start");
        assert_eq!(eligible_records[1].id, "end");
    }

    #[test]
    fn ignores_known_conversation_metadata() {
        let lines = [
            r#"{"id":"title","timestamp":100,"type":"ai-title"}"#,
            r#"{"id":"history","timestamp":110,"type":"file-history-snapshot"}"#,
        ];
        let records = lines
            .iter()
            .map(|line| serde_json::from_str::<WorkBuddyRecord>(line).expect("测试记录应可解析"))
            .collect::<Vec<_>>();

        let events = collect_events(&records, "session-1", 42).expect("已知元数据应被显式忽略");

        assert!(events.is_empty());
    }

    #[test]
    fn preserves_native_identity_for_parallel_calls_with_reused_record_id() {
        let lines = [
            r#"{"id":"u1","timestamp":100,"type":"message","role":"user","sessionId":"native-session","cwd":"C:\\workspace","content":[{"type":"input_text","text":"context<user_query>读取文件</user_query>"}]}"#,
            r#"{"id":"shared","parentId":"u1","timestamp":110,"type":"function_call","sessionId":"native-session","cwd":"C:\\workspace","name":"Read","callId":"call-1","arguments":"{\"file_path\":\"C:\\\\workspace\\\\a.txt\"}","providerData":{"agent":"cli","messageId":"message-1"}}"#,
            r#"{"id":"shared","parentId":"u1","timestamp":110,"type":"function_call","sessionId":"native-session","cwd":"C:\\workspace","name":"Read","callId":"call-2","arguments":"{\"file_path\":\"C:\\\\workspace\\\\b.txt\"}","providerData":{"agent":"cli","messageId":"message-1"}}"#,
        ];
        let records = lines
            .iter()
            .map(|line| serde_json::from_str::<WorkBuddyRecord>(line).expect("测试记录应可解析"))
            .collect::<Vec<_>>();

        let events = collect_events(&records, "observer-session", 42).expect("并行调用应可转换");
        let event_ids = events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<HashSet<_>>();
        let tool_events = events
            .iter()
            .filter(|event| event.content_kind == Some(SemanticContentKind::ToolCall))
            .collect::<Vec<_>>();

        assert_eq!(event_ids.len(), events.len());
        assert_eq!(tool_events.len(), 2);
        assert_eq!(
            tool_events[0].agent_session_id.as_deref(),
            Some("native-session")
        );
        assert_eq!(tool_events[0].turn_id.as_deref(), Some("u1"));
        assert_eq!(
            tool_events[0].workspace_path.as_deref(),
            Some("C:\\workspace")
        );
        assert_eq!(tool_events[0].agent_id.as_deref(), Some("cli"));
        assert_eq!(
            tool_events[0].provider_message_id.as_deref(),
            Some("message-1")
        );
        assert_ne!(tool_events[0].tool_call_id, tool_events[1].tool_call_id);
    }

    #[test]
    fn captures_known_request_context_and_extended_usage() {
        let lines = [
            r#"{"id":"u1","timestamp":100,"type":"message","role":"user","sessionId":"native-session","cwd":"C:\\workspace","content":[{"type":"input_text","text":"<current_time>now</current_time><project_layout>layout</project_layout><project_context>context</project_context><additional_data>extra</additional_data><user_query>回答</user_query>"}]}"#,
            r#"{"id":"a1","parentId":"u1","timestamp":110,"type":"message","role":"assistant","sessionId":"native-session","cwd":"C:\\workspace","status":"completed","content":[{"type":"output_text","text":"完成"}],"providerData":{"model":"model-1","rawUsage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"cached_tokens":3,"completion_thinking_tokens":2},"usage":{"inputTokens":10,"outputTokens":4,"totalTokens":14,"requests":1,"inputTokensDetails":[{"cached_tokens":3}],"outputTokensDetails":[{"reasoning_tokens":2}]}}}"#,
        ];
        let records = lines
            .iter()
            .map(|line| serde_json::from_str::<WorkBuddyRecord>(line).expect("测试记录应可解析"))
            .collect::<Vec<_>>();

        let events = collect_events(&records, "observer-session", 42).expect("上下文应可转换");
        let kinds = events
            .iter()
            .filter_map(|event| event.content_kind)
            .collect::<Vec<_>>();
        let response = events
            .iter()
            .find(|event| event.content_kind == Some(SemanticContentKind::LlmResponse))
            .expect("应生成模型响应事件");

        assert!(kinds.contains(&SemanticContentKind::RuntimeContext));
        assert!(kinds.contains(&SemanticContentKind::ProjectContext));
        assert!(kinds.contains(&SemanticContentKind::AdditionalContext));
        assert_eq!(response.input_tokens, Some(10));
        assert_eq!(response.output_tokens, Some(4));
        assert_eq!(response.total_tokens, Some(14));
        assert_eq!(response.cached_tokens, Some(3));
        assert_eq!(response.reasoning_tokens, Some(2));
        assert_eq!(response.request_count, Some(1));
    }

    #[test]
    fn rejects_unknown_private_schema_fields() {
        let value = serde_json::json!({
            "id": "u1",
            "timestamp": 100,
            "type": "message",
            "unexpected": true
        });

        let error = validate_record_schema(&value, Path::new("fixture.jsonl"), 1)
            .expect_err("未知私有字段必须显式失败");

        assert!(error.contains("unexpected"));
        assert!(error.contains("workbuddy-private-jsonl-observed-v1"));
    }
}
