import "./styles.css";

import {
  applyRetentionPlan,
  createDiagnosticExport,
  loadEvidencePage,
  loadObserverRuntime,
  loadProductSettings,
  loadRetentionPlan,
  loadSessionsPage,
  loadSnapshot,
} from "./api";
import {
  ASSOCIATION_PRESENTATIONS,
  COVERAGE_PRESENTATIONS,
  EVIDENCE_KIND_NAMES,
  SYSTEM_VIEW_NAMES,
} from "./presentation";
import { SYSTEM_VIEWS } from "./types";
import type {
  ActivityViewModel,
  AgentCapabilityKind,
  ApplicationSnapshot,
  AssociationState,
  CapabilityFacetViewModel,
  CapabilityFacetsViewModel,
  CapabilityGroup,
  CapabilityViewModel,
  CoverageState,
  EvidenceKind,
  EvidenceViewModel,
  ObserverState,
  ObserverRuntimeState,
  ObserverRuntimeViewModel,
  ProcessPermissionViewModel,
  ProductSettingsView,
  RetentionPlan,
  StatusPresentation,
  StatusTone,
  SystemView,
  TimelineItemViewModel,
} from "./types";

type EvidenceFilter = "all" | EvidenceKind;

interface ApplicationViewState {
  selectedActivityId: string;
  selectedTimelineId: string;
  selectedCapability: AgentCapabilityKind;
  systemView: SystemView;
  evidenceFilter: EvidenceFilter;
  inspectorOpen: boolean;
  inspectorWidth: number;
  sessionNextCursor: string | null;
  sessionTotal: number;
  evidenceNextCursor: string | null;
  evidenceTotal: number;
}

interface ProductActions {
  readonly previewRetention: (status: HTMLElement, applyButton: HTMLButtonElement) => Promise<void>;
  readonly applyRetention: (status: HTMLElement, applyButton: HTMLButtonElement) => Promise<void>;
  readonly exportDiagnostics: (status: HTMLElement) => Promise<void>;
}

interface RenderActions extends ProductActions {
  readonly selectActivity: (activityId: string) => void;
  readonly selectTimelineItem: (itemId: string) => void;
  readonly selectCapability: (kind: AgentCapabilityKind) => void;
  readonly selectSystemView: (view: SystemView) => void;
  readonly setEvidenceFilter: (filter: EvidenceFilter) => void;
  readonly closeInspector: () => void;
  readonly setInspectorWidth: (width: number) => void;
  readonly loadMoreSessions: () => Promise<void>;
  readonly loadMoreEvidence: () => Promise<void>;
}

const CAPABILITY_GROUP_NAMES: ReadonlyArray<readonly [CapabilityGroup, string, string]> = [
  ["execution_access", "执行与访问", "Agent 直接使用本地或外部能力完成任务。"],
  ["extension_tool", "扩展与工具", "Agent 加载并调用扩展能力。"],
  ["autonomous_run", "自主运行", "Agent 分派任务或安排未来执行。"],
  ["data_context", "数据与上下文", "Agent 使用的敏感信息、长期状态与模型用量。"],
  ["runtime_environment", "运行环境", "Agent 自身的权限、隔离、控制判断和资源消耗。"],
];

const CAPABILITY_FACETS: ReadonlyArray<readonly [keyof CapabilityFacetsViewModel, string]> = [
  ["invocation", "调用"],
  ["parametersTarget", "参数 / 目标"],
  ["agentFeedback", "Agent 反馈"],
  ["nativeIdentity", "原生身份"],
  ["runtimeCorroboration", "运行时佐证"],
  ["dataHealth", "数据健康"],
];

const INTEGRITY_LEVEL_NAMES: Readonly<Record<ProcessPermissionViewModel["integrityLevel"], string>> = {
  untrusted: "不受信任",
  low: "低",
  medium: "中",
  medium_plus: "中高",
  high: "高",
  system: "System",
  protected_process: "受保护进程",
};

const createElement = <K extends keyof HTMLElementTagNameMap>(tag: K, className: string): HTMLElementTagNameMap[K] => {
  const element = document.createElement(tag);
  if (className !== "") element.className = className;
  return element;
};

const createTextElement = <K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text: string): HTMLElementTagNameMap[K] => {
  const element = createElement(tag, className);
  element.textContent = text;
  return element;
};

const requireElement = <T extends HTMLElement>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (element === null) throw new Error(`缺少必需界面元素：${selector}`);
  return element;
};

const createScrollableRegion = (className: string, label: string): HTMLDivElement => {
  const region = createElement("div", className);
  region.tabIndex = 0;
  region.setAttribute("role", "region");
  region.setAttribute("aria-label", label);
  return region;
};

const isSystemView = (value: string | undefined): value is SystemView => value !== undefined && SYSTEM_VIEWS.some((candidate) => candidate === value);

const initialSystemView = (): SystemView => {
  const value = new URL(window.location.href).searchParams.get("view") ?? undefined;
  if (value === undefined) return "sessions";
  if (!isSystemView(value)) throw new Error(`未知主页面：${value}`);
  return value;
};

const updateLocationView = (view: SystemView): void => {
  const url = new URL(window.location.href);
  url.searchParams.set("view", view);
  window.history.replaceState(null, "", url);
};

const getCoveragePresentation = (state: CoverageState): StatusPresentation => {
  const match = COVERAGE_PRESENTATIONS.find(([candidate]) => candidate === state);
  if (match === undefined) throw new Error(`未知覆盖状态：${state}`);
  return match[1];
};

const getAssociationPresentation = (state: AssociationState): StatusPresentation => {
  const match = ASSOCIATION_PRESENTATIONS.find(([candidate]) => candidate === state);
  if (match === undefined) throw new Error(`未知关联状态：${state}`);
  return match[1];
};

const getObserverPresentation = (state: ObserverState): StatusPresentation => {
  switch (state) {
    case "healthy": return { label: "Observer 正常", icon: "✓", tone: "success" };
    case "idle": return { label: "Observer 空闲", icon: "○", tone: "neutral" };
    case "starting": return { label: "Observer 正在启动", icon: "…", tone: "info" };
    case "degraded": return { label: "Observer 降级", icon: "!", tone: "danger" };
    case "unknown": return { label: "Observer 未知", icon: "?", tone: "warning" };
    case "stopped": return { label: "Observer 已停止", icon: "×", tone: "danger" };
  }
};

const getObserverRuntimePresentation = (state: ObserverRuntimeState): StatusPresentation => {
  switch (state) {
    case "running": return { label: "Observer 正常", icon: "✓", tone: "success" };
    case "idle": return { label: "Observer 空闲", icon: "○", tone: "neutral" };
    case "starting": return { label: "Observer 正在启动", icon: "…", tone: "info" };
    case "not_installed": return { label: "Observer 未安装", icon: "!", tone: "danger" };
    case "permission_required": return { label: "Observer 需要权限", icon: "!", tone: "warning" };
    case "identity_mismatch": return { label: "Observer 身份不匹配", icon: "!", tone: "danger" };
    case "degraded": return { label: "Observer 降级", icon: "!", tone: "danger" };
    case "stopped": return { label: "Observer 已停止", icon: "×", tone: "danger" };
    case "failed": return { label: "Observer 启动失败", icon: "×", tone: "danger" };
  }
};

const getSystemViewName = (view: SystemView): string => {
  const match = SYSTEM_VIEW_NAMES.find(([candidate]) => candidate === view);
  if (match === undefined) throw new Error(`未知系统视图：${view}`);
  return match[1];
};

const getEvidenceKindName = (kind: EvidenceKind): string => {
  const match = EVIDENCE_KIND_NAMES.find(([candidate]) => candidate === kind);
  if (match === undefined) throw new Error(`未知证据类型：${kind}`);
  return match[1];
};

const formatTimestamp = (timestampUnixMs: number): string => new Intl.DateTimeFormat("zh-CN", {
  year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
}).format(new Date(timestampUnixMs));

const formatShortTimestamp = (timestampUnixMs: number): string => new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
}).format(new Date(timestampUnixMs));

const formatDuration = (startedAtUnixMs: number, endedAtUnixMs: number): string => {
  const durationMs = Math.max(0, endedAtUnixMs - startedAtUnixMs);
  if (durationMs < 1_000) return `${String(durationMs)} ms`;
  if (durationMs < 60_000) return `${(durationMs / 1_000).toFixed(1)} s`;
  const minutes = Math.floor(durationMs / 60_000);
  const seconds = Math.floor((durationMs % 60_000) / 1_000);
  return `${String(minutes)}m ${String(seconds)}s`;
};

const formatBytes = (bytes: number): string => {
  if (bytes < 1024) return `${String(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
};

const createBadge = (presentation: StatusPresentation): HTMLSpanElement => {
  const badge = createElement("span", `badge ${presentation.tone}`);
  badge.textContent = `${presentation.icon} ${presentation.label}`;
  return badge;
};

const createSimpleBadge = (label: string, tone: StatusTone): HTMLSpanElement => {
  const badge = createElement("span", `badge ${tone}`);
  badge.textContent = label;
  return badge;
};

const createPageHeader = (eyebrow: string, title: string, description: string, badge: HTMLElement | null): HTMLElement => {
  const header = createElement("div", "page-head");
  const copy = createElement("div", "");
  copy.append(createTextElement("div", "eyebrow", eyebrow), createTextElement("h1", "", title), createTextElement("p", "", description));
  header.append(copy);
  if (badge !== null) {
    const actions = createElement("div", "head-actions");
    actions.append(badge);
    header.append(actions);
  }
  return header;
};

const createSectionTitle = (title: string, description: string): HTMLElement => {
  const heading = createElement("div", "section-title");
  const copy = createElement("div", "");
  copy.append(createTextElement("h2", "", title), createTextElement("p", "", description));
  heading.append(copy);
  return heading;
};

const createDefinitionGrid = (items: ReadonlyArray<readonly [string, string]>): HTMLElement => {
  const grid = createElement("dl", "defs");
  items.forEach(([label, value]) => {
    const row = createElement("div", "");
    row.append(createTextElement("dt", "", label), createTextElement("dd", "raw-content", value));
    grid.append(row);
  });
  return grid;
};

const createMetric = (value: string, label: string): HTMLElement => {
  const metric = createElement("article", "card metric");
  metric.append(createTextElement("b", "", value), createTextElement("span", "", label));
  return metric;
};

const createBoundary = (text: string): HTMLElement => createTextElement("div", "boundary", text);

const createFact = (icon: string, title: string, detail: string): HTMLElement => {
  const fact = createElement("div", "fact");
  const copy = createElement("div", "");
  copy.append(createTextElement("strong", "", title), createTextElement("span", "", detail));
  fact.append(createTextElement("div", "fact-icon", icon), copy);
  return fact;
};

const createSourceRow = (index: string, title: string, detail: string, status: HTMLElement): HTMLElement => {
  const row = createElement("div", "source-row");
  const copy = createElement("div", "");
  copy.append(createTextElement("b", "", title), createTextElement("span", "", detail));
  row.append(createTextElement("div", "source-num", index), copy, status);
  return row;
};

const findActivity = (snapshot: ApplicationSnapshot, activityId: string): ActivityViewModel | null => snapshot.activities.find((activity) => activity.id === activityId) ?? null;

const findTimelineItem = (activity: ActivityViewModel, itemId: string): TimelineItemViewModel | null => activity.timeline.find((item) => item.id === itemId) ?? null;

const capabilityName = (snapshot: ApplicationSnapshot, kind: AgentCapabilityKind): string => snapshot.capabilities.find((item) => item.kind === kind)?.name ?? kind;

const createCapabilityChips = (snapshot: ApplicationSnapshot, capabilities: readonly AgentCapabilityKind[]): HTMLElement => {
  const chips = createElement("div", "cap-mini");
  capabilities.forEach((kind) => chips.append(createTextElement("span", "cap-chip", capabilityName(snapshot, kind))));
  return chips;
};

const renderOverview = (snapshot: ApplicationSnapshot, actions: RenderActions): HTMLElement => {
  const page = createElement("section", "view active");
  const observed = snapshot.capabilities.filter((item) => item.state === "observed").length;
  const partial = snapshot.capabilities.filter((item) => item.state === "partial").length;
  const unavailable = snapshot.capabilities.filter((item) => item.state === "not_observable").length;
  const semanticEvents = snapshot.activities.reduce((total, activity) => total + activity.timeline.length, 0);
  const toolCalls = snapshot.activities.reduce((total, activity) => total + activity.counts.tools, 0);
  page.append(createPageHeader("Agent overview", "总览", "先回答最近的 Agent 会话发生了什么，再进入能力与原始证据。", createSimpleBadge(`${snapshot.agent.name} ${snapshot.agent.productVersion}`, "neutral")));

  const overview = createElement("div", "overview-grid");
  const hero = createElement("article", "card hero-status");
  const heroMetrics = createElement("div", "hero-metrics");
  [[String(snapshot.activities.length), "已分析真实会话"], [String(semanticEvents), "语义事件"], [String(toolCalls), "Tool 调用"], ["0", "已应用控制"]].forEach(([value, label]) => {
    const metric = createElement("div", "hero-metric");
    metric.append(createTextElement("b", "", value ?? "0"), createTextElement("span", "", label ?? ""));
    heroMetrics.append(metric);
  });
  const observer = getObserverPresentation(snapshot.observer.state);
  const observerLabel = createTextElement("h2", "", observer.label);
  observerLabel.id = "observer-overview-label";
  const observerReason = createTextElement("p", "", snapshot.observer.reason);
  observerReason.id = "observer-overview-reason";
  hero.append(createTextElement("div", "eyebrow hero-eyebrow", "当前观测状态"), observerLabel, observerReason, heroMetrics);
  const factsCard = createElement("article", "card card-pad");
  const facts = createElement("div", "facts");
  facts.append(
    createFact(String(snapshot.coverage.summary.partial), "部分观测能力仍保留边界", snapshot.coverage.summary.gateReason),
    createFact("ID", "只使用原生标识建立因果关系", "缺少共同原生 ID 时保留断点，不用时间补齐。"),
    createFact("0", "Runtime Enforcement 未启用", `Capability Audit 已记录 ${String(snapshot.capabilityAudit.records.length)} 项判断，applied=false。`),
  );
  factsCard.append(createSectionTitle("需要注意的事实", "只陈述已记录的 Agent 行为。"), facts);
  overview.append(hero, factsCard);
  page.append(overview);

  const metrics = createElement("div", "metric-grid");
  metrics.append(createMetric(`${String(observed)} / 16`, "完整观测"), createMetric(`${String(partial)} / 16`, "部分观测"), createMetric(`${String(unavailable)} / 16`, "当前无法观测"), createMetric(String(snapshot.coverage.summary.notTriggered), "本次未触发"));
  page.append(metrics);

  const recent = createElement("div", "recent-layout");
  const sessionsCard = createElement("article", "card");
  sessionsCard.append(createSectionTitle("最近真实会话", "点击任一行进入完整操作过程。"));
  const tableWrap = createScrollableRegion("table-wrap", "最近真实会话");
  const table = createElement("table", "session-table");
  const head = createElement("thead", "");
  const headRow = createElement("tr", "");
  ["用户任务", "能力", "观测", "时间"].forEach((label) => headRow.append(createTextElement("th", "", label)));
  head.append(headRow);
  const body = createElement("tbody", "");
  snapshot.activities.slice(0, 6).forEach((activity) => {
    const row = createElement("tr", "");
    const request = createElement("td", "request-cell");
    const open = createTextElement("button", "table-link", activity.title);
    open.type = "button";
    open.addEventListener("click", () => actions.selectActivity(activity.id));
    request.append(open, createTextElement("span", "", activity.request));
    const capabilities = createElement("td", "");
    capabilities.append(createCapabilityChips(snapshot, activity.capabilities));
    const state = createElement("td", "");
    state.append(createBadge(getCoveragePresentation(activity.coverage)));
    row.append(request, capabilities, state, createTextElement("td", "", formatShortTimestamp(activity.startedAtUnixMs)));
    body.append(row);
  });
  table.append(head, body);
  tableWrap.append(table);
  sessionsCard.append(tableWrap);
  const boundaryCard = createElement("article", "card card-pad");
  const sources = createElement("div", "source-stack");
  sources.append(
    createSourceRow("1", "Agent 原生记录", "请求、ToolCall、结果与 Token。", createSimpleBadge("使用中", "success")),
    createSourceRow("2", "Agent 拥有的协议", "MCP、Skill、Hook 与 Scheduler。", createSimpleBadge("部分", "warning")),
    createSourceRow("3", "Agent 进程范围", "只接受能证明属于目标 Agent 的佐证。", createSimpleBadge("使用中", "info")),
  );
  boundaryCard.append(createSectionTitle("数据边界", "这不是端点监控产品。"), sources);
  recent.append(sessionsCard, boundaryCard);
  page.append(recent);
  return page;
};

const timelineTone = (item: TimelineItemViewModel): string => {
  if (item.correlationState === "confirmed") return "observed";
  if (item.correlationState === "partial") return "partial";
  if (item.correlationState === "unlinked" || item.limitation !== null) return "danger";
  return "observed";
};

const groupTimeline = (timeline: readonly TimelineItemViewModel[]): ReadonlyArray<readonly [string, readonly TimelineItemViewModel[]]> => {
  const groups = new Map<string, TimelineItemViewModel[]>();
  timeline.forEach((item) => {
    const items = groups.get(item.phase);
    if (items === undefined) groups.set(item.phase, [item]);
    else items.push(item);
  });
  return Array.from(groups.entries());
};

const renderSessionList = (snapshot: ApplicationSnapshot, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const sidebar = createElement("aside", "session-list");
  const head = createElement("div", "session-list-head");
  const search = createElement("input", "search");
  search.type = "search";
  search.placeholder = "搜索请求、结果或 ID";
  search.setAttribute("aria-label", "搜索会话");
  head.append(createTextElement("h1", "", "会话"), createTextElement("p", "", "一次用户活动是一条完整主线"), search);
  const list = createElement("div", "sessions");
  snapshot.activities.forEach((activity) => {
    const button = createElement("button", activity.id === state.selectedActivityId ? "session-item active" : "session-item");
    button.type = "button";
    button.dataset.searchText = `${activity.id} ${activity.title} ${activity.request} ${activity.finalResult ?? ""}`.toLocaleLowerCase("zh-CN");
    const top = createElement("div", "session-item-top");
    top.append(createTextElement("strong", "", activity.title), createTextElement("time", "", formatShortTimestamp(activity.startedAtUnixMs)));
    const bottom = createElement("div", "session-item-bottom");
    bottom.append(createBadge(getCoveragePresentation(activity.coverage)), createTextElement("span", "", `${String(activity.timeline.length)} 条事件`));
    button.append(top, createTextElement("p", "", activity.request), bottom);
    button.addEventListener("click", () => actions.selectActivity(activity.id));
    list.append(button);
  });
  if (snapshot.activities.length === 0) list.append(createTextElement("p", "empty-state", "当前没有可归组的真实会话。"));
  if (state.sessionNextCursor !== null) {
    const more = createTextElement("button", "button load-more", `加载更多（${String(snapshot.activities.length)} / ${String(state.sessionTotal)}）`);
    more.type = "button";
    more.addEventListener("click", () => { void actions.loadMoreSessions(); });
    list.append(more);
  }
  search.addEventListener("input", () => {
    const query = search.value.trim().toLocaleLowerCase("zh-CN");
    list.querySelectorAll<HTMLButtonElement>(".session-item").forEach((button) => {
      button.hidden = query !== "" && !(button.dataset.searchText ?? "").includes(query);
    });
  });
  sidebar.append(head, list);
  return sidebar;
};

const createStoryButton = (label: string, content: string, result: boolean, onSelect: () => void): HTMLButtonElement => {
  const button = createElement("button", result ? "card story result" : "card story");
  button.type = "button";
  button.append(createTextElement("span", "story-label", label), createTextElement("blockquote", "raw-content", content));
  button.addEventListener("click", onSelect);
  return button;
};

const renderFlow = (activity: ActivityViewModel, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const card = createElement("article", "card flow-card");
  const toolbar = createElement("div", "flow-toolbar");
  toolbar.append(createSectionTitle("操作过程", "按 Agent 原生顺序展示；运行时佐证不会因时间接近自动连线。"));
  const legend = createElement("div", "legend");
  [["success", "已记录"], ["warning", "部分"], ["danger", "断点 / 限制"]].forEach(([tone, label]) => {
    const item = createElement("span", "");
    const dot = createElement("i", tone ?? "");
    item.append(dot, document.createTextNode(label ?? ""));
    legend.append(item);
  });
  toolbar.append(legend);
  const region = createScrollableRegion("flow-scroll", "会话操作过程，可横向滚动");
  const flow = createElement("div", "flow");
  const groups = groupTimeline(activity.timeline);
  groups.forEach(([phase, items], index) => {
    const last = index === groups.length - 1 ? " last" : "";
    flow.append(createTextElement("div", `lane-label${last}`, phase));
    const track = createElement("div", `lane-track${last}`);
    items.forEach((item) => {
      const selected = item.id === state.selectedTimelineId;
      const node = createElement("button", `flow-node ${timelineTone(item)}${selected ? " selected" : ""}`);
      node.type = "button";
      node.dataset.timelineItemId = item.id;
      node.setAttribute("aria-pressed", String(selected));
      node.append(createElement("span", "dot"), createTextElement("b", "", item.title), createTextElement("span", "raw-content", item.nativeCallId ?? item.id));
      node.addEventListener("click", () => actions.selectTimelineItem(item.id));
      track.append(node);
    });
    flow.append(track);
  });
  if (groups.length === 0) flow.append(createTextElement("p", "empty-state", "当前会话没有可展示的真实事件。"));
  region.append(flow);
  card.append(toolbar, region);
  return card;
};

const renderTimelineDetail = (activity: ActivityViewModel, item: TimelineItemViewModel, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const panel = createElement("aside", "detail-pane open");
  const resizer = createElement("button", "resizer");
  resizer.type = "button";
  resizer.setAttribute("role", "separator");
  resizer.setAttribute("aria-label", "调整详情宽度");
  resizer.setAttribute("aria-orientation", "vertical");
  resizer.addEventListener("pointerdown", (event) => {
    event.preventDefault();
    resizer.setPointerCapture(event.pointerId);
    const workbench = panel.closest<HTMLElement>(".session-workbench");
    if (workbench === null) throw new Error("无法定位会话工作台");
    const update = (moveEvent: PointerEvent): void => {
      if (moveEvent.pointerId !== event.pointerId) return;
      const bounds = workbench.getBoundingClientRect();
      const width = Math.max(310, Math.min(560, bounds.right - moveEvent.clientX));
      workbench.style.setProperty("--detail", `${String(width)}px`);
      actions.setInspectorWidth(width);
    };
    const stop = (stopEvent: PointerEvent): void => {
      if (stopEvent.pointerId !== event.pointerId) return;
      resizer.removeEventListener("pointermove", update);
      resizer.removeEventListener("pointerup", stop);
      resizer.removeEventListener("pointercancel", stop);
    };
    resizer.addEventListener("pointermove", update);
    resizer.addEventListener("pointerup", stop);
    resizer.addEventListener("pointercancel", stop);
  });
  resizer.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const width = Math.max(310, Math.min(560, state.inspectorWidth + (event.key === "ArrowLeft" ? 24 : -24)));
    const workbench = panel.closest<HTMLElement>(".session-workbench");
    workbench?.style.setProperty("--detail", `${String(width)}px`);
    actions.setInspectorWidth(width);
  });
  const head = createElement("div", "detail-head");
  const headRow = createElement("div", "detail-head-row");
  const headCopy = createElement("div", "");
  headCopy.append(createTextElement("div", "eyebrow", item.phase), createTextElement("h2", "", item.title), createTextElement("p", "raw-content", item.source));
  const close = createTextElement("button", "detail-close", "×");
  close.type = "button";
  close.setAttribute("aria-label", "关闭详情");
  close.addEventListener("click", actions.closeInspector);
  headRow.append(headCopy, close);
  head.append(headRow, item.correlationState === null ? createSimpleBadge("语义已记录", "info") : createBadge(getAssociationPresentation(item.correlationState)));
  const body = createElement("div", "detail-body");
  const content = createElement("section", "detail-section");
  content.append(createTextElement("h3", "", "Agent 调用或内容"), createTextElement("div", "detail-copy raw-content", item.detail));
  const feedback = createElement("section", "detail-section");
  const feedbackText = item.firstBreakpoint ?? item.limitation ?? (item.mcpResponseObserved === true ? "已观察到原生 MCP 响应。" : "当前记录没有额外反馈字段。");
  feedback.append(createTextElement("h3", "", "观测与关联"), createTextElement("div", "detail-copy raw-content", feedbackText));
  const native = createElement("section", "detail-section");
  native.append(createTextElement("h3", "", "原生身份"), createDefinitionGrid([
    ["用户活动", activity.id],
    ["事件", item.id],
    ["Tool / callId", item.nativeCallId ?? "未提供"],
    ["MCP JSON-RPC ID", item.mcpJsonRpcRequestId ?? "未提供"],
    ["MCP process_id", item.mcpProcessId === null ? "未提供" : String(item.mcpProcessId)],
    ["已关联 OS 事件", String(item.linkedOsEvents)],
    ["时间", formatTimestamp(item.timestampUnixMs)],
  ]));
  if (item.causalEdges.length > 0) {
    const edges = createElement("section", "detail-section");
    edges.append(createTextElement("h3", "", "原生因果边"));
    item.causalEdges.forEach((edge) => edges.append(createTextElement("div", "boundary raw-content", `${edge.from.source}:${edge.from.value} → ${edge.to.source}:${edge.to.value}\n依据：${edge.evidence_fields.join(" · ")}`)));
    body.append(content, feedback, native, edges);
  } else {
    const boundary = createElement("section", "detail-section");
    boundary.append(createTextElement("h3", "", "能力边界"), createBoundary(activity.limitation));
    body.append(content, feedback, native, boundary);
  }
  panel.append(resizer, head, body);
  return panel;
};

const renderSessions = (snapshot: ApplicationSnapshot, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const page = createElement("section", "view active session-view");
  const activity = findActivity(snapshot, state.selectedActivityId);
  if (activity === null) {
    page.append(createPageHeader("Sessions", "尚无真实会话", "语义 Observer 当前没有可按原生 user_activity_id 归组的用户活动。", createBadge(getObserverPresentation(snapshot.observer.state))));
    return page;
  }
  const selected = findTimelineItem(activity, state.selectedTimelineId) ?? activity.timeline[0] ?? null;
  const workbench = createElement("div", state.inspectorOpen ? "session-workbench" : "session-workbench no-detail");
  workbench.style.setProperty("--detail", `${String(state.inspectorWidth)}px`);
  const main = createElement("section", "session-main");
  const header = createElement("article", "card session-header");
  const badges = createElement("div", "session-badges");
  badges.append(createBadge(getCoveragePresentation(activity.coverage)), createSimpleBadge(`${String(activity.timeline.length)} 条语义事件`, "neutral"), createSimpleBadge("仅观察 Agent", "info"));
  header.append(createTextElement("div", "crumb raw-content", `${snapshot.agent.name} / 会话 / ${activity.id}`), createTextElement("h1", "", activity.title), createTextElement("p", "", activity.limitation), badges);
  const story = createElement("div", "story-pair");
  const promptItem = activity.timeline.find((item) => item.phase.toLocaleLowerCase("en-US").includes("user")) ?? activity.timeline[0] ?? null;
  const finalItem = [...activity.timeline].reverse().find((item) => item.phase.toLocaleLowerCase("en-US").includes("final")) ?? activity.timeline.at(-1) ?? null;
  story.append(
    createStoryButton("用户原始请求", activity.request, false, () => { if (promptItem !== null) actions.selectTimelineItem(promptItem.id); }),
    createStoryButton("Agent 最终结果", activity.finalResult ?? "当前活动没有记录最终结果", true, () => { if (finalItem !== null) actions.selectTimelineItem(finalItem.id); }),
  );
  const metrics = createElement("div", "metric-grid");
  metrics.append(createMetric(String(activity.timeline.length), "语义事件"), createMetric(String(activity.counts.tools), "Tool 调用"), createMetric(formatDuration(activity.startedAtUnixMs, activity.endedAtUnixMs), "会话跨度"), createMetric(String(activity.capabilities.length), "涉及能力"));
  main.append(header, story, metrics, renderFlow(activity, state, actions));
  workbench.append(renderSessionList(snapshot, state, actions), main);
  if (selected !== null && state.inspectorOpen) workbench.append(renderTimelineDetail(activity, selected, state, actions));
  page.append(workbench);
  return page;
};

const updateTimelineSelection = (
  main: HTMLElement,
  snapshot: ApplicationSnapshot,
  state: ApplicationViewState,
  actions: RenderActions,
  itemId: string,
): void => {
  const activity = findActivity(snapshot, state.selectedActivityId);
  if (activity === null) throw new Error(`无法在当前会话中选择节点：activity_id=${state.selectedActivityId}`);
  const item = findTimelineItem(activity, itemId);
  if (item === null) throw new Error(`无法在当前会话中选择节点：activity_id=${activity.id} item_id=${itemId}`);
  const workbench = main.querySelector<HTMLElement>(".session-workbench");
  if (workbench === null) throw new Error(`无法更新会话节点详情：activity_id=${activity.id} item_id=${itemId}`);

  workbench.querySelectorAll<HTMLButtonElement>(".flow-node").forEach((node) => {
    const selected = node.dataset.timelineItemId === itemId;
    node.classList.toggle("selected", selected);
    node.setAttribute("aria-pressed", String(selected));
  });
  workbench.classList.remove("no-detail");
  const detail = renderTimelineDetail(activity, item, state, actions);
  const currentDetail = workbench.querySelector<HTMLElement>(".detail-pane");
  if (currentDetail === null) workbench.append(detail);
  else currentDetail.replaceWith(detail);
};

const capabilityFacetCell = (facet: CapabilityFacetViewModel): HTMLTableCellElement => {
  const cell = createElement("td", "");
  const state = getCoveragePresentation(facet.state);
  const mark = createElement("span", `state-mark ${facet.state}`);
  mark.append(createElement("i", ""), document.createTextNode(state.label));
  mark.title = `${facet.source} · ${String(facet.evidenceCount)} 条证据${facet.limitation === null ? "" : ` · ${facet.limitation}`}`;
  cell.append(mark);
  return cell;
};

const renderPermission = (snapshot: ApplicationSnapshot): HTMLElement => {
  const section = createElement("section", "cap-detail-section");
  section.append(createSectionTitle("Agent 进程权限", "只展示已归属于目标 Agent 的 Token、完整性级别和特权。"));
  if (!snapshot.permission.observed) {
    section.append(createBoundary("当前会话没有发布 Agent 进程权限快照。"));
    return section;
  }
  snapshot.permission.processes.forEach((process) => {
    const details = createElement("details", "native-details");
    const summary = createElement("summary", "");
    summary.append(createTextElement("strong", "", `PID ${String(process.processId)} · ${INTEGRITY_LEVEL_NAMES[process.integrityLevel]}`), createSimpleBadge(process.isElevated ? "已提升" : "未提升", process.isElevated ? "warning" : "neutral"));
    const enabled = process.privileges.filter((item) => item.enabled).map((item) => item.name);
    details.append(summary, createDefinitionGrid([["进程实例", process.processInstanceId], ["完整性 RID", String(process.integrityRid)], ["特权数量", String(process.privileges.length)], ["已启用特权", enabled.join(" · ") || "无"]]));
    section.append(details);
  });
  return section;
};

const renderCapabilityAudit = (snapshot: ApplicationSnapshot): HTMLElement => {
  const section = createElement("section", "cap-detail-section");
  section.append(createSectionTitle("Capability Audit 决策", "原生 request_id 与 decision_id；当前始终 applied=false。"));
  section.append(createBoundary(`mode=${snapshot.capabilityAudit.mode} · applied=false · source=${snapshot.capabilityAudit.source}`));
  snapshot.capabilityAudit.records.forEach((record) => {
    const details = createElement("details", "native-details");
    const summary = createElement("summary", "");
    summary.append(createTextElement("strong", "", `${record.capability} / ${record.operation}`), createSimpleBadge(record.disposition, record.disposition === "would_block" ? "warning" : "neutral"));
    details.append(summary, createDefinitionGrid([["Request ID", record.requestId], ["Decision ID", record.decisionId], ["资源", record.resourceIdentifier], ["规则", record.ruleId], ["原因", record.reason], ["已应用", "false"]]));
    section.append(details);
  });
  if (snapshot.capabilityAudit.records.length === 0) section.append(createTextElement("p", "empty-state", "当前会话没有 Capability Audit 记录。"));
  return section;
};

const renderCapabilityDetail = (snapshot: ApplicationSnapshot, capability: CapabilityViewModel, actions: RenderActions): HTMLElement => {
  const section = createElement("article", "card card-pad capability-detail");
  const title = createElement("div", "cap-detail-title");
  title.append(createSectionTitle(`${capability.name} 详情`, capability.summary), createBadge(getCoveragePresentation(capability.state)));
  const facets = createElement("div", "facet-detail-grid");
  CAPABILITY_FACETS.forEach(([key, label]) => {
    const facet = capability.facets[key];
    const card = createElement("div", "facet-detail");
    card.append(createTextElement("strong", "", label), createBadge(getCoveragePresentation(facet.state)), createTextElement("span", "raw-content", facet.source), createTextElement("small", "", `${String(facet.evidenceCount)} 条证据${facet.limitation === null ? "" : ` · ${facet.limitation}`}`));
    facets.append(card);
  });
  section.append(title, facets);
  const related = createElement("div", "related-sessions");
  related.append(createTextElement("strong", "", "相关真实会话"));
  capability.relatedActivityIds.forEach((activityId) => {
    const activity = findActivity(snapshot, activityId);
    if (activity === null) return;
    const button = createTextElement("button", "button", activity.title);
    button.type = "button";
    button.addEventListener("click", () => actions.selectActivity(activity.id));
    related.append(button);
  });
  if (capability.relatedActivityIds.length === 0) related.append(createTextElement("span", "", "当前发布窗口没有相关会话。"));
  section.append(related);
  if (capability.kind === "permission") section.append(renderPermission(snapshot));
  if (capability.kind === "runtime_enforcement") section.append(renderCapabilityAudit(snapshot));
  return section;
};

const renderCapabilities = (snapshot: ApplicationSnapshot, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const page = createElement("section", "view active");
  page.append(createPageHeader("16 capability domains", "能力", "能力页回答“Agent 使用了什么，以及我们对调用、参数和反馈实际看到了多少”。", createSimpleBadge("无综合安全分数", "neutral")));
  const groups = createElement("div", "cap-groups");
  CAPABILITY_GROUP_NAMES.forEach(([group, title, description]) => {
    const section = createElement("section", "cap-group");
    section.append(createTextElement("h2", "", title), createTextElement("p", "", description));
    const grid = createElement("div", "cap-grid");
    snapshot.capabilities.filter((item) => item.group === group).forEach((capability) => {
      const card = createElement("button", capability.kind === state.selectedCapability ? "card cap-card selected" : "card cap-card");
      card.type = "button";
      const head = createElement("div", "cap-card-head");
      head.append(createTextElement("h3", "", capability.name), createBadge(getCoveragePresentation(capability.state)));
      const stats = createElement("div", "cap-stats");
      stats.append(createTextElement("span", "", capability.metric), createTextElement("span", "", "查看详情 ›"));
      card.append(head, createTextElement("p", "", capability.summary), stats);
      card.addEventListener("click", () => actions.selectCapability(capability.kind));
      grid.append(card);
    });
    section.append(grid);
    groups.append(section);
  });
  page.append(groups);
  const matrixCard = createElement("article", "card card-pad matrix-card");
  matrixCard.append(createSectionTitle("能力覆盖矩阵", "一个状态不能掩盖字段缺口；运行时佐证不是 Agent 调用成立的必要条件。"));
  const wrap = createScrollableRegion("matrix-wrap", "16 项能力六维覆盖矩阵");
  const table = createElement("table", "matrix");
  const head = createElement("thead", "");
  const headRow = createElement("tr", "");
  headRow.append(createTextElement("th", "", "能力"));
  CAPABILITY_FACETS.forEach(([, label]) => headRow.append(createTextElement("th", "", label)));
  head.append(headRow);
  const body = createElement("tbody", "");
  snapshot.capabilities.forEach((capability) => {
    const row = createElement("tr", capability.kind === state.selectedCapability ? "selected" : "");
    row.dataset.capabilityRow = capability.kind;
    const name = createTextElement("th", "", capability.name);
    name.scope = "row";
    row.append(name);
    CAPABILITY_FACETS.forEach(([key]) => row.append(capabilityFacetCell(capability.facets[key])));
    body.append(row);
  });
  table.append(head, body);
  wrap.append(table);
  matrixCard.append(wrap);
  page.append(matrixCard);
  const selected = snapshot.capabilities.find((item) => item.kind === state.selectedCapability) ?? snapshot.capabilities[0];
  if (selected !== undefined) page.append(renderCapabilityDetail(snapshot, selected, actions));
  return page;
};

const renderAgent = (snapshot: ApplicationSnapshot): HTMLElement => {
  const page = createElement("section", "view active");
  page.append(createPageHeader("Target agent", "Agent", "证明当前被观察的是哪个产品，并明确每一种数据来源的边界。", null));
  const layout = createElement("div", "agent-layout");
  const identityColumn = createElement("div", "");
  const identityCard = createElement("article", "card card-pad");
  const identity = createElement("div", "identity");
  const copy = createElement("div", "");
  copy.append(createTextElement("h2", "", snapshot.agent.name), createTextElement("p", "", `${snapshot.agent.productVersion} · ${snapshot.agent.publisher}`), createBadge(getAssociationPresentation(snapshot.agent.binding)));
  const icon = createElement("img", "");
  icon.src = "/assets/agent-workbuddy.svg";
  icon.alt = "WorkBuddy";
  identity.append(icon, copy);
  identityCard.append(identity, createDefinitionGrid([["程序", snapshot.agent.executablePath], ["SHA-256", snapshot.agent.sha256], ["语义会话", snapshot.agent.semanticSessionId], ["OS 会话", snapshot.agent.osSessionId], ["工作区", snapshot.agent.workspace ?? "未提供"], ["活动进程", String(snapshot.agent.activeProcesses)]]));
  const resource = snapshot.capabilities.find((item) => item.kind === "local_resource_usage");
  const resourceCard = createElement("article", "card card-pad resource-card");
  resourceCard.append(createSectionTitle("本地资源观测", "只统计已归属到目标 Agent 的进程范围。"), createBoundary(resource === undefined ? "当前契约缺少本地资源占用能力。" : `${resource.metric} · ${resource.summary}`));
  identityColumn.append(identityCard, resourceCard);
  const sourcesCard = createElement("aside", "card card-pad");
  const sources = createElement("div", "source-stack");
  sources.append(
    createSourceRow("1", "WorkBuddy 原生记录", "会话、ToolCall、参数、反馈与 Token。", createSimpleBadge("首选", "success")),
    createSourceRow("2", "MCP 原生协议", "JSON-RPC request/response 与进程上下文。", createSimpleBadge(snapshot.diagnostics.mcpProtocolRecords > 0 ? "已接入" : "未发布", snapshot.diagnostics.mcpProtocolRecords > 0 ? "success" : "warning")),
    createSourceRow("3", "Agent 进程范围", "权限、沙箱和资源状态的运行时佐证。", createSimpleBadge(snapshot.permission.observed ? "已接入" : "未发布", snapshot.permission.observed ? "success" : "warning")),
  );
  sourcesCard.append(createSectionTitle("数据来源", "只允许 Agent 中心来源。"), sources, createBoundary("不进入浏览器、不扫描操作系统、不使用 TLS 中间人；无法从 Agent 取得的数据保留为能力边界。"));
  layout.append(identityColumn, sourcesCard);
  page.append(layout);
  return page;
};

const createEvidenceItem = (evidence: EvidenceViewModel): HTMLButtonElement => {
  const button = createElement("button", "evidence-item");
  button.type = "button";
  const top = createElement("div", "evidence-item-top");
  top.append(createTextElement("b", "", evidence.title), createSimpleBadge(getEvidenceKindName(evidence.kind), "neutral"));
  button.append(top, createTextElement("p", "raw-content", evidence.content), createTextElement("p", "raw-content evidence-id", evidence.id));
  button.addEventListener("click", () => {
    const toast = requireElement<HTMLElement>("#toast");
    toast.textContent = `证据 ID：${evidence.id} · 来源：${evidence.source}`;
    toast.classList.add("show");
    window.setTimeout(() => toast.classList.remove("show"), 2600);
  });
  return button;
};

const renderEvidence = (snapshot: ApplicationSnapshot, state: ApplicationViewState, actions: RenderActions): HTMLElement => {
  const page = createElement("section", "view active");
  page.append(createPageHeader("Evidence & coverage", "证据与覆盖", "面向专家复核真实来源、原生 ID、数据健康和当前能力边界。", createSimpleBadge("本机只读", "info")));
  const layout = createElement("div", "evidence-layout");
  const listCard = createElement("article", "card card-pad");
  listCard.append(createSectionTitle("原始证据索引", "只列出能够回指当前真实数据集的记录。"));
  const filters = createElement("div", "evidence-filters");
  const filterOptions: ReadonlyArray<readonly [EvidenceFilter, string]> = [["all", "全部"], ...EVIDENCE_KIND_NAMES];
  filterOptions.forEach(([filter, label]) => {
    const button = createTextElement("button", filter === state.evidenceFilter ? "filter active" : "filter", label);
    button.type = "button";
    button.addEventListener("click", () => actions.setEvidenceFilter(filter));
    filters.append(button);
  });
  const list = createElement("div", "evidence-list");
  snapshot.evidence.items.filter((item) => state.evidenceFilter === "all" || item.kind === state.evidenceFilter).forEach((item) => list.append(createEvidenceItem(item)));
  if (list.childElementCount === 0) list.append(createTextElement("p", "empty-state", "当前筛选没有真实证据。"));
  if (state.evidenceNextCursor !== null && state.evidenceFilter === "all") {
    const more = createTextElement("button", "button", `加载更多（${String(snapshot.evidence.items.length)} / ${String(state.evidenceTotal)}）`);
    more.type = "button";
    more.addEventListener("click", () => { void actions.loadMoreEvidence(); });
    list.append(more);
  }
  listCard.append(filters, list);
  const side = createElement("aside", "");
  const health = createElement("article", "card card-pad");
  health.append(createSectionTitle("采集健康", formatTimestamp(snapshot.observer.lastHealthyAtUnixMs)), createDefinitionGrid([["ETW 丢失", String(snapshot.observer.eventsLost)], ["解析失败", String(snapshot.observer.parseFailures)], ["写入失败", String(snapshot.observer.writeFailures)], ["队列丢弃", String(snapshot.observer.queueDrops)], ["身份冲突", String(snapshot.observer.identityCollisions)], ["时钟偏差", snapshot.observer.clockSkewMs === null ? "未提供" : `${String(snapshot.observer.clockSkewMs)} ms`]]));
  const boundary = createElement("article", "card card-pad evidence-boundary-card");
  boundary.append(createSectionTitle("当前边界", "Gate 0 结论"), createBoundary(snapshot.coverage.summary.gateReason));
  side.append(health, boundary);
  layout.append(listCard, side);
  page.append(layout);
  const coverageCard = createElement("article", "card card-pad coverage-card");
  coverageCard.append(createSectionTitle("12 层观测矩阵", "每一层显示真实来源、证据数和对结论的影响。"));
  const wrap = createScrollableRegion("matrix-wrap", "12 层观测矩阵");
  const table = createElement("table", "coverage-table");
  const head = createElement("thead", "");
  const headRow = createElement("tr", "");
  ["层", "状态", "证据", "来源", "限制", "影响"].forEach((label) => headRow.append(createTextElement("th", "", label)));
  head.append(headRow);
  const body = createElement("tbody", "");
  snapshot.coverage.layers.forEach((layer) => {
    const row = createElement("tr", "");
    const name = createTextElement("th", "", `${String(layer.index)} · ${layer.name}`);
    name.scope = "row";
    const status = createElement("td", "");
    status.append(createBadge(getCoveragePresentation(layer.state)));
    row.append(name, status, createTextElement("td", "", String(layer.evidenceCount)), createTextElement("td", "raw-content", layer.source), createTextElement("td", "", layer.limitation), createTextElement("td", "", layer.impact));
    body.append(row);
  });
  table.append(head, body);
  wrap.append(table);
  coverageCard.append(wrap);
  page.append(coverageCard);
  return page;
};

const createSetting = (title: string, value: string, description: string): HTMLElement => {
  const card = createElement("article", "card setting");
  card.append(createTextElement("h3", "", title), createTextElement("p", "", description), createTextElement("div", "setting-value raw-content", value));
  return card;
};

const renderSettings = (snapshot: ApplicationSnapshot, settings: ProductSettingsView, actions: ProductActions): HTMLElement => {
  const page = createElement("section", "view active");
  page.append(createPageHeader("Local product", "设置与诊断", "所有原始内容保留在本机；当前界面不会修改 Agent、系统或浏览器。", null));
  const grid = createElement("div", "settings-grid");
  const left = createElement("div", "setting-column");
  left.append(
    createSetting("观测模式", "mode = observe_only · applied = false", "Runtime Enforcement 当前只记录意图与 Audit 判断。"),
    createSetting("证据保留", `retention_days = ${String(settings.retentionDays)}`, "产品配置由当前 Windows 用户 DPAPI 保护。"),
    createSetting("诊断导出", settings.diagnosticExportMode, "只通过用户显式操作导出 manifest 已发布内容。"),
    createSetting("当前会话", settings.selectedSessionId, "OS、语义与 MCP 必须按共享原生 session_id 选择。"),
  );
  const right = createElement("div", "setting-column");
  right.append(
    createSetting("Agent 原生来源", snapshot.diagnostics.semanticSource, `覆盖状态：${snapshot.diagnostics.semanticCoverageStatus}`),
    createSetting("MCP 来源", snapshot.diagnostics.mcpSource, `${String(snapshot.diagnostics.mcpProtocolRecords)} 条已发布记录 · ${snapshot.diagnostics.mcpPublicationBoundary}`),
    createSetting("OS Observer", snapshot.diagnostics.osRunRoot, `覆盖状态：${snapshot.diagnostics.osCoverageStatus}`),
    createSetting("GUI 数据策略", snapshot.diagnostics.dataPolicy, "不使用 fixture 回退，不向外部服务发送。"),
  );
  grid.append(left, right);
  page.append(grid);
  const operations = createElement("article", "card card-pad product-actions");
  operations.append(createSectionTitle("本地数据管理", "改变状态的操作只在用户显式触发后执行。"));
  const buttons = createElement("div", "setting-actions");
  const preview = createTextElement("button", "button", "预览过期证据");
  preview.type = "button";
  const apply = createTextElement("button", "button danger-button", "删除已预览的过期证据");
  apply.type = "button";
  apply.disabled = true;
  const exportButton = createTextElement("button", "button", "生成诊断导出");
  exportButton.type = "button";
  const status = createTextElement("p", "action-status raw-content", "尚未执行数据管理操作。");
  status.setAttribute("aria-live", "polite");
  preview.addEventListener("click", () => { void actions.previewRetention(status, apply); });
  apply.addEventListener("click", () => { void actions.applyRetention(status, apply); });
  exportButton.addEventListener("click", () => { void actions.exportDiagnostics(status); });
  buttons.append(preview, apply, exportButton);
  operations.append(buttons, status);
  page.append(operations);
  return page;
};

const updateShell = (snapshot: ApplicationSnapshot, state: ApplicationViewState): void => {
  requireElement<HTMLElement>("#workbuddy-version").textContent = `${snapshot.agent.productVersion} · ${getAssociationPresentation(snapshot.agent.binding).label}`;
  document.querySelectorAll<HTMLButtonElement>("[data-system-view]").forEach((button) => button.classList.toggle("active", button.dataset.systemView === state.systemView));
};

const updateRuntimeShell = (runtime: ObserverRuntimeViewModel): void => {
  const observer = getObserverRuntimePresentation(runtime.state);
  const health = requireElement<HTMLElement>("#observer-top-status");
  health.textContent = observer.label;
  health.className = `health-pill ${observer.tone}`;
};

const updateObserverElements = (snapshot: ApplicationSnapshot): void => {
  const observer = getObserverPresentation(snapshot.observer.state);
  const overviewLabel = document.querySelector<HTMLElement>("#observer-overview-label");
  const overviewReason = document.querySelector<HTMLElement>("#observer-overview-reason");
  if (overviewLabel !== null) overviewLabel.textContent = observer.label;
  if (overviewReason !== null) overviewReason.textContent = snapshot.observer.reason;
};

const waitForObserverRuntime = async (): Promise<ObserverRuntimeViewModel> => {
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const runtime = await loadObserverRuntime();
    updateRuntimeShell(runtime);
    if (runtime.state === "running" || runtime.state === "idle") return runtime;
    if (runtime.state !== "starting") {
      throw new Error(`Observer 自动启动失败：state=${runtime.state} detail=${runtime.detail}`);
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 250));
  }
  throw new Error("Observer 自动启动超时：120 秒内未进入 running 或 idle");
};

const renderFailure = (main: HTMLElement, error: Error): void => {
  const page = createElement("section", "view active");
  page.append(createPageHeader("Observer query failed", "无法读取真实 Observer 数据", "界面没有回退到 fixture、缓存或演示数据。", createSimpleBadge("读取失败", "danger")), createTextElement("pre", "error-state card raw-content", error.message));
  main.replaceChildren(page);
};

const createApplication = async (): Promise<void> => {
  const main = requireElement<HTMLElement>("#main-content");
  const refreshButton = requireElement<HTMLButtonElement>("#refresh-data");
  const navigationStatus = requireElement<HTMLElement>("#navigation-status");
  let snapshot: ApplicationSnapshot | null = null;
  let productSettings: ProductSettingsView | null = null;
  let retentionPlan: RetentionPlan | null = null;
  let observerRefreshRunning = false;
  const state: ApplicationViewState = {
    selectedActivityId: "",
    selectedTimelineId: "",
    selectedCapability: "shell",
    systemView: initialSystemView(),
    evidenceFilter: "all",
    inspectorOpen: true,
    inspectorWidth: 390,
    sessionNextCursor: null,
    sessionTotal: 0,
    evidenceNextCursor: null,
    evidenceTotal: 0,
  };

  let actions: RenderActions;

  const renderCurrentView = (): void => {
    if (snapshot === null || productSettings === null) {
      main.replaceChildren(createTextElement("p", "loading-card", "正在读取真实 Observer 数据…"));
      return;
    }
    updateShell(snapshot, state);
    switch (state.systemView) {
      case "overview": main.replaceChildren(renderOverview(snapshot, actions)); break;
      case "sessions": main.replaceChildren(renderSessions(snapshot, state, actions)); break;
      case "capabilities": main.replaceChildren(renderCapabilities(snapshot, state, actions)); break;
      case "agents": main.replaceChildren(renderAgent(snapshot)); break;
      case "evidence": main.replaceChildren(renderEvidence(snapshot, state, actions)); break;
      case "settings": main.replaceChildren(renderSettings(snapshot, productSettings, actions)); break;
    }
  };

  actions = {
    selectActivity: (activityId) => {
      if (snapshot === null) return;
      const activity = findActivity(snapshot, activityId);
      if (activity === null) throw new Error(`未知会话：${activityId}`);
      state.selectedActivityId = activityId;
      state.selectedTimelineId = activity.timeline[0]?.id ?? "";
      state.inspectorOpen = true;
      state.systemView = "sessions";
      updateLocationView("sessions");
      renderCurrentView();
      navigationStatus.textContent = `已打开会话：${activity.title}`;
    },
    selectTimelineItem: (itemId) => {
      if (snapshot === null) throw new Error(`无法选择会话节点：item_id=${itemId} snapshot=unavailable`);
      state.selectedTimelineId = itemId;
      state.inspectorOpen = true;
      updateTimelineSelection(main, snapshot, state, actions, itemId);
      navigationStatus.textContent = "已在右侧打开当前节点详情";
    },
    selectCapability: (kind) => {
      state.selectedCapability = kind;
      renderCurrentView();
      window.requestAnimationFrame(() => document.querySelector(`[data-capability-row="${kind}"]`)?.scrollIntoView({ block: "center", behavior: "smooth" }));
      navigationStatus.textContent = `已打开能力：${snapshot === null ? kind : capabilityName(snapshot, kind)}`;
    },
    selectSystemView: (view) => {
      state.systemView = view;
      updateLocationView(view);
      renderCurrentView();
      main.scrollTop = 0;
      navigationStatus.textContent = `已打开${getSystemViewName(view)}`;
    },
    setEvidenceFilter: (filter) => {
      state.evidenceFilter = filter;
      renderCurrentView();
    },
    closeInspector: () => {
      state.inspectorOpen = false;
      renderCurrentView();
      navigationStatus.textContent = "已关闭节点详情；再次选择节点可重新打开";
    },
    setInspectorWidth: (width) => { state.inspectorWidth = width; },
    loadMoreSessions: async () => {
      if (snapshot === null || state.sessionNextCursor === null) return;
      const page = await loadSessionsPage(snapshot.agent.id, state.sessionNextCursor, 50);
      snapshot = { ...snapshot, activities: [...snapshot.activities, ...page.items] };
      state.sessionNextCursor = page.nextCursor;
      state.sessionTotal = page.total;
      renderCurrentView();
    },
    loadMoreEvidence: async () => {
      if (snapshot === null || state.evidenceNextCursor === null) return;
      const page = await loadEvidencePage(snapshot.agent.semanticSessionId, state.evidenceNextCursor, 200);
      snapshot = { ...snapshot, evidence: { ...snapshot.evidence, items: [...snapshot.evidence.items, ...page.items] } };
      state.evidenceNextCursor = page.nextCursor;
      state.evidenceTotal = page.total;
      renderCurrentView();
    },
    previewRetention: async (status, applyButton) => {
      applyButton.disabled = true;
      status.textContent = "正在计算过期证据…";
      try {
        retentionPlan = await loadRetentionPlan();
        const bytes = retentionPlan.candidates.reduce((total, candidate) => total + candidate.bytes, 0);
        status.textContent = `预览完成：${String(retentionPlan.candidates.length)} 个运行目录，${formatBytes(bytes)}，截止 ${formatTimestamp(retentionPlan.cutoffUnixMs)}。`;
        applyButton.disabled = retentionPlan.candidates.length === 0;
      } catch (error) {
        retentionPlan = null;
        status.textContent = error instanceof Error ? error.message : String(error);
      }
    },
    applyRetention: async (status, applyButton) => {
      if (retentionPlan === null) {
        status.textContent = "删除过期证据前必须重新预览。";
        return;
      }
      if (!window.confirm(`确定删除 ${String(retentionPlan.candidates.length)} 个已预览的过期 Observer 目录吗？当前选中运行不会被删除。`)) {
        status.textContent = "已取消删除。";
        return;
      }
      applyButton.disabled = true;
      const application = await applyRetentionPlan(retentionPlan.confirmationToken);
      status.textContent = `已删除 ${String(application.deletedDirectories.length)} 个目录，释放 ${formatBytes(application.deletedBytes)}。`;
      retentionPlan = null;
    },
    exportDiagnostics: async (status) => {
      status.textContent = "正在生成本地诊断导出…";
      const summary = await createDiagnosticExport(`agentreins-diagnostic-${String(Date.now())}.zip`);
      status.textContent = `导出完成：${summary.archivePath}，${String(summary.files)} 个文件，${formatBytes(summary.bytes)}，SHA-256 ${summary.sha256}。`;
    },
  };

  const refresh = async (preserveScroll: boolean): Promise<void> => {
    const previousScrollTop = main.scrollTop;
    refreshButton.disabled = true;
    main.setAttribute("aria-busy", "true");
    try {
      const [nextSnapshot, nextSettings] = await Promise.all([loadSnapshot(), loadProductSettings()]);
      const [sessionPage, evidencePage] = await Promise.all([loadSessionsPage(nextSnapshot.agent.id, "0", 50), loadEvidencePage(nextSnapshot.agent.semanticSessionId, "0", 200)]);
      snapshot = { ...nextSnapshot, activities: sessionPage.items, evidence: { ...nextSnapshot.evidence, items: evidencePage.items } };
      productSettings = nextSettings;
      state.sessionNextCursor = sessionPage.nextCursor;
      state.sessionTotal = sessionPage.total;
      state.evidenceNextCursor = evidencePage.nextCursor;
      state.evidenceTotal = evidencePage.total;
      const selectedActivity = findActivity(snapshot, state.selectedActivityId) ?? snapshot.activities[0] ?? null;
      state.selectedActivityId = selectedActivity?.id ?? "";
      if (selectedActivity !== null && findTimelineItem(selectedActivity, state.selectedTimelineId) === null) state.selectedTimelineId = selectedActivity.timeline[0]?.id ?? "";
      if (!snapshot.capabilities.some((item) => item.kind === state.selectedCapability)) state.selectedCapability = snapshot.capabilities[0]?.kind ?? "shell";
      renderCurrentView();
      if (preserveScroll) main.scrollTop = previousScrollTop;
      navigationStatus.textContent = `真实数据已刷新：${formatTimestamp(snapshot.generatedAtUnixMs)}`;
    } catch (error) {
      renderFailure(main, error instanceof Error ? error : new Error(String(error)));
      requireElement<HTMLElement>("#observer-top-status").textContent = "读取失败";
    } finally {
      main.removeAttribute("aria-busy");
      refreshButton.disabled = false;
    }
  };

  const refreshObserverStatus = async (): Promise<void> => {
    if (snapshot === null || observerRefreshRunning) return;
    observerRefreshRunning = true;
    try {
      const runtime = await loadObserverRuntime();
      updateRuntimeShell(runtime);
      if (runtime.state !== "running" && runtime.state !== "idle") {
        navigationStatus.textContent = runtime.detail;
        return;
      }
      const nextSnapshot = await loadSnapshot();
      if (nextSnapshot.agent.semanticSessionId !== snapshot.agent.semanticSessionId) {
        await refresh(true);
        return;
      }
      snapshot = {
        ...snapshot,
        observer: nextSnapshot.observer,
        agent: nextSnapshot.agent,
        capabilities: nextSnapshot.capabilities,
        coverage: nextSnapshot.coverage,
        diagnostics: nextSnapshot.diagnostics,
      };
      updateObserverElements(snapshot);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      navigationStatus.textContent = `Observer 状态自动刷新失败：${detail}`;
    } finally {
      observerRefreshRunning = false;
    }
  };

  document.querySelectorAll<HTMLButtonElement>("[data-system-view]").forEach((button) => {
    const view = button.dataset.systemView;
    if (!isSystemView(view)) throw new Error(`未知系统视图：${String(view)}`);
    button.addEventListener("click", () => actions.selectSystemView(view));
  });
  requireElement<HTMLButtonElement>("#workbuddy-agent").addEventListener("click", () => actions.selectSystemView("agents"));
  refreshButton.addEventListener("click", () => { void refresh(true); });
  try {
    await waitForObserverRuntime();
    await refresh(false);
    window.setInterval(() => { void refreshObserverStatus(); }, 5_000);
  } catch (error) {
    renderFailure(main, error instanceof Error ? error : new Error(String(error)));
    requireElement<HTMLElement>("#observer-top-status").textContent = "启动失败";
  }
};

void createApplication();
