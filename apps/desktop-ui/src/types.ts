export const SESSION_TABS = ["overview", "process", "events", "correlation", "evidence"] as const;
export const SYSTEM_VIEWS = ["overview", "sessions", "capabilities", "agents", "evidence", "settings"] as const;

export type SessionTab = (typeof SESSION_TABS)[number];
export type SystemView = (typeof SYSTEM_VIEWS)[number];
export type ProcessMode = "fit" | "actual";
export type EventFilter = "all" | "tool" | "native" | "breakpoint";
export type StatusTone = "success" | "warning" | "danger" | "info" | "neutral";
export type ObserverState = "healthy" | "idle" | "starting" | "degraded" | "unknown" | "stopped";
export type ObserverRuntimeState =
  | "not_installed"
  | "starting"
  | "running"
  | "idle"
  | "degraded"
  | "stopped"
  | "identity_mismatch"
  | "permission_required"
  | "failed";
export type GateStatus = "passed_with_boundaries" | "evidence_degraded";
export type AssociationState = "confirmed" | "partial" | "unlinked";
export type CoverageState = "observed" | "partial" | "not_observable" | "not_triggered" | "degraded";
export type EvidenceKind = "semantic" | "process" | "file" | "network" | "health";
export type EvidenceContentMode = "raw_local" | "metadata_only";
export type AgentKind = "work_buddy" | "codex" | "claude_code" | "claude_desktop";
export type AgentCapabilityKind =
  | "shell"
  | "file"
  | "browser"
  | "mcp"
  | "skill"
  | "credential"
  | "memory"
  | "sub_agent"
  | "scheduler"
  | "hook"
  | "sandbox"
  | "permission"
  | "runtime_enforcement"
  | "token"
  | "cache"
  | "local_resource_usage";
export type CapabilityGroup = "execution_access" | "extension_tool" | "autonomous_run" | "data_context" | "runtime_environment";

export interface LabelValue {
  readonly label: string;
  readonly value: string;
}

export interface StatusPresentation {
  readonly label: string;
  readonly icon: string;
  readonly tone: StatusTone;
}

export interface ObserverHealthViewModel {
  readonly state: ObserverState;
  readonly reason: string;
  readonly lastHealthyAtUnixMs: number;
  readonly eventsLost: number;
  readonly parseFailures: number;
  readonly writeFailures: number;
  readonly queueDrops: number;
  readonly identityCollisions: number;
  readonly clockSkewMs: number | null;
  readonly semanticEvents: number;
  readonly osEvents: number;
}

export interface ObserverRuntimeViewModel {
  readonly schemaVersion: "0.1.0";
  readonly updatedAtUnixMs: number;
  readonly state: ObserverRuntimeState;
  readonly osObserverState: ObserverRuntimeState;
  readonly semanticObserverState: ObserverRuntimeState;
  readonly sessionId: string | null;
  readonly detail: string;
  readonly applied: false;
}

export interface AgentSummaryViewModel {
  readonly id: string;
  readonly kind: AgentKind;
  readonly name: string;
  readonly productVersion: string;
  readonly publisher: string;
  readonly executablePath: string;
  readonly sha256: string;
  readonly binding: AssociationState;
  readonly rootInstances: number;
  readonly activeProcesses: number;
  readonly workspace: string | null;
  readonly semanticSessionId: string;
  readonly osSessionId: string;
}

export interface TokenPrivilegeViewModel {
  readonly name: string;
  readonly enabled: boolean;
  readonly enabledByDefault: boolean;
  readonly removed: boolean;
  readonly usedForAccess: boolean;
}

export interface ProcessPermissionViewModel {
  readonly processId: number;
  readonly processInstanceId: string;
  readonly isElevated: boolean;
  readonly integrityLevel: "untrusted" | "low" | "medium" | "medium_plus" | "high" | "system" | "protected_process";
  readonly integrityRid: number;
  readonly privileges: readonly TokenPrivilegeViewModel[];
}

export interface PermissionSnapshotViewModel {
  readonly observed: boolean;
  readonly processes: readonly ProcessPermissionViewModel[];
}

export interface CapabilityAuditRecordViewModel {
  readonly requestId: string;
  readonly decisionId: string;
  readonly eventTimestampUnixMs: number;
  readonly sessionId: string;
  readonly userActivityId: string | null;
  readonly capability: string;
  readonly operation: string;
  readonly resourceKind: string;
  readonly resourceIdentifier: string;
  readonly disposition: string;
  readonly ruleId: string;
  readonly reason: string;
  readonly requiredGrant: string;
  readonly recovery: string;
  readonly applied: false;
  readonly evidenceEventIds: readonly string[];
  readonly limitations: readonly string[];
}

export interface CapabilityAuditViewModel {
  readonly observed: boolean;
  readonly mode: "audit";
  readonly source: string;
  readonly enforcementActionsApplied: 0;
  readonly records: readonly CapabilityAuditRecordViewModel[];
}

export interface ActivityCountsViewModel {
  readonly tools: number;
  readonly processes: number | null;
  readonly files: number | null;
  readonly network: number | null;
}

export interface AssociationCountsViewModel {
  readonly confirmed: number;
  readonly partial: number;
  readonly unlinked: number;
}

export interface TimelineItemViewModel {
  readonly id: string;
  readonly phase: string;
  readonly title: string;
  readonly detail: string;
  readonly timestampUnixMs: number;
  readonly semanticObserved: boolean;
  readonly correlationRelevant: boolean;
  readonly correlationState: AssociationState | null;
  readonly source: string;
  readonly nativeCallId: string | null;
  readonly linkedOsEvents: number;
  readonly clockBasis: "event_timestamp" | "observed_at";
  readonly correlationBasis: string | null;
  readonly linkedOsEventIds: readonly string[];
  readonly firstBreakpoint: string | null;
  readonly causalEdges: readonly CausalEdge[];
  readonly mcpJsonRpcRequestId: string | null;
  readonly mcpProcessId: number | null;
  readonly mcpResponseObserved: boolean | null;
  readonly limitation: string | null;
  readonly capabilities: readonly AgentCapabilityKind[];
}

export interface NativeIdentifier {
  readonly source: string;
  readonly kind: string;
  readonly scope: string;
  readonly value: string;
}

export interface CausalEdge {
  readonly kind: string;
  readonly from: NativeIdentifier;
  readonly to: NativeIdentifier;
  readonly evidence_fields: readonly string[];
}

export interface ActivityViewModel {
  readonly id: string;
  readonly title: string;
  readonly request: string;
  readonly startedAtUnixMs: number;
  readonly endedAtUnixMs: number;
  readonly finalResult: string | null;
  readonly coverage: CoverageState;
  readonly counts: ActivityCountsViewModel;
  readonly associations: AssociationCountsViewModel;
  readonly unattributedEvents: number;
  readonly limitation: string;
  readonly capabilities: readonly AgentCapabilityKind[];
  readonly timeline: readonly TimelineItemViewModel[];
}

export interface CapabilityFacetViewModel {
  readonly state: CoverageState;
  readonly source: string;
  readonly evidenceCount: number;
  readonly limitation: string | null;
}

export interface CapabilityFacetsViewModel {
  readonly invocation: CapabilityFacetViewModel;
  readonly parametersTarget: CapabilityFacetViewModel;
  readonly agentFeedback: CapabilityFacetViewModel;
  readonly nativeIdentity: CapabilityFacetViewModel;
  readonly runtimeCorroboration: CapabilityFacetViewModel;
  readonly dataHealth: CapabilityFacetViewModel;
}

export interface CapabilityViewModel {
  readonly kind: AgentCapabilityKind;
  readonly name: string;
  readonly group: CapabilityGroup;
  readonly state: CoverageState;
  readonly metric: string;
  readonly summary: string;
  readonly facets: CapabilityFacetsViewModel;
  readonly relatedActivityIds: readonly string[];
}

export interface CoverageSummaryViewModel {
  readonly observed: number;
  readonly partial: number;
  readonly notObservable: number;
  readonly notTriggered: number;
  readonly degraded: number;
  readonly gateStatus: GateStatus;
  readonly gateReason: string;
}

export interface CoverageLayerViewModel {
  readonly index: number;
  readonly name: string;
  readonly state: CoverageState;
  readonly source: string;
  readonly lastHealthyAtUnixMs: number;
  readonly limitation: string;
  readonly impact: string;
  readonly evidenceCount: number;
}

export interface CoverageViewModel {
  readonly summary: CoverageSummaryViewModel;
  readonly layers: readonly CoverageLayerViewModel[];
}

export interface EvidenceViewModel {
  readonly id: string;
  readonly kind: EvidenceKind;
  readonly title: string;
  readonly content: string;
  readonly source: string;
  readonly timestampUnixMs: number;
  readonly contentMode: EvidenceContentMode;
  readonly observed: boolean;
  readonly correlationState: AssociationState | null;
}

export interface EvidenceFeedViewModel {
  readonly items: readonly EvidenceViewModel[];
  readonly semanticEventCount: number;
  readonly providerOsEventCount: number;
  readonly publishedOsEventCount: number;
  readonly displayedOsEventCount: number;
  readonly osScope: string;
}

export interface DiagnosticsViewModel {
  readonly osRunRoot: string;
  readonly semanticRunRoot: string;
  readonly osManifest: string;
  readonly semanticSource: string;
  readonly mcpSource: string;
  readonly mcpProtocolRecords: number;
  readonly mcpPublicationBoundary: string;
  readonly capabilityAuditSource: string;
  readonly osCoverageStatus: string;
  readonly semanticCoverageStatus: string;
  readonly providerOsEventCount: number;
  readonly targetOsEventCount: number;
  readonly dataPolicy: string;
}

export interface ApplicationSnapshot {
  readonly schemaVersion: "0.6.0";
  readonly generatedAtUnixMs: number;
  readonly observer: ObserverHealthViewModel;
  readonly agent: AgentSummaryViewModel;
  readonly permission: PermissionSnapshotViewModel;
  readonly capabilityAudit: CapabilityAuditViewModel;
  readonly activities: readonly ActivityViewModel[];
  readonly capabilities: readonly CapabilityViewModel[];
  readonly coverage: CoverageViewModel;
  readonly evidence: EvidenceFeedViewModel;
  readonly diagnostics: DiagnosticsViewModel;
}

export interface SessionPageViewModel {
  readonly items: readonly ActivityViewModel[];
  readonly total: number;
  readonly nextCursor: string | null;
}

export interface EvidencePageViewModel {
  readonly items: readonly EvidenceViewModel[];
  readonly total: number;
  readonly nextCursor: string | null;
  readonly scope: "current_projection_window";
}

export interface ProductSettingsView {
  readonly retentionDays: number;
  readonly rawEvidenceAccess: "current_windows_user";
  readonly diagnosticExportMode: "explicit_user_action";
  readonly diagnosticExportRoot: string;
  readonly selectedSessionId: string;
}

export interface RetentionCandidate {
  readonly runRoot: string;
  readonly lastModifiedUnixMs: number;
  readonly bytes: number;
}

export interface RetentionPlan {
  readonly retentionDays: number;
  readonly cutoffUnixMs: number;
  readonly candidates: readonly RetentionCandidate[];
  readonly confirmationToken: string;
}

export interface RetentionApplication {
  readonly deletedDirectories: readonly string[];
  readonly deletedBytes: number;
}

export interface DiagnosticExportSummary {
  readonly archivePath: string;
  readonly files: number;
  readonly bytes: number;
  readonly sha256: string;
}
