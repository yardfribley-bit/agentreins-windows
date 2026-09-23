use native_contracts::{ActionKind, EvidenceRetention, RetentionClass};

const SUMMARY_WINDOW_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionDisposition {
    Keep(RetentionClass),
    Drop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionDecision {
    pub disposition: RetentionDisposition,
    pub reason: &'static str,
    pub resource_category: &'static str,
}

pub fn decide_retention(action: ActionKind, resource_identifier: &str) -> RetentionDecision {
    if matches!(action, ActionKind::ProcessStart | ActionKind::ProcessStop) {
        return keep_detail("process_lifecycle", "process");
    }
    if matches!(
        action,
        ActionKind::NetworkConnect | ActionKind::NetworkDisconnect | ActionKind::NetworkAccept
    ) {
        return keep_detail("network_connection_lifecycle", "network");
    }
    if matches!(action, ActionKind::NetworkSend | ActionKind::NetworkReceive) {
        return keep_summary("network_transfer_summary", "network");
    }
    if action == ActionKind::FileOperationResult {
        return drop_event("redundant_file_completion", "file_completion");
    }

    let resource_category = classify_file_resource(resource_identifier);
    if resource_category == "unresolved_file_pointer" {
        return drop_event("unresolved_file_pointer", resource_category);
    }
    if matches!(
        action,
        ActionKind::FileWrite | ActionKind::FileDelete | ActionKind::FileRename
    ) && matches!(
        resource_category,
        "test_fixture_or_credential" | "user_workspace" | "user_profile" | "skill_mcp_policy"
    ) {
        return keep_detail("user_or_security_relevant_change", resource_category);
    }
    if matches!(action, ActionKind::FileRead | ActionKind::FileWrite)
        && matches!(
            resource_category,
            "test_fixture_or_credential" | "user_workspace" | "user_profile" | "skill_mcp_policy"
        )
    {
        return keep_detail("user_or_security_relevant_access", resource_category);
    }
    if action == ActionKind::FileOpen && resource_category == "test_fixture_or_credential" {
        return keep_detail("credential_or_fixture_open", resource_category);
    }
    if matches!(
        action,
        ActionKind::FileWrite | ActionKind::FileDelete | ActionKind::FileRename
    ) && resource_category == "other_file"
    {
        return keep_detail("external_file_change", resource_category);
    }
    if matches!(
        action,
        ActionKind::FileOpen
            | ActionKind::FileRead
            | ActionKind::FileWrite
            | ActionKind::FileDelete
            | ActionKind::FileRename
    ) {
        return keep_summary("background_file_activity_summary", resource_category);
    }
    if action == ActionKind::CredentialObserved {
        return keep_detail("credential_observed", "credential");
    }

    RetentionDecision {
        disposition: RetentionDisposition::Keep(RetentionClass::Review),
        reason: "unsupported_action_requires_review",
        resource_category: "unknown",
    }
}

pub fn summary_window(timestamp_unix_ms: u64) -> u64 {
    timestamp_unix_ms / SUMMARY_WINDOW_MS
}

pub fn evidence_retention(decision: RetentionDecision) -> Option<EvidenceRetention> {
    let RetentionDisposition::Keep(class) = decision.disposition else {
        return None;
    };
    Some(EvidenceRetention {
        class,
        reason: String::from(decision.reason),
        resource_category: String::from(decision.resource_category),
    })
}

fn keep_detail(reason: &'static str, resource_category: &'static str) -> RetentionDecision {
    RetentionDecision {
        disposition: RetentionDisposition::Keep(RetentionClass::Detail),
        reason,
        resource_category,
    }
}

fn keep_summary(reason: &'static str, resource_category: &'static str) -> RetentionDecision {
    RetentionDecision {
        disposition: RetentionDisposition::Keep(RetentionClass::Summary),
        reason,
        resource_category,
    }
}

fn drop_event(reason: &'static str, resource_category: &'static str) -> RetentionDecision {
    RetentionDecision {
        disposition: RetentionDisposition::Drop,
        reason,
        resource_category,
    }
}

fn classify_file_resource(raw_resource: &str) -> &'static str {
    let resource = raw_resource.replace('/', "\\").to_lowercase();
    if resource.starts_with("file-object-") {
        return "unresolved_file_pointer";
    }
    let is_connector_resource = resource.contains("\\.workbuddy\\connectors\\")
        || resource.contains("\\.workbuddy\\connectors-marketplace\\connectors\\");
    if is_connector_resource
        && (resource.ends_with("\\mcp.json") || resource.ends_with("\\token-schema.json"))
    {
        return "skill_mcp_policy";
    }
    if is_credential_resource(&resource) {
        return "test_fixture_or_credential";
    }
    if resource.contains("\\.workbuddy\\skills\\")
        || resource.contains("\\.workbuddy\\connectors\\")
        || resource.contains("\\.workbuddy\\connectors-marketplace\\connectors\\")
        || resource.contains("\\.workbuddy\\security\\")
        || resource.ends_with("\\.workbuddy\\settings.json")
    {
        return "skill_mcp_policy";
    }
    if resource.contains("codebuddy-marketplace-install-") {
        return "marketplace_temp";
    }
    if resource.contains("\\microsoft\\edge\\user data\\") {
        return "edge_runtime";
    }
    if resource.contains("\\.workbuddy\\logs\\") {
        return "workbuddy_logs";
    }
    if resource.contains("\\.workbuddy\\") {
        return "workbuddy_runtime";
    }
    if resource.contains("\\appdata\\local\\programs\\workbuddy\\") {
        return "workbuddy_installation";
    }
    if resource.contains("\\appdata\\local\\temp\\") {
        return "temporary_file";
    }
    if resource.contains("\\users\\") && resource.contains("\\workbuddy\\") {
        return "user_workspace";
    }
    if resource.contains("\\users\\") && resource.contains("\\appdata\\") {
        return "application_runtime";
    }
    if resource.contains("\\users\\") {
        return "user_profile";
    }
    if resource.contains("\\windows\\") || resource.contains("\\program files\\") {
        return "system_runtime";
    }
    "other_file"
}

fn is_credential_resource(resource: &str) -> bool {
    resource.contains("\\agentreins-lab\\runtime\\agent-activity-v1\\")
        || resource.contains("\\credential\\")
        || resource.contains("\\credentials\\")
        || resource.contains("\\.ssh\\")
        || resource.contains("\\.aws\\")
        || resource.contains("token")
        || resource.contains("secret")
        || resource.ends_with(".env")
        || resource.ends_with(".pem")
        || resource.ends_with(".key")
        || resource.ends_with(".npmrc")
        || resource.ends_with(".pypirc")
        || resource.ends_with(".netrc")
        || resource.ends_with("\\.git-credentials")
        || resource.ends_with("\\.config\\gh\\hosts.yml")
        || resource.ends_with("\\.kube\\config")
        || resource.ends_with("\\.docker\\config.json")
}

#[cfg(test)]
mod tests {
    use super::{RetentionDisposition, classify_file_resource, decide_retention};
    use native_contracts::{ActionKind, RetentionClass};

    #[test]
    fn preserves_user_and_credential_access_as_detail() {
        let user_file = decide_retention(
            ActionKind::FileRead,
            r"C:\Users\AgentReinsTestUser\Documents\project\README.md",
        );
        let credential = decide_retention(
            ActionKind::FileOpen,
            r"C:\Users\AgentReinsTestUser\.config\gh\hosts.yml",
        );

        assert_eq!(
            user_file.disposition,
            RetentionDisposition::Keep(RetentionClass::Detail)
        );
        assert_eq!(
            credential.disposition,
            RetentionDisposition::Keep(RetentionClass::Detail)
        );
    }

    #[test]
    fn summarizes_runtime_and_network_transfer_noise() {
        let runtime = decide_retention(
            ActionKind::FileRead,
            r"C:\Users\AgentReinsTestUser\.workbuddy\projects\session.jsonl",
        );
        let network = decide_retention(ActionKind::NetworkReceive, "tcp");

        assert_eq!(
            runtime.disposition,
            RetentionDisposition::Keep(RetentionClass::Summary)
        );
        assert_eq!(
            network.disposition,
            RetentionDisposition::Keep(RetentionClass::Summary)
        );
    }

    #[test]
    fn summarizes_non_credential_file_open_noise() {
        let workspace = decide_retention(
            ActionKind::FileOpen,
            r"C:\Users\AgentReinsTestUser\Documents\project\README.md",
        );
        let policy = decide_retention(
            ActionKind::FileOpen,
            r"C:\Users\AgentReinsTestUser\.workbuddy\connectors\weather\mcp.json",
        );

        assert_eq!(
            workspace.disposition,
            RetentionDisposition::Keep(RetentionClass::Summary)
        );
        assert_eq!(
            policy.disposition,
            RetentionDisposition::Keep(RetentionClass::Summary)
        );
    }

    #[test]
    fn connector_token_schema_is_policy_not_credential() {
        assert_eq!(
            classify_file_resource(
                r"C:\Users\AgentReinsTestUser\.workbuddy\connectors-marketplace\connectors\weather-token\token-schema.json"
            ),
            "skill_mcp_policy"
        );
        assert_eq!(
            classify_file_resource(
                r"C:\Users\AgentReinsTestUser\.workbuddy\connectors-marketplace\connectors\weather-token\mcp.json"
            ),
            "skill_mcp_policy"
        );
        assert_eq!(
            classify_file_resource(
                r"C:\Users\AgentReinsTestUser\.workbuddy\connectors-marketplace\connectors\weather-token\client-secret.env"
            ),
            "test_fixture_or_credential"
        );
    }

    #[test]
    fn drops_only_redundant_or_unresolved_file_events() {
        let completion = decide_retention(ActionKind::FileOperationResult, "operation=FileRead");
        let unresolved = decide_retention(ActionKind::FileRead, "file-object-unknown");

        assert_eq!(completion.disposition, RetentionDisposition::Drop);
        assert_eq!(unresolved.disposition, RetentionDisposition::Drop);
    }

    #[test]
    fn routes_unhandled_actions_to_review() {
        let decision = decide_retention(ActionKind::ToolCall, "future-etw-action");

        assert_eq!(
            decision.disposition,
            RetentionDisposition::Keep(RetentionClass::Review)
        );
    }
}
