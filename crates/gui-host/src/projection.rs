use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use evidence_correlator::{
    CorrelationResult, CorrelationStatus, McpProtocolIndex, McpProtocolRecord, OsEventIndex,
    ProcessRootAssociation, SemanticEventIndex, correlate_semantic_evidence,
};
use native_contracts::{
    ActionKind, AgentCapabilityKind, AgentIdentity, AgentKind, EVENT_CLOCK_SKEW_LIMIT_MS,
    ObservationEvent, ObservationLayer, ProcessEvidenceEvent, SemanticContentKind, SemanticEvent,
};
use serde::{Deserialize, Serialize};

use crate::capabilities::{LocalResourceSnapshot, build_capability_views, classify_semantic_event};
use crate::model::{
    ActivityCountsViewModel, ActivityViewModel, AgentSummaryViewModel, ApplicationSnapshot,
    AssociationCountsViewModel, AssociationState, CapabilityAuditRecordViewModel,
    CapabilityAuditViewModel, CoverageLayerViewModel, CoverageState, CoverageSummaryViewModel,
    CoverageViewModel, DiagnosticsViewModel, EvidenceContentMode, EvidenceFeedViewModel,
    EvidenceKind, EvidenceViewModel, GateStatus, ObserverHealthViewModel, ObserverRuntimeState,
    ObserverRuntimeViewModel, ObserverState, PermissionSnapshotViewModel,
    ProcessPermissionViewModel, TimelineItemViewModel, TokenPrivilegeViewModel,
};

const DISPLAYED_OS_EVENT_LIMIT: usize = 200;
const CORRELATION_WINDOW_MS: u64 = 5_000;
const CAPABILITY_AUDIT_FILE_NAME: &str = "capability-audit.json";

pub struct ProjectionConfiguration {
    pub os_run_root: PathBuf,
    pub semantic_run_root: PathBuf,
    pub mcp_manifest: Option<PathBuf>,
}

#[derive(Default)]
pub struct ProjectionCache {
    os_evidence: Mutex<Option<CachedOsEvidence>>,
}

#[derive(Deserialize)]
struct OsObserverState {
    identity_path: PathBuf,
    health_output_path: PathBuf,
    output_directory: PathBuf,
    health_interval_seconds: u64,
}

#[derive(Deserialize)]
struct SemanticObserverState {
    health_output_path: PathBuf,
    semantic_manifest_path: PathBuf,
    session_id: String,
    poll_interval_seconds: u64,
}

#[derive(Deserialize, serde::Serialize)]
struct EtwHealth {
    events_lost: u64,
    log_buffers_lost: u64,
    realtime_buffers_lost: u64,
}

#[derive(Deserialize, serde::Serialize)]
struct OsHealthSnapshot {
    captured_at_unix_ms: u64,
    session_id: String,
    coverage_status: String,
    active_workbuddy_root_instances: u64,
    active_workbuddy_processes: u64,
    event_clock_skew_ms: Option<u64>,
    identity_collision_count: u64,
    parse_failures: u64,
    output_batches_dropped: u64,
    provider_events_received: u64,
    full_events_written: u64,
    write_failures: u64,
    etw: EtwHealth,
    #[serde(default)]
    agent_resource_sample: Option<AgentResourceHealthSample>,
}

#[derive(Deserialize, serde::Serialize)]
struct AgentResourceHealthSample {
    process_count: u64,
    unavailable_process_count: u64,
    working_set_bytes: u64,
    private_memory_bytes: u64,
    cpu_percent_normalized: Option<f64>,
    job_process_count: u64,
    app_container_process_count: u64,
    elevated_process_count: u64,
    #[serde(default)]
    permission_processes: Vec<ProcessPermissionHealthSample>,
}

#[derive(Deserialize, serde::Serialize)]
struct ProcessPermissionHealthSample {
    process_id: u32,
    process_instance_id: String,
    is_elevated: bool,
    integrity_level: String,
    integrity_rid: u32,
    privileges: Vec<TokenPrivilegeHealthSample>,
}

#[derive(Deserialize, serde::Serialize)]
struct TokenPrivilegeHealthSample {
    name: String,
    enabled: bool,
    enabled_by_default: bool,
    removed: bool,
    used_for_access: bool,
}

#[derive(Deserialize, serde::Serialize)]
struct SemanticSyncResult {
    total_event_ids: u64,
}

#[derive(Deserialize, serde::Serialize)]
struct SemanticHealthSnapshot {
    captured_at_unix_ms: u64,
    status: String,
    coverage_status: String,
    sync_result: Option<SemanticSyncResult>,
    error: String,
}

#[derive(Deserialize)]
struct SemanticEvidenceManifest {
    session_id: String,
    semantic_file: PathBuf,
    published_bytes: u64,
    semantic_events: u64,
}

#[derive(Deserialize)]
struct McpEvidenceManifest {
    schema_version: String,
    session_id: String,
    mcp_file: PathBuf,
    published_bytes: u64,
    protocol_records: u64,
    complete: bool,
}

#[derive(Deserialize)]
struct CapabilityAuditReport {
    schema_version: String,
    mode: String,
    requests: Vec<CapabilityAuditRequest>,
    decisions: Vec<CapabilityAuditDecision>,
    summary: CapabilityAuditSummary,
}

#[derive(Deserialize)]
struct CapabilityAuditRequest {
    request_id: String,
    event_timestamp_unix_ms: u64,
    subject: CapabilityAuditSubject,
    capability: String,
    operation: String,
    resource: CapabilityAuditResource,
    evidence_event_ids: Vec<String>,
    limitations: Vec<String>,
}

#[derive(Deserialize)]
struct CapabilityAuditSubject {
    session_id: String,
    user_activity_id: Option<String>,
}

#[derive(Deserialize)]
struct CapabilityAuditResource {
    kind: String,
    identifier: String,
}

#[derive(Deserialize)]
struct CapabilityAuditDecision {
    decision_id: String,
    request_id: String,
    disposition: String,
    rule_id: String,
    reason: String,
    required_grant: String,
    recovery: String,
    control_effect: CapabilityAuditControlEffect,
}

#[derive(Deserialize)]
struct CapabilityAuditControlEffect {
    mode: String,
    applied: bool,
    effect: String,
}

#[derive(Deserialize)]
struct CapabilityAuditSummary {
    requests: u64,
    enforcement_actions_applied: u64,
}

#[derive(Deserialize)]
struct RollingManifest {
    session_id: String,
    complete: bool,
    segments: Vec<SegmentRecord>,
}

#[derive(Deserialize)]
struct SegmentRecord {
    full_file: String,
    filtered_file: String,
    filtered_events: u64,
}

#[derive(Deserialize, Serialize)]
struct OsEvidenceData {
    target_event_count: u64,
    correlation_events: Vec<ObservationEvent>,
    process_roots: Vec<ProcessRootAssociation>,
    action_counts: OsActionCounts,
    recent_full_events: Vec<ProcessEvidenceEvent>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
struct OsActionCounts {
    process: u64,
    file: u64,
    network: u64,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
struct OsEvidenceCacheKey {
    manifest_path: PathBuf,
    manifest_size: u64,
    correlation_fingerprint: u64,
}

struct CachedOsEvidence {
    key: OsEvidenceCacheKey,
    value: Arc<OsEvidenceData>,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
struct OsSegmentIndexKey {
    session_id: String,
    filtered_file: String,
    full_file: String,
    filtered_size: u64,
    full_size: u64,
    filtered_events: u64,
    correlation_fingerprint: u64,
}

#[derive(Deserialize, Serialize)]
struct PersistedOsSegmentIndex {
    schema_version: String,
    key: OsSegmentIndexKey,
    data: OsSegmentEvidenceData,
}

#[derive(Serialize)]
struct PersistedOsSegmentIndexWrite<'a> {
    schema_version: &'static str,
    key: &'a OsSegmentIndexKey,
    data: &'a OsSegmentEvidenceData,
}

#[derive(Deserialize, Serialize)]
struct OsSegmentEvidenceData {
    target_event_count: u64,
    correlation_events: Vec<ObservationEvent>,
    process_roots: Vec<ProcessRootAssociation>,
    action_counts: OsActionCounts,
    recent_full_events: Vec<ProcessEvidenceEvent>,
}

#[derive(Clone, Copy)]
struct CorrelationWindow {
    action: ActionKind,
    start_unix_ms: u64,
    end_unix_ms: u64,
}

#[derive(Deserialize)]
struct ObservationIndexRecord<'a> {
    #[serde(borrow)]
    event_id: &'a str,
    event_timestamp_unix_ms: u64,
    observed_at_unix_ms: u64,
    #[serde(borrow)]
    actor: ActorIndexRecord<'a>,
    #[serde(borrow)]
    session: SessionIndexRecord<'a>,
    process: ProcessIndexRecord,
    action: ActionIndexRecord,
}

#[derive(Deserialize)]
struct ActorIndexRecord<'a> {
    #[serde(borrow)]
    root_process_instance_id: &'a str,
}

#[derive(Deserialize)]
struct SessionIndexRecord<'a> {
    #[serde(borrow)]
    id: &'a str,
}

#[derive(Deserialize)]
struct ProcessIndexRecord {
    pid: u32,
}

#[derive(Deserialize)]
struct ActionIndexRecord {
    kind: ActionKind,
}

#[derive(Deserialize)]
struct EventIdRecord<'a> {
    #[serde(borrow)]
    event_id: &'a str,
}

struct CoverageBoundary {
    limitation: &'static str,
    impact: &'static str,
}

struct EvidenceCounts {
    provider_os_events: u64,
    target_os_events: u64,
    semantic_events: u64,
}

pub fn load_snapshot(
    configuration: &ProjectionConfiguration,
    cache: &ProjectionCache,
) -> Result<ApplicationSnapshot, String> {
    validate_directory(&configuration.os_run_root, "OS Observer 运行目录")?;
    validate_directory(&configuration.semantic_run_root, "语义 Observer 运行目录")?;
    if let Some(mcp_manifest) = configuration.mcp_manifest.as_ref() {
        validate_file(mcp_manifest, "MCP 证据 manifest")?;
    }

    let generated_at_unix_ms = unix_time_ms()?;
    let observer_runtime = load_optional_observer_runtime()?;
    let os_state: OsObserverState =
        read_json(&configuration.os_run_root.join("observer-process.json"))?;
    let semantic_state: SemanticObserverState = read_json(
        &configuration
            .semantic_run_root
            .join("semantic-observer-process.json"),
    )?;
    let identity: AgentIdentity = read_json(&os_state.identity_path)?;
    let os_health: OsHealthSnapshot = read_last_ndjson(&os_state.health_output_path)?;
    let semantic_health: SemanticHealthSnapshot =
        read_last_ndjson(&semantic_state.health_output_path)?;
    let semantic_manifest: SemanticEvidenceManifest =
        read_json(&semantic_state.semantic_manifest_path)?;
    if semantic_manifest.session_id != semantic_state.session_id {
        return Err(format!(
            "语义证据 manifest 会话不匹配 manifest_session={} observer_session={} path={}",
            semantic_manifest.session_id,
            semantic_state.session_id,
            semantic_state.semantic_manifest_path.display()
        ));
    }
    let semantic_path = semantic_manifest.semantic_file;
    let mut semantic_events: Vec<SemanticEvent> =
        read_ndjson_prefix(&semantic_path, semantic_manifest.published_bytes)?;
    let semantic_event_count = u64::try_from(semantic_events.len())
        .map_err(|error| format!("语义事件数量超出范围 error={error}"))?;
    if semantic_event_count != semantic_manifest.semantic_events {
        return Err(format!(
            "语义证据 manifest 计数不闭合 expected={} actual={} path={}",
            semantic_manifest.semantic_events,
            semantic_event_count,
            semantic_state.semantic_manifest_path.display()
        ));
    }
    semantic_events.sort_by_key(|event| event.event_timestamp_unix_ms);
    let capability_audit_path = configuration
        .semantic_run_root
        .join(CAPABILITY_AUDIT_FILE_NAME);
    let capability_audit =
        load_capability_audit(&capability_audit_path, &semantic_state.session_id)?;
    let (mcp_protocol_records, mcp_source, mcp_publication_boundary) = load_mcp_protocol_records(
        configuration.mcp_manifest.as_deref(),
        &semantic_state.session_id,
    )?;
    let mcp_protocol_record_count = u64::try_from(mcp_protocol_records.len())
        .map_err(|error| format!("MCP 协议记录数量超出范围 error={error}"))?;
    let mcp_protocol_index = McpProtocolIndex::new(&mcp_protocol_records)?;
    let mcp_process_ids = mcp_protocol_index.process_ids();

    let manifest_path = latest_manifest_path(&os_state.output_directory)?;
    let (os_evidence, os_session_id, os_manifest_source) = match manifest_path.as_ref() {
        Some(path) => {
            let manifest: RollingManifest = read_json(path)?;
            let evidence = read_os_evidence_cached(
                &os_state.output_directory,
                &manifest,
                path,
                &semantic_events,
                &mcp_process_ids,
                DISPLAYED_OS_EVENT_LIMIT,
                cache,
            )?;
            (evidence, manifest.session_id, path.display().to_string())
        }
        None => (
            Arc::new(OsEvidenceData {
                target_event_count: 0,
                correlation_events: Vec::new(),
                process_roots: Vec::new(),
                action_counts: OsActionCounts::default(),
                recent_full_events: Vec::new(),
            }),
            os_health.session_id.clone(),
            String::from("尚无已发布 OS manifest（Observer 预热中）"),
        ),
    };

    let observer_state = observer_state(
        generated_at_unix_ms,
        &os_health,
        os_state.health_interval_seconds,
        &semantic_health,
        semantic_state.poll_interval_seconds,
        observer_runtime.as_ref(),
    );
    let observer = build_observer_health(
        observer_state,
        &os_health,
        &semantic_health,
        semantic_events.len(),
        os_evidence.target_event_count,
        observer_runtime_detail(observer_runtime.as_ref()),
    )?;
    let agent = build_agent(&identity, &os_health, &semantic_state, &semantic_events);
    let activities = build_activities(
        &semantic_events,
        &os_evidence.correlation_events,
        &os_evidence.process_roots,
        &os_session_id,
        &mcp_protocol_index,
    );
    let local_resource =
        os_health
            .agent_resource_sample
            .as_ref()
            .map(|sample| LocalResourceSnapshot {
                process_count: sample.process_count,
                unavailable_process_count: sample.unavailable_process_count,
                working_set_bytes: sample.working_set_bytes,
                private_memory_bytes: sample.private_memory_bytes,
                cpu_percent_normalized: sample.cpu_percent_normalized,
                job_process_count: sample.job_process_count,
                app_container_process_count: sample.app_container_process_count,
                elevated_process_count: sample.elevated_process_count,
            });
    let capability_audit_request_count = u64::try_from(capability_audit.records.len())
        .map_err(|error| format!("Capability Audit GUI 记录数量超出范围 error={error}"))?;
    let capabilities = build_capability_views(
        &semantic_events,
        &activities,
        observer_state,
        local_resource,
        capability_audit_request_count,
    );
    let permission = build_permission_snapshot(os_health.agent_resource_sample.as_ref());
    let coverage = build_coverage(
        &semantic_events,
        os_evidence.action_counts,
        &os_health,
        &semantic_health,
        observer_state,
    );
    let evidence = build_evidence(
        &semantic_events,
        &os_evidence.recent_full_events,
        &os_health,
        &semantic_health,
        &os_manifest_source,
        EvidenceCounts {
            provider_os_events: os_health.provider_events_received,
            target_os_events: os_evidence.target_event_count,
            semantic_events: semantic_event_count,
        },
    )?;
    let diagnostics = DiagnosticsViewModel {
        os_run_root: configuration.os_run_root.display().to_string(),
        semantic_run_root: configuration.semantic_run_root.display().to_string(),
        os_manifest: os_manifest_source,
        semantic_source: semantic_path.display().to_string(),
        mcp_source,
        mcp_protocol_records: mcp_protocol_record_count,
        mcp_publication_boundary,
        capability_audit_source: capability_audit.source.clone(),
        os_coverage_status: os_health.coverage_status.clone(),
        semantic_coverage_status: semantic_health.coverage_status.clone(),
        provider_os_event_count: os_health.provider_events_received,
        target_os_event_count: os_evidence.target_event_count,
        data_policy: "本机完整内容，只读展示，不向外部发送",
    };

    Ok(ApplicationSnapshot {
        schema_version: "0.6.0",
        generated_at_unix_ms,
        observer,
        agent,
        permission,
        capability_audit,
        activities,
        capabilities,
        coverage,
        evidence,
        diagnostics,
    })
}

fn load_mcp_protocol_records(
    manifest_path: Option<&Path>,
    selected_session_id: &str,
) -> Result<(Vec<McpProtocolRecord>, String, String), String> {
    let Some(path) = manifest_path else {
        return Ok((
            Vec::new(),
            String::from("当前会话没有 MCP 原生发布物"),
            String::from(
                "mcp_request_to_os_operation_native_id_missing：未发现当前会话的 MCP 原生 request/response 证据，未进行时间弱关联",
            ),
        ));
    };
    let manifest: McpEvidenceManifest = read_json(path)?;
    if manifest.schema_version != "1.1.0" {
        return Err(format!(
            "不支持的 MCP manifest 契约 schema_version={} path={}",
            manifest.schema_version,
            path.display()
        ));
    }
    if manifest.session_id != selected_session_id {
        return Err(format!(
            "MCP 证据 manifest 会话不匹配 expected={} actual={} path={}",
            selected_session_id,
            manifest.session_id,
            path.display()
        ));
    }
    let records = read_ndjson_prefix(&manifest.mcp_file, manifest.published_bytes)?;
    let record_count = u64::try_from(records.len())
        .map_err(|error| format!("MCP 协议记录数量超出范围 error={error}"))?;
    if record_count != manifest.protocol_records {
        return Err(format!(
            "MCP 证据 manifest 计数不闭合 expected={} actual={} path={}",
            manifest.protocol_records,
            record_count,
            path.display()
        ));
    }
    Ok((
        records,
        manifest.mcp_file.display().to_string(),
        format!(
            "manifest={} published_bytes={} protocol_records={} complete={}",
            path.display(),
            manifest.published_bytes,
            manifest.protocol_records,
            manifest.complete
        ),
    ))
}

fn load_capability_audit(
    path: &Path,
    selected_session_id: &str,
) -> Result<CapabilityAuditViewModel, String> {
    if !path.exists() {
        return Ok(CapabilityAuditViewModel {
            observed: false,
            mode: "audit",
            source: path.display().to_string(),
            enforcement_actions_applied: 0,
            records: Vec::new(),
        });
    }
    let report: CapabilityAuditReport = read_json(path)?;
    if report.schema_version != "0.1.0" || report.mode != "audit" {
        return Err(format!(
            "Capability Audit 报告契约不支持 schema_version={} mode={} path={}",
            report.schema_version,
            report.mode,
            path.display()
        ));
    }
    if report.summary.enforcement_actions_applied != 0 {
        return Err(format!(
            "Capability Audit 报告声称已应用控制 effect_count={} path={}",
            report.summary.enforcement_actions_applied,
            path.display()
        ));
    }
    let request_count = u64::try_from(report.requests.len())
        .map_err(|error| format!("Capability Audit 请求数量超出范围 error={error}"))?;
    if report.summary.requests != request_count {
        return Err(format!(
            "Capability Audit 请求计数不闭合 expected={} actual={} path={}",
            report.summary.requests,
            request_count,
            path.display()
        ));
    }

    let mut decisions = HashMap::new();
    for decision in report.decisions {
        if decision.control_effect.mode != "audit"
            || decision.control_effect.applied
            || decision.control_effect.effect != "observation_only"
        {
            return Err(format!(
                "Capability Audit 决策超出只读边界 decision_id={} applied={} path={}",
                decision.decision_id,
                decision.control_effect.applied,
                path.display()
            ));
        }
        let request_id = decision.request_id.clone();
        if decisions.insert(request_id.clone(), decision).is_some() {
            return Err(format!(
                "Capability Audit 请求存在重复决策 request_id={request_id} path={}",
                path.display()
            ));
        }
    }

    let mut request_ids = HashSet::new();
    let mut records = Vec::with_capacity(report.requests.len());
    for request in report.requests {
        if request.subject.session_id != selected_session_id {
            return Err(format!(
                "Capability Audit 报告与当前会话不匹配 expected={} actual={} request_id={} path={}",
                selected_session_id,
                request.subject.session_id,
                request.request_id,
                path.display()
            ));
        }
        if !request_ids.insert(request.request_id.clone()) {
            return Err(format!(
                "Capability Audit 包含重复 request_id={} path={}",
                request.request_id,
                path.display()
            ));
        }
        let decision = decisions.remove(&request.request_id).ok_or_else(|| {
            format!(
                "Capability Audit 请求缺少决策 request_id={} path={}",
                request.request_id,
                path.display()
            )
        })?;
        records.push(CapabilityAuditRecordViewModel {
            request_id: request.request_id,
            decision_id: decision.decision_id,
            event_timestamp_unix_ms: request.event_timestamp_unix_ms,
            session_id: request.subject.session_id,
            user_activity_id: request.subject.user_activity_id,
            capability: request.capability,
            operation: request.operation,
            resource_kind: request.resource.kind,
            resource_identifier: request.resource.identifier,
            disposition: decision.disposition,
            rule_id: decision.rule_id,
            reason: decision.reason,
            required_grant: decision.required_grant,
            recovery: decision.recovery,
            applied: false,
            evidence_event_ids: request.evidence_event_ids,
            limitations: request.limitations,
        });
    }
    if let Some((request_id, decision)) = decisions.into_iter().next() {
        return Err(format!(
            "Capability Audit 决策缺少对应请求 request_id={} decision_id={} path={}",
            request_id,
            decision.decision_id,
            path.display()
        ));
    }
    records.sort_by_key(|record| record.event_timestamp_unix_ms);
    Ok(CapabilityAuditViewModel {
        observed: true,
        mode: "audit",
        source: path.display().to_string(),
        enforcement_actions_applied: 0,
        records,
    })
}

fn build_permission_snapshot(
    resource_sample: Option<&AgentResourceHealthSample>,
) -> PermissionSnapshotViewModel {
    let processes = resource_sample
        .into_iter()
        .flat_map(|sample| sample.permission_processes.iter())
        .map(|process| ProcessPermissionViewModel {
            process_id: process.process_id,
            process_instance_id: process.process_instance_id.clone(),
            is_elevated: process.is_elevated,
            integrity_level: process.integrity_level.clone(),
            integrity_rid: process.integrity_rid,
            privileges: process
                .privileges
                .iter()
                .map(|privilege| TokenPrivilegeViewModel {
                    name: privilege.name.clone(),
                    enabled: privilege.enabled,
                    enabled_by_default: privilege.enabled_by_default,
                    removed: privilege.removed,
                    used_for_access: privilege.used_for_access,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    PermissionSnapshotViewModel {
        observed: resource_sample.is_some(),
        processes,
    }
}

fn build_observer_health(
    state: ObserverState,
    os_health: &OsHealthSnapshot,
    semantic_health: &SemanticHealthSnapshot,
    semantic_event_count: usize,
    published_os_event_count: u64,
    lifecycle_detail: Option<String>,
) -> Result<ObserverHealthViewModel, String> {
    let semantic_events = u64::try_from(semantic_event_count)
        .map_err(|error| format!("语义事件数量超出范围 error={error}"))?;
    let events_lost = os_health
        .etw
        .events_lost
        .saturating_add(os_health.etw.log_buffers_lost)
        .saturating_add(os_health.etw.realtime_buffers_lost);
    let evidence_reason = match state {
        ObserverState::Healthy => {
            String::from("OS 与语义 Observer 健康快照新鲜，且未报告采集链错误。")
        }
        ObserverState::Idle => String::from("Observer 正常运行，当前没有已验证的 Agent 根进程。"),
        ObserverState::Starting => String::from("Observer 正在启动并确认原生会话。"),
        ObserverState::Degraded => format!(
            "Observer 报告降级：os_status={} semantic_status={} semantic_error={}",
            os_health.coverage_status, semantic_health.coverage_status, semantic_health.error
        ),
        ObserverState::Unknown => String::from("Observer 健康状态无法从当前快照确定。"),
        ObserverState::Stopped => String::from("Observer 健康快照已过期，不能声称仍在运行。"),
    };
    let reason = lifecycle_detail.unwrap_or(evidence_reason);
    Ok(ObserverHealthViewModel {
        state,
        reason,
        last_healthy_at_unix_ms: os_health
            .captured_at_unix_ms
            .min(semantic_health.captured_at_unix_ms),
        events_lost,
        parse_failures: os_health.parse_failures,
        write_failures: os_health.write_failures,
        queue_drops: os_health.output_batches_dropped,
        identity_collisions: os_health.identity_collision_count,
        clock_skew_ms: os_health.event_clock_skew_ms,
        semantic_events,
        os_events: published_os_event_count,
    })
}

pub fn load_observer_runtime() -> Result<ObserverRuntimeViewModel, String> {
    load_optional_observer_runtime()?.ok_or_else(|| {
        String::from("Observer 生命周期状态不存在 path=%LOCALAPPDATA%\\AgentReins\\runtime\\observer-lifecycle.json")
    })
}

fn load_optional_observer_runtime() -> Result<Option<ObserverRuntimeViewModel>, String> {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Ok(None);
    };
    let path = local_app_data
        .join("AgentReins")
        .join("runtime")
        .join("observer-lifecycle.json");
    if !path.is_file() {
        return Ok(None);
    }
    let runtime: ObserverRuntimeViewModel = read_json(&path)?;
    if runtime.schema_version != "0.1.0" {
        return Err(format!(
            "不支持的 Observer 生命周期契约 expected=0.1.0 actual={} path={}",
            runtime.schema_version,
            path.display()
        ));
    }
    if runtime.applied {
        return Err(format!(
            "Observer 生命周期状态越过只观测边界 applied=true path={}",
            path.display()
        ));
    }
    Ok(Some(runtime))
}

fn observer_runtime_detail(runtime: Option<&ObserverRuntimeViewModel>) -> Option<String> {
    runtime.and_then(|snapshot| {
        (!matches!(
            snapshot.state,
            ObserverRuntimeState::Running | ObserverRuntimeState::Idle
        ))
        .then(|| {
            format!(
                "Observer 自动启动状态={:?}；{}",
                snapshot.state, snapshot.detail
            )
        })
    })
}

fn observer_state(
    now: u64,
    os_health: &OsHealthSnapshot,
    os_interval_seconds: u64,
    semantic_health: &SemanticHealthSnapshot,
    semantic_interval_seconds: u64,
    runtime: Option<&ObserverRuntimeViewModel>,
) -> ObserverState {
    let mut runtime_running = false;
    if let Some(runtime) = runtime {
        match runtime.state {
            ObserverRuntimeState::Starting => return ObserverState::Starting,
            ObserverRuntimeState::Stopped => return ObserverState::Stopped,
            ObserverRuntimeState::NotInstalled
            | ObserverRuntimeState::Degraded
            | ObserverRuntimeState::IdentityMismatch
            | ObserverRuntimeState::PermissionRequired
            | ObserverRuntimeState::Failed => return ObserverState::Degraded,
            ObserverRuntimeState::Running | ObserverRuntimeState::Idle => {
                runtime_running = true;
            }
        }
    }
    let os_stale_after = os_interval_seconds.saturating_mul(3).saturating_mul(1_000);
    let semantic_stale_after = semantic_interval_seconds
        .saturating_mul(3)
        .saturating_mul(1_000);
    let os_stale = now.saturating_sub(os_health.captured_at_unix_ms) > os_stale_after;
    let semantic_stale =
        now.saturating_sub(semantic_health.captured_at_unix_ms) > semantic_stale_after;
    if os_stale || semantic_stale {
        return if runtime_running {
            ObserverState::Degraded
        } else {
            ObserverState::Stopped
        };
    }
    if os_health.coverage_status == "healthy"
        && semantic_health.coverage_status == "healthy"
        && semantic_health.status == "running"
        && semantic_health.error.is_empty()
    {
        return ObserverState::Healthy;
    }
    if os_health.coverage_status == "healthy"
        && semantic_health.coverage_status == "idle"
        && semantic_health.status == "idle"
        && semantic_health.error.is_empty()
    {
        return ObserverState::Idle;
    }
    if os_health.coverage_status == "degraded"
        || semantic_health.coverage_status == "degraded"
        || !semantic_health.error.is_empty()
    {
        return ObserverState::Degraded;
    }
    ObserverState::Unknown
}

fn build_agent(
    identity: &AgentIdentity,
    os_health: &OsHealthSnapshot,
    semantic_state: &SemanticObserverState,
    semantic_events: &[SemanticEvent],
) -> AgentSummaryViewModel {
    let workspace = semantic_events
        .iter()
        .rev()
        .find_map(|event| event.workspace_path.clone());
    AgentSummaryViewModel {
        id: "workbuddy",
        kind: AgentKind::WorkBuddy,
        name: "WorkBuddy",
        product_version: identity.file_version.clone(),
        publisher: identity.signer_subject.clone(),
        executable_path: identity.executable_path.clone(),
        sha256: identity.sha256.clone(),
        binding: AssociationState::Confirmed,
        root_instances: os_health.active_workbuddy_root_instances,
        active_processes: os_health.active_workbuddy_processes,
        workspace,
        semantic_session_id: semantic_state.session_id.clone(),
        os_session_id: os_health.session_id.clone(),
    }
}

fn build_activities(
    events: &[SemanticEvent],
    os_events: &[ObservationEvent],
    process_roots: &[ProcessRootAssociation],
    os_session_id: &str,
    mcp_protocol_index: &McpProtocolIndex,
) -> Vec<ActivityViewModel> {
    let os_event_index = OsEventIndex::with_process_roots(os_events, process_roots);
    let semantic_event_index = SemanticEventIndex::new(events);
    let mut grouped = BTreeMap::<String, Vec<&SemanticEvent>>::new();
    for event in events {
        if event
            .user_activity_id
            .starts_with("workbuddy:unattributed:")
        {
            continue;
        }
        grouped
            .entry(event.user_activity_id.clone())
            .or_default()
            .push(event);
    }
    let mut activities = grouped
        .into_iter()
        .filter_map(|(activity_id, mut activity_events)| {
            activity_events.sort_by_key(|event| event.event_timestamp_unix_ms);
            let request_event = activity_events.iter().find(|event| {
                event.content_kind == Some(SemanticContentKind::RawUserPrompt)
                    && event.action_kind == ActionKind::UserInput
            })?;
            let request = request_event.content.clone();
            let final_result = activity_events
                .iter()
                .rev()
                .find(|event| event.content_kind == Some(SemanticContentKind::FinalResult))
                .map(|event| event.content.clone());
            let started_at_unix_ms = activity_events
                .first()
                .map(|event| event.event_timestamp_unix_ms)
                .unwrap_or(request_event.event_timestamp_unix_ms);
            let ended_at_unix_ms = activity_events
                .last()
                .map(|event| event.event_timestamp_unix_ms)
                .unwrap_or(started_at_unix_ms);
            let tools = activity_events
                .iter()
                .filter(|event| event.content_kind == Some(SemanticContentKind::ToolCall))
                .count() as u64;
            let correlated_events = activity_events
                .iter()
                .map(|event| {
                    let correlation = correlate_semantic_evidence(
                        event,
                        &semantic_event_index,
                        &os_event_index,
                        Some(mcp_protocol_index),
                        Some(os_session_id),
                        CORRELATION_WINDOW_MS,
                    );
                    (*event, correlation)
                })
                .collect::<Vec<_>>();
            let timeline = correlated_events
                .iter()
                .map(|(event, correlation)| build_timeline_item(event, correlation.as_ref()))
                .collect::<Vec<_>>();
            let capabilities = timeline
                .iter()
                .flat_map(|item| item.capabilities.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let associations = association_counts(&correlated_events);
            Some(ActivityViewModel {
                id: activity_id,
                title: compact_title(&request, 72),
                request,
                started_at_unix_ms,
                ended_at_unix_ms,
                final_result,
                coverage: CoverageState::Partial,
                counts: ActivityCountsViewModel {
                    tools,
                    processes: associated_event_count(&correlated_events, is_process_action),
                    files: associated_event_count(&correlated_events, is_file_action),
                    network: associated_event_count(&correlated_events, is_network_action),
                },
                associations,
                unattributed_events: 0,
                limitation: "活动按原生 user_activity_id 归组；只在当前选择的 OS Observer 运行目录中关联。进程、文件和网络数字只统计原生证据图已经连接的 OS 事件；时间、路径、参数、端点和同进程树不参与因果选择。",
                capabilities,
                timeline,
            })
        })
        .collect::<Vec<_>>();
    activities.sort_by_key(|activity| std::cmp::Reverse(activity.started_at_unix_ms));
    activities
}

fn build_timeline_item(
    event: &SemanticEvent,
    correlation: Option<&CorrelationResult<'_>>,
) -> TimelineItemViewModel {
    let mut capabilities = classify_semantic_event(event)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if correlation.is_some_and(|value| value.mcp_jsonrpc_request_id.is_some()) {
        capabilities.insert(AgentCapabilityKind::Mcp);
    }
    let linked_os_event_ids = correlation
        .map(|value| {
            value
                .linked_events
                .iter()
                .take(20)
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let clock_basis =
        correlation
            .and_then(|value| value.time_basis)
            .map_or("event_timestamp", |value| {
                if value == "observed_at_due_to_clock_skew" {
                    "observed_at"
                } else {
                    "event_timestamp"
                }
            });
    let limitation = correlation.and_then(correlation_limitation);
    TimelineItemViewModel {
        id: event.event_id.clone(),
        phase: layer_name(event.observation_layer),
        title: content_kind_name(event.content_kind),
        detail: event.content.clone(),
        timestamp_unix_ms: event.event_timestamp_unix_ms,
        semantic_observed: true,
        correlation_relevant: correlation.is_some(),
        correlation_state: correlation.map(|value| association_state(value.status)),
        source: format!(
            "{} · {}",
            event
                .source_schema_profile
                .as_deref()
                .unwrap_or("unknown-schema"),
            event.source_record_id
        ),
        native_call_id: event
            .tool_call_id
            .clone()
            .or_else(|| event.operation_id.clone()),
        linked_os_events: correlation.map_or(0, |value| value.linked_events.len() as u64),
        clock_basis,
        correlation_basis: correlation.and_then(|value| value.basis.map(String::from)),
        linked_os_event_ids,
        first_breakpoint: correlation.and_then(|value| value.first_breakpoint.map(String::from)),
        causal_edges: correlation.map_or_else(Vec::new, |value| value.causal_edges.clone()),
        mcp_json_rpc_request_id: correlation.and_then(|value| value.mcp_jsonrpc_request_id.clone()),
        mcp_process_id: correlation.and_then(|value| value.mcp_process_id),
        mcp_response_observed: correlation.map(|value| value.mcp_response_observed),
        limitation,
        capabilities: capabilities.into_iter().collect(),
    }
}

fn association_state(status: CorrelationStatus) -> AssociationState {
    match status {
        CorrelationStatus::Confirmed => AssociationState::Confirmed,
        CorrelationStatus::Partial => AssociationState::Partial,
        CorrelationStatus::Unlinked => AssociationState::Unlinked,
    }
}

fn correlation_limitation(correlation: &CorrelationResult<'_>) -> Option<String> {
    match correlation.status {
        CorrelationStatus::Confirmed => None,
        CorrelationStatus::Partial => Some(format!(
            "原生因果链只闭合到部分层级，已停止在首个断点 breakpoint={}",
            correlation.first_breakpoint.unwrap_or("unknown")
        )),
        CorrelationStatus::Unlinked => Some(format!(
            "当前记录没有跨越语义层的原生因果边 breakpoint={}",
            correlation.first_breakpoint.unwrap_or("unknown")
        )),
    }
}

fn association_counts(
    events: &[(&SemanticEvent, Option<CorrelationResult<'_>>)],
) -> AssociationCountsViewModel {
    let mut counts = AssociationCountsViewModel {
        confirmed: 0,
        partial: 0,
        unlinked: 0,
    };
    for correlation in events.iter().filter_map(|(_, value)| value.as_ref()) {
        match correlation.status {
            CorrelationStatus::Confirmed => counts.confirmed += 1,
            CorrelationStatus::Partial => counts.partial += 1,
            CorrelationStatus::Unlinked => counts.unlinked += 1,
        }
    }
    counts
}

fn associated_event_count(
    events: &[(&SemanticEvent, Option<CorrelationResult<'_>>)],
    predicate: fn(ActionKind) -> bool,
) -> Option<u64> {
    let relevant = events
        .iter()
        .any(|(event, _)| event.expected_os_action.is_some_and(predicate));
    if !relevant {
        return None;
    }
    let event_ids = events
        .iter()
        .filter_map(|(_, correlation)| correlation.as_ref())
        .flat_map(|correlation| correlation.linked_events.iter())
        .filter(|event| predicate(event.action.kind))
        .map(|event| event.event_id.as_str())
        .collect::<HashSet<_>>();
    Some(event_ids.len() as u64)
}

fn is_process_action(action: ActionKind) -> bool {
    matches!(action, ActionKind::ProcessStart | ActionKind::ProcessStop)
}

fn is_file_action(action: ActionKind) -> bool {
    matches!(
        action,
        ActionKind::FileOpen
            | ActionKind::FileRead
            | ActionKind::FileWrite
            | ActionKind::FileDelete
            | ActionKind::FileRename
            | ActionKind::FileOperationResult
            | ActionKind::CredentialObserved
    )
}

fn is_network_action(action: ActionKind) -> bool {
    matches!(
        action,
        ActionKind::NetworkConnect
            | ActionKind::NetworkAccept
            | ActionKind::NetworkSend
            | ActionKind::NetworkReceive
            | ActionKind::NetworkDisconnect
    )
}

fn build_coverage(
    semantic_events: &[SemanticEvent],
    action_counts: OsActionCounts,
    os_health: &OsHealthSnapshot,
    semantic_health: &SemanticHealthSnapshot,
    observer_state: ObserverState,
) -> CoverageViewModel {
    let semantic_count = |layer: ObservationLayer| -> u64 {
        semantic_events
            .iter()
            .filter(|event| event.observation_layer == Some(layer))
            .count() as u64
    };
    let process_count = action_counts.process;
    let file_count = action_counts.file;
    let network_count = action_counts.network;
    let last_healthy_at = os_health
        .captured_at_unix_ms
        .min(semantic_health.captured_at_unix_ms);
    let layers = vec![
        coverage_layer(
            1,
            "User Prompt",
            state_when_present(
                semantic_count(ObservationLayer::UserPrompt),
                CoverageState::Partial,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "无法证明 UI 输入前的全部上下文。",
                impact: "展示已记录原文，但不声称等同完整用户意图。",
            },
            semantic_count(ObservationLayer::UserPrompt),
        ),
        coverage_layer(
            2,
            "Base / System Instructions",
            CoverageState::NotObservable,
            "客户端请求包装",
            last_healthy_at,
            CoverageBoundary {
                limitation: "服务端隐藏指令没有公开来源。",
                impact: "不能证明最终系统指令全集。",
            },
            semantic_count(ObservationLayer::BaseSystemInstructions),
        ),
        coverage_layer(
            3,
            "Skills and Agent Policy",
            state_when_present(
                semantic_count(ObservationLayer::SkillsAgentPolicy),
                CoverageState::Partial,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "最终优先级与合并结果不完整。",
                impact: "策略结论保持未知或部分。",
            },
            semantic_count(ObservationLayer::SkillsAgentPolicy),
        ),
        coverage_layer(
            4,
            "Available Tools / MCP",
            state_when_present(
                semantic_count(ObservationLayer::AvailableToolsMcp),
                CoverageState::Partial,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "每轮完整工具集合不可证明。",
                impact: "只展示实际出现的工具。",
            },
            semantic_count(ObservationLayer::AvailableToolsMcp),
        ),
        coverage_layer(
            5,
            "LLM Request / Response",
            state_when_present(
                semantic_count(ObservationLayer::LlmRequestResponse),
                CoverageState::Partial,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "服务端内部过程不可见。",
                impact: "不把客户端记录等同服务端完整过程。",
            },
            semantic_count(ObservationLayer::LlmRequestResponse),
        ),
        coverage_layer(
            6,
            "Tool / MCP Calls",
            state_when_present(
                semantic_count(ObservationLayer::ToolMcpCalls),
                CoverageState::Observed,
            ),
            "原生 callId",
            last_healthy_at,
            CoverageBoundary {
                limitation: "callId 未传播到 OS 层。",
                impact: "调用本身可确认，OS 副作用仍不能确定关联。",
            },
            semantic_count(ObservationLayer::ToolMcpCalls),
        ),
        coverage_layer(
            7,
            "Process and Script",
            state_when_present(process_count, CoverageState::Partial),
            "ETW 已发布完整证据",
            last_healthy_at,
            CoverageBoundary {
                limitation: "任意未记录 stdin 正文不可证明。",
                impact: "进程树可见，脚本正文可能缺失。",
            },
            process_count,
        ),
        coverage_layer(
            8,
            "Filesystem / Credential",
            state_when_present(file_count, CoverageState::Partial),
            "ETW 已发布完整证据",
            last_healthy_at,
            CoverageBoundary {
                limitation: "不实施全系统内容扫描。",
                impact: "路径与动作可见，文件内容取决于语义来源。",
            },
            file_count,
        ),
        coverage_layer(
            9,
            "Network",
            state_when_present(network_count, CoverageState::Partial),
            "ETW TCP 生命周期",
            last_healthy_at,
            CoverageBoundary {
                limitation: "DNS、TLS 身份和加密请求正文不可观测。",
                impact: "只显示实际端点，不包装为已验证域名。",
            },
            network_count,
        ),
        coverage_layer(
            10,
            "ToolResult",
            state_when_present(
                semantic_count(ObservationLayer::ToolResult),
                CoverageState::Observed,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "不能伪造对应 OS 完成事件。",
                impact: "结果原文与 OS 影响分开陈述。",
            },
            semantic_count(ObservationLayer::ToolResult),
        ),
        coverage_layer(
            11,
            "LLM Final Result",
            state_when_present(
                semantic_count(ObservationLayer::LlmFinalResult),
                CoverageState::Observed,
            ),
            "WorkBuddy 原始语义记录",
            last_healthy_at,
            CoverageBoundary {
                limitation: "回复正确不等于执行路径安全。",
                impact: "仅证明客户端保存的原始回复。",
            },
            semantic_count(ObservationLayer::LlmFinalResult),
        ),
        coverage_layer(
            12,
            "Evidence Coverage",
            if matches!(observer_state, ObserverState::Healthy | ObserverState::Idle) {
                CoverageState::Partial
            } else {
                CoverageState::Degraded
            },
            "OS 与语义 Observer 健康快照",
            last_healthy_at,
            CoverageBoundary {
                limitation: "请求到具体 OS 操作的原生标识仍受 WorkBuddy 与 Windows 数据源边界限制。",
                impact: "不生成弱关联；能力边界不等于证据降级。",
            },
            2,
        ),
    ];
    let gate_status = gate_status(observer_state);
    let gate_reason = match gate_status {
        GateStatus::PassedWithBoundaries => {
            "原生可得范围已闭合；不可得字段显式保留为能力边界，不使用弱关联补齐"
        }
        GateStatus::EvidenceDegraded => {
            "当前 Observer 停止、过期或证据降级，不能使用本快照证明 Gate 0"
        }
    };
    let summary = CoverageSummaryViewModel {
        observed: count_coverage_state(&layers, CoverageState::Observed),
        partial: count_coverage_state(&layers, CoverageState::Partial),
        not_observable: count_coverage_state(&layers, CoverageState::NotObservable),
        not_triggered: count_coverage_state(&layers, CoverageState::NotTriggered),
        degraded: count_coverage_state(&layers, CoverageState::Degraded),
        gate_status,
        gate_reason,
    };
    CoverageViewModel { summary, layers }
}

fn gate_status(observer_state: ObserverState) -> GateStatus {
    match observer_state {
        ObserverState::Healthy | ObserverState::Idle => GateStatus::PassedWithBoundaries,
        ObserverState::Starting
        | ObserverState::Degraded
        | ObserverState::Unknown
        | ObserverState::Stopped => GateStatus::EvidenceDegraded,
    }
}

fn coverage_layer(
    index: u8,
    name: &'static str,
    state: CoverageState,
    source: &str,
    last_healthy_at_unix_ms: u64,
    boundary: CoverageBoundary,
    evidence_count: u64,
) -> CoverageLayerViewModel {
    CoverageLayerViewModel {
        index,
        name,
        state,
        source: String::from(source),
        last_healthy_at_unix_ms,
        limitation: boundary.limitation,
        impact: boundary.impact,
        evidence_count,
    }
}

fn state_when_present(count: u64, present_state: CoverageState) -> CoverageState {
    if count == 0 {
        CoverageState::NotTriggered
    } else {
        present_state
    }
}

fn count_coverage_state(layers: &[CoverageLayerViewModel], state: CoverageState) -> u64 {
    layers.iter().filter(|layer| layer.state == state).count() as u64
}

fn build_evidence(
    semantic_events: &[SemanticEvent],
    os_events: &[ProcessEvidenceEvent],
    os_health: &OsHealthSnapshot,
    semantic_health: &SemanticHealthSnapshot,
    os_manifest_source: &str,
    counts: EvidenceCounts,
) -> Result<EvidenceFeedViewModel, String> {
    let mut items = semantic_events
        .iter()
        .rev()
        .map(|event| EvidenceViewModel {
            id: event.event_id.clone(),
            kind: EvidenceKind::Semantic,
            title: format!(
                "{} · {}",
                layer_name(event.observation_layer),
                content_kind_name(event.content_kind)
            ),
            content: event.content.clone(),
            source: format!("{}#{}", event.source_record_id, event.event_id),
            timestamp_unix_ms: event.event_timestamp_unix_ms,
            content_mode: EvidenceContentMode::RawLocal,
            observed: true,
            correlation_state: None,
        })
        .collect::<Vec<_>>();
    items.extend(os_events.iter().rev().map(|event| {
        EvidenceViewModel {
            id: event.event_id.clone(),
            kind: evidence_kind(event.action.kind),
            title: format!("{:?} · {}", event.action.kind, event.resource.identifier),
            content: serde_json::to_string_pretty(event)
                .unwrap_or_else(|error| format!("无法序列化 OS 证据 error={error}")),
            source: format!("{os_manifest_source}#{}", event.event_id),
            timestamp_unix_ms: event.event_timestamp_unix_ms,
            content_mode: EvidenceContentMode::RawLocal,
            observed: true,
            correlation_state: None,
        }
    }));
    items.push(EvidenceViewModel {
        id: format!("os-health-{}", os_health.captured_at_unix_ms),
        kind: EvidenceKind::Health,
        title: String::from("OS Observer 健康快照"),
        content: serde_json::to_string_pretty(os_health)
            .map_err(|error| format!("无法序列化 OS 健康快照 error={error}"))?,
        source: String::from("observer-health.ndjson"),
        timestamp_unix_ms: os_health.captured_at_unix_ms,
        content_mode: EvidenceContentMode::MetadataOnly,
        observed: true,
        correlation_state: None,
    });
    items.push(EvidenceViewModel {
        id: format!("semantic-health-{}", semantic_health.captured_at_unix_ms),
        kind: EvidenceKind::Health,
        title: String::from("语义 Observer 健康快照"),
        content: serde_json::to_string_pretty(semantic_health)
            .map_err(|error| format!("无法序列化语义健康快照 error={error}"))?,
        source: String::from("semantic-observer-health.ndjson"),
        timestamp_unix_ms: semantic_health.captured_at_unix_ms,
        content_mode: EvidenceContentMode::MetadataOnly,
        observed: true,
        correlation_state: None,
    });
    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp_unix_ms));
    Ok(EvidenceFeedViewModel {
        items,
        semantic_event_count: counts.semantic_events,
        provider_os_event_count: counts.provider_os_events,
        published_os_event_count: counts.target_os_events,
        displayed_os_event_count: os_events.len() as u64,
        os_scope: "全部语义事件；OS 统计覆盖 manifest 全部已发布 WorkBuddy 目标事件；列表展示最近 200 条目标事件的完整原文，完整段路径在诊断页显示",
    })
}

fn evidence_kind(action: ActionKind) -> EvidenceKind {
    match action {
        ActionKind::ProcessStart | ActionKind::ProcessStop => EvidenceKind::Process,
        ActionKind::FileOpen
        | ActionKind::FileRead
        | ActionKind::FileWrite
        | ActionKind::FileDelete
        | ActionKind::FileRename
        | ActionKind::FileOperationResult
        | ActionKind::CredentialObserved => EvidenceKind::File,
        ActionKind::NetworkConnect
        | ActionKind::NetworkAccept
        | ActionKind::NetworkSend
        | ActionKind::NetworkReceive
        | ActionKind::NetworkDisconnect => EvidenceKind::Network,
        _ => EvidenceKind::Process,
    }
}

fn layer_name(layer: Option<ObservationLayer>) -> String {
    match layer {
        Some(ObservationLayer::UserPrompt) => String::from("User Prompt"),
        Some(ObservationLayer::BaseSystemInstructions) => {
            String::from("Base / System Instructions")
        }
        Some(ObservationLayer::SkillsAgentPolicy) => String::from("Skills and Agent Policy"),
        Some(ObservationLayer::AvailableToolsMcp) => String::from("Available Tools / MCP"),
        Some(ObservationLayer::LlmRequestResponse) => String::from("LLM Request / Response"),
        Some(ObservationLayer::ToolMcpCalls) => String::from("Tool / MCP Calls"),
        Some(ObservationLayer::ProcessScript) => String::from("Process and Script"),
        Some(ObservationLayer::FilesystemCredential) => String::from("Filesystem / Credential"),
        Some(ObservationLayer::Network) => String::from("Network"),
        Some(ObservationLayer::ToolResult) => String::from("ToolResult"),
        Some(ObservationLayer::LlmFinalResult) => String::from("LLM Final Result"),
        Some(ObservationLayer::EvidenceCoverage) => String::from("Evidence Coverage"),
        None => String::from("未分类语义事件"),
    }
}

fn content_kind_name(kind: Option<SemanticContentKind>) -> String {
    kind.map_or_else(|| String::from("语义事件"), |value| format!("{value:?}"))
}

fn compact_title(value: &str, maximum_characters: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = single_line.chars();
    let title = characters
        .by_ref()
        .take(maximum_characters)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

fn validate_directory(path: &Path, label: &str) -> Result<(), String> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!("{label}不存在 path={}", path.display()))
    }
}

fn validate_file(path: &Path, label: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{label}不存在 path={}", path.display()))
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("无法读取 JSON path={} error={error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("无法解析 JSON path={} error={error}", path.display()))
}

fn read_ndjson_prefix<T: for<'de> Deserialize<'de>>(
    path: &Path,
    published_bytes: u64,
) -> Result<Vec<T>, String> {
    let mut file = File::open(path).map_err(|error| {
        format!(
            "无法打开已发布 NDJSON path={} error={error}",
            path.display()
        )
    })?;
    let actual_bytes = file
        .metadata()
        .map_err(|error| {
            format!(
                "无法读取 NDJSON 元数据 path={} error={error}",
                path.display()
            )
        })?
        .len();
    if actual_bytes < published_bytes {
        return Err(format!(
            "NDJSON 长度小于已发布边界 path={} published_bytes={} actual_bytes={}",
            path.display(),
            published_bytes,
            actual_bytes
        ));
    }
    let capacity = usize::try_from(published_bytes).map_err(|error| {
        format!("已发布 NDJSON 边界超出地址空间 bytes={published_bytes} error={error}")
    })?;
    let mut published = vec![0_u8; capacity];
    file.read_exact(&mut published).map_err(|error| {
        format!(
            "无法读取已发布 NDJSON 前缀 path={} published_bytes={} error={error}",
            path.display(),
            published_bytes
        )
    })?;
    if published.last().is_some_and(|byte| *byte != b'\n') {
        return Err(format!(
            "已发布 NDJSON 边界不在完整记录末尾 path={} published_bytes={}",
            path.display(),
            published_bytes
        ));
    }
    let text = std::str::from_utf8(&published).map_err(|error| {
        format!(
            "已发布 NDJSON 不是 UTF-8 path={} error={error}",
            path.display()
        )
    })?;
    let mut values = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<T>(line).map_err(|error| {
            format!(
                "无法解析已发布 NDJSON path={} line={} error={error}",
                path.display(),
                index + 1
            )
        })?;
        values.push(value);
    }
    Ok(values)
}

fn for_each_ndjson_line<F>(path: &Path, mut visit: F) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
{
    let file = File::open(path)
        .map_err(|error| format!("无法打开 NDJSON path={} error={error}", path.display()))?;
    let reader = BufReader::new(file);
    for (index, line_result) in reader.lines().enumerate() {
        let line = line_result.map_err(|error| {
            format!(
                "无法读取 NDJSON path={} line={} error={error}",
                path.display(),
                index + 1
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        visit(&line).map_err(|error| {
            format!(
                "无法处理 NDJSON path={} line={} error={error}",
                path.display(),
                index + 1
            )
        })?;
    }
    Ok(())
}

fn read_last_ndjson<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("无法打开 NDJSON path={} error={error}", path.display()))?;
    let mut position = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("无法定位 NDJSON 末尾 path={} error={error}", path.display()))?;
    let mut suffix = Vec::new();
    while position > 0 {
        let read_size = position.min(8_192) as usize;
        position -= read_size as u64;
        file.seek(SeekFrom::Start(position)).map_err(|error| {
            format!(
                "无法定位 NDJSON path={} offset={position} error={error}",
                path.display()
            )
        })?;
        let mut chunk = vec![0_u8; read_size];
        file.read_exact(&mut chunk).map_err(|error| {
            format!(
                "无法读取 NDJSON path={} offset={position} error={error}",
                path.display()
            )
        })?;
        chunk.extend_from_slice(&suffix);
        suffix = chunk;

        let Some(line_end) = suffix
            .iter()
            .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
            .map(|index| index + 1)
        else {
            continue;
        };
        if let Some(line_start) = suffix[..line_end].iter().rposition(|byte| *byte == b'\n') {
            let line = std::str::from_utf8(&suffix[line_start + 1..line_end]).map_err(|error| {
                format!(
                    "最新 NDJSON 记录不是 UTF-8 path={} error={error}",
                    path.display()
                )
            })?;
            return serde_json::from_str(line).map_err(|error| {
                format!("无法解析最新 NDJSON path={} error={error}", path.display())
            });
        }
        if position == 0 {
            let line = std::str::from_utf8(&suffix[..line_end]).map_err(|error| {
                format!(
                    "最新 NDJSON 记录不是 UTF-8 path={} error={error}",
                    path.display()
                )
            })?;
            return serde_json::from_str(line).map_err(|error| {
                format!("无法解析最新 NDJSON path={} error={error}", path.display())
            });
        }
    }
    Err(format!("NDJSON 没有有效记录 path={}", path.display()))
}

fn latest_manifest_path(directory: &Path) -> Result<Option<PathBuf>, String> {
    let mut candidates = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "无法读取 manifest 目录 path={} error={error}",
                directory.display()
            )
        })?
        .filter_map(|entry_result| entry_result.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("-manifest-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    Ok(candidates.pop())
}

fn read_os_evidence_cached(
    directory: &Path,
    manifest: &RollingManifest,
    manifest_path: &Path,
    semantic_events: &[SemanticEvent],
    mcp_process_ids: &HashSet<u32>,
    limit: usize,
    cache: &ProjectionCache,
) -> Result<Arc<OsEvidenceData>, String> {
    let fingerprint = correlation_fingerprint(semantic_events, mcp_process_ids);
    let key = OsEvidenceCacheKey {
        manifest_path: manifest_path.to_path_buf(),
        manifest_size: file_size(manifest_path)?,
        correlation_fingerprint: fingerprint,
    };
    let mut cached = cache
        .os_evidence
        .lock()
        .map_err(|error| format!("GUI OS 证据缓存锁已损坏 error={error}"))?;
    if let Some(existing) = cached.as_ref().filter(|existing| existing.key == key) {
        return Ok(Arc::clone(&existing.value));
    }

    let value = Arc::new(read_os_evidence(
        directory,
        manifest,
        semantic_events,
        mcp_process_ids,
        limit,
        fingerprint,
    )?);
    *cached = Some(CachedOsEvidence {
        key,
        value: Arc::clone(&value),
    });
    Ok(value)
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("无法读取文件元数据 path={} error={error}", path.display()))
}

fn read_os_evidence(
    directory: &Path,
    manifest: &RollingManifest,
    semantic_events: &[SemanticEvent],
    mcp_process_ids: &HashSet<u32>,
    limit: usize,
    fingerprint: u64,
) -> Result<OsEvidenceData, String> {
    if manifest.segments.is_empty() {
        return Err(format!(
            "manifest 没有已发布证据段 session_id={} complete={}",
            manifest.session_id, manifest.complete
        ));
    }
    let windows = correlation_windows(semantic_events);
    let semantic_process_ids = semantic_events
        .iter()
        .map(|event| event.process_id)
        .collect::<HashSet<_>>();
    let cache_directory = directory
        .parent()
        .unwrap_or(directory)
        .join(".projection-cache")
        .join(&manifest.session_id);
    let mut target_event_count = 0_u64;
    let mut action_counts = OsActionCounts::default();
    let mut correlation_events = Vec::new();
    let mut process_root_entries = HashSet::<(String, u32, String)>::new();
    let mut recent_full_events = VecDeque::<ProcessEvidenceEvent>::with_capacity(limit);
    for segment in &manifest.segments {
        let segment_data = read_os_segment_index(
            directory,
            &cache_directory,
            &manifest.session_id,
            segment,
            &windows,
            &semantic_process_ids,
            mcp_process_ids,
            limit,
            fingerprint,
        )?;
        target_event_count = target_event_count
            .checked_add(segment_data.target_event_count)
            .ok_or_else(|| String::from("目标 OS 事件数量超出范围"))?;
        action_counts.process = action_counts
            .process
            .saturating_add(segment_data.action_counts.process);
        action_counts.file = action_counts
            .file
            .saturating_add(segment_data.action_counts.file);
        action_counts.network = action_counts
            .network
            .saturating_add(segment_data.action_counts.network);
        correlation_events.extend(segment_data.correlation_events);
        for process_root in segment_data.process_roots {
            process_root_entries.insert((
                process_root.session_id,
                process_root.process_id,
                process_root.root_process_instance_id,
            ));
        }
        for event in segment_data.recent_full_events {
            if recent_full_events.len() == limit {
                recent_full_events.pop_front();
            }
            recent_full_events.push_back(event);
        }
    }
    let process_roots = process_root_entries
        .into_iter()
        .map(
            |(session_id, process_id, root_process_instance_id)| ProcessRootAssociation {
                session_id,
                process_id,
                root_process_instance_id,
            },
        )
        .collect::<Vec<_>>();
    Ok(OsEvidenceData {
        target_event_count,
        correlation_events,
        process_roots,
        action_counts,
        recent_full_events: recent_full_events.into_iter().collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn read_os_segment_index(
    directory: &Path,
    cache_directory: &Path,
    session_id: &str,
    segment: &SegmentRecord,
    windows: &[CorrelationWindow],
    semantic_process_ids: &HashSet<u32>,
    mcp_process_ids: &HashSet<u32>,
    limit: usize,
    fingerprint: u64,
) -> Result<OsSegmentEvidenceData, String> {
    let filtered_path = directory.join(&segment.filtered_file);
    let full_path = directory.join(&segment.full_file);
    let key = OsSegmentIndexKey {
        session_id: session_id.to_owned(),
        filtered_file: segment.filtered_file.clone(),
        full_file: segment.full_file.clone(),
        filtered_size: file_size(&filtered_path)?,
        full_size: file_size(&full_path)?,
        filtered_events: segment.filtered_events,
        correlation_fingerprint: fingerprint,
    };
    let cache_path = os_segment_cache_path(cache_directory, &key)?;
    if cache_path.is_file() {
        let persisted: PersistedOsSegmentIndex = read_json(&cache_path)?;
        if persisted.schema_version != "0.1.0" || persisted.key != key {
            return Err(format!(
                "OS 持久索引契约或键不匹配 path={}",
                cache_path.display()
            ));
        }
        return Ok(persisted.data);
    }

    let data = build_os_segment_index(
        &filtered_path,
        &full_path,
        segment.filtered_events,
        windows,
        semantic_process_ids,
        mcp_process_ids,
        limit,
    )?;
    persist_os_segment_index(&cache_path, &key, &data)?;
    Ok(data)
}

fn os_segment_cache_path(
    cache_directory: &Path,
    key: &OsSegmentIndexKey,
) -> Result<PathBuf, String> {
    let file_name = Path::new(&key.filtered_file)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("无效的 OS 证据段文件名 value={}", key.filtered_file))?;
    Ok(cache_directory.join(format!(
        "{file_name}.{:016x}.{}.{}.json",
        key.correlation_fingerprint, key.filtered_size, key.full_size
    )))
}

fn persist_os_segment_index(
    path: &Path,
    key: &OsSegmentIndexKey,
    data: &OsSegmentEvidenceData,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("OS 持久索引路径缺少父目录 path={}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "无法创建 OS 持久索引目录 path={} error={error}",
            parent.display()
        )
    })?;
    let temporary_path = path.with_extension(format!("tmp-{}", std::process::id()));
    if temporary_path.exists() {
        return Err(format!(
            "OS 持久索引临时文件已存在 path={}",
            temporary_path.display()
        ));
    }
    let contents = serde_json::to_vec(&PersistedOsSegmentIndexWrite {
        schema_version: "0.1.0",
        key,
        data,
    })
    .map_err(|error| {
        format!(
            "无法序列化 OS 持久索引 path={} error={error}",
            path.display()
        )
    })?;
    fs::write(&temporary_path, contents).map_err(|error| {
        format!(
            "无法写入 OS 持久索引临时文件 path={} error={error}",
            temporary_path.display()
        )
    })?;
    fs::rename(&temporary_path, path).map_err(|error| {
        format!(
            "无法发布 OS 持久索引 source={} target={} error={error}",
            temporary_path.display(),
            path.display()
        )
    })
}

fn build_os_segment_index(
    filtered_path: &Path,
    full_path: &Path,
    expected_filtered_events: u64,
    windows: &[CorrelationWindow],
    semantic_process_ids: &HashSet<u32>,
    mcp_process_ids: &HashSet<u32>,
    limit: usize,
) -> Result<OsSegmentEvidenceData, String> {
    let mut target_event_count = 0_u64;
    let mut action_counts = OsActionCounts::default();
    let mut correlation_events = Vec::new();
    let mut process_root_entries = HashSet::<(String, u32, String)>::new();
    let mut recent_target_ids = VecDeque::<String>::with_capacity(limit);
    for_each_ndjson_line(filtered_path, |line| {
        let event = serde_json::from_str::<ObservationIndexRecord<'_>>(line)
            .map_err(|error| format!("无法解析目标 OS 事件索引 error={error}"))?;
        target_event_count = target_event_count
            .checked_add(1)
            .ok_or_else(|| String::from("目标 OS 事件数量超出范围"))?;
        increment_action_counts(&mut action_counts, event.action.kind);
        if recent_target_ids.len() == limit {
            recent_target_ids.pop_front();
        }
        recent_target_ids.push_back(event.event_id.to_owned());
        if semantic_process_ids.contains(&event.process.pid)
            || mcp_process_ids.contains(&event.process.pid)
        {
            process_root_entries.insert((
                event.session.id.to_owned(),
                event.process.pid,
                event.actor.root_process_instance_id.to_owned(),
            ));
        }
        if matches_index_correlation_window(&event, windows)
            || mcp_process_ids.contains(&event.process.pid)
        {
            correlation_events.push(serde_json::from_str::<ObservationEvent>(line).map_err(
                |error| {
                    format!(
                        "无法解析关联候选 OS 事件 event_id={} error={error}",
                        event.event_id
                    )
                },
            )?);
        }
        Ok(())
    })?;
    if target_event_count != expected_filtered_events {
        return Err(format!(
            "manifest 目标事件计数不闭合 expected={expected_filtered_events} actual={target_event_count} path={}",
            filtered_path.display()
        ));
    }
    let recent_target_ids = recent_target_ids.into_iter().collect::<Vec<_>>();
    let mut unresolved_ids = recent_target_ids.iter().cloned().collect::<HashSet<_>>();
    let mut full_events_by_id = HashMap::<String, ProcessEvidenceEvent>::new();
    for_each_ndjson_line(full_path, |line| {
        let event_id = serde_json::from_str::<EventIdRecord<'_>>(line)
            .map_err(|error| format!("无法解析完整 OS 事件索引 error={error}"))?;
        if unresolved_ids.remove(event_id.event_id) {
            let event = serde_json::from_str::<ProcessEvidenceEvent>(line).map_err(|error| {
                format!(
                    "无法解析完整 OS 目标事件 event_id={} error={error}",
                    event_id.event_id
                )
            })?;
            full_events_by_id.insert(event.event_id.clone(), event);
        }
        Ok(())
    })?;
    if !unresolved_ids.is_empty() {
        let mut missing_ids = unresolved_ids.into_iter().collect::<Vec<_>>();
        missing_ids.sort();
        return Err(format!(
            "完整 OS 证据缺少目标事件 count={} first={} path={}",
            missing_ids.len(),
            missing_ids.first().map_or("unknown", String::as_str),
            full_path.display()
        ));
    }
    let recent_full_events = recent_target_ids
        .iter()
        .map(|event_id| {
            full_events_by_id
                .remove(event_id)
                .ok_or_else(|| format!("完整 OS 证据索引缺少目标事件 event_id={event_id}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let process_roots = process_root_entries
        .into_iter()
        .map(
            |(session_id, process_id, root_process_instance_id)| ProcessRootAssociation {
                session_id,
                process_id,
                root_process_instance_id,
            },
        )
        .collect();
    Ok(OsSegmentEvidenceData {
        target_event_count,
        correlation_events,
        process_roots,
        action_counts,
        recent_full_events,
    })
}

fn correlation_windows(events: &[SemanticEvent]) -> Vec<CorrelationWindow> {
    events
        .iter()
        .filter_map(|event| {
            event.expected_os_action.map(|action| CorrelationWindow {
                action,
                start_unix_ms: event
                    .event_timestamp_unix_ms
                    .saturating_sub(CORRELATION_WINDOW_MS),
                end_unix_ms: event
                    .event_timestamp_unix_ms
                    .saturating_add(CORRELATION_WINDOW_MS),
            })
        })
        .collect()
}

fn correlation_fingerprint(events: &[SemanticEvent], mcp_process_ids: &HashSet<u32>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for event in events {
        event.event_id.hash(&mut hasher);
        event.event_timestamp_unix_ms.hash(&mut hasher);
        event.process_id.hash(&mut hasher);
        event.expected_os_action.hash(&mut hasher);
        event.operation_id.hash(&mut hasher);
        event.tool_name.hash(&mut hasher);
        event.content.hash(&mut hasher);
    }
    let mut sorted_mcp_process_ids = mcp_process_ids.iter().copied().collect::<Vec<_>>();
    sorted_mcp_process_ids.sort_unstable();
    sorted_mcp_process_ids.hash(&mut hasher);
    hasher.finish()
}

fn matches_index_correlation_window(
    event: &ObservationIndexRecord<'_>,
    windows: &[CorrelationWindow],
) -> bool {
    let timestamp = if event
        .event_timestamp_unix_ms
        .abs_diff(event.observed_at_unix_ms)
        > EVENT_CLOCK_SKEW_LIMIT_MS
    {
        event.observed_at_unix_ms
    } else {
        event.event_timestamp_unix_ms
    };
    windows.iter().any(|window| {
        event.action.kind == window.action
            && timestamp >= window.start_unix_ms
            && timestamp <= window.end_unix_ms
    })
}

fn increment_action_counts(counts: &mut OsActionCounts, action: ActionKind) {
    if is_process_action(action) {
        counts.process = counts.process.saturating_add(1);
    }
    if is_file_action(action) {
        counts.file = counts.file.saturating_add(1);
    }
    if is_network_action(action) {
        counts.network = counts.network.saturating_add(1);
    }
}

fn unix_time_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间早于 Unix epoch error={error}"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|error| format!("系统时间超出毫秒范围 error={error}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::Deserialize;
    use serde_json::json;

    use crate::model::{GateStatus, ObserverState};

    use super::{gate_status, load_capability_audit, read_ndjson_prefix};

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct TestRecord {
        value: u64,
    }

    #[test]
    fn gate_passes_for_running_or_idle_observer_with_declared_boundaries() {
        assert_eq!(
            gate_status(ObserverState::Healthy),
            GateStatus::PassedWithBoundaries
        );
        assert_eq!(
            gate_status(ObserverState::Idle),
            GateStatus::PassedWithBoundaries
        );
        for observer_state in [
            ObserverState::Starting,
            ObserverState::Degraded,
            ObserverState::Unknown,
            ObserverState::Stopped,
        ] {
            assert_eq!(gate_status(observer_state), GateStatus::EvidenceDegraded);
        }
    }

    #[test]
    fn published_prefix_excludes_uncommitted_tail() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应晚于 Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "agentreins-semantic-prefix-{}-{unique}.ndjson",
            std::process::id()
        ));
        let published = b"{\"value\":1}\n";
        let mut contents = published.to_vec();
        contents.extend_from_slice(b"{\"value\":2");
        fs::write(&path, contents).expect("应写入语义发布边界测试文件");

        let published_bytes = u64::try_from(published.len()).expect("测试前缀长度应可转换");
        let records = read_ndjson_prefix::<TestRecord>(&path, published_bytes)
            .expect("完整发布前缀应保持可读");
        let partial = read_ndjson_prefix::<TestRecord>(
            &path,
            fs::metadata(&path).expect("应读取测试文件元数据").len(),
        );
        fs::remove_file(&path).expect("应清理语义发布边界测试文件");

        assert_eq!(records, vec![TestRecord { value: 1 }]);
        assert!(
            partial
                .expect_err("半行不得进入已发布边界")
                .contains("不在完整记录末尾")
        );
    }

    #[test]
    fn capability_audit_preserves_native_ids_and_read_only_boundary() {
        let path = temporary_path("capability-audit-valid", "json");
        fs::write(&path, capability_audit_report(false).to_string())
            .expect("应写入 Capability Audit 测试报告");

        let audit = load_capability_audit(&path, "session-1").expect("只读 Audit 报告应保持可读");
        fs::remove_file(&path).expect("应清理 Capability Audit 测试报告");

        assert!(audit.observed);
        assert_eq!(audit.enforcement_actions_applied, 0);
        assert_eq!(audit.records.len(), 1);
        assert_eq!(audit.records[0].request_id, "request-1");
        assert_eq!(audit.records[0].decision_id, "decision:request-1");
        assert!(!audit.records[0].applied);
    }

    #[test]
    fn capability_audit_rejects_applied_control_effect() {
        let path = temporary_path("capability-audit-applied", "json");
        fs::write(&path, capability_audit_report(true).to_string())
            .expect("应写入越界 Audit 测试报告");

        let error = load_capability_audit(&path, "session-1")
            .err()
            .expect("已应用控制必须被拒绝");
        fs::remove_file(&path).expect("应清理越界 Audit 测试报告");

        assert!(error.contains("已应用控制"));
    }

    fn temporary_path(label: &str, extension: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试系统时间应晚于 Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentreins-{label}-{}-{unique}.{extension}",
            std::process::id()
        ))
    }

    fn capability_audit_report(applied: bool) -> serde_json::Value {
        json!({
            "schema_version": "0.1.0",
            "mode": "audit",
            "requests": [{
                "request_id": "request-1",
                "event_timestamp_unix_ms": 1,
                "subject": {
                    "session_id": "session-1",
                    "user_activity_id": "activity-1"
                },
                "capability": "filesystem",
                "operation": "read",
                "resource": {
                    "kind": "filesystem",
                    "identifier": "C:\\\\workspace\\\\file.txt"
                },
                "evidence_event_ids": ["semantic-1"],
                "limitations": []
            }],
            "decisions": [{
                "decision_id": "decision:request-1",
                "request_id": "request-1",
                "disposition": "would_allow",
                "rule_id": "workspace-read",
                "reason": "工作区内读取",
                "required_grant": "none",
                "recovery": "none",
                "control_effect": {
                    "mode": "audit",
                    "applied": applied,
                    "effect": "observation_only"
                }
            }],
            "summary": {
                "requests": 1,
                "enforcement_actions_applied": if applied { 1 } else { 0 }
            }
        })
    }
}
