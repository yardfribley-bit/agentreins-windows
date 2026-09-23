use native_contracts::{AgentCapabilityKind, AgentKind, CausalEdge};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSnapshot {
    pub schema_version: &'static str,
    pub generated_at_unix_ms: u64,
    pub observer: ObserverHealthViewModel,
    pub agent: AgentSummaryViewModel,
    pub permission: PermissionSnapshotViewModel,
    pub capability_audit: CapabilityAuditViewModel,
    pub activities: Vec<ActivityViewModel>,
    pub capabilities: Vec<CapabilityViewModel>,
    pub coverage: CoverageViewModel,
    pub evidence: EvidenceFeedViewModel,
    pub diagnostics: DiagnosticsViewModel,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPageViewModel {
    pub items: Vec<ActivityViewModel>,
    pub total: u64,
    pub next_cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidencePageViewModel {
    pub items: Vec<EvidenceViewModel>,
    pub total: u64,
    pub next_cursor: Option<String>,
    pub scope: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityAuditViewModel {
    pub observed: bool,
    pub mode: &'static str,
    pub source: String,
    pub enforcement_actions_applied: u64,
    pub records: Vec<CapabilityAuditRecordViewModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityAuditRecordViewModel {
    pub request_id: String,
    pub decision_id: String,
    pub event_timestamp_unix_ms: u64,
    pub session_id: String,
    pub user_activity_id: Option<String>,
    pub capability: String,
    pub operation: String,
    pub resource_kind: String,
    pub resource_identifier: String,
    pub disposition: String,
    pub rule_id: String,
    pub reason: String,
    pub required_grant: String,
    pub recovery: String,
    pub applied: bool,
    pub evidence_event_ids: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSnapshotViewModel {
    pub observed: bool,
    pub processes: Vec<ProcessPermissionViewModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessPermissionViewModel {
    pub process_id: u32,
    pub process_instance_id: String,
    pub is_elevated: bool,
    pub integrity_level: String,
    pub integrity_rid: u32,
    pub privileges: Vec<TokenPrivilegeViewModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPrivilegeViewModel {
    pub name: String,
    pub enabled: bool,
    pub enabled_by_default: bool,
    pub removed: bool,
    pub used_for_access: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserverState {
    Healthy,
    Idle,
    Starting,
    Degraded,
    Unknown,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserverRuntimeState {
    NotInstalled,
    Starting,
    Running,
    Idle,
    Degraded,
    Stopped,
    IdentityMismatch,
    PermissionRequired,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserverRuntimeViewModel {
    pub schema_version: String,
    pub updated_at_unix_ms: u64,
    pub state: ObserverRuntimeState,
    pub os_observer_state: ObserverRuntimeState,
    pub semantic_observer_state: ObserverRuntimeState,
    pub session_id: Option<String>,
    pub detail: String,
    pub applied: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    PassedWithBoundaries,
    EvidenceDegraded,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserverHealthViewModel {
    pub state: ObserverState,
    pub reason: String,
    pub last_healthy_at_unix_ms: u64,
    pub events_lost: u64,
    pub parse_failures: u64,
    pub write_failures: u64,
    pub queue_drops: u64,
    pub identity_collisions: u64,
    pub clock_skew_ms: Option<u64>,
    pub semantic_events: u64,
    pub os_events: u64,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationState {
    Confirmed,
    Partial,
    Unlinked,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummaryViewModel {
    pub id: &'static str,
    pub kind: AgentKind,
    pub name: &'static str,
    pub product_version: String,
    pub publisher: String,
    pub executable_path: String,
    pub sha256: String,
    pub binding: AssociationState,
    pub root_instances: u64,
    pub active_processes: u64,
    pub workspace: Option<String>,
    pub semantic_session_id: String,
    pub os_session_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCountsViewModel {
    pub tools: u64,
    pub processes: Option<u64>,
    pub files: Option<u64>,
    pub network: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationCountsViewModel {
    pub confirmed: u64,
    pub partial: u64,
    pub unlinked: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityViewModel {
    pub id: String,
    pub title: String,
    pub request: String,
    pub started_at_unix_ms: u64,
    pub ended_at_unix_ms: u64,
    pub final_result: Option<String>,
    pub coverage: CoverageState,
    pub counts: ActivityCountsViewModel,
    pub associations: AssociationCountsViewModel,
    pub unattributed_events: u64,
    pub limitation: &'static str,
    pub capabilities: Vec<AgentCapabilityKind>,
    pub timeline: Vec<TimelineItemViewModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItemViewModel {
    pub id: String,
    pub phase: String,
    pub title: String,
    pub detail: String,
    pub timestamp_unix_ms: u64,
    pub semantic_observed: bool,
    pub correlation_relevant: bool,
    pub correlation_state: Option<AssociationState>,
    pub source: String,
    pub native_call_id: Option<String>,
    pub linked_os_events: u64,
    pub clock_basis: &'static str,
    pub correlation_basis: Option<String>,
    pub linked_os_event_ids: Vec<String>,
    pub first_breakpoint: Option<String>,
    pub causal_edges: Vec<CausalEdge>,
    pub mcp_json_rpc_request_id: Option<String>,
    pub mcp_process_id: Option<u32>,
    pub mcp_response_observed: Option<bool>,
    pub limitation: Option<String>,
    pub capabilities: Vec<AgentCapabilityKind>,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Observed,
    Partial,
    NotObservable,
    NotTriggered,
    Degraded,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityGroup {
    ExecutionAccess,
    ExtensionTool,
    AutonomousRun,
    DataContext,
    RuntimeEnvironment,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFacetViewModel {
    pub state: CoverageState,
    pub source: String,
    pub evidence_count: u64,
    pub limitation: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityFacetsViewModel {
    pub invocation: CapabilityFacetViewModel,
    pub parameters_target: CapabilityFacetViewModel,
    pub agent_feedback: CapabilityFacetViewModel,
    pub native_identity: CapabilityFacetViewModel,
    pub runtime_corroboration: CapabilityFacetViewModel,
    pub data_health: CapabilityFacetViewModel,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityViewModel {
    pub kind: AgentCapabilityKind,
    pub name: &'static str,
    pub group: CapabilityGroup,
    pub state: CoverageState,
    pub metric: String,
    pub summary: &'static str,
    pub facets: CapabilityFacetsViewModel,
    pub related_activity_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageViewModel {
    pub summary: CoverageSummaryViewModel,
    pub layers: Vec<CoverageLayerViewModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummaryViewModel {
    pub observed: u64,
    pub partial: u64,
    pub not_observable: u64,
    pub not_triggered: u64,
    pub degraded: u64,
    pub gate_status: GateStatus,
    pub gate_reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageLayerViewModel {
    pub index: u8,
    pub name: &'static str,
    pub state: CoverageState,
    pub source: String,
    pub last_healthy_at_unix_ms: u64,
    pub limitation: &'static str,
    pub impact: &'static str,
    pub evidence_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Semantic,
    Process,
    File,
    Network,
    Health,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceContentMode {
    RawLocal,
    MetadataOnly,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceViewModel {
    pub id: String,
    pub kind: EvidenceKind,
    pub title: String,
    pub content: String,
    pub source: String,
    pub timestamp_unix_ms: u64,
    pub content_mode: EvidenceContentMode,
    pub observed: bool,
    pub correlation_state: Option<AssociationState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceFeedViewModel {
    pub items: Vec<EvidenceViewModel>,
    pub semantic_event_count: u64,
    pub provider_os_event_count: u64,
    pub published_os_event_count: u64,
    pub displayed_os_event_count: u64,
    pub os_scope: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsViewModel {
    pub os_run_root: String,
    pub semantic_run_root: String,
    pub os_manifest: String,
    pub semantic_source: String,
    pub mcp_source: String,
    pub mcp_protocol_records: u64,
    pub mcp_publication_boundary: String,
    pub capability_audit_source: String,
    pub os_coverage_status: String,
    pub semantic_coverage_status: String,
    pub provider_os_event_count: u64,
    pub target_os_event_count: u64,
    pub data_policy: &'static str,
}
