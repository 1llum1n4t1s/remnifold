import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import worker from "../src/index.ts";
import { handleMcpRequest } from "../src/mcp.ts";

function fixture(withOAuth = true) {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  const migrations = resolve(import.meta.dirname, "../migrations");
  for (const name of readdirSync(migrations).filter((name) => name.endsWith(".sql") && (withOAuth || !name.startsWith("0008_"))).sort()) db.exec(readFileSync(resolve(migrations, name), "utf8"));
  const prepare = (sql) => {
    const operation = (values = []) => ({ bind: (...next) => operation(next), first: async () => db.prepare(sql).get(...values), all: async () => ({ results: db.prepare(sql).all(...values) }), run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }), execute: () => /^\s*(?:SELECT|WITH|PRAGMA)\b/iu.test(sql) ? { results: db.prepare(sql).all(...values), meta: { changes: 0 } } : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } } });
    return operation();
  };
  const DB = { prepare, batch: async (operations) => { db.exec("BEGIN"); try { const results = operations.map((op) => op.execute()); db.exec("COMMIT"); return results; } catch (error) { db.exec("ROLLBACK"); throw error; } } };
  const pending = [];
  const ctx = { waitUntil: (promise) => pending.push(Promise.resolve(promise)) };
  const env = { DB, MEMORY_API_KEY: "test-configured-secret", AI_ENRICHMENT_MODE: "off", MEMORY_VECTORS: { deleteByIds: async () => ({}) } };
  const api = (req) => worker.fetch(req, env, ctx);
  const opts = { api, version: "test", apiKey: env.MEMORY_API_KEY };
  const headers = { "Content-Type": "application/json", Accept: "application/json, text/event-stream" };
  const request = (message, extra = {}) => api(new Request("https://example.test/mcp", { method: "POST", headers: { ...headers, Authorization: `Bearer ${env.MEMORY_API_KEY}`, ...extra }, body: JSON.stringify(message) }));
  const rpc = (method, params = {}) => request({ jsonrpc: "2.0", id: 1, method, params });
  const call = async (name, args = {}) => (await (await rpc("tools/call", { name, arguments: args })).json()).result;
  const rest = async (path, method, body) => api(new Request(`https://example.test${path}`, { method, headers: { Authorization: `Bearer ${env.MEMORY_API_KEY}`, "Content-Type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}) }));
  return { db, pending, request, rpc, call, rest, opts, headers, api, env };
}

test("stateless MCP accesses existing Worker memories and saves redacted retry-safe data", async () => {
  const f = fixture();
  try {
    const legacy = await (await f.rest("/v3/documents", "POST", { containerTag: "legacy-project", content: "Orion legacy launch decision", customId: "old-record", metadata: { sm_project_id: "old-project", sm_source: "codex" } })).json();
    const search = await f.call("search_memory", { query: "Orion" });
    assert.equal(search.isError, undefined);
    assert.equal(search.structuredContent.results[0].id, legacy.id);
    assert.equal(search.structuredContent.results[0].content, undefined);
    const document = await f.call("getDocument", { documentId: legacy.id });
    assert.equal(document.structuredContent.id, legacy.id);
    assert.equal(document.structuredContent.metadata.sm_project_id, "old-project");
    const args = { content: 'Orion saved test-configured-secret password="hide this" ghp_12345678901234567890', projectId: "project-42", idempotencyKey: "one-attempt" };
    const saved = await f.call("add_memory", args);
    assert.equal(saved.isError, undefined);
    const retried = await f.call("add_memory", args);
    assert.equal(saved.structuredContent.id, retried.structuredContent.id);
    const anotherProject = await f.call("add_memory", { ...args, projectId: "different-project" });
    assert.notEqual(anotherProject.structuredContent.id, saved.structuredContent.id);
    const revisedContent = await f.call("add_memory", { ...args, content: "A revised Orion decision" });
    assert.notEqual(revisedContent.structuredContent.id, saved.structuredContent.id);
    const stored = (await f.call("getDocument", { documentId: saved.structuredContent.id })).structuredContent;
    assert.match(stored.content, /Orion saved \[REDACTED\] password="\[REDACTED\]" \[REDACTED\]/);
    assert.equal(stored.metadata.sm_project_id, "project-42");
    assert.equal(stored.metadata.sm_source, "remote-mcp");
    const forgotten = await f.call("add_memory", { action: "forget", containerTag: "legacy-project", documentId: legacy.id });
    assert.equal(forgotten.isError, undefined, JSON.stringify(forgotten));
    assert.equal((await f.call("getDocument", { documentId: legacy.id })).isError, true);
    assert.equal((await f.call("add_memory", { content: "test", documentId: "invalid" })).isError, true);
    assert.equal((await f.call("search_memory", { query: "test", unexpected: true })).isError, true);
    assert.equal((await f.call("search_memory", { query: "test", constructor: "unexpected" })).isError, true);
    const unicode = "漢".repeat(200000);
    const unicodeSaved = await f.call("add_memory", { content: unicode });
    assert.equal(unicodeSaved.isError, undefined);
    assert.equal((await f.call("getDocument", { documentId: unicodeSaved.structuredContent.id })).structuredContent.content, unicode);
    assert.equal((await f.call("add_memory", { content: "test", projectId: "test-configured-secret" })).isError, true);
    const failedOptions = { ...f.opts, api: async () => new Response("private error details", { status: 500 }) };
    const failed = await handleMcpRequest(new Request("https://example.test/mcp", { method: "POST", headers: f.headers, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "add_memory", arguments: { content: "test" } } }) }), failedOptions);
    const failedResult = (await failed.json()).result;
    assert.equal(failedResult.isError, true);
    assert.doesNotMatch(JSON.stringify(failedResult), /private error details/);
    await Promise.all(f.pending);
  } finally { f.db.close(); }
});

test("additive OAuth migration is repeatable and preserves existing memories and old clients", async () => {
  const f = fixture(false);
  try {
    const old = await (await f.rest("/v3/documents", "POST", { containerTag: "codex-old-space", customId: "original-capture-id", content: "Existing migration decision", metadata: { sm_project_id: "old-project", sm_client: "codex", captureKey: "legacy-key" } })).json();
    const before = await (await f.call("getDocument", { documentId: old.id })).structuredContent;
    assert.equal((await f.rest("/v3/session", "GET")).status, 200);
    const registration = () => f.api(new Request("https://example.test/oauth/register", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ redirect_uris: ["https://client.example/callback"] }) }));
    assert.equal((await registration()).status, 503);
    const migration = readFileSync(new URL("../migrations/0008_remote_mcp_oauth.sql", import.meta.url), "utf8");
    f.db.exec(migration);
    f.db.exec(migration);
    assert.equal((await registration()).status, 201);
    const after = (await f.call("getDocument", { documentId: old.id })).structuredContent;
    assert.deepEqual(after, before);
    assert.equal(after.id, old.id);
    assert.equal((await f.rest("/v3/session", "GET")).status, 200);
    await Promise.all(f.pending);
  } finally { f.db.close(); }
});

test("Worker MCP authentication, origin checks and browser discovery preserve the legacy API", async () => {
  const f = fixture();
  try {
    const unauthorized = await f.request({ jsonrpc: "2.0", id: 1, method: "tools/list" }, { Authorization: "Bearer invalid" });
    assert.equal(unauthorized.status, 401);
    assert.match(unauthorized.headers.get("www-authenticate"), /oauth-protected-resource\/mcp/);
    assert.equal((await f.request({ jsonrpc: "2.0", id: 1, method: "ping" }, { Origin: "https://attacker.example" })).status, 403);
    const allowed = await f.request({ jsonrpc: "2.0", id: 1, method: "ping" }, { Origin: "https://example.test" });
    assert.equal(allowed.status, 200);
    assert.equal(allowed.headers.get("access-control-allow-origin"), "https://example.test");
    assert.equal((await f.api(new Request("https://example.test/mcp", { method: "OPTIONS", headers: { Origin: "https://example.test" } }))).status, 204);
    const discovery = await (await f.api(new Request("https://example.test/.well-known/oauth-protected-resource/mcp"))).json();
    assert.equal(discovery.resource, "https://example.test/mcp");
    assert.equal((await f.rest("/v3/session", "GET")).status, 200);
  } finally { f.db.close(); }
});

test("MCP transport negotiates and rejects malformed requests", async () => {
  const f = fixture();
  try {
    const initialized = await (await f.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "client", version: "1" } })).json();
    assert.equal(initialized.result.protocolVersion, "2025-06-18");
    assert.match(initialized.result.instructions, /Never save secrets/);
    assert.equal((await f.request({ jsonrpc: "2.0", method: "notifications/initialized" })).status, 202);
    assert.equal((await f.rpc("tools/list")).status, 200);
    assert.equal((await f.request({ jsonrpc: "2.0", id: 1, method: "ping" }, { "MCP-Protocol-Version": "bad" })).status, 400);
    assert.equal((await f.request({ jsonrpc: "2.0", id: 1, method: "ping" }, { Accept: "application/json" })).status, 406);
    assert.equal((await f.request({ jsonrpc: "2.0", id: 1, method: "ping", params: [] })).status, 400);
    assert.equal((await handleMcpRequest(new Request("https://example.test/mcp", { method: "GET" }), f.opts)).status, 405);
    assert.equal((await f.request({ jsonrpc: "2.0", id: 1, method: "ping", params: { data: "x".repeat(2100000) } })).status, 413);
    const unauthorized = await worker.fetch(new Request("https://example.test/v3/session"), { MEMORY_API_KEY: "test-configured-secret" }, { waitUntil() {} });
    assert.equal(unauthorized.status, 401);
  } finally { f.db.close(); }
});
