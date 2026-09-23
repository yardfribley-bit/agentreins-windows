use serde::{Deserialize, Serialize};

pub const EVENT_SCHEMA_VERSION: &str = "0.6.0";
pub const EVENT_CLOCK_SKEW_LIMIT_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationLayer {
    UserPrompt,
    BaseSystemInstructions,
    SkillsAgentPolicy,
    AvailableToolsMcp,
    LlmRequestResponse,
    ToolMcpCalls,
    ProcessScript,
    FilesystemCredential,
    Network,
    ToolResult,
    LlmFinalResult,
    EvidenceCoverage,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Observed,
    #[default]
    Partial,
    Missing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    Detail,
    Summary,
    Review,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct EvidenceRetention {
    pub class: RetentionClass,
    pub reason: String,
    pub resource_category: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticContentKind {
    RawUserPrompt,
    Attachment,
    PromptEnvelope,
    BaseSystemInstruction,
    RuntimeContext,
    ProjectContext,
    AdditionalContext,
    AgentPolicy,
    SkillPolicy,
    AvailableTool,
    AvailableMcp,
    LlmRequest,
    LlmResponse,
    ToolCall,
    ToolResult,
    FinalResult,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    WorkBuddy,
    Codex,
    ClaudeCode,
    ClaudeDesktop,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapabilityKind {
    Shell,
    File,
    Browser,
    Mcp,
    Skill,
    Credential,
    Memory,
    SubAgent,
    Scheduler,
    Hook,
    Sandbox,
    Permission,
    RuntimeEnforcement,
    Token,
    Cache,
    LocalResourceUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    Candidate,
    Verified,
    Inherited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPhase {
    Live,
    Rundown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ProcessStart,
    ProcessStop,
    FileOpen,
    FileRead,
    FileWrite,
    FileDelete,
    FileRename,
    FileOperationResult,
    NetworkConnect,
    NetworkAccept,
    NetworkSend,
    NetworkReceive,
    NetworkDisconnect,
    UserInput,
    LlmRequest,
    ToolCall,
    ToolResult,
    LlmResult,
    CredentialObserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Process,
    File,
    Network,
    Credential,
    Package,
    Tool,
    Llm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationMode {
    Observe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOutcome {
    Observed,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Etw,
    AgentSemantic,
    SyntheticFixture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeIdSource {
    WorkBuddy,
    McpJsonRpc,
    Etw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeIdKind {
    AgentSession,
    Record,
    ToolCall,
    ProviderTrace,
    ConversationRequest,
    JsonRpcRequest,
    JsonRpcResponse,
    Activity,
    RelatedActivity,
    ProcessStartKey,
    UniqueProcessKey,
    Process,
    ParentProcess,
    Thread,
    Irp,
    FileObject,
    TcpConnection,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct NativeIdentifier {
    pub source: NativeIdSource,
    pub kind: NativeIdKind,
    pub scope: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalEdgeKind {
    SameNativeId,
    ExplicitParent,
    RequestResponse,
    RuntimeOwnership,
    Lifecycle,
    CoRecordedMapping,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CausalEdge {
    pub kind: CausalEdgeKind,
    pub from: NativeIdentifier,
    pub to: NativeIdentifier,
    pub evidence_fields: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct NativeEvidence {
    pub identifiers: Vec<NativeIdentifier>,
    pub edges: Vec<CausalEdge>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationIdOrigin {
    WorkBuddyToolCallId,
    WorkBuddyProviderTraceId,
    EtwIrp,
    EtwTcpConnectionId,
    AgentReinsGenerated,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentIdentity {
    pub executable_path: String,
    pub file_version: String,
    pub sha256: String,
    pub signer_subject: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Actor {
    pub agent_kind: AgentKind,
    pub identity_status: IdentityStatus,
    pub root_process_instance_id: String,
    pub identity: AgentIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SessionRef {
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProcessRef {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub unique_process_key: Option<u64>,
    pub process_instance_id: String,
    pub process_session_id: Option<u32>,
    pub user_sid: Option<String>,
    pub image_name: String,
    pub image_path: Option<String>,
    pub command_line_observed: bool,
    pub command_line: Option<String>,
    pub exit_status: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Action {
    pub kind: ActionKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Resource {
    pub kind: ResourceKind,
    pub identifier: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Destination {
    pub kind: ResourceKind,
    pub identifier: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObservationResult {
    pub status_code: Option<u32>,
    pub success: Option<bool>,
    pub bytes_transferred: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Verdict {
    pub mode: ObservationMode,
    pub outcome: ObservationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub provider_id: String,
    pub provider_event_id: u16,
    pub provider_opcode: u8,
    pub provider_event_version: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct EventOccurrence {
    pub event_id: String,
    pub event_timestamp_unix_ms: u64,
    pub observed_at_unix_ms: u64,
    pub operation_id: Option<String>,
    pub bytes_transferred: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct EventAggregation {
    pub occurrence_count: u64,
    pub first_event_timestamp_unix_ms: u64,
    pub last_event_timestamp_unix_ms: u64,
    pub total_bytes_transferred: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<Vec<EventOccurrence>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProcessEvidenceEvent {
    pub schema_version: String,
    pub event_id: String,
    pub event_timestamp_unix_ms: u64,
    pub observed_at_unix_ms: u64,
    pub phase: EventPhase,
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id_origin: Option<OperationIdOrigin>,
    #[serde(default)]
    pub native_evidence: NativeEvidence,
    pub session: SessionRef,
    pub process: ProcessRef,
    pub action: Action,
    pub resource: Resource,
    pub destination: Option<Destination>,
    pub result: ObservationResult,
    pub evidence: Evidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<EvidenceRetention>,
    #[serde(default)]
    pub aggregation: Option<EventAggregation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObservationEvent {
    pub schema_version: String,
    pub event_id: String,
    pub event_timestamp_unix_ms: u64,
    pub observed_at_unix_ms: u64,
    pub phase: EventPhase,
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id_origin: Option<OperationIdOrigin>,
    #[serde(default)]
    pub native_evidence: NativeEvidence,
    pub actor: Actor,
    pub session: SessionRef,
    pub process: ProcessRef,
    pub action: Action,
    pub resource: Resource,
    pub destination: Option<Destination>,
    pub result: ObservationResult,
    pub verdict: Verdict,
    pub evidence: Evidence,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<EvidenceRetention>,
    #[serde(default)]
    pub aggregation: Option<EventAggregation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SemanticEvent {
    pub schema_version: String,
    pub event_id: String,
    pub event_timestamp_unix_ms: u64,
    pub session_id: String,
    pub process_id: u32,
    #[serde(default)]
    pub user_activity_id: String,
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub source_schema_profile: Option<String>,
    #[serde(default)]
    pub observation_layer: Option<ObservationLayer>,
    #[serde(default)]
    pub observation_status: ObservationStatus,
    #[serde(default)]
    pub content_kind: Option<SemanticContentKind>,
    pub action_kind: ActionKind,
    pub content: String,
    pub tool_name: Option<String>,
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id_origin: Option<OperationIdOrigin>,
    #[serde(default)]
    pub native_evidence: NativeEvidence,
    pub expected_os_action: Option<ActionKind>,
    pub evidence_source: EvidenceSource,
    pub source_record_id: String,
    pub parent_record_id: Option<String>,
    pub trace_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub request_model_id: Option<String>,
    #[serde(default)]
    pub request_model_name: Option<String>,
    #[serde(default)]
    pub conversation_request_id: Option<String>,
    #[serde(default)]
    pub record_status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cached_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub request_count: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::ObservationEvent;

    #[test]
    fn reads_pre_aggregation_observation_event() {
        let event: ObservationEvent = serde_json::from_str(include_str!(
            "../../../tests/fixtures/contracts/observation-event-v0.2.0.json"
        ))
        .expect("0.2.0 观测事件应保持可读");

        assert_eq!(event.schema_version, "0.2.0");
        assert_eq!(event.aggregation, None);
    }
}
