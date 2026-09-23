use std::net::IpAddr;
use std::path::Path;

use native_contracts::{ActionKind, ObservationEvent, SemanticContentKind, SemanticEvent};
use serde::{Deserialize, Serialize};

pub const CAPABILITY_AUDIT_SCHEMA_VERSION: &str = "0.1.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Tool,
    Credential,
    Network,
    Filesystem,
    PackageManager,
    Sandbox,
    RuntimeEnforcement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityOperation {
    Invoke,
    Execute,
    Read,
    Write,
    Delete,
    Rename,
    Connect,
    Install,
    Access,
    DisableIsolation,
    ChangePolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRelation {
    InWorkspace,
    OutsideWorkspace,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Confirmed,
    Partial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDisposition {
    WouldAllow,
    WouldWarn,
    WouldBlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementMode {
    Audit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedEffect {
    ObservationOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilitySubject {
    pub agent_id: String,
    pub session_id: String,
    pub user_activity_id: Option<String>,
    pub process_id: u32,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuditContext {
    pub workspace_root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityResource {
    pub kind: CapabilityKind,
    pub identifier: String,
    pub workspace_relation: WorkspaceRelation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityRequest {
    pub schema_version: &'static str,
    pub request_id: String,
    pub event_timestamp_unix_ms: u64,
    pub subject: CapabilitySubject,
    pub capability: CapabilityKind,
    pub operation: CapabilityOperation,
    pub resource: CapabilityResource,
    pub evidence_strength: EvidenceStrength,
    pub evidence_event_ids: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControlEffect {
    pub mode: EnforcementMode,
    pub requested_disposition: AuditDisposition,
    pub applied: bool,
    pub effect: AppliedEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DecisionEvidence {
    pub event_ids: Vec<String>,
    pub basis: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyDecision {
    pub schema_version: &'static str,
    pub decision_id: String,
    pub request_id: String,
    pub disposition: AuditDisposition,
    pub rule_id: &'static str,
    pub reason: &'static str,
    pub required_grant: &'static str,
    pub recovery: &'static str,
    pub evidence: DecisionEvidence,
    pub control_effect: ControlEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuditSummary {
    pub semantic_events: u64,
    pub os_events_considered: u64,
    pub os_events_excluded: u64,
    pub requests: u64,
    pub would_allow: u64,
    pub would_warn: u64,
    pub would_block: u64,
    pub enforcement_actions_applied: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityAuditReport {
    pub schema_version: &'static str,
    pub mode: EnforcementMode,
    pub context: AuditContext,
    pub requests: Vec<CapabilityRequest>,
    pub decisions: Vec<PolicyDecision>,
    pub summary: AuditSummary,
}

#[derive(Deserialize)]
struct ToolArguments {
    file_path: Option<String>,
    path: Option<String>,
    url: Option<String>,
    command: Option<String>,
}

pub fn audit_evidence(
    semantic_events: &[SemanticEvent],
    os_events: &[ObservationEvent],
    context: &AuditContext,
) -> Result<CapabilityAuditReport, String> {
    let workspace_root = resolve_workspace_root(semantic_events, context)?;
    let scoped_os_events = os_events
        .iter()
        .filter(|event| os_event_in_semantic_scope(event, semantic_events))
        .collect::<Vec<_>>();
    let mut requests = Vec::new();
    for event in semantic_events {
        requests.extend(normalize_semantic_event_in_context(
            event,
            workspace_root.as_deref(),
        )?);
    }
    for event in &scoped_os_events {
        requests.extend(normalize_os_event(event, workspace_root.as_deref()));
    }
    let decisions = requests.iter().map(evaluate_request).collect::<Vec<_>>();
    let requests_count = u64::try_from(requests.len())
        .map_err(|error| format!("Capability 请求数量超出范围 error={error}"))?;
    let summary = AuditSummary {
        semantic_events: count(semantic_events.len(), "语义事件")?,
        os_events_considered: count(scoped_os_events.len(), "范围内 OS 事件")?,
        os_events_excluded: count(os_events.len() - scoped_os_events.len(), "范围外 OS 事件")?,
        requests: requests_count,
        would_allow: decision_count(&decisions, AuditDisposition::WouldAllow)?,
        would_warn: decision_count(&decisions, AuditDisposition::WouldWarn)?,
        would_block: decision_count(&decisions, AuditDisposition::WouldBlock)?,
        enforcement_actions_applied: 0,
    };
    Ok(CapabilityAuditReport {
        schema_version: CAPABILITY_AUDIT_SCHEMA_VERSION,
        mode: EnforcementMode::Audit,
        context: AuditContext {
            workspace_root: workspace_root.clone(),
        },
        requests,
        decisions,
        summary,
    })
}

fn os_event_in_semantic_scope(event: &ObservationEvent, semantic_events: &[SemanticEvent]) -> bool {
    if semantic_events.is_empty() {
        return true;
    }
    if !semantic_events
        .iter()
        .any(|semantic_event| semantic_event.session_id == event.session.id)
    {
        return false;
    }
    let Some(first_timestamp) = semantic_events
        .iter()
        .map(|semantic_event| semantic_event.event_timestamp_unix_ms)
        .min()
    else {
        return true;
    };
    let Some(last_timestamp) = semantic_events
        .iter()
        .map(|semantic_event| semantic_event.event_timestamp_unix_ms)
        .max()
    else {
        return true;
    };
    let timestamp_in_scope =
        |timestamp: u64| timestamp >= first_timestamp && timestamp <= last_timestamp;
    timestamp_in_scope(event.event_timestamp_unix_ms)
        || timestamp_in_scope(event.observed_at_unix_ms)
        || event.aggregation.as_ref().is_some_and(|aggregation| {
            aggregation.first_event_timestamp_unix_ms <= last_timestamp
                && aggregation.last_event_timestamp_unix_ms >= first_timestamp
        })
}

pub fn normalize_semantic_event(event: &SemanticEvent) -> Result<Vec<CapabilityRequest>, String> {
    normalize_semantic_event_in_context(event, event.workspace_path.as_deref())
}

fn normalize_semantic_event_in_context(
    event: &SemanticEvent,
    workspace_root: Option<&str>,
) -> Result<Vec<CapabilityRequest>, String> {
    if event.content_kind != Some(SemanticContentKind::ToolCall) {
        return Ok(Vec::new());
    }
    let tool_name = event.tool_name.as_deref().unwrap_or("未提供工具名称");
    let subject = semantic_subject(event);
    let mut requests = vec![request(
        event,
        &subject,
        resource(CapabilityKind::Tool, tool_name, WorkspaceRelation::Unknown),
        CapabilityOperation::Invoke,
        EvidenceStrength::Confirmed,
        Vec::new(),
        0,
    )];
    let arguments = match serde_json::from_str::<ToolArguments>(&event.content) {
        Ok(value) => value,
        Err(error) => {
            requests[0].evidence_strength = EvidenceStrength::Partial;
            requests[0]
                .limitations
                .push(format!("工具参数无法解析为对象 error={error}"));
            return Ok(requests);
        }
    };
    let workspace = event.workspace_path.as_deref().or(workspace_root);
    let file_path = arguments.file_path.as_deref().or(arguments.path.as_deref());
    if let Some(path) = file_path {
        let operation = semantic_file_operation(event);
        requests.push(request(
            event,
            &subject,
            resource(
                CapabilityKind::Filesystem,
                path,
                workspace_relation(path, workspace),
            ),
            operation,
            EvidenceStrength::Confirmed,
            Vec::new(),
            1,
        ));
        if is_sensitive_path(path) {
            requests.push(request(
                event,
                &subject,
                resource(
                    CapabilityKind::Credential,
                    path,
                    workspace_relation(path, workspace),
                ),
                CapabilityOperation::Access,
                EvidenceStrength::Partial,
                vec![String::from("路径模式表明可能包含凭据；未检查文件内容")],
                2,
            ));
        }
    }
    if let Some(url) = arguments.url.as_deref() {
        requests.push(request(
            event,
            &subject,
            resource(CapabilityKind::Network, url, WorkspaceRelation::Unknown),
            CapabilityOperation::Connect,
            EvidenceStrength::Partial,
            vec![String::from("语义参数提供目标，但 OS 层未提供请求载荷")],
            3,
        ));
    }
    if let Some(command) = arguments.command.as_deref() {
        append_command_requests(event, &subject, command, &mut requests);
    }
    if is_extension_tool(tool_name) {
        requests.push(request(
            event,
            &subject,
            resource(CapabilityKind::Tool, tool_name, WorkspaceRelation::Unknown),
            CapabilityOperation::Execute,
            EvidenceStrength::Partial,
            vec![String::from("现有证据不能区分安装、启用、启动和调用")],
            7,
        ));
    }
    Ok(requests)
}

pub fn normalize_os_event(
    event: &ObservationEvent,
    workspace_root: Option<&str>,
) -> Vec<CapabilityRequest> {
    let subject = CapabilitySubject {
        agent_id: format!("{:?}", event.actor.agent_kind),
        session_id: event.session.id.clone(),
        user_activity_id: None,
        process_id: event.process.pid,
        tool_name: None,
        tool_call_id: event.operation_id.clone(),
    };
    let base = |capability: CapabilityKind,
                operation: CapabilityOperation,
                identifier: &str,
                relation: WorkspaceRelation,
                index: u8| CapabilityRequest {
        schema_version: CAPABILITY_AUDIT_SCHEMA_VERSION,
        request_id: format!("capability:{}:{capability:?}:{index}", event.event_id),
        event_timestamp_unix_ms: event.event_timestamp_unix_ms,
        subject: subject.clone(),
        capability,
        operation,
        resource: CapabilityResource {
            kind: capability,
            identifier: String::from(identifier),
            workspace_relation: relation,
        },
        evidence_strength: EvidenceStrength::Confirmed,
        evidence_event_ids: vec![event.event_id.clone()],
        limitations: Vec::new(),
    };
    match event.action.kind {
        ActionKind::CredentialObserved => vec![base(
            CapabilityKind::Credential,
            CapabilityOperation::Access,
            &event.resource.identifier,
            workspace_relation(&event.resource.identifier, workspace_root),
            0,
        )],
        ActionKind::NetworkConnect => vec![base(
            CapabilityKind::Network,
            CapabilityOperation::Connect,
            event
                .destination
                .as_ref()
                .map_or(event.resource.identifier.as_str(), |value| {
                    value.identifier.as_str()
                }),
            WorkspaceRelation::Unknown,
            0,
        )],
        ActionKind::FileWrite | ActionKind::FileDelete | ActionKind::FileRename => vec![base(
            CapabilityKind::Filesystem,
            os_file_operation(event.action.kind),
            &event.resource.identifier,
            workspace_relation(&event.resource.identifier, workspace_root),
            0,
        )],
        ActionKind::FileRead if is_sensitive_path(&event.resource.identifier) => vec![base(
            CapabilityKind::Credential,
            CapabilityOperation::Access,
            &event.resource.identifier,
            workspace_relation(&event.resource.identifier, workspace_root),
            0,
        )],
        ActionKind::ProcessStart if is_package_manager(&event.process.image_name) => vec![base(
            CapabilityKind::PackageManager,
            CapabilityOperation::Execute,
            &event.process.image_name,
            WorkspaceRelation::Unknown,
            0,
        )],
        ActionKind::ProcessStart if is_shell(&event.process.image_name) => vec![base(
            CapabilityKind::Tool,
            CapabilityOperation::Execute,
            &event.process.image_name,
            WorkspaceRelation::Unknown,
            0,
        )],
        _ => Vec::new(),
    }
}

pub fn evaluate_request(request: &CapabilityRequest) -> PolicyDecision {
    let (disposition, rule_id, reason, required_grant) = if request
        .limitations
        .iter()
        .any(|value| value.starts_with("工具参数无法解析"))
    {
        (
            AuditDisposition::WouldWarn,
            "audit.tool.arguments_unparsed",
            "工具调用参数无法完整解析，不能证明真实资源范围。",
            "需要可解析的强类型工具参数后再决定放行范围。",
        )
    } else {
        match (
            request.capability,
            request.operation,
            request.resource.workspace_relation,
        ) {
            (CapabilityKind::Credential, _, _) => (
                AuditDisposition::WouldBlock,
                "audit.credential.explicit_access",
                "请求访问已确认或疑似凭据资源。",
                "需要针对具体凭据、用途和目标的单次授权。",
            ),
            (CapabilityKind::Sandbox, CapabilityOperation::DisableIsolation, _) => (
                AuditDisposition::WouldBlock,
                "audit.sandbox.disable_isolation",
                "请求使用关闭或绕过隔离的参数。",
                "需要明确说明隔离不可用原因并批准一次性例外。",
            ),
            (CapabilityKind::RuntimeEnforcement, CapabilityOperation::ChangePolicy, _) => (
                AuditDisposition::WouldBlock,
                "audit.runtime.change_policy",
                "请求修改系统安全、服务或防火墙策略。",
                "需要管理员批准具体策略变更和可验证回滚步骤。",
            ),
            (
                CapabilityKind::Filesystem,
                CapabilityOperation::Write
                | CapabilityOperation::Delete
                | CapabilityOperation::Rename,
                WorkspaceRelation::OutsideWorkspace,
            ) => (
                AuditDisposition::WouldBlock,
                "audit.filesystem.outside_workspace_mutation",
                "请求修改工作区之外的文件资源。",
                "需要对具体路径和动作进行单次授权。",
            ),
            (CapabilityKind::Filesystem, _, WorkspaceRelation::OutsideWorkspace) => (
                AuditDisposition::WouldWarn,
                "audit.filesystem.outside_workspace_read",
                "请求读取工作区之外的文件资源。",
                "需要确认路径与当前任务直接相关。",
            ),
            (CapabilityKind::Network, CapabilityOperation::Connect, _)
                if !is_local_destination(&request.resource.identifier) =>
            {
                (
                    AuditDisposition::WouldWarn,
                    "audit.network.external_destination",
                    "请求连接非本机网络目标，现有证据不能证明请求载荷。",
                    "需要批准目标、用途和允许发送的数据范围。",
                )
            }
            (CapabilityKind::PackageManager, _, _) => (
                AuditDisposition::WouldWarn,
                "audit.package_manager.execution",
                "请求执行包管理器或安装命令，可能下载并运行第三方代码。",
                "需要批准包名、版本、来源和安装脚本范围。",
            ),
            (CapabilityKind::Tool, _, _) if is_shell(&request.resource.identifier) => (
                AuditDisposition::WouldWarn,
                "audit.tool.shell_execution",
                "请求调用脚本解释器或命令外壳。",
                "需要批准命令、参数和预期副作用范围。",
            ),
            (CapabilityKind::Tool, CapabilityOperation::Execute, _) => (
                AuditDisposition::WouldWarn,
                "audit.tool.extension_lifecycle",
                "请求启动 Skill、MCP 或插件能力，生命周期证据不完整。",
                "需要区分安装、启用、启动和单次调用授权。",
            ),
            _ => (
                AuditDisposition::WouldAllow,
                "audit.baseline.observed",
                "当前证据未命中首批高风险审计规则。",
                "不需要额外授权；后续证据变化时重新评估。",
            ),
        }
    };
    PolicyDecision {
        schema_version: CAPABILITY_AUDIT_SCHEMA_VERSION,
        decision_id: format!("decision:{}", request.request_id),
        request_id: request.request_id.clone(),
        disposition,
        rule_id,
        reason,
        required_grant,
        recovery: "Audit 模式未改变系统状态；无需执行恢复操作。",
        evidence: DecisionEvidence {
            event_ids: request.evidence_event_ids.clone(),
            basis: vec![format!(
                "capability={:?} operation={:?} resource={}",
                request.capability, request.operation, request.resource.identifier
            )],
            limitations: request.limitations.clone(),
        },
        control_effect: ControlEffect {
            mode: EnforcementMode::Audit,
            requested_disposition: disposition,
            applied: false,
            effect: AppliedEffect::ObservationOnly,
        },
    }
}

fn semantic_subject(event: &SemanticEvent) -> CapabilitySubject {
    CapabilitySubject {
        agent_id: event
            .agent_id
            .clone()
            .unwrap_or_else(|| String::from("workbuddy")),
        session_id: event.session_id.clone(),
        user_activity_id: (!event.user_activity_id.is_empty())
            .then(|| event.user_activity_id.clone()),
        process_id: event.process_id,
        tool_name: event.tool_name.clone(),
        tool_call_id: event
            .tool_call_id
            .clone()
            .or_else(|| event.operation_id.clone()),
    }
}

fn request(
    event: &SemanticEvent,
    subject: &CapabilitySubject,
    resource: CapabilityResource,
    operation: CapabilityOperation,
    strength: EvidenceStrength,
    limitations: Vec<String>,
    index: u8,
) -> CapabilityRequest {
    let capability = resource.kind;
    CapabilityRequest {
        schema_version: CAPABILITY_AUDIT_SCHEMA_VERSION,
        request_id: format!("capability:{}:{capability:?}:{index}", event.event_id),
        event_timestamp_unix_ms: event.event_timestamp_unix_ms,
        subject: subject.clone(),
        capability,
        operation,
        resource,
        evidence_strength: strength,
        evidence_event_ids: vec![event.event_id.clone()],
        limitations,
    }
}

fn resource(
    kind: CapabilityKind,
    identifier: &str,
    workspace_relation: WorkspaceRelation,
) -> CapabilityResource {
    CapabilityResource {
        kind,
        identifier: String::from(identifier),
        workspace_relation,
    }
}

fn append_command_requests(
    event: &SemanticEvent,
    subject: &CapabilitySubject,
    command: &str,
    requests: &mut Vec<CapabilityRequest>,
) {
    if contains_any(
        command,
        &[
            "--no-sandbox",
            "bypasspermissions",
            "dangerously-skip-permissions",
        ],
    ) {
        requests.push(request(
            event,
            subject,
            resource(CapabilityKind::Sandbox, command, WorkspaceRelation::Unknown),
            CapabilityOperation::DisableIsolation,
            EvidenceStrength::Confirmed,
            Vec::new(),
            4,
        ));
    }
    if contains_any(
        command,
        &[
            "npm install",
            "pnpm add",
            "yarn add",
            "pip install",
            "cargo install",
            "winget install",
        ],
    ) {
        requests.push(request(
            event,
            subject,
            resource(
                CapabilityKind::PackageManager,
                command,
                WorkspaceRelation::Unknown,
            ),
            CapabilityOperation::Install,
            EvidenceStrength::Confirmed,
            Vec::new(),
            5,
        ));
    }
    if contains_any(
        command,
        &[
            "set-mppreference",
            "netsh advfirewall",
            "sc.exe config",
            "reg.exe add",
            "set-netfirewallprofile",
        ],
    ) {
        requests.push(request(
            event,
            subject,
            resource(
                CapabilityKind::RuntimeEnforcement,
                command,
                WorkspaceRelation::Unknown,
            ),
            CapabilityOperation::ChangePolicy,
            EvidenceStrength::Confirmed,
            Vec::new(),
            6,
        ));
    }
}

fn semantic_file_operation(event: &SemanticEvent) -> CapabilityOperation {
    match event.expected_os_action {
        Some(ActionKind::FileWrite) => CapabilityOperation::Write,
        Some(ActionKind::FileDelete) => CapabilityOperation::Delete,
        Some(ActionKind::FileRename) => CapabilityOperation::Rename,
        _ => CapabilityOperation::Read,
    }
}

fn os_file_operation(action: ActionKind) -> CapabilityOperation {
    match action {
        ActionKind::FileWrite => CapabilityOperation::Write,
        ActionKind::FileDelete => CapabilityOperation::Delete,
        ActionKind::FileRename => CapabilityOperation::Rename,
        _ => CapabilityOperation::Read,
    }
}

fn common_workspace_root(events: &[SemanticEvent]) -> Option<String> {
    let mut values = events
        .iter()
        .filter_map(|event| event.workspace_path.as_deref())
        .map(normalize_path)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    (values.len() == 1).then(|| values.remove(0))
}

fn resolve_workspace_root(
    events: &[SemanticEvent],
    context: &AuditContext,
) -> Result<Option<String>, String> {
    let observed = common_workspace_root(events);
    let supplied = context.workspace_root.as_deref().map(normalize_path);
    if let (Some(observed_root), Some(supplied_root)) = (&observed, &supplied)
        && observed_root != supplied_root
    {
        return Err(format!(
            "Audit 工作区上下文与语义证据冲突 observed={observed_root} supplied={supplied_root}"
        ));
    }
    Ok(observed.or(supplied))
}

fn workspace_relation(path: &str, workspace: Option<&str>) -> WorkspaceRelation {
    let Some(workspace) = workspace else {
        return WorkspaceRelation::Unknown;
    };
    if !Path::new(path).is_absolute() || path.starts_with("\\Device\\") {
        return WorkspaceRelation::Unknown;
    }
    let normalized_path = normalize_path(path);
    let normalized_workspace = normalize_path(workspace);
    if normalized_path == normalized_workspace
        || normalized_path.starts_with(&format!("{normalized_workspace}\\"))
    {
        WorkspaceRelation::InWorkspace
    } else {
        WorkspaceRelation::OutsideWorkspace
    }
}

fn normalize_path(value: &str) -> String {
    value
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn is_sensitive_path(value: &str) -> bool {
    let normalized = normalize_path(value);
    normalized.split('\\').any(|part| {
        matches!(
            part,
            ".env" | "credential" | "credentials" | "id_rsa" | "id_ed25519"
        )
    }) || normalized.ends_with(".pem")
        || normalized.ends_with(".pfx")
        || normalized.ends_with(".key")
}

fn is_shell(value: &str) -> bool {
    let name = value
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(value)
        .to_lowercase();
    matches!(
        name.as_str(),
        "powershell"
            | "powershell.exe"
            | "pwsh"
            | "pwsh.exe"
            | "cmd"
            | "cmd.exe"
            | "bash"
            | "bash.exe"
    )
}

fn is_package_manager(value: &str) -> bool {
    let name = value
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(value)
        .to_lowercase();
    matches!(
        name.as_str(),
        "npm"
            | "npm.exe"
            | "npm.cmd"
            | "pnpm"
            | "pnpm.exe"
            | "pnpm.cmd"
            | "yarn"
            | "yarn.exe"
            | "yarn.cmd"
            | "pip"
            | "pip.exe"
            | "cargo"
            | "cargo.exe"
            | "winget"
            | "winget.exe"
    )
}

fn is_extension_tool(value: &str) -> bool {
    contains_any(value, &["mcp", "plugin", "skill"])
}

fn is_local_destination(value: &str) -> bool {
    let lowercase = value.to_lowercase();
    if lowercase.starts_with("http://localhost")
        || lowercase.starts_with("https://localhost")
        || lowercase.starts_with("http://127.0.0.1")
        || lowercase.starts_with("https://127.0.0.1")
        || lowercase.starts_with("http://[::1]")
        || lowercase.starts_with("https://[::1]")
    {
        return true;
    }
    let host = lowercase
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or(lowercase.as_str())
        .split(':')
        .next()
        .unwrap_or(lowercase.as_str());
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn contains_any(value: &str, patterns: &[&str]) -> bool {
    let lowercase = value.to_lowercase();
    patterns.iter().any(|pattern| lowercase.contains(pattern))
}

fn decision_count(
    decisions: &[PolicyDecision],
    disposition: AuditDisposition,
) -> Result<u64, String> {
    u64::try_from(
        decisions
            .iter()
            .filter(|decision| decision.disposition == disposition)
            .count(),
    )
    .map_err(|error| format!("Audit 决策数量超出范围 error={error}"))
}

fn count(value: usize, name: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|error| format!("{name}数量超出范围 error={error}"))
}

#[cfg(test)]
mod tests {
    use native_contracts::{
        ActionKind, EvidenceSource, ObservationStatus, SemanticContentKind, SemanticEvent,
    };

    use super::{
        AuditContext, AuditDisposition, CapabilityKind, audit_evidence, evaluate_request,
        normalize_semantic_event,
    };

    fn tool_event(tool_name: &str, content: &str, workspace: &str) -> SemanticEvent {
        SemanticEvent {
            schema_version: String::from("0.5.0"),
            event_id: String::from("semantic-1"),
            event_timestamp_unix_ms: 1_000,
            session_id: String::from("session-1"),
            process_id: 10,
            user_activity_id: String::from("activity-1"),
            agent_session_id: Some(String::from("agent-session-1")),
            turn_id: Some(String::from("activity-1")),
            tool_call_id: Some(String::from("call-1")),
            workspace_path: Some(String::from(workspace)),
            agent_id: Some(String::from("workbuddy")),
            provider_message_id: None,
            source_schema_profile: Some(String::from("test")),
            observation_layer: None,
            observation_status: ObservationStatus::Observed,
            content_kind: Some(SemanticContentKind::ToolCall),
            action_kind: ActionKind::ToolCall,
            content: String::from(content),
            tool_name: Some(String::from(tool_name)),
            operation_id: Some(String::from("call-1")),
            operation_id_origin: Some(native_contracts::OperationIdOrigin::WorkBuddyToolCallId),
            native_evidence: native_contracts::NativeEvidence::default(),
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
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cached_tokens: None,
            reasoning_tokens: None,
            request_count: None,
        }
    }

    #[test]
    fn outside_workspace_write_would_block_without_applying_control() {
        let mut event = tool_event(
            "Write",
            r#"{"file_path":"C:\\Users\\AgentReinsTestUser\\outside.txt"}"#,
            r"C:\workspace",
        );
        event.expected_os_action = Some(ActionKind::FileWrite);
        let requests = normalize_semantic_event(&event).expect("应生成写入 Capability 请求");
        let filesystem = requests
            .iter()
            .find(|request| request.capability == CapabilityKind::Filesystem)
            .expect("应包含文件系统请求");
        let decision = evaluate_request(filesystem);

        assert_eq!(decision.disposition, AuditDisposition::WouldBlock);
        assert!(!decision.control_effect.applied);
    }

    #[test]
    fn no_sandbox_flag_would_block_in_audit_only_mode() {
        let event = tool_event(
            "PowerShell",
            r#"{"command":"agent.exe --no-sandbox"}"#,
            r"C:\workspace",
        );
        let requests = normalize_semantic_event(&event).expect("应生成 Sandbox 请求");
        let sandbox = requests
            .iter()
            .find(|request| request.capability == CapabilityKind::Sandbox)
            .expect("应包含 Sandbox 请求");
        let decision = evaluate_request(sandbox);

        assert_eq!(decision.disposition, AuditDisposition::WouldBlock);
        assert!(!decision.control_effect.applied);
    }

    #[test]
    fn external_network_would_warn_and_loopback_would_allow() {
        let external = tool_event(
            "WebFetch",
            r#"{"url":"https://example.com/api"}"#,
            r"C:\workspace",
        );
        let local = tool_event(
            "WebFetch",
            r#"{"url":"http://127.0.0.1:43180/echo"}"#,
            r"C:\workspace",
        );
        let external_request = normalize_semantic_event(&external)
            .expect("应生成外部网络请求")
            .into_iter()
            .find(|request| request.capability == CapabilityKind::Network)
            .expect("应包含外部网络请求");
        let local_request = normalize_semantic_event(&local)
            .expect("应生成本机网络请求")
            .into_iter()
            .find(|request| request.capability == CapabilityKind::Network)
            .expect("应包含本机网络请求");

        assert_eq!(
            evaluate_request(&external_request).disposition,
            AuditDisposition::WouldWarn
        );
        assert_eq!(
            evaluate_request(&local_request).disposition,
            AuditDisposition::WouldAllow
        );
    }

    #[test]
    fn report_never_applies_enforcement() {
        let event = tool_event(
            "PowerShell",
            r#"{"command":"Set-MpPreference -DisableRealtimeMonitoring $true"}"#,
            r"C:\workspace",
        );
        let report = audit_evidence(
            &[event],
            &[],
            &AuditContext {
                workspace_root: None,
            },
        )
        .expect("应生成 Audit 报告");

        assert!(report.summary.would_block >= 1);
        assert_eq!(report.summary.enforcement_actions_applied, 0);
        assert!(
            report
                .decisions
                .iter()
                .all(|decision| !decision.control_effect.applied)
        );
    }
}
