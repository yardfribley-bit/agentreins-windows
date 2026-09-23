// @ts-check

import { appendFileSync, mkdirSync, readFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, resolve } from "node:path";

/** @typedef {{ host: string, port: number, logPath: string, tokenFile: string }} ServiceOptions */

const MAX_BODY_BYTES = 1024 * 1024;

/**
 * @param {string[]} args
 * @returns {ServiceOptions}
 */
function parseArgs(args) {
  /** @type {Map<string, string>} */
  const values = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (key === undefined || value === undefined || !key.startsWith("--")) {
      throw new TypeError(`参数必须成对提供 key=${String(key)} value=${String(value)}`);
    }
    values.set(key, value);
  }

  const host = requiredValue(values, "--host");
  const portText = requiredValue(values, "--port");
  const port = Number(portText);
  if (host !== "127.0.0.1") {
    throw new RangeError(`受控服务只允许监听 127.0.0.1 host=${host}`);
  }
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new RangeError(`端口无效 port=${portText}`);
  }

  return {
    host,
    port,
    logPath: resolve(requiredValue(values, "--log")),
    tokenFile: resolve(requiredValue(values, "--token-file")),
  };
}

/**
 * @param {Map<string, string>} values
 * @param {string} key
 * @returns {string}
 */
function requiredValue(values, key) {
  const value = values.get(key);
  if (value === undefined || value.length === 0) {
    throw new TypeError(`缺少必填参数 key=${key}`);
  }
  return value;
}

/**
 * @param {import("node:http").IncomingMessage} request
 * @returns {Promise<string>}
 */
async function readBody(request) {
  /** @type {Buffer[]} */
  const chunks = [];
  let totalBytes = 0;
  for await (const chunk of request) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    totalBytes += buffer.length;
    if (totalBytes > MAX_BODY_BYTES) {
      throw new RangeError(`请求正文超过限制 bytes=${totalBytes} limit=${MAX_BODY_BYTES}`);
    }
    chunks.push(buffer);
  }
  return Buffer.concat(chunks).toString("utf8");
}

/**
 * @param {string} value
 * @param {string} field
 * @returns {string}
 */
function requireText(value, field) {
  if (value.length === 0) {
    throw new TypeError(`缺少必填字段 field=${field}`);
  }
  return value;
}

/**
 * @param {import("node:http").ServerResponse} response
 * @param {number} statusCode
 * @param {object} value
 * @returns {void}
 */
function sendJson(response, statusCode, value) {
  const body = JSON.stringify(value);
  response.writeHead(statusCode, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
  });
  response.end(body);
}

/**
 * @param {string} city
 * @param {string} runId
 * @param {string} channel
 * @returns {object}
 */
function weatherResult(city, runId, channel) {
  if (city !== "北京") {
    throw new RangeError(`受控天气夹具不包含该城市 city=${city}`);
  }
  if (channel !== "skill" && channel !== "mcp") {
    throw new RangeError(`调用通道无效 channel=${channel}`);
  }
  return {
    schema_version: "1.0",
    run_id: runId,
    city,
    measurement: {
      temperature_c: 22.5,
      condition: "晴（受控测试数据）",
      observed_at: "2026-09-12T08:00:00.000Z",
    },
    provenance: {
      source: "agentreins-controlled-weather-v1",
      fixture: true,
    },
    execution: channel === "skill"
      ? { channel, component: "agentreins-weather-skill", tool_name: "query-weather.ps1" }
      : { channel, component: "agentreins-weather-mcp", tool_name: "get_controlled_weather" },
  };
}

const options = parseArgs(process.argv.slice(2));
mkdirSync(dirname(options.logPath), { recursive: true });
const expectedToken = readFileSync(options.tokenFile, "utf8").trimEnd();
if (expectedToken.length === 0) {
  throw new Error(`测试凭据文件为空 path=${options.tokenFile}`);
}

const server = createServer(async (request, response) => {
  const receivedAt = new Date().toISOString();
  const method = request.method ?? "";
  const requestUrl = new URL(request.url ?? "", `http://${options.host}:${options.port}`);
  try {
    const body = await readBody(request);
    appendFileSync(options.logPath, `${JSON.stringify({
      received_at: receivedAt,
      method,
      url: requestUrl.pathname + requestUrl.search,
      remote_address: request.socket.remoteAddress,
      headers: request.headers,
      body,
    })}\n`, "utf8");

    if (method === "GET" && requestUrl.pathname === "/fixed") {
      const runId = requireText(requestUrl.searchParams.get("run_id") ?? "", "run_id");
      sendJson(response, 200, { run_id: runId, value: "AGENTREINS_FIXED_READ_V1" });
      return;
    }
    if (method === "GET" && requestUrl.pathname === "/weather") {
      const city = requireText(requestUrl.searchParams.get("city") ?? "", "city");
      const runId = requireText(requestUrl.searchParams.get("run_id") ?? "", "run_id");
      const channel = requireText(requestUrl.searchParams.get("channel") ?? "", "channel");
      sendJson(response, 200, weatherResult(city, runId, channel));
      return;
    }
    if (method === "POST" && requestUrl.pathname === "/echo") {
      const parsed = JSON.parse(body);
      sendJson(response, 200, { received_at: receivedAt, received: parsed });
      return;
    }
    if (method === "POST" && requestUrl.pathname === "/auth") {
      const authorization = request.headers.authorization ?? "";
      if (authorization !== `Bearer ${expectedToken}`) {
        sendJson(response, 401, { error: "invalid_test_credential" });
        return;
      }
      sendJson(response, 200, { authenticated: true, received_at: receivedAt });
      return;
    }
    sendJson(response, 404, { error: "route_not_found", method, path: requestUrl.pathname });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    sendJson(response, error instanceof SyntaxError || error instanceof TypeError || error instanceof RangeError ? 400 : 500, {
      error: "request_failed",
      message,
    });
  }
});

server.on("error", (error) => {
  process.stderr.write(`${JSON.stringify({ level: "error", event: "server_error", message: error.message })}\n`);
  process.exitCode = 1;
});

server.listen(options.port, options.host, () => {
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error(`无法获取监听地址 address=${String(address)}`);
  }
  process.stderr.write(`${JSON.stringify({ level: "info", event: "ready", host: address.address, port: address.port })}\n`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    server.close((error) => {
      if (error !== undefined) {
        process.stderr.write(`${JSON.stringify({ level: "error", event: "shutdown_failed", message: error.message })}\n`);
        process.exitCode = 1;
      }
    });
  });
}
