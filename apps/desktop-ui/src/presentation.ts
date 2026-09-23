import type {
  AssociationState,
  CoverageState,
  EvidenceKind,
  SessionTab,
  StatusPresentation,
  SystemView,
} from "./types";

export const SESSION_TAB_NAMES: ReadonlyArray<readonly [SessionTab, string]> = [
  ["overview", "总览"],
  ["process", "操作过程"],
  ["events", "事件"],
  ["correlation", "原生关联"],
  ["evidence", "原始证据"],
];

export const SYSTEM_VIEW_NAMES: ReadonlyArray<readonly [SystemView, string]> = [
  ["overview", "总览"],
  ["sessions", "会话"],
  ["capabilities", "能力"],
  ["agents", "Agent"],
  ["evidence", "证据与覆盖"],
  ["settings", "设置与诊断"],
];

export const ASSOCIATION_PRESENTATIONS: ReadonlyArray<readonly [AssociationState, StatusPresentation]> = [
  ["confirmed", { label: "已确认", icon: "✓", tone: "success" }],
  ["partial", { label: "部分闭合", icon: "◐", tone: "warning" }],
  ["unlinked", { label: "未建立原生关联", icon: "—", tone: "danger" }],
];

export const COVERAGE_PRESENTATIONS: ReadonlyArray<readonly [CoverageState, StatusPresentation]> = [
  ["observed", { label: "已观测", icon: "✓", tone: "success" }],
  ["partial", { label: "部分观测", icon: "◐", tone: "warning" }],
  ["not_observable", { label: "当前无法观测", icon: "×", tone: "danger" }],
  ["not_triggered", { label: "本次未发生", icon: "○", tone: "neutral" }],
  ["degraded", { label: "证据不完整", icon: "!", tone: "danger" }],
];

export const EVIDENCE_KIND_NAMES: ReadonlyArray<readonly [EvidenceKind, string]> = [
  ["semantic", "语义"],
  ["process", "进程"],
  ["file", "文件"],
  ["network", "网络"],
  ["health", "采集健康"],
];
