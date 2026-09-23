import type {
  ApplicationSnapshot,
  DiagnosticExportSummary,
  EvidencePageViewModel,
  ObserverRuntimeViewModel,
  ProductSettingsView,
  RetentionApplication,
  RetentionPlan,
  SessionPageViewModel,
} from "./types";

interface ErrorResponse {
  readonly error: string;
}

export const loadSnapshot = async (): Promise<ApplicationSnapshot> => {
  const response = await fetch("/api/v1/snapshot", {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: ApplicationSnapshot | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`读取真实 Observer 数据失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("schemaVersion" in value) || value.schemaVersion !== "0.6.0") {
    throw new Error(`读取真实 Observer 数据失败：不支持的响应契约 response=${JSON.stringify(value)}`);
  }
  return value;
};

export const loadObserverRuntime = async (): Promise<ObserverRuntimeViewModel> => {
  const response = await fetch("/api/v1/observer-runtime", {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: ObserverRuntimeViewModel | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`读取 Observer 生命周期失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("schemaVersion" in value) || value.schemaVersion !== "0.1.0" || value.applied !== false) {
    throw new Error(`读取 Observer 生命周期失败：不支持的响应契约 response=${JSON.stringify(value)}`);
  }
  return value;
};

export const loadSessionsPage = async (
  agentId: string,
  cursor: string,
  limit: number,
): Promise<SessionPageViewModel> => {
  const query = new URLSearchParams({ agentId, cursor, limit: String(limit) });
  const response = await fetch(`/api/v1/sessions?${query.toString()}`, {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: SessionPageViewModel | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`读取会话分页失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("items" in value)) throw new Error(`会话分页响应无效：${JSON.stringify(value)}`);
  return value;
};

export const loadEvidencePage = async (
  sessionId: string,
  cursor: string,
  limit: number,
): Promise<EvidencePageViewModel> => {
  const query = new URLSearchParams({ sessionId, cursor, limit: String(limit) });
  const response = await fetch(`/api/v1/evidence?${query.toString()}`, {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: EvidencePageViewModel | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`读取证据分页失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("items" in value)) throw new Error(`证据分页响应无效：${JSON.stringify(value)}`);
  return value;
};

export const loadProductSettings = async (): Promise<ProductSettingsView> => {
  const response = await fetch("/api/v1/product-settings", {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: ProductSettingsView | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`读取产品设置失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("retentionDays" in value)) throw new Error(`产品设置响应无效：${JSON.stringify(value)}`);
  return value;
};

export const loadRetentionPlan = async (): Promise<RetentionPlan> => {
  const response = await fetch("/api/v1/retention-plan", {
    method: "GET",
    headers: { Accept: "application/json" },
    cache: "no-store",
    credentials: "same-origin",
  });
  const value: RetentionPlan | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`预览证据保留计划失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("confirmationToken" in value)) throw new Error(`证据保留计划响应无效：${JSON.stringify(value)}`);
  return value;
};

export const applyRetentionPlan = async (confirmationToken: string): Promise<RetentionApplication> => {
  const response = await fetch("/api/v1/expired-observer-runs", {
    method: "DELETE",
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    cache: "no-store",
    credentials: "same-origin",
    body: JSON.stringify({ confirmationToken }),
  });
  const value: RetentionApplication | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`执行证据保留计划失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("deletedDirectories" in value)) throw new Error(`证据保留执行响应无效：${JSON.stringify(value)}`);
  return value;
};

export const createDiagnosticExport = async (archiveName: string): Promise<DiagnosticExportSummary> => {
  const response = await fetch("/api/v1/diagnostic-exports", {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    cache: "no-store",
    credentials: "same-origin",
    body: JSON.stringify({ archiveName }),
  });
  const value: DiagnosticExportSummary | ErrorResponse = await response.json();
  if (!response.ok) {
    const detail = "error" in value ? value.error : JSON.stringify(value);
    throw new Error(`生成诊断导出失败：status=${String(response.status)} response=${detail}`);
  }
  if (!("archivePath" in value)) throw new Error(`诊断导出响应无效：${JSON.stringify(value)}`);
  return value;
};
