use std::collections::BTreeSet;

use native_contracts::{ActionKind, AgentCapabilityKind, SemanticContentKind, SemanticEvent};

use crate::model::{
    ActivityViewModel, CapabilityFacetViewModel, CapabilityFacetsViewModel, CapabilityGroup,
    CapabilityViewModel, CoverageState, ObserverState,
};

const ALL_CAPABILITIES: [AgentCapabilityKind; 16] = [
    AgentCapabilityKind::Shell,
    AgentCapabilityKind::File,
    AgentCapabilityKind::Browser,
    AgentCapabilityKind::Mcp,
    AgentCapabilityKind::Skill,
    AgentCapabilityKind::Credential,
    AgentCapabilityKind::Memory,
    AgentCapabilityKind::SubAgent,
    AgentCapabilityKind::Scheduler,
    AgentCapabilityKind::Hook,
    AgentCapabilityKind::Sandbox,
    AgentCapabilityKind::Permission,
    AgentCapabilityKind::RuntimeEnforcement,
    AgentCapabilityKind::Token,
    AgentCapabilityKind::Cache,
    AgentCapabilityKind::LocalResourceUsage,
];

struct CapabilityFacts {
    semantic_events: u64,
    invocation_events: u64,
    feedback_events: u64,
    native_identity_events: u64,
    runtime_events: u64,
    related_activity_ids: Vec<String>,
    local_resource: Option<LocalResourceSnapshot>,
}

#[derive(Clone, Copy)]
pub struct LocalResourceSnapshot {
    pub process_count: u64,
    pub unavailable_process_count: u64,
    pub working_set_bytes: u64,
    pub private_memory_bytes: u64,
    pub cpu_percent_normalized: Option<f64>,
    pub job_process_count: u64,
    pub app_container_process_count: u64,
    pub elevated_process_count: u64,
}

pub fn classify_semantic_event(event: &SemanticEvent) -> Vec<AgentCapabilityKind> {
    let mut capabilities = BTreeSet::new();
    if matches!(
        event.content_kind,
        Some(SemanticContentKind::ToolCall | SemanticContentKind::ToolResult)
    ) && let Some(tool_name) = event.tool_name.as_deref()
    {
        classify_tool_name(tool_name, &mut capabilities);
    }
    if event.content_kind == Some(SemanticContentKind::SkillPolicy) {
        capabilities.insert(AgentCapabilityKind::Skill);
    }
    if event.content_kind == Some(SemanticContentKind::AvailableMcp) {
        capabilities.insert(AgentCapabilityKind::Mcp);
    }
    if event.input_tokens.is_some()
        || event.output_tokens.is_some()
        || event.total_tokens.is_some()
        || event.reasoning_tokens.is_some()
        || event.request_count.is_some()
    {
        capabilities.insert(AgentCapabilityKind::Token);
    }
    if event.cached_tokens.is_some() {
        capabilities.insert(AgentCapabilityKind::Cache);
    }
    if matches!(
        event.content_kind,
        Some(
            SemanticContentKind::RuntimeContext
                | SemanticContentKind::ToolCall
                | SemanticContentKind::ToolResult
        )
    ) {
        classify_content(&event.content, &mut capabilities);
    }
    capabilities.into_iter().collect()
}

pub fn build_capability_views(
    semantic_events: &[SemanticEvent],
    activities: &[ActivityViewModel],
    observer_state: ObserverState,
    local_resource: Option<LocalResourceSnapshot>,
    capability_audit_requests: u64,
) -> Vec<CapabilityViewModel> {
    ALL_CAPABILITIES
        .into_iter()
        .map(|kind| {
            let facts = capability_facts(
                kind,
                semantic_events,
                activities,
                local_resource,
                capability_audit_requests,
            );
            capability_view(kind, facts, observer_state)
        })
        .collect()
}

fn classify_tool_name(tool_name: &str, capabilities: &mut BTreeSet<AgentCapabilityKind>) {
    let normalized = tool_name.to_ascii_lowercase();
    let capability = match normalized.as_str() {
        "bash" | "cmd" | "shell" | "powershell" | "terminal" => Some(AgentCapabilityKind::Shell),
        "read" | "write" | "edit" | "glob" | "grep" | "search" => Some(AgentCapabilityKind::File),
        "websearch" | "browser" | "webfetch" => Some(AgentCapabilityKind::Browser),
        "skill" => Some(AgentCapabilityKind::Skill),
        "memory" => Some(AgentCapabilityKind::Memory),
        "task" | "subagent" | "sub-agent" => Some(AgentCapabilityKind::SubAgent),
        "schedule" | "scheduler" | "cron" => Some(AgentCapabilityKind::Scheduler),
        "hook" => Some(AgentCapabilityKind::Hook),
        _ if normalized.contains("mcp") => Some(AgentCapabilityKind::Mcp),
        _ => None,
    };
    if let Some(value) = capability {
        capabilities.insert(value);
    }
}

fn classify_content(content: &str, capabilities: &mut BTreeSet<AgentCapabilityKind>) {
    let normalized = content.to_ascii_lowercase().replace('/', "\\");
    if normalized.contains("\\.workbuddy\\memory\\") {
        capabilities.insert(AgentCapabilityKind::Memory);
    }
    if contains_credential_path(&normalized) {
        capabilities.insert(AgentCapabilityKind::Credential);
    }
    if normalized.contains("--no-sandbox") {
        capabilities.insert(AgentCapabilityKind::Sandbox);
    }
    if normalized.contains("bypasspermissions")
        || normalized.contains("dangerously-skip-permissions")
    {
        capabilities.insert(AgentCapabilityKind::Permission);
    }
}

fn contains_credential_path(content: &str) -> bool {
    content.contains("credential")
        || content.contains("credentials")
        || content.contains("controlled-service-token")
        || content.contains("id_rsa")
        || content.contains("id_ed25519")
        || content.contains(".env")
}

fn capability_facts(
    kind: AgentCapabilityKind,
    semantic_events: &[SemanticEvent],
    activities: &[ActivityViewModel],
    local_resource: Option<LocalResourceSnapshot>,
    capability_audit_requests: u64,
) -> CapabilityFacts {
    let matching_events = semantic_events
        .iter()
        .filter(|event| classify_semantic_event(event).contains(&kind))
        .collect::<Vec<_>>();
    let mcp_request_ids = activities
        .iter()
        .flat_map(|activity| activity.timeline.iter())
        .filter_map(|item| {
            item.capabilities
                .contains(&AgentCapabilityKind::Mcp)
                .then_some(item.mcp_json_rpc_request_id.as_deref())
                .flatten()
        })
        .collect::<BTreeSet<_>>();
    let invocation_records = matching_events
        .iter()
        .copied()
        .filter(|event| is_invocation_event(kind, event))
        .collect::<Vec<_>>();
    let invocation_call_ids = invocation_records
        .iter()
        .filter_map(|event| event.tool_call_id.as_deref())
        .collect::<BTreeSet<_>>();
    let invocation_events = match kind {
        AgentCapabilityKind::RuntimeEnforcement => capability_audit_requests,
        AgentCapabilityKind::Mcp => mcp_request_ids.len() as u64,
        AgentCapabilityKind::Sandbox
        | AgentCapabilityKind::Permission
        | AgentCapabilityKind::LocalResourceUsage => {
            local_resource.map_or(0, |sample| sample.process_count)
        }
        _ => invocation_records.len() as u64,
    };
    let feedback_events = if kind == AgentCapabilityKind::Mcp {
        activities
            .iter()
            .flat_map(|activity| activity.timeline.iter())
            .filter(|item| {
                item.capabilities.contains(&AgentCapabilityKind::Mcp)
                    && item.mcp_response_observed == Some(true)
            })
            .filter_map(|item| item.mcp_json_rpc_request_id.as_deref())
            .collect::<BTreeSet<_>>()
            .len() as u64
    } else if kind == AgentCapabilityKind::RuntimeEnforcement {
        capability_audit_requests
    } else if matches!(
        kind,
        AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::LocalResourceUsage
    ) {
        u64::from(local_resource.is_some())
    } else {
        matching_events
            .iter()
            .filter(|event| {
                event.content_kind == Some(SemanticContentKind::ToolResult)
                    && event
                        .tool_call_id
                        .as_deref()
                        .is_some_and(|call_id| invocation_call_ids.contains(call_id))
            })
            .count() as u64
    };
    let native_identity_events = if kind == AgentCapabilityKind::Mcp {
        mcp_request_ids.len() as u64
    } else if kind == AgentCapabilityKind::RuntimeEnforcement {
        capability_audit_requests
    } else if matches!(
        kind,
        AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::LocalResourceUsage
    ) {
        local_resource.map_or(0, |sample| sample.process_count)
    } else {
        invocation_records
            .iter()
            .filter(|event| !event.native_evidence.identifiers.is_empty())
            .count() as u64
    };
    let related_activity_ids = activities
        .iter()
        .filter(|activity| activity.capabilities.contains(&kind))
        .map(|activity| activity.id.clone())
        .collect::<Vec<_>>();
    let runtime_events = if matches!(
        kind,
        AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::LocalResourceUsage
    ) {
        local_resource.map_or(0, |sample| sample.process_count)
    } else {
        activities
            .iter()
            .flat_map(|activity| activity.timeline.iter())
            .filter(|item| item.capabilities.contains(&kind))
            .map(|item| item.linked_os_events)
            .sum()
    };
    CapabilityFacts {
        semantic_events: matching_events.len() as u64,
        invocation_events,
        feedback_events,
        native_identity_events,
        runtime_events,
        related_activity_ids,
        local_resource,
    }
}

fn is_invocation_event(kind: AgentCapabilityKind, event: &SemanticEvent) -> bool {
    match kind {
        AgentCapabilityKind::Skill => {
            event.content_kind == Some(SemanticContentKind::SkillPolicy)
                || event.action_kind == ActionKind::ToolCall
        }
        AgentCapabilityKind::Token | AgentCapabilityKind::Cache => true,
        AgentCapabilityKind::RuntimeEnforcement | AgentCapabilityKind::LocalResourceUsage => false,
        _ => event.action_kind == ActionKind::ToolCall,
    }
}

fn capability_view(
    kind: AgentCapabilityKind,
    facts: CapabilityFacts,
    observer_state: ObserverState,
) -> CapabilityViewModel {
    let state = capability_state(kind, &facts, observer_state);
    let invocation_state = state_when_present(kind, facts.invocation_events);
    let feedback_state = state_when_present(kind, facts.feedback_events);
    let native_identity_state = state_when_present(kind, facts.native_identity_events);
    let runtime_state = runtime_state(kind, facts.runtime_events);
    CapabilityViewModel {
        kind,
        name: capability_name(kind),
        group: capability_group(kind),
        state,
        metric: capability_metric(
            kind,
            facts.invocation_events,
            facts.semantic_events,
            facts.local_resource,
        ),
        summary: capability_summary(kind),
        facets: CapabilityFacetsViewModel {
            invocation: facet(
                invocation_state,
                invocation_source(kind, facts.local_resource.is_some()),
                facts.invocation_events,
                missing_source_limitation(kind, invocation_state),
            ),
            parameters_target: facet(
                invocation_state,
                parameters_source(kind, facts.local_resource.is_some()),
                facts.invocation_events,
                missing_source_limitation(kind, invocation_state),
            ),
            agent_feedback: facet(
                feedback_state,
                feedback_source(kind, facts.local_resource.is_some()),
                facts.feedback_events,
                missing_source_limitation(kind, feedback_state),
            ),
            native_identity: facet(
                native_identity_state,
                native_identity_source(kind, facts.local_resource.is_some()),
                facts.native_identity_events,
                missing_source_limitation(kind, native_identity_state),
            ),
            runtime_corroboration: facet(
                runtime_state,
                "严格限定到 Agent 的 OS 原生证据",
                facts.runtime_events,
                resource_or_runtime_limitation(kind, &facts, runtime_state),
            ),
            data_health: facet(
                health_state(observer_state),
                "OS 与语义 Observer 健康快照、MCP 发布边界",
                3,
                health_limitation(observer_state),
            ),
        },
        related_activity_ids: facts.related_activity_ids,
    }
}

fn capability_state(
    kind: AgentCapabilityKind,
    facts: &CapabilityFacts,
    observer_state: ObserverState,
) -> CoverageState {
    if !matches!(observer_state, ObserverState::Healthy | ObserverState::Idle) {
        return CoverageState::Degraded;
    }
    if matches!(
        kind,
        AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::LocalResourceUsage
    ) && facts.local_resource.is_some()
    {
        return local_resource_state(facts.local_resource);
    }
    if facts.invocation_events == 0 {
        return state_without_evidence(kind);
    }
    if requires_runtime_corroboration(kind) && facts.runtime_events == 0 {
        return CoverageState::Partial;
    }
    if facts.feedback_events == 0 && expects_feedback(kind) {
        return CoverageState::Partial;
    }
    CoverageState::Observed
}

fn local_resource_state(sample: Option<LocalResourceSnapshot>) -> CoverageState {
    let Some(sample) = sample else {
        return CoverageState::NotObservable;
    };
    if sample.process_count == 0 && sample.unavailable_process_count == 0 {
        return CoverageState::NotTriggered;
    }
    if sample.unavailable_process_count > 0 {
        return CoverageState::Partial;
    }
    CoverageState::Observed
}

fn state_when_present(kind: AgentCapabilityKind, count: u64) -> CoverageState {
    if count > 0 {
        CoverageState::Observed
    } else {
        state_without_evidence(kind)
    }
}

fn state_without_evidence(kind: AgentCapabilityKind) -> CoverageState {
    match kind {
        AgentCapabilityKind::SubAgent
        | AgentCapabilityKind::Scheduler
        | AgentCapabilityKind::Hook
        | AgentCapabilityKind::Sandbox
        | AgentCapabilityKind::Permission
        | AgentCapabilityKind::LocalResourceUsage => CoverageState::NotObservable,
        AgentCapabilityKind::RuntimeEnforcement => CoverageState::Partial,
        _ => CoverageState::NotTriggered,
    }
}

fn runtime_state(kind: AgentCapabilityKind, count: u64) -> CoverageState {
    if count > 0 {
        return CoverageState::Observed;
    }
    if requires_runtime_corroboration(kind) {
        return CoverageState::Partial;
    }
    CoverageState::NotObservable
}

fn requires_runtime_corroboration(kind: AgentCapabilityKind) -> bool {
    matches!(
        kind,
        AgentCapabilityKind::Shell
            | AgentCapabilityKind::File
            | AgentCapabilityKind::Mcp
            | AgentCapabilityKind::Credential
            | AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::LocalResourceUsage
    )
}

fn expects_feedback(kind: AgentCapabilityKind) -> bool {
    !matches!(
        kind,
        AgentCapabilityKind::Token
            | AgentCapabilityKind::Cache
            | AgentCapabilityKind::Sandbox
            | AgentCapabilityKind::Permission
            | AgentCapabilityKind::RuntimeEnforcement
            | AgentCapabilityKind::LocalResourceUsage
    )
}

fn health_state(observer_state: ObserverState) -> CoverageState {
    match observer_state {
        ObserverState::Healthy | ObserverState::Idle => CoverageState::Observed,
        ObserverState::Starting
        | ObserverState::Degraded
        | ObserverState::Unknown
        | ObserverState::Stopped => CoverageState::Degraded,
    }
}

fn facet(
    state: CoverageState,
    source: &str,
    evidence_count: u64,
    limitation: Option<String>,
) -> CapabilityFacetViewModel {
    CapabilityFacetViewModel {
        state,
        source: String::from(source),
        evidence_count,
        limitation,
    }
}

fn missing_source_limitation(kind: AgentCapabilityKind, state: CoverageState) -> Option<String> {
    (state != CoverageState::Observed).then(|| match state {
        CoverageState::NotTriggered => String::from("当前已发布会话没有触发该能力。"),
        CoverageState::NotObservable => format!(
            "WorkBuddy 当前没有提供可验证的 {} 原生来源。",
            capability_name(kind)
        ),
        CoverageState::Partial => String::from("只能观察部分参数或产品状态。"),
        CoverageState::Degraded => String::from("当前 Observer 证据不完整。"),
        CoverageState::Observed => String::new(),
    })
}

fn runtime_limitation(kind: AgentCapabilityKind, state: CoverageState) -> Option<String> {
    if state == CoverageState::Observed {
        return None;
    }
    Some(if requires_runtime_corroboration(kind) {
        String::from("没有共同原生 ID 时不使用时间、PID、路径或参数建立运行时因果关系。")
    } else {
        String::from("该能力默认只依赖 Agent 原生事实，不采集系统或其他应用内部数据。")
    })
}

fn resource_or_runtime_limitation(
    kind: AgentCapabilityKind,
    facts: &CapabilityFacts,
    state: CoverageState,
) -> Option<String> {
    if kind != AgentCapabilityKind::LocalResourceUsage {
        return runtime_limitation(kind, state);
    }
    match facts.local_resource {
        None => Some(String::from(
            "当前 OS Observer 健康记录尚未包含 Agent 资源样本。",
        )),
        Some(sample) if sample.unavailable_process_count > 0 => Some(format!(
            "有 {} 个已验证 Agent 进程在采样时不可用，失败已保留在健康证据中。",
            sample.unavailable_process_count
        )),
        Some(_) => None,
    }
}

fn health_limitation(observer_state: ObserverState) -> Option<String> {
    (!matches!(observer_state, ObserverState::Healthy | ObserverState::Idle))
        .then(|| String::from("Observer 停止、未知或降级，当前窗口不能证明数据完整。"))
}

fn capability_name(kind: AgentCapabilityKind) -> &'static str {
    match kind {
        AgentCapabilityKind::Shell => "Shell",
        AgentCapabilityKind::File => "File",
        AgentCapabilityKind::Browser => "Browser",
        AgentCapabilityKind::Mcp => "MCP",
        AgentCapabilityKind::Skill => "Skill",
        AgentCapabilityKind::Credential => "Credential",
        AgentCapabilityKind::Memory => "Memory",
        AgentCapabilityKind::SubAgent => "Sub-Agent",
        AgentCapabilityKind::Scheduler => "Scheduler",
        AgentCapabilityKind::Hook => "Hook",
        AgentCapabilityKind::Sandbox => "Sandbox",
        AgentCapabilityKind::Permission => "Permission",
        AgentCapabilityKind::RuntimeEnforcement => "Runtime Enforcement",
        AgentCapabilityKind::Token => "Token",
        AgentCapabilityKind::Cache => "Cache",
        AgentCapabilityKind::LocalResourceUsage => "本地资源占用",
    }
}

fn capability_metric(
    kind: AgentCapabilityKind,
    invocation_events: u64,
    semantic_events: u64,
    local_resource: Option<LocalResourceSnapshot>,
) -> String {
    match kind {
        AgentCapabilityKind::RuntimeEnforcement => String::from("applied = false"),
        AgentCapabilityKind::Sandbox => local_resource.map_or_else(
            || format!("{invocation_events} 次真实调用"),
            |sample| {
                format!(
                    "AppContainer {}/{} · Job {}/{}",
                    sample.app_container_process_count,
                    sample.process_count,
                    sample.job_process_count,
                    sample.process_count
                )
            },
        ),
        AgentCapabilityKind::Permission => local_resource.map_or_else(
            || format!("{invocation_events} 次真实调用"),
            |sample| {
                format!(
                    "Elevated {}/{}",
                    sample.elevated_process_count, sample.process_count
                )
            },
        ),
        AgentCapabilityKind::LocalResourceUsage => local_resource.map_or_else(
            || String::from("持续采样尚未接入"),
            |sample| {
                let cpu = sample.cpu_percent_normalized.map_or_else(
                    || String::from("CPU 预热中"),
                    |value| format!("CPU {value:.2}%"),
                );
                format!(
                    "{cpu} · 工作集 {:.1} MB · 私有内存 {:.1} MB",
                    bytes_to_megabytes(sample.working_set_bytes),
                    bytes_to_megabytes(sample.private_memory_bytes)
                )
            },
        ),
        AgentCapabilityKind::Token | AgentCapabilityKind::Cache => {
            format!("{semantic_events} 条真实用量记录")
        }
        _ => format!("{invocation_events} 次真实调用"),
    }
}

fn bytes_to_megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn invocation_source(kind: AgentCapabilityKind, has_runtime_sample: bool) -> &'static str {
    match kind {
        AgentCapabilityKind::Sandbox | AgentCapabilityKind::Permission if has_runtime_sample => {
            "目标 Agent 进程安全快照"
        }
        AgentCapabilityKind::Token | AgentCapabilityKind::Cache => "WorkBuddy provider usage",
        AgentCapabilityKind::RuntimeEnforcement => "Capability Audit 模式",
        AgentCapabilityKind::LocalResourceUsage => "目标 Agent 进程采样",
        _ => "WorkBuddy 原生语义记录",
    }
}

fn parameters_source(kind: AgentCapabilityKind, has_runtime_sample: bool) -> &'static str {
    match kind {
        AgentCapabilityKind::Sandbox if has_runtime_sample => "AppContainer 与 Job 状态",
        AgentCapabilityKind::Permission if has_runtime_sample => "Token elevation 状态",
        AgentCapabilityKind::Token | AgentCapabilityKind::Cache => "模型请求与 usage 原生字段",
        AgentCapabilityKind::RuntimeEnforcement => "Audit 请求、规则与判断",
        AgentCapabilityKind::LocalResourceUsage => "进程实例与采样窗口",
        _ => "WorkBuddy ToolCall 参数或原生内容",
    }
}

fn feedback_source(kind: AgentCapabilityKind, has_runtime_sample: bool) -> &'static str {
    match kind {
        AgentCapabilityKind::Sandbox | AgentCapabilityKind::Permission if has_runtime_sample => {
            "Agent 进程安全采样结果"
        }
        AgentCapabilityKind::Token | AgentCapabilityKind::Cache => "WorkBuddy provider usage",
        AgentCapabilityKind::RuntimeEnforcement => "Capability Audit 判断",
        AgentCapabilityKind::LocalResourceUsage => "Agent 进程资源样本",
        _ => "WorkBuddy ToolResult",
    }
}

fn native_identity_source(kind: AgentCapabilityKind, has_runtime_sample: bool) -> &'static str {
    match kind {
        AgentCapabilityKind::Sandbox | AgentCapabilityKind::Permission if has_runtime_sample => {
            "ProcessStartKey / UniqueProcessKey"
        }
        AgentCapabilityKind::Mcp => "WorkBuddy callId / MCP JSON-RPC ID",
        AgentCapabilityKind::LocalResourceUsage => "ProcessStartKey / UniqueProcessKey",
        _ => "WorkBuddy sessionId / recordId / callId",
    }
}

fn capability_group(kind: AgentCapabilityKind) -> CapabilityGroup {
    match kind {
        AgentCapabilityKind::Shell | AgentCapabilityKind::File | AgentCapabilityKind::Browser => {
            CapabilityGroup::ExecutionAccess
        }
        AgentCapabilityKind::Mcp | AgentCapabilityKind::Skill | AgentCapabilityKind::Hook => {
            CapabilityGroup::ExtensionTool
        }
        AgentCapabilityKind::SubAgent | AgentCapabilityKind::Scheduler => {
            CapabilityGroup::AutonomousRun
        }
        AgentCapabilityKind::Credential
        | AgentCapabilityKind::Memory
        | AgentCapabilityKind::Token
        | AgentCapabilityKind::Cache => CapabilityGroup::DataContext,
        AgentCapabilityKind::Sandbox
        | AgentCapabilityKind::Permission
        | AgentCapabilityKind::RuntimeEnforcement
        | AgentCapabilityKind::LocalResourceUsage => CapabilityGroup::RuntimeEnvironment,
    }
}

fn capability_summary(kind: AgentCapabilityKind) -> &'static str {
    match kind {
        AgentCapabilityKind::Shell => "展示 Agent 发出的命令、参数和 Agent 收到的执行结果。",
        AgentCapabilityKind::File => "展示 Agent 请求的文件动作、目标路径和返回结果。",
        AgentCapabilityKind::Browser => "只展示 Agent 的浏览或搜索调用及其反馈，不进入浏览器内部。",
        AgentCapabilityKind::Mcp => "展示 ToolCall、MCP JSON-RPC 请求响应及原生关联断点。",
        AgentCapabilityKind::Skill => "展示 Agent 加载或使用 Skill 的原生上下文和结果。",
        AgentCapabilityKind::Credential => "只展示 Agent 实际访问或发送的凭据，不扫描凭据库。",
        AgentCapabilityKind::Memory => "展示 Agent 原生 Memory 调用或明确的 Agent Memory 持久化。",
        AgentCapabilityKind::SubAgent => "展示原生父子会话、委派内容、结果、取消和退出。",
        AgentCapabilityKind::Scheduler => "只展示 Agent 创建、修改、触发或取消的调度任务。",
        AgentCapabilityKind::Hook => "只接入 Agent 原生 Hook，不实施进程注入或系统 Hook。",
        AgentCapabilityKind::Sandbox => "展示 Agent 隔离参数和目标 Agent 进程的隔离状态。",
        AgentCapabilityKind::Permission => "展示 Agent 权限参数和目标 Agent 进程权限事实。",
        AgentCapabilityKind::RuntimeEnforcement => "当前只展示意图与 Audit 判断，控制功能未启用。",
        AgentCapabilityKind::Token => "展示 Agent 原生报告的输入、输出、总量和推理 Token。",
        AgentCapabilityKind::Cache => "展示 Agent 原生报告的缓存 Token 或明确缓存调用。",
        AgentCapabilityKind::LocalResourceUsage => {
            "只采样已验证 Agent 进程树的 CPU、工作集和私有内存。"
        }
    }
}

#[cfg(test)]
mod tests {
    use native_contracts::{
        ActionKind, AgentCapabilityKind, EvidenceSource, NativeEvidence, ObservationStatus,
        SemanticContentKind, SemanticEvent,
    };

    use super::{LocalResourceSnapshot, build_capability_views, classify_semantic_event};
    use crate::model::{CoverageState, ObserverState};

    fn semantic_event(tool_name: &str, content: &str) -> SemanticEvent {
        SemanticEvent {
            schema_version: String::from("0.6.0"),
            event_id: String::from("event-1"),
            event_timestamp_unix_ms: 1,
            session_id: String::from("observer-session-1"),
            process_id: 42,
            user_activity_id: String::from("activity-1"),
            agent_session_id: Some(String::from("agent-session-1")),
            turn_id: Some(String::from("turn-1")),
            tool_call_id: Some(String::from("call-1")),
            workspace_path: Some(String::from("C:\\workspace")),
            agent_id: Some(String::from("workbuddy")),
            provider_message_id: Some(String::from("message-1")),
            source_schema_profile: Some(String::from("workbuddy-project-jsonl-v1")),
            observation_layer: None,
            observation_status: ObservationStatus::Observed,
            content_kind: Some(SemanticContentKind::ToolCall),
            action_kind: ActionKind::ToolCall,
            content: String::from(content),
            tool_name: Some(String::from(tool_name)),
            operation_id: Some(String::from("call-1")),
            operation_id_origin: None,
            native_evidence: NativeEvidence::default(),
            expected_os_action: Some(ActionKind::ProcessStart),
            evidence_source: EvidenceSource::AgentSemantic,
            source_record_id: String::from("record-1"),
            parent_record_id: None,
            trace_id: None,
            model_id: None,
            request_model_id: None,
            request_model_name: None,
            conversation_request_id: None,
            record_status: None,
            error: None,
            input_tokens: Some(12),
            output_tokens: Some(4),
            total_tokens: Some(16),
            cached_tokens: Some(3),
            reasoning_tokens: None,
            request_count: Some(1),
        }
    }

    #[test]
    fn classifies_agent_native_capabilities_without_time_inference() {
        let event = semantic_event("PowerShell", r#"{"command":"agent.exe --no-sandbox"}"#);
        let capabilities = classify_semantic_event(&event);

        assert!(capabilities.contains(&AgentCapabilityKind::Shell));
        assert!(capabilities.contains(&AgentCapabilityKind::Sandbox));
        assert!(!capabilities.contains(&AgentCapabilityKind::RuntimeEnforcement));
        assert!(capabilities.contains(&AgentCapabilityKind::Token));
        assert!(capabilities.contains(&AgentCapabilityKind::Cache));
    }

    #[test]
    fn does_not_treat_available_tools_or_context_text_as_invocations() {
        let mut event = semantic_event("Read", "项目说明提到了 .env 和 credential");
        event.content_kind = Some(SemanticContentKind::AvailableTool);
        event.action_kind = ActionKind::LlmRequest;

        let classified = classify_semantic_event(&event);

        assert!(!classified.contains(&AgentCapabilityKind::File));
        assert!(!classified.contains(&AgentCapabilityKind::Credential));
    }

    #[test]
    fn available_mcp_does_not_claim_a_real_invocation() {
        let mut event = semantic_event("present_files", "MCP 工具可用");
        event.content_kind = Some(SemanticContentKind::AvailableMcp);
        event.action_kind = ActionKind::LlmRequest;
        event.input_tokens = None;
        event.output_tokens = None;
        event.total_tokens = None;
        event.cached_tokens = None;
        event.request_count = None;

        let capabilities = build_capability_views(&[event], &[], ObserverState::Healthy, None, 0);
        let mcp = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Mcp)
            .expect("MCP 能力必须存在");

        assert!(mcp.state == CoverageState::NotTriggered);
        assert!(mcp.metric == "0 次真实调用");
    }

    #[test]
    fn feedback_requires_the_same_native_tool_call_id() {
        let call = semantic_event("Read", r#"{"file_path":"C:\\keys\\id_rsa"}"#);
        let mut matching_result = semantic_event("Read", "credential loaded");
        matching_result.content_kind = Some(SemanticContentKind::ToolResult);
        matching_result.action_kind = ActionKind::ToolResult;
        let mut unrelated_result = matching_result.clone();
        unrelated_result.tool_call_id = Some(String::from("call-2"));

        let capabilities = build_capability_views(
            &[call, matching_result, unrelated_result],
            &[],
            ObserverState::Healthy,
            None,
            0,
        );
        let credential = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Credential)
            .expect("Credential 能力必须存在");

        assert!(credential.facets.invocation.evidence_count == 1);
        assert!(credential.facets.agent_feedback.evidence_count == 1);
    }

    #[test]
    fn distinguishes_not_triggered_from_missing_native_source() {
        let capabilities = build_capability_views(&[], &[], ObserverState::Healthy, None, 0);
        let browser = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Browser)
            .expect("Browser 能力必须存在");
        let hook = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Hook)
            .expect("Hook 能力必须存在");
        let sandbox = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Sandbox)
            .expect("Sandbox 能力必须存在");
        let runtime = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::RuntimeEnforcement)
            .expect("Runtime Enforcement 能力必须存在");

        assert!(browser.state == CoverageState::NotTriggered);
        assert!(hook.state == CoverageState::NotObservable);
        assert!(sandbox.state == CoverageState::NotObservable);
        assert!(runtime.state == CoverageState::Partial);
    }

    #[test]
    fn displays_real_local_resource_sample() {
        let capabilities = build_capability_views(
            &[],
            &[],
            ObserverState::Healthy,
            Some(LocalResourceSnapshot {
                process_count: 2,
                unavailable_process_count: 0,
                working_set_bytes: 157_286_400,
                private_memory_bytes: 104_857_600,
                cpu_percent_normalized: Some(1.25),
                job_process_count: 2,
                app_container_process_count: 0,
                elevated_process_count: 0,
            }),
            0,
        );
        let resource = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::LocalResourceUsage)
            .expect("本地资源占用能力必须存在");
        let sandbox = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Sandbox)
            .expect("Sandbox 能力必须存在");
        let permission = capabilities
            .iter()
            .find(|item| item.kind == AgentCapabilityKind::Permission)
            .expect("Permission 能力必须存在");

        assert!(resource.state == CoverageState::Observed);
        assert!(resource.metric == "CPU 1.25% · 工作集 150.0 MB · 私有内存 100.0 MB");
        assert!(sandbox.state == CoverageState::Observed);
        assert!(sandbox.metric == "AppContainer 0/2 · Job 2/2");
        assert!(permission.state == CoverageState::Observed);
        assert!(permission.metric == "Elevated 0/2");
    }
}
