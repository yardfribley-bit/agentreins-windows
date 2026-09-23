// @ts-check

import { createInterface } from "node:readline";
import { appendFileSync, existsSync, renameSync, statSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

/** @typedef {string | number | null} JsonRpcId */
/** @typedef {{ jsonrpc: "2.0", id?: JsonRpcId, method?: string, params?: Record<string, unknown> }} JsonRpcMessage */
/** @typedef {{ initialized: boolean, protocolLogPath: string, protocolManifestPath: string, protocolRecords: number }} ServerState */

const SERVER_NAME = "agentreins-weather-mcp";
const SERVER_VERSION = "0.1.0";
const TOOL_NAME = "get_controlled_weather";
const SUPPORTED_PROTOCOLS = new Set(["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]);

/**
 * @param {string[]} args
 * @returns {{ baseUrl: URL, protocolLogPath: string, protocolManifestPath: string }}
 */
function parseArguments(args) {
  if (args.length !== 6 || args[0] !== "--base-url" || args[1] === undefined || args[2] !== "--protocol-log" || args[3] === undefined || args[4] !== "--protocol-manifest" || args[5] === undefined) {
    throw new TypeError(`必须提供 --base-url、--protocol-log 和 --protocol-manifest 参数 args=${JSON.stringify(args)}`);
  }
  const url = new URL(args[1]);
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1") {
    throw new RangeError(`测试 MCP 只允许访问 127.0.0.1 HTTP 服务 url=${url.toString()}`);
  }
  if (args[3].trim().length === 0) {
    throw new TypeError("--protocol-log 不得为空");
  }
  if (args[5].trim().length === 0) {
    throw new TypeError("--protocol-manifest 不得为空");
  }
  return { baseUrl: url, protocolLogPath: resolve(args[3]), protocolManifestPath: resolve(args[5]) };
}

/**
 * @param {ServerState} state
 * @param {boolean} complete
 * @returns {void}
 */
function publishProtocolManifest(state, complete) {
  const temporaryPath = `${state.protocolManifestPath}.tmp`;
  writeFileSync(temporaryPath, JSON.stringify({
    schema_version: "1.0.0",
    mcp_file: state.protocolLogPath,
    published_bytes: statSync(state.protocolLogPath).size,
    protocol_records: state.protocolRecords,
    complete,
  }), "utf8");
  renameSync(temporaryPath, state.protocolManifestPath);
}

/**
 * @param {ServerState} state
 * @param {"request" | "response"} direction
 * @param {string} rawJson
 * @returns {void}
 */
function writeProtocolRecord(state, direction, rawJson) {
  appendFileSync(state.protocolLogPath, `${JSON.stringify({
    schema_version: "1.0.0",
    captured_at_unix_ms: Date.now(),
    direction,
    transport: "stdio",
    process_id: process.pid,
    parent_process_id: process.ppid,
    working_directory: process.cwd(),
    raw_json: rawJson,
  })}\n`, "utf8");
  state.protocolRecords += 1;
  publishProtocolManifest(state, false);
}

/**
 * @param {JsonRpcId} id
 * @param {object} result
 * @param {ServerState} state
 * @returns {void}
 */
function writeResult(id, result, state) {
  const response = JSON.stringify({ jsonrpc: "2.0", id, result });
  writeProtocolRecord(state, "response", response);
  process.stdout.write(`${response}\n`);
}

/**
 * @param {JsonRpcId} id
 * @param {number} code
 * @param {string} message
 * @param {object} data
 * @param {ServerState} state
 * @returns {void}
 */
function writeError(id, code, message, data, state) {
  const response = JSON.stringify({ jsonrpc: "2.0", id, error: { code, message, data } });
  writeProtocolRecord(state, "response", response);
  process.stdout.write(`${response}\n`);
}

/**
 * @param {Record<string, unknown> | undefined} params
 * @param {string} name
 * @returns {string}
 */
function requireString(params, name) {
  const value = params?.[name];
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`字段必须是非空字符串 field=${name} value=${JSON.stringify(value)}`);
  }
  return value;
}

/**
 * @param {URL} baseUrl
 * @param {string} city
 * @param {string} runId
 * @returns {Promise<Record<string, unknown>>}
 */
async function fetchWeather(baseUrl, city, runId) {
  const url = new URL("/weather", baseUrl);
  url.searchParams.set("city", city);
  url.searchParams.set("run_id", runId);
  url.searchParams.set("channel", "mcp");

  /** @type {Error | undefined} */
  let lastError;
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    try {
      const response = await fetch(url, {
        method: "GET",
        headers: { accept: "application/json" },
        signal: AbortSignal.timeout(5000),
      });
      const responseBody = await response.text();
      if (!response.ok) {
        throw new Error(`天气服务返回失败 status=${response.status} body=${responseBody} url=${url.toString()}`);
      }
      const parsed = JSON.parse(responseBody);
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
        throw new TypeError(`天气服务响应不是对象 body=${responseBody}`);
      }
      return parsed;
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));
      process.stderr.write(`${JSON.stringify({
        level: "warn",
        event: "weather_request_failed",
        attempt,
        url: url.toString(),
        message: lastError.message,
      })}\n`);
    }
  }
  throw lastError ?? new Error(`天气服务请求失败但没有错误对象 url=${url.toString()}`);
}

/**
 * @param {JsonRpcMessage} message
 * @param {URL} baseUrl
 * @param {ServerState} state
 * @returns {Promise<void>}
 */
async function handleMessage(message, baseUrl, state) {
  const id = message.id ?? null;
  if (message.method === "notifications/initialized") {
    state.initialized = true;
    return;
  }
  if (message.method === "initialize") {
    const protocolVersion = requireString(message.params, "protocolVersion");
    if (!SUPPORTED_PROTOCOLS.has(protocolVersion)) {
      writeError(id, -32602, "Unsupported protocol version", {
        requested: protocolVersion,
        supported: [...SUPPORTED_PROTOCOLS],
      }, state);
      return;
    }
    writeResult(id, {
      protocolVersion,
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: SERVER_NAME, title: "AgentReins 受控天气 MCP", version: SERVER_VERSION },
      instructions: "仅查询 AgentReins 受控天气夹具，不代表真实天气。",
    }, state);
    return;
  }
  if (message.method === "ping") {
    writeResult(id, {}, state);
    return;
  }
  if (!state.initialized) {
    writeError(id, -32002, "Server is not initialized", { method: message.method ?? null }, state);
    return;
  }
  if (message.method === "tools/list") {
    writeResult(id, {
      tools: [{
        name: TOOL_NAME,
        title: "查询受控天气",
        description: "查询北京的固定天气测试数据，用于识别 MCP ToolCall；调用方生成内部关联标识，不向用户索取。",
        inputSchema: {
          type: "object",
          additionalProperties: false,
          required: ["city", "run_id"],
          properties: {
            city: { type: "string", description: "城市；当前夹具只支持北京" },
            run_id: { type: "string", description: "调用方生成的内部证据关联标识" },
          },
        },
        outputSchema: {
          type: "object",
          required: ["schema_version", "run_id", "city", "measurement", "provenance", "execution"],
        },
      }],
    }, state);
    return;
  }
  if (message.method === "tools/call") {
    const name = requireString(message.params, "name");
    if (name !== TOOL_NAME) {
      writeError(id, -32602, "Unknown tool", { requested: name, available: [TOOL_NAME] }, state);
      return;
    }
    const rawArguments = message.params?.arguments;
    if (rawArguments === null || typeof rawArguments !== "object" || Array.isArray(rawArguments)) {
      writeError(id, -32602, "Tool arguments must be an object", { arguments: rawArguments ?? null }, state);
      return;
    }
    const argumentsObject = /** @type {Record<string, unknown>} */ (rawArguments);
    const city = requireString(argumentsObject, "city");
    const runId = requireString(argumentsObject, "run_id");
    try {
      const weather = await fetchWeather(baseUrl, city, runId);
      writeResult(id, {
        content: [{ type: "text", text: JSON.stringify(weather) }],
        structuredContent: weather,
        isError: false,
      }, state);
    } catch (error) {
      const messageText = error instanceof Error ? error.message : String(error);
      writeResult(id, {
        content: [{ type: "text", text: messageText }],
        isError: true,
      }, state);
    }
    return;
  }
  writeError(id, -32601, "Method not found", { method: message.method ?? null }, state);
}

const { baseUrl, protocolLogPath, protocolManifestPath } = parseArguments(process.argv.slice(2));
if (existsSync(protocolLogPath) && statSync(protocolLogPath).size !== 0) {
  throw new Error(`MCP 协议日志必须是新文件 path=${protocolLogPath}`);
}
writeFileSync(protocolLogPath, "", { encoding: "utf8", flag: "a" });
/** @type {ServerState} */
const state = { initialized: false, protocolLogPath, protocolManifestPath, protocolRecords: 0 };
publishProtocolManifest(state, false);
const input = createInterface({ input: process.stdin, crlfDelay: Infinity });

input.on("line", (line) => {
  if (line.length === 0) {
    return;
  }
  writeProtocolRecord(state, "request", line);
  try {
    const parsed = JSON.parse(line);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new TypeError(`MCP 消息必须是 JSON 对象 line=${line}`);
    }
    void handleMessage(/** @type {JsonRpcMessage} */ (parsed), baseUrl, state).catch((error) => {
      const messageText = error instanceof Error ? error.message : String(error);
      const invalidParameters = error instanceof TypeError || error instanceof RangeError;
      writeError(
        parsed.id ?? null,
        invalidParameters ? -32602 : -32603,
        invalidParameters ? "Invalid params" : "Internal error",
        { message: messageText },
        state,
      );
    });
  } catch (error) {
    const messageText = error instanceof Error ? error.message : String(error);
    writeError(null, -32700, "Parse error", { message: messageText }, state);
  }
});

input.on("close", () => publishProtocolManifest(state, true));
