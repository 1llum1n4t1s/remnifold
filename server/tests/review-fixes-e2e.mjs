// 追加失敗: custom-ID事前確認後のcheckpoint公開をupsertが上書きし、正本・active状態・source membershipを破壊する。
// 検証する失敗: source忘却/PATCH/upsertの無効化失敗による部分commit、遅延cleanupによる復元済みfacts/topics消失、
// 状態だけのUPDATEによるFTS再索引、本文/container更新やDELETEの索引不整合、project優先/fallbackの変化、
// 初回40件backfillで全project集計を繰り返すこと、expression indexが対象照会に使われないこと。
// 実行: node --experimental-transform-types server/tests/review-fixes-e2e.mjs
// 認証済み Worker API、全 migration 適用済み SQLite、非同期完了を通す統合検証。
import assert from "node:assert/strict";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import worker from "../src/index.ts";
import { adapter } from "./d1-adapter.mjs";

const artifactPath = resolve(import.meta.dirname, "../../dist/server-review-fixes-validation.json");
const migrations = readdirSync(resolve(import.meta.dirname, "../migrations")).filter((name) => name.endsWith(".sql")).sort();
const checkpoint = { title: "Checkpoint", overview: "Verified restart state.", verified: ["Sources stored."], unverified: [], unresolved: [], nextActions: [] };
const output = (value, finish_reason = "stop") => ({ choices: [{ finish_reason, message: { content: typeof value === "string" ? value : JSON.stringify(value) } }] });
const embedding = Array.from({ length: 1024 }, (_, index) => index === 0 ? 1 : 0);

function fixture() {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  for (const migration of migrations) db.exec(readFileSync(resolve(import.meta.dirname, "../migrations", migration), "utf8"));
  const DB = adapter(db);
  const waits = [];
  const ctx = { waitUntil(promise) { waits.push(Promise.resolve(promise).then(() => ({ status: "fulfilled" }), (error) => ({ status: "rejected", error: String(error) }))); } };
  const calls = [];
  let respond = async () => output(checkpoint);
  const env = { DB, MEMORY_API_KEY: "local-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      if (input.text) return { data: [embedding] };
      if (input.response_format?.json_schema?.name === "memory_consolidation") { calls.push(input); return respond(input, calls.length); }
      return output({ facts: [{ subject: "Source", predicate: "has", object: "evidence", confidence: 1, exclusive: false }], topics: ["Validation"] });
    } },
    MEMORY_VECTORS: { upsert: async () => ({ mutationId: "local-vector" }), deleteByIds: async () => ({}), getByIds: async () => [] },
  };
  const api = async (path, body, method = "POST") => {
    const response = await worker.fetch(new Request(`https://local.example${path}`, { method,
      headers: { Authorization: "Bearer local-fixture", "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) }), env, ctx);
    return { status: response.status, body: response.status === 204 ? null : await response.json() };
  };
  return { db, DB, env, ctx, calls, api, metrics: {}, respond(callback) { respond = callback; },
    drain: async () => { const results = []; while (waits.length) results.push(...await Promise.all(waits.splice(0))); return results; },
    seed: async (count = 2) => { const ids = []; env.AI_ENRICHMENT_MODE = "off";
      for (let i = 0; i < count; i++) { const saved = await api("/v3/documents", { containerTag: "memories", customId: `source-${i}`, content: `Verified source ${i}`, metadata: { sm_project_id: "log-project" } }); assert.equal(saved.status, 201, JSON.stringify(saved.body)); ids.push(saved.body.id); }
      env.AI_ENRICHMENT_MODE = "on"; return ids; },
    consolidate: () => api("/v4/consolidate", { projectId: "log-project", force: true }),
    active: () => db.prepare("SELECT * FROM memory_consolidations WHERE status = 'active'").all(),
    lease: () => db.prepare("SELECT lease_token FROM memory_consolidation_projects").get()?.lease_token,
  };
}

const cases = [];
function scenario(name, run) { cases.push({ name, run }); }
for (const operation of ["forget", "patch", "upsert"]) scenario(`${operation} invalidation failure rolls back source and checkpoint together`, async (f) => {
  const [id] = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  const before = f.db.prepare("SELECT * FROM memories WHERE id = ?").get(id);
  f.DB.hooks.execute = (sql) => { if (/UPDATE memory_consolidations(?: AS consolidation)? SET status = 'invalid'/u.test(sql)) throw new Error("injected-invalidation-failure"); };
  const invoke = () => operation === "forget" ? f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE")
    : operation === "patch" ? f.api(`/v3/documents/${id}`, { content: "Replacement" }, "PATCH")
    : f.api("/v3/documents", { containerTag: "memories", customId: "source-0", content: "Replacement", metadata: { sm_project_id: "log-project" } });
  assert.equal((await invoke()).status, 500);
  assert.deepEqual(f.db.prepare("SELECT * FROM memories WHERE id = ?").get(id), before);
  assert.equal(f.active()[0].memory_id, first.memoryId);
  assert.equal((await f.api(`/v3/documents/${first.memoryId}`, undefined, "GET")).status, 200);
  f.DB.hooks.execute = undefined; assert.ok([200,201].includes((await invoke()).status)); await f.drain();
  assert.equal(f.active().length, 0);
  assert.equal((await f.api(`/v3/documents/${first.memoryId}`, undefined, "GET")).status, 404);
});
scenario("restored custom ID keeps new facts and topics across delayed forget cleanup", async (f) => {
  const [id] = await f.seed(); await f.drain(); f.env.AI_ENRICHMENT_MODE = "on";
  let restored = false;
  const restore = async () => {
    if (restored || f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten !== 1) return;
    restored = true;
    f.DB.hooks.all = undefined; f.DB.hooks.batch = undefined;
    const response = await f.api("/v3/documents", { containerTag: "memories", customId: "source-0", content: "Restored current facts", metadata: { sm_project_id: "log-project" }, topics: ["Restored"] });
    assert.equal(response.status, 201); assert.equal(response.body.id, id); await f.drain();
  };
  f.DB.hooks.all = async (sql) => { if (sql.includes("FROM memory_consolidations")) await restore(); };
  f.DB.hooks.batch = async (operations) => { if (operations[0]?.sql.includes("DELETE FROM facts")) await restore(); };
  assert.equal((await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE")).status, 200); await f.drain();
  assert.equal(restored, true);
  assert.equal(f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten, 0);
  assert.ok(f.db.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(id).count > 0);
  assert.ok(f.db.prepare("SELECT COUNT(*) AS count FROM memory_topics WHERE memory_id = ?").get(id).count > 0);
});
scenario("status-only mutations avoid FTS work while indexed changes retain search", async (f) => {
  const [id] = await f.seed(); await f.drain();
  const search = (query) => f.db.prepare("SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?").all(query);
  const before = search("Verified"); const changes = f.db.prepare("SELECT total_changes() AS n").get().n;
  f.db.prepare("UPDATE memories SET vector_status = 'pending' WHERE id = ?").run(id);
  f.metrics.statusOnlySqliteChanges = f.db.prepare("SELECT total_changes() AS n").get().n - changes; assert.equal(f.metrics.statusOnlySqliteChanges, 1);
  assert.deepEqual(search("Verified"), before);
  assert.equal((await f.api(`/v3/documents/${id}`, { content: "Replacement searchable content" }, "PATCH")).status, 200); await f.drain();
  assert.ok(search("Replacement").length > 0); assert.equal(search("Verified").length, 1);
  const now = new Date().toISOString(); f.db.prepare("INSERT INTO container_tags VALUES ('other', 'Other', ?, ?)").run(now, now);
  f.db.prepare("UPDATE memories SET container_tag = 'other' WHERE id = ?").run(id);
  assert.ok(search("Replacement").length > 0);
  assert.equal((await f.api(`/v3/documents/${id}`, undefined, "DELETE")).status, 204); await f.drain();
  assert.equal(search("Replacement").length, 0);
});
scenario("target scans use expression index and preserve project priority with legacy fallback", async (f) => {
  await f.seed(41); await f.drain();
  const legacy = await f.api("/v3/documents", { containerTag: "log-project", customId: "legacy", content: "Legacy container source" }); assert.equal(legacy.status, 201); await f.drain();
  const scans = []; f.DB.hooks.all = (sql, values) => { if (sql.includes("WITH raw AS")) scans.push({ sql, values }); };
  const first = await f.consolidate(); assert.equal(first.body.sourceCount, 40); await f.drain();
  assert.ok(scans.length > 0); assert.ok(scans.every(({ sql }) => /AND .*project:|AND CASE/u.test(sql.replace(/\s+/gu, " "))));
  for (const { sql, values } of scans) {
    const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...values);
    assert.ok(plan.some(({ detail }) => detail.includes("idx_memories_consolidation_project")), JSON.stringify(plan)); (f.metrics.targetPlans ??= []).push(plan.map(({ detail }) => detail));
  }
  const next = await f.consolidate(); assert.equal(next.body.sourceCount, 41); assert.equal(JSON.parse(f.calls.at(-1).messages[1].content).newMemoryRecords.length, 1); await f.drain();
  const fallback = await f.consolidate(); assert.equal(fallback.body.sourceCount, 1); await f.drain();
  const input = JSON.parse(f.calls.at(-1).messages[1].content); assert.equal(input.newMemoryRecords[0].id, legacy.body.id);
});
scenario("initial scheduled backfill discovers globally once then scans only its target", async (f) => {
  await f.seed(41); await f.drain(); const scans = [];
  f.DB.hooks.all = (sql, values) => { if (sql.includes("WITH raw AS")) scans.push({ sql, values }); };
  worker.scheduled({ cron: "0 18 * * *", scheduledTime: Date.now() }, f.env, f.ctx); await f.drain();
  assert.equal(scans.filter(({ values }) => values.length === 0).length, 1);
  assert.ok(scans.filter(({ values }) => values.length > 0).length >= 2);
  assert.deepEqual(f.calls.map((input) => JSON.parse(input.messages[1].content).newMemoryRecords.length), [40, 1]);
  assert.equal(f.db.prepare("SELECT initial_backfill_completed FROM memory_consolidation_projects WHERE project_key = 'project:log-project'").get().initial_backfill_completed, 1); f.metrics.globalScans = scans.filter(({ values }) => !values.length).length; f.metrics.targetScans = scans.filter(({ values }) => values.length).length; f.metrics.batchSourceCounts = [40, 1];
});
scenario("Unicode truncation keeps valid surrogate pairs at AI input and persisted output limits", async (f, logs) => {
  const canary = (units) => "a".repeat(units - 1) + "😀末尾";
  const [id] = await f.seed(); await f.drain(); const baseRun = f.env.AI.run; const seen = [];
  f.env.AI.run = async (model, input) => {
    if (input.text || input.response_format?.json_schema?.name === "durable_memory_enrichment") {
      const text = input.text?.[0] ?? input.messages[1].content; seen.push(text); assert.equal(text.isWellFormed(), true);
    }
    if (input.response_format?.json_schema?.name === "durable_memory_enrichment") return output({ facts: [{ subject: canary(200), predicate: canary(160), object: canary(1000), confidence: 1, exclusive: false }], topics: ["Validation"] });
    return baseRun(model, input);
  };
  assert.equal((await f.api(`/v3/documents/${id}`, { content: canary(24000) + "b".repeat(35996) + "😀tail" }, "PATCH")).status, 200); await f.drain();
  assert.ok(seen.length >= 2); f.metrics.aiInputLengths = seen.map((text) => text.length); assert.deepEqual([...f.metrics.aiInputLengths].sort((a, b) => a - b), [23999, 59999]); assert.ok(f.db.prepare("SELECT subject, predicate, object FROM facts WHERE source_memory_id = ?").all(id).every((row) => row.subject.isWellFormed() && row.predicate.isWellFormed() && row.object.isWellFormed() && row.subject.length === 199 && row.predicate.length === 159 && row.object.length === 999));
  f.respond(async () => output({ ...checkpoint, title: canary(200), overview: canary(1000), verified: [canary(1000)] }));
  const result = await f.consolidate(); assert.equal(result.status, 200); await f.drain();
  const saved = await f.api(`/v3/documents/${result.body.memoryId}`, undefined, "GET"); assert.equal(saved.status, 200);
  const stored = f.db.prepare("SELECT content, metadata_json FROM memories WHERE id = ?").get(result.body.memoryId);
  assert.equal(stored.content.isWellFormed(), true); assert.equal(stored.metadata_json.isWellFormed(), true);
  const meta = JSON.parse(stored.metadata_json); assert.equal(meta.title.length, 199); assert.equal(meta.memoryIndex.description.length, 999);
  f.DB.hooks.all = (sql) => { if (sql.includes("memories_fts")) throw new Error(canary(2000)); };
  assert.equal((await f.api("/v4/search", { q: "Replacement", containerTag: "memories", searchMode: "keyword" })).status, 200);
  const diagnostic = logs.find((entry) => entry.event === "fts_search_failed"); assert.ok(diagnostic); assert.equal(diagnostic.error.isWellFormed(), true); assert.equal(diagnostic.error.length, 1999);
  f.metrics.diagnosticLength = diagnostic.error.length;
});
for (const operation of ["patch", "forget"]) scenario(`stale ${operation} cannot invalidate a newer publication`, async (f) => {
  const [id] = await f.seed(); await f.consolidate(); await f.drain(); let latest;
  f.DB.hooks.batch = async (operations) => {
    if (!operations[0]?.sql.includes("UPDATE memories")) return;
    f.DB.hooks.batch = undefined;
    assert.equal((await f.api(`/v3/documents/${id}`, { content: "Newer revision" }, "PATCH")).status, 200); await f.drain();
    latest = (await f.consolidate()).body.memoryId; await f.drain(); assert.ok(latest);
  };
  const result = operation === "patch" ? await f.api(`/v3/documents/${id}`, { content: "Stale replacement" }, "PATCH")
    : await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE");
  assert.equal(result.status, operation === "patch" ? 409 : 200); await f.drain();
  if (operation === "forget") assert.equal(result.body.id, null);
  assert.equal(f.active()[0].memory_id, latest);
  assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = ?").get(id).content, "Newer revision");
});
scenario("checkpoint publication between upsert preflight and write preserves canonical generated state", async (f) => {
  const ids = await f.seed(); await f.drain();
  const customId = "consolidation:project:log-project:1";
  let published; let canonical; let membership; let active;
  f.DB.hooks.batch = async (operations) => {
    if (!operations.some(({ sql, values }) => sql.includes("ON CONFLICT(container_tag, custom_id)") && values.includes(customId))) return;
    f.DB.hooks.batch = undefined;
    const response = await f.consolidate(); assert.equal(response.status, 200); assert.equal(response.body.status, "consolidated");
    await f.drain(); published = response.body;
    canonical = f.db.prepare("SELECT * FROM memories WHERE id = ?").get(published.memoryId);
    assert.equal(canonical.custom_id, customId);
    membership = f.db.prepare("SELECT * FROM memory_consolidation_sources WHERE consolidation_id = ? ORDER BY memory_id").all(f.active()[0].id);
    active = f.active();
  };
  const result = await f.api("/v3/documents", {
    containerTag: "memories", customId, content: "Concurrent ordinary upsert must not replace canonical checkpoint",
    metadata: { sm_project_id: "log-project", title: "Untrusted replacement" },
  });
  assert.ok(published); assert.equal(result.status, 409); await f.drain();
  assert.deepEqual(f.db.prepare("SELECT * FROM memories WHERE id = ?").get(published.memoryId), canonical);
  assert.deepEqual(f.active(), active);
  assert.deepEqual(f.db.prepare("SELECT * FROM memory_consolidation_sources WHERE consolidation_id = ? ORDER BY memory_id").all(active[0].id), membership);
  assert.deepEqual(membership.map((row) => row.memory_id).sort(), [...ids].sort());
  const response = await f.api(`/v3/documents/${published.memoryId}`, undefined, "GET"); assert.equal(response.status, 200);
  assert.equal(response.body.content, canonical.content);
  assert.equal((await f.consolidate()).body.status, "no_unconsolidated_memories");
  f.metrics.upsertStatus = result.status; f.metrics.checkpointStatus = active[0].status; f.metrics.sourceMembershipCount = membership.length;
});
const report = { baseline: { originalCases: 6, originalFailures: 6, statusOnlySqliteChanges: 9, checkpointPublicationRaceUpsertStatus: 201 }, command: "node --experimental-transform-types server/tests/review-fixes-e2e.mjs", node: process.version, migrations, scope: "authenticated Worker API with migrated real SQLite and deterministic local AI/vector fixtures", results: [] };
for (const { name, run } of cases) {
  const f = fixture(); const original = { log: console.log, error: console.error, warn: console.warn }; const logs = [];
  console.log = console.warn = console.error = (...values) => { for (const value of values) { try { logs.push(JSON.parse(value)); } catch {} } };
  try { await run(f, logs); report.results.push({ name, status: "passed", measurements: f.metrics }); }
  catch (error) { report.results.push({ name, status: "failed", error: String(error), stack: error.stack, events: logs }); }
  finally { await f.drain(); Object.assign(console, original); f.db.close(); }
}
report.passed = report.results.filter((result) => result.status === "passed").length; report.failed = report.results.length - report.passed;
mkdirSync(resolve(artifactPath, ".."), { recursive: true }); writeFileSync(artifactPath, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify({ artifact: artifactPath, passed: report.passed, failed: report.failed }));
if (report.failed) process.exitCode = 1;
