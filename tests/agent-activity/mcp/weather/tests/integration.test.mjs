// @ts-check

import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

/** @typedef {{ jsonrpc: "2.0", id: number, result?: Record<string, unknown>, error?: { code: number, message: string, data: object } }} RpcResponse */

class StdioMcpClient {
  /**
   * @param {import("node:child_process").ChildProcessWithoutNullStreams} processHandle
   */
  constructor(processHandle) {
    this.processHandle = processHandle;
    this.nextId = 1;
    /** @type {Map<number, { resolve: (value: RpcResponse) => void, reject: (error: Error) => void }>} */
    this.pending = new Map();
    const output = createInterface({ input: processHandle.stdout, crlfDelay: Infinity });
    output.on("line", (line) => {
      const response = /** @type {RpcResponse} */ (JSON.parse(line));
      const pending = this.pending.get(response.id);
      if (pending === undefined) {
        return;
      }
      this.pending.delete(response.id);
      pending.resolve(response);
    });
    processHandle.on("error", (error) => {
      for (const pending of this.pending.values()) {
        pending.reject(error);
      }
      this.pending.clear();
    });
  }

  /**
   * @param {string} method
   * @param {Record<string, unknown>} params
   * @returns {Promise<RpcResponse>}
   */
  request(method, params) {
    const id = this.nextId;
    this.nextId += 1;
    const response = new Promise((resolveResponse, rejectResponse) => {
      this.pending.set(id, { resolve: resolveResponse, reject: rejectResponse });
    });
    this.processHandle.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return response;
  }

  /**
   * @param {string} method
   * @param {Record<string, unknown>} params
   * @returns {void}
   */
  notify(method, params) {
    this.processHandle.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }
}

/**
 * @param {NodeJS.ReadableStream} stream
 * @param {string} eventName
 * @returns {Promise<Record<string, unknown>>}
 */
function waitForStructuredEvent(stream, eventName) {
  const lines = createInterface({ input: stream, crlfDelay: Infinity });
  return new Promise((resolveEvent, rejectEvent) => {
    lines.on("line", (line) => {
      try {
        const event = JSON.parse(line);
        if (event.event === eventName) {
          resolveEvent(event);
        }
      } catch (error) {
        rejectEvent(error instanceof Error ? error : new Error(String(error)));
      }
    });
  });
}

const currentDirectory = dirname(fileURLToPath(import.meta.url));
const activityRoot = resolve(currentDirectory, "../../..");
const servicePath = resolve(activityRoot, "services/controlled-service.mjs");
const serverPath = resolve(currentDirectory, "../server.mjs");

test("受控服务与天气 MCP 形成真实 stdio 和 HTTP 调用链", async (context) => {
  const temporaryRoot = mkdtempSync(resolve(tmpdir(), "agentreins-weather-"));
  const logPath = resolve(temporaryRoot, "requests.ndjson");
  const protocolLogPath = resolve(temporaryRoot, "mcp-protocol.ndjson");
  const protocolManifestPath = resolve(temporaryRoot, "mcp-protocol-manifest.json");
  const tokenPath = resolve(temporaryRoot, "test-token.txt");
  const testToken = "agentreins-integration-token-value";
  writeFileSync(tokenPath, testToken, "utf8");

  const service = spawn(process.execPath, [
    servicePath,
    "--host", "127.0.0.1",
    "--port", "0",
    "--log", logPath,
    "--token-file", tokenPath,
  ], { stdio: ["ignore", "pipe", "pipe"] });
  context.after(() => {
    service.kill("SIGTERM");
    rmSync(temporaryRoot, { recursive: true, force: true });
  });
  const ready = await waitForStructuredEvent(service.stderr, "ready");
  assert.equal(ready.host, "127.0.0.1");
  assert.equal(typeof ready.port, "number");
  const baseUrl = `http://127.0.0.1:${String(ready.port)}`;

  const fixed = await fetch(`${baseUrl}/fixed?run_id=R02-test`).then((response) => response.json());
  assert.deepEqual(fixed, { run_id: "R02-test", value: "AGENTREINS_FIXED_READ_V1" });

  const echoPayload = { run_id: "N01-test", marker: "AGENTREINS_ECHO_V1" };
  const echo = await fetch(`${baseUrl}/echo`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(echoPayload),
  }).then((response) => response.json());
  assert.deepEqual(echo.received, echoPayload);

  const denied = await fetch(`${baseUrl}/auth`, {
    method: "POST",
    headers: { authorization: "Bearer wrong-value" },
  });
  assert.equal(denied.status, 401);

  const allowed = await fetch(`${baseUrl}/auth`, {
    method: "POST",
    headers: { authorization: `Bearer ${testToken}` },
  });
  assert.equal(allowed.status, 200);

  const mcpProcess = spawn(process.execPath, [serverPath, "--base-url", baseUrl, "--protocol-log", protocolLogPath, "--protocol-manifest", protocolManifestPath], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  context.after(() => mcpProcess.kill("SIGTERM"));
  const client = new StdioMcpClient(mcpProcess);

  const initialize = await client.request("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "agentreins-test-client", version: "0.1.0" },
  });
  assert.equal(initialize.result?.protocolVersion, "2025-11-25");
  client.notify("notifications/initialized", {});

  const listed = await client.request("tools/list", {});
  const tools = /** @type {Array<Record<string, unknown>>} */ (listed.result?.tools);
  assert.equal(tools.length, 1);
  assert.equal(tools[0]?.name, "get_controlled_weather");

  const invalid = await client.request("tools/call", {
    name: "get_controlled_weather",
    arguments: { city: "北京" },
  });
  assert.equal(invalid.error?.code, -32602);

  const called = await client.request("tools/call", {
    name: "get_controlled_weather",
    arguments: { city: "北京", run_id: "M01-test" },
  });
  assert.equal(called.result?.isError, false);
  const structured = /** @type {Record<string, unknown>} */ (called.result?.structuredContent);
  assert.equal(structured.run_id, "M01-test");
  assert.deepEqual(structured.execution, {
    channel: "mcp",
    component: "agentreins-weather-mcp",
    tool_name: "get_controlled_weather",
  });

  const failed = await client.request("tools/call", {
    name: "get_controlled_weather",
    arguments: { city: "上海", run_id: "M01-error" },
  });
  assert.equal(failed.result?.isError, true);

  mcpProcess.stdin.end();
  await once(mcpProcess, "exit");
  const requestLog = readFileSync(logPath, "utf8");
  const protocolLog = readFileSync(protocolLogPath, "utf8");
  const protocolManifest = /** @type {{ mcp_file: string, published_bytes: number, protocol_records: number, complete: boolean }} */ (JSON.parse(readFileSync(protocolManifestPath, "utf8")));
  const protocolRecords = protocolLog.trim().split("\n").map((line) => /** @type {{ direction: string, process_id: number, raw_json: string }} */ (JSON.parse(line)));
  const protocolMessages = protocolRecords.map((record) => /** @type {Record<string, unknown>} */ (JSON.parse(record.raw_json)));
  assert.match(requestLog, new RegExp(testToken));
  assert.match(requestLog, /channel=mcp/);
  assert.match(protocolLog, /"direction":"request"/);
  assert.match(protocolLog, /"direction":"response"/);
  assert.ok(protocolMessages.some((message) => message.method === "tools/call"));
  assert.ok(protocolRecords.every((record) => Number.isInteger(record.process_id)));
  assert.equal(protocolManifest.mcp_file, protocolLogPath);
  assert.equal(protocolManifest.published_bytes, Buffer.byteLength(protocolLog, "utf8"));
  assert.equal(protocolManifest.protocol_records, protocolRecords.length);
  assert.equal(protocolManifest.complete, true);
});
