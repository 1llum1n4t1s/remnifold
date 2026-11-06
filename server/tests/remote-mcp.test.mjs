import { assert } from "./support/test.mjs";
import { migrations, applyMigrations } from "./support/migrations.mjs";

import { DatabaseSync } from "./support/sqlite.mjs";
import test from "./support/test.mjs";
import worker from "../src/index.ts";
import { handleMcpRequest } from "../src/mcp.ts";
import { adapter } from "./d1-adapter.mjs";

function fixture(withOAuth = true) {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  applyMigrations(db, withOAuth);  const DB = adapter(db);
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

test("MCP summary search discovers paged source identities and preserves original getDocument", async () => {
  const f = fixture();
  try {
    for (let i = 0; i < 7; i++) assert.equal((await f.rest('/v3/documents', 'POST', { containerTag: 'memories', content: `SourceLink original evidence ${i}` })).status, 201);
    const batch = await (await f.rest('/v4/organize/prepare', 'POST', {})).json();
    const publishedResponse = await f.rest('/v4/organize/commit', 'POST', { batchId: batch.batchId,
      summaries: [{ ref: 's', title: 'SourceLink final conclusion', topic: 'SourceLink', content: 'SourceLink selected the verified approach because the original evidence supports it.', sources: batch.sources.map(s => s.id) }],
      coverage: batch.sources.map(s => ({ id: s.id, revision: s.revision, status: 'complete', summaries: ['s'] })) });
    assert.equal(publishedResponse.status, 200);
    const published = await publishedResponse.json();
    const id = published.summaries[0].memoryId;
    const search = await f.call('search_memory', { query: 'SourceLink' });
    assert.ok(search.structuredContent.results.some(row => row.id === id));
    const first = (await f.call('getDocument', { documentId: id })).structuredContent.organizationSources;
    assert.equal(first.sources.length, 5); assert.equal(first.hasMore, true);
    const continuation = (await f.call('getDocument', { documentId: id, sourceCursor: first.nextCursor })).structuredContent;
    assert.deepEqual(Object.keys(continuation), ['organizationSources']);
    const second = continuation.organizationSources;
    assert.equal(second.sources.length, 2); assert.equal(second.hasMore, false);
    const ids = [...first.sources, ...second.sources].map(row => row.id);
    assert.equal(new Set(ids).size, 7);
    for (const documentId of ids) assert.match((await f.call('getDocument', { documentId })).structuredContent.content, /original evidence/);
    const denied = await f.api(new Request('https://example.test/v4/organize/sources', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ documentId: id }) }));
    assert.equal(denied.status, 401);
  } finally { await Promise.all(f.pending); f.db.close(); }
});

test('MCP summary search follows a checkpoint through member continuation to original evidence', async () => {
  const f = fixture();
  try {
    const ids = [];
    for (let n = 0; n < 9; n++) {
      const response = await f.rest('/v3/documents', 'POST', { containerTag: 'memories', content: n < 7 ? `RemoteJourney original evidence ${n}` : n === 7 ? 'RemoteJourney historical checkpoint' : 'RemoteJourney final conclusion' });
      assert.equal(response.status, 201); ids.push((await response.json()).id);
    }
    f.db.prepare("INSERT INTO memory_consolidation_projects(project_key,source_container_tag,created_at,updated_at) VALUES ('mcp-journey','memories','now','now')").run();
    f.db.prepare("INSERT INTO memory_consolidations VALUES ('mcp-journey','mcp-journey',?,1,'active','now','now')").run(ids[7]);
    f.db.prepare("INSERT INTO memory_consolidation_sources SELECT 'mcp-journey',id,COALESCE(topic_revision,updated_at) FROM memories WHERE id IN(SELECT value FROM json_each(?))").run(JSON.stringify(ids.slice(0,7)));
    f.db.prepare("INSERT INTO memory_organization_summaries VALUES ('mcp-journey',?,'r1','active','now')").run(ids[8]);
    f.db.prepare("INSERT INTO memory_organization_versions VALUES (?,'mcp-journey')").run(ids[8]);
    f.db.prepare("INSERT INTO memory_organization_dependencies SELECT 'mcp-journey',id,COALESCE(topic_revision,updated_at) FROM memories WHERE id=?").run(ids[7]);
    const call = async (name, args) => { const result = await f.call(name, args); assert.equal(result.isError, undefined); return result.structuredContent; };
    const discovered = (await call('search_memory', { query: 'RemoteJourney final conclusion' })).results.find(row => row.id === ids[8]);
    assert.ok(discovered);
    const source = (await call('getDocument', { documentId: discovered.id })).organizationSources.sources[0];
    assert.equal(source.sourceKind, 'checkpoint'); assert.equal(source.id, ids[7]);
    const first = (await call('getDocument', { documentId: source.id })).organizationSources;
    assert.equal(first.sources.length, 5); assert.equal(first.hasMore, true);
    const continuation = await call('getDocument', { documentId: source.id, sourceCursor: first.nextCursor });
    assert.deepEqual(Object.keys(continuation), ['organizationSources']);
    const second = continuation.organizationSources;
    assert.equal(second.sources.length, 2); assert.equal(second.hasMore, false);
    const member = second.sources[0]; assert.equal(member.sourceKind, 'document');
    assert.match((await call('getDocument', { documentId: member.id })).content, /RemoteJourney original evidence/);
  } finally { await Promise.all(f.pending); f.db.close(); }
});

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
    const migration = migrations.find(m => m.name.startsWith("0008_")).sql;
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
