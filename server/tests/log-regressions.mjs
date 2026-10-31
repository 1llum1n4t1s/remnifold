// 実行: node --experimental-transform-types server/tests/log-regressions.mjs
// 認証済み Worker API、全 migration 適用済み SQLite、非同期完了を通す統合検証。
import assert from "node:assert/strict";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import worker from "../src/index.ts";
import { adapter } from "./d1-adapter.mjs";

const artifactPath = resolve(import.meta.dirname, "../../dist/server-log-validation.json");
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
      headers: { Authorization: "Bearer local-fixture", "Content-Type": "application/json" }, body: JSON.stringify(body) }), env, ctx);
    return { status: response.status, body: response.status === 204 ? null : await response.json() };
  };
  return { db, DB, env, ctx, calls, api, respond(callback) { respond = callback; },
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
scenario("truncated output retries once with compact schema and identical source input", async (f) => {
  const ids = await f.seed(41);
  f.respond(async (_input, call) => call === 1 ? output(checkpoint, "length") : output(checkpoint));
  const result = await f.consolidate(); assert.equal(result.body.status, "consolidated"); assert.equal(result.body.sourceCount, 40);
  assert.equal(f.calls.length, 2); assert.equal(f.calls[0].messages[1].content, f.calls[1].messages[1].content);
  const schema = f.calls[1].response_format.json_schema.schema.properties;
  assert.equal(schema.title.maxLength, 80); assert.equal(schema.overview.maxLength, 400);
  for (const key of ["verified", "unverified", "unresolved", "nextActions"]) { assert.equal(schema[key].maxItems, 3); assert.equal(schema[key].items.maxLength, 120); }
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 40);
  assert.equal(ids.length, 41); assert.equal(f.lease(), null); await f.drain();
});
scenario("valid first output publishes without retry", async (f) => {
  await f.seed(); assert.equal((await f.consolidate()).body.status, "consolidated"); assert.equal(f.calls.length, 1); await f.drain();
});
scenario("two invalid outputs leave sources pending and release lease", async (f) => {
  const ids = await f.seed(); f.respond(async () => output("{"));
  const result = await f.consolidate(); assert.equal(result.status, 500); assert.equal(f.calls.length, 2);
  assert.equal(f.active().length, 0); assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 0);
  assert.equal(f.lease(), null); assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memories WHERE is_forgotten = 0").get().count, ids.length);
  f.respond(async () => output(checkpoint)); assert.equal((await f.consolidate()).body.sourceCount, ids.length); await f.drain();
});
scenario("source re-enrichment invalidates checkpoint and next request recreates it", async (f) => {
  const ids = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  assert.equal((await f.api("/v4/enrich", { id: ids[0] })).status, 202); await f.drain();
  assert.equal(f.active().length, 0); assert.equal(f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(first.memoryId).is_forgotten, 1);
  const replacement = (await f.consolidate()).body; assert.equal(replacement.status, "consolidated"); assert.notEqual(replacement.memoryId, first.memoryId);
  assert.equal(JSON.parse(f.calls.at(-1).messages[1].content).previousCheckpoint, null); await f.drain();
});
scenario("checkpoint retry keeps facts disabled and checkpoint active", async (f) => {
  await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  assert.equal((await f.api("/v4/enrich", { id: first.memoryId })).status, 202); await f.drain();
  assert.equal(f.active().length, 1); assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(first.memoryId).count, 0);
  assert.equal(f.db.prepare("SELECT fact_status FROM memories WHERE id = ?").get(first.memoryId).fact_status, "disabled");
});
scenario("checkpoint PATCH rejects edits without changing its active persisted state", async (f) => {
  await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  const before = f.db.prepare("SELECT * FROM memories WHERE id = ?").get(first.memoryId);
  for (const body of [{ content: "Edited checkpoint" }, { metadata: {} }]) {
    const result = await f.api(`/v3/documents/${first.memoryId}`, body, "PATCH");
    assert.equal(result.status, 409, JSON.stringify(result.body));
    assert.deepEqual(f.db.prepare("SELECT * FROM memories WHERE id = ?").get(first.memoryId), before);
    assert.equal(f.active()[0].memory_id, first.memoryId);
  }
});
scenario("checkpoint custom-ID upsert rejects edits while ordinary source upserts stay supported", async (f) => {
  const [sourceId] = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  const before = f.db.prepare("SELECT * FROM memories WHERE id = ?").get(first.memoryId);
  const result = await f.api("/v3/documents", { containerTag: "memories", customId: before.custom_id, content: "Edited checkpoint", metadata: {} });
  assert.equal(result.status, 409, JSON.stringify(result.body));
  assert.deepEqual(f.db.prepare("SELECT * FROM memories WHERE id = ?").get(first.memoryId), before);
  assert.equal(f.active()[0].memory_id, first.memoryId);
  const source = await f.api("/v3/documents", { containerTag: "memories", customId: "source-0", content: "Updated source", metadata: { sm_project_id: "log-project" } });
  assert.equal(source.status, 201); assert.equal(source.body.id, sourceId); await f.drain();
  assert.equal(f.active().length, 0);
});
for (const deleteCheckpoint of [false, true]) scenario(`hard delete ${deleteCheckpoint ? "checkpoint" : "legacy source"} restores facts in checkpoint container`, async (f) => {
  const [survivingSource] = await f.seed(); f.env.AI_ENRICHMENT_MODE = "off";
  const legacy = await f.api("/v3/documents", { containerTag: "legacy-space", customId: "legacy-source", content: "Legacy evidence", metadata: { sm_project_id: "log-project" } });
  assert.equal(legacy.status, 201); f.env.AI_ENRICHMENT_MODE = "on";
  const first = (await f.consolidate()).body; await f.drain();
  const now = new Date().toISOString();
  const insertFact = f.db.prepare(`INSERT INTO facts (
    id, container_tag, source_memory_id, subject, predicate, object,
    subject_key, predicate_key, object_key, is_exclusive, confidence, status, created_at, updated_at
  ) VALUES (?, 'memories', ?, 'Source', 'has', ?, 'source', 'has', ?, 1, 1, ?, ?, ?)`);
  insertFact.run("surviving-source-fact", survivingSource, "original evidence", "original evidence", "superseded", now, now);
  insertFact.run("legacy-checkpoint-fact", first.memoryId, "synthetic checkpoint claim", "synthetic checkpoint claim", "active", now, now);
  f.db.prepare(`INSERT INTO fact_relations (id, container_tag, from_fact_id, relation, to_fact_id, source_memory_id, confidence, created_at)
    VALUES ('legacy-supersession', 'memories', 'legacy-checkpoint-fact', 'supersedes', 'surviving-source-fact', ?, 1, ?)`)
    .run(first.memoryId, now);
  const before = await f.api("/v4/profile", { containerTag: "memories" });
  assert.deepEqual(before.body.profile.dynamic, ["Source has synthetic checkpoint claim"]);
  const deletedId = deleteCheckpoint ? first.memoryId : legacy.body.id;
  assert.equal((await f.api(`/v3/documents/${deletedId}`, undefined, "DELETE")).status, 204); await f.drain();
  assert.equal(f.active().length, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(first.memoryId).count, 0);
  assert.equal(f.db.prepare("SELECT status FROM facts WHERE id = 'surviving-source-fact'").get().status, "active");
  const after = await f.api("/v4/profile", { containerTag: "memories" });
  assert.deepEqual(after.body.profile.dynamic, ["Source has original evidence"]);
});
scenario("legacy polluted checkpoint retry removes synthetic facts and restores superseded source profile", async (f) => {
  const [sourceId] = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  const now = new Date().toISOString();
  const insertFact = f.db.prepare(`INSERT INTO facts (
    id, container_tag, source_memory_id, subject, predicate, object,
    subject_key, predicate_key, object_key, is_exclusive, confidence, status, created_at, updated_at
  ) VALUES (?, 'memories', ?, 'Source', 'has', ?, 'source', 'has', ?, 1, 1, ?, ?, ?)`);
  // 旧版checkpointのfactが元資料をsupersedeした永続状態を再現する。
  insertFact.run("original-source-fact", sourceId, "original evidence", "original evidence", "superseded", now, now);
  insertFact.run("polluted-checkpoint-fact", first.memoryId, "synthetic checkpoint claim", "synthetic checkpoint claim", "active", now, now);
  f.db.prepare(`INSERT INTO fact_relations (
    id, container_tag, from_fact_id, relation, to_fact_id, source_memory_id, confidence, created_at
  ) VALUES ('polluted-supersession', 'memories', 'polluted-checkpoint-fact', 'supersedes', 'original-source-fact', ?, 1, ?)`)
    .run(first.memoryId, now);
  f.db.prepare("UPDATE memories SET fact_status = 'done' WHERE id = ?").run(first.memoryId);
  const before = await f.api("/v4/profile", { containerTag: "memories" });
  assert.equal(before.status, 200); assert.deepEqual(before.body.profile.dynamic, ["Source has synthetic checkpoint claim"]);
  assert.equal((await f.api("/v4/enrich", { id: first.memoryId })).status, 202); await f.drain();
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(first.memoryId).count, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM fact_relations WHERE id = 'polluted-supersession'").get().count, 0);
  assert.equal(f.db.prepare("SELECT status FROM facts WHERE id = 'original-source-fact'").get().status, "active");
  const after = await f.api("/v4/profile", { containerTag: "memories" });
  assert.equal(after.status, 200); assert.deepEqual(after.body.profile.dynamic, ["Source has original evidence"]);
  assert.equal(f.active().length, 1); assert.equal(f.active()[0].memory_id, first.memoryId);
  assert.equal(f.db.prepare("SELECT fact_status FROM memories WHERE id = ?").get(first.memoryId).fact_status, "disabled");
});
scenario("re-enrichment invalidation database failure atomically rolls back source revision", async (f) => {
  const [id] = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  const revision = f.db.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
  f.DB.hooks.execute = (sql) => { if (sql.includes("UPDATE memory_consolidations AS consolidation SET status = 'invalid'")) throw new Error("injected-atomic-invalidation-failure"); };
  assert.equal((await f.api("/v4/enrich", { id })).status, 500);
  assert.equal(f.db.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision, revision);
  assert.equal(f.active().length, 1); assert.equal(f.active()[0].memory_id, first.memoryId);
  assert.equal(f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(first.memoryId).is_forgotten, 0);
  f.DB.hooks.execute = undefined; assert.equal((await f.api("/v4/enrich", { id })).status, 202); await f.drain(); assert.equal(f.active().length, 0);
});
scenario("stale legacy active checkpoint recovers without feeding old summary to AI", async (f) => {
  const ids = await f.seed(); const first = (await f.consolidate()).body; await f.drain();
  f.db.prepare("UPDATE memories SET topic_revision = 'legacy-broken-revision' WHERE id = ?").run(ids[0]);
  const recovered = (await f.consolidate()).body; assert.equal(recovered.status, "consolidated");
  assert.equal(f.db.prepare("SELECT status FROM memory_consolidations WHERE memory_id = ?").get(first.memoryId).status, "invalid");
  assert.equal(JSON.parse(f.calls.at(-1).messages[1].content).previousCheckpoint, null); assert.equal(f.active().length, 1); await f.drain();
});
scenario("late source mutation prevents publication and releases lease", async (f) => {
  const ids = await f.seed(); f.respond(async () => {
    f.env.AI_ENRICHMENT_MODE = "off";
    assert.equal((await f.api(`/v3/documents/${ids[0]}`, { content: "Changed during generation" }, "PATCH")).status, 200);
    f.env.AI_ENRICHMENT_MODE = "on"; return output(checkpoint);
  });
  const result = await f.consolidate(); assert.notEqual(result.body.status, "consolidated"); assert.equal(f.active().length, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 0); assert.equal(f.lease(), null); await f.drain();
});
scenario("source forgotten during generation prevents publication and consumption", async (f) => {
  const ids = await f.seed(); f.respond(async () => {
    assert.equal((await f.api("/v4/memories", { containerTag: "memories", documentId: ids[0] }, "DELETE")).status, 200); return output(checkpoint);
  });
  assert.equal((await f.consolidate()).status, 409); assert.equal(f.active().length, 0); assert.equal(f.lease(), null);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 0); await f.drain();
});
scenario("project lease prevents overlapping generation", async (f) => {
  await f.seed(); let started; let release;
  const begun = new Promise((done) => { started = done; }); const blocked = new Promise((done) => { release = done; });
  f.respond(async () => { started(); await blocked; return output(checkpoint); });
  const pending = f.consolidate(); await begun;
  try { assert.equal((await f.consolidate()).body.status, "busy"); assert.equal(f.calls.length, 1); }
  finally { release(); }
  assert.equal((await pending).body.status, "consolidated"); assert.equal(f.lease(), null); await f.drain();
});
scenario("outer enrichment database failure logs revision and rejects background promise", async (f, logs) => {
  const [id] = await f.seed();
  f.env.AI.run = async (_model, input) => {
    if (input.text) return { data: [embedding] };
    throw new Error("injected-classification-failure");
  };
  f.DB.hooks.run = (sql) => { if (sql.includes("UPDATE memories SET enrichment_error")) throw new Error(`injected-enrichment-db-failure ${f.env.MEMORY_API_KEY}`); };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202);
  const results = await f.drain(); assert.ok(results.some((r) => r.status === "rejected" && r.error.includes("injected-enrichment-db-failure")));
  const log = logs.find((entry) => entry.event === "memory_enrichment_failed"); assert.equal(log?.memoryId, id); assert.ok(log?.revision); assert.match(log.error, /injected-enrichment-db-failure/u);
  const row = f.db.prepare("SELECT embedding_status, vector_status, fact_status, topic_status, enrichment_error FROM memories WHERE id = ?").get(id);
  assert.equal(row.embedding_status, "done"); assert.equal(row.vector_status, "queued"); assert.equal(row.fact_status, "failed"); assert.equal(row.topic_status, "failed");
  assert.match(row.enrichment_error, /injected-enrichment-db-failure/u); assert.ok(!row.enrichment_error.includes(f.env.MEMORY_API_KEY));
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY));
});
scenario("initial processing write failure marks all pending stages failed", async (f, logs) => {
  const [id] = await f.seed();
  f.DB.hooks.run = (sql) => { if (sql.includes("SET embedding_status = 'processing'")) throw new Error("injected-initial-processing-failure"); };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202);
  const results = await f.drain(); assert.ok(results.some((result) => result.status === "rejected" && result.error.includes("injected-initial-processing-failure")));
  const row = f.db.prepare("SELECT embedding_status, vector_status, fact_status, topic_status, enrichment_error FROM memories WHERE id = ?").get(id);
  for (const key of ["embedding_status", "vector_status", "fact_status", "topic_status"]) assert.equal(row[key], "failed");
  assert.match(row.enrichment_error, /injected-initial-processing-failure/u); assert.ok(logs.some((entry) => entry.event === "memory_enrichment_failed"));
});
scenario("failure status persistence error logs secondary context and preserves original rejection", async (f, logs) => {
  const [id] = await f.seed();
  f.DB.hooks.run = (sql) => {
    if (sql.includes("SET embedding_status = 'processing'")) throw new Error("injected-original-processing-failure");
    if (sql.includes("embedding_status IN ('pending', 'processing')")) throw new Error(`injected-secondary-status-failure ${f.env.MEMORY_API_KEY}`);
  };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202);
  const results = await f.drain(); assert.equal(results.filter((result) => result.status === "rejected").length, 1);
  assert.match(results.find((result) => result.status === "rejected").error, /injected-original-processing-failure/u);
  const original = logs.find((entry) => entry.event === "memory_enrichment_failed");
  const secondary = logs.find((entry) => entry.event === "memory_enrichment_status_failed");
  assert.equal(secondary?.memoryId, id); assert.equal(secondary?.revision, original?.revision); assert.match(secondary.error, /injected-secondary-status-failure/u);
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY));
});
scenario("late revision replacement rejects stale failure status update", async (f, logs) => {
  const [id] = await f.seed(); let replacement;
  f.DB.hooks.run = (sql) => {
    if (!sql.includes("SET embedding_status = 'processing'")) return;
    // 他の要求が新しい版を受理した直後に旧処理が失敗する順序を再現する。
    f.db.prepare("UPDATE memories SET topic_revision = 'replacement-revision', updated_at = '2099-01-01T00:00:00.000Z', enrichment_error = 'new revision state' WHERE id = ?").run(id);
    replacement = f.db.prepare("SELECT * FROM memories WHERE id = ?").get(id);
    throw new Error("injected-stale-processing-failure");
  };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202);
  const results = await f.drain(); assert.ok(results.some((result) => result.status === "rejected" && result.error.includes("injected-stale-processing-failure")));
  assert.deepEqual(f.db.prepare("SELECT * FROM memories WHERE id = ?").get(id), replacement);
  assert.ok(logs.some((entry) => entry.event === "memory_enrichment_failed" && entry.revision !== "replacement-revision"));
});
scenario("failed AI enrichment records errors in completion log", async (f, logs) => {
  const [id] = await f.seed(); f.env.AI.run = async () => { throw new Error(`injected-ai-failure ${f.env.MEMORY_API_KEY} ${"x".repeat(2100)}`); };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202); await f.drain();
  const log = logs.find((entry) => entry.event === "memory_enriched" && entry.memoryId === id); assert.equal(log?.ok, false); assert.ok(Array.isArray(log.errors)); assert.ok(log.errors.some((error) => error.includes("injected-ai-failure")));
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY));
  const stored = f.db.prepare("SELECT enrichment_error FROM memories WHERE id = ?").get(id).enrichment_error;
  assert.match(stored, /injected-ai-failure/u); assert.match(stored, /\[redacted\]/u); assert.ok(stored.length <= 2_000);
  assert.ok(!stored.includes(f.env.MEMORY_API_KEY));
  const readErrors = async () => {
    const document = await f.api(`/v3/documents/${id}`, undefined, "GET");
    const list = await f.api("/v3/documents/list", { projection: "full" });
    assert.equal(document.status, 200); assert.equal(list.status, 200);
    return [document.body.enrichment.error, list.body.documents.find((item) => item.id === id).enrichment.error];
  };
  assert.deepEqual(await readErrors(), [stored, stored]);
  const legacy = `legacy diagnostic ${f.env.MEMORY_API_KEY} ${"y".repeat(2100)}`;
  f.db.prepare("UPDATE memories SET enrichment_error = ? WHERE id = ?").run(legacy, id);
  for (const value of await readErrors()) {
    assert.match(value, /legacy diagnostic \[redacted\]/u); assert.ok(value.length <= 2_000); assert.ok(!value.includes(f.env.MEMORY_API_KEY));
  }
  assert.equal(f.db.prepare("SELECT enrichment_error FROM memories WHERE id = ?").get(id).enrichment_error, legacy);
});
scenario("derived data persistence failures report failed stages and errors", async (f, logs) => {
  const [id] = await f.seed();
  f.DB.hooks.batch = (operations) => {
    if (operations.some((operation) => /INSERT INTO (facts|memory_topics)/u.test(operation.sql))) throw new Error("injected-derived-write-failure");
  };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202); await f.drain();
  const log = logs.find((entry) => entry.event === "memory_enriched" && entry.memoryId === id);
  assert.equal(log?.ok, false); assert.equal(log.facts, "failed"); assert.equal(log.topics, "failed"); assert.ok(log.errors.some((error) => error.includes("injected-derived-write-failure")));
  const row = f.db.prepare("SELECT fact_status, topic_status FROM memories WHERE id = ?").get(id); assert.equal(row.fact_status, "failed"); assert.equal(row.topic_status, "failed");
});
for (const recurring of [false, true]) scenario(`${recurring ? "recurring" : "initial"} scheduled consolidation redacts and bounds provider errors`, async (f, logs) => {
  await f.seed(20);
  if (recurring) {
    assert.equal((await f.consolidate()).body.status, "consolidated"); await f.drain();
    f.env.AI_ENRICHMENT_MODE = "off";
    assert.equal((await f.api("/v3/documents", { containerTag: "memories", customId: "next-source", content: "Next evidence", metadata: { sm_project_id: "log-project" } })).status, 201);
    f.env.AI_ENRICHMENT_MODE = "on";
    f.db.prepare("UPDATE memory_consolidation_projects SET initial_backfill_completed = 1, last_success_at = '2020-01-01T00:00:00.000Z'").run();
  }
  f.respond(async () => { throw new Error(`injected-consolidation-failure ${f.env.MEMORY_API_KEY} ${"x".repeat(2_500)}`); });
  worker.scheduled({ cron: "0 18 * * *", scheduledTime: Date.now() }, f.env, f.ctx);
  await f.drain();
  const log = logs.find((entry) => entry.event === (recurring ? "memory_consolidation_failed" : "memory_initial_backfill_failed"));
  assert.match(log?.error, /injected-consolidation-failure/u);
  assert.ok(log.error.includes("[redacted]")); assert.equal(log.error.length, 2_000);
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY)); assert.equal(f.lease(), null);
});
for (const cron of ["0 18 * * *", "*/15 * * * *"]) scenario(`cron ${cron} outer database failure logs and rejects`, async (f, logs) => {
  f.env.AI_ENRICHMENT_MODE = "on";
  f.DB.hooks.all = () => { throw new Error(`injected-cron-db-failure ${f.env.MEMORY_API_KEY}`); };
  worker.scheduled({ cron, scheduledTime: Date.now() }, f.env, f.ctx);
  const results = await f.drain(); assert.ok(results.some((r) => r.status === "rejected" && r.error.includes("injected-cron-db-failure")));
  const log = logs.find((entry) => entry.event === "scheduled_failed"); assert.equal(log?.cron, cron); assert.match(log.error, /injected-cron-db-failure/u);
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY));
});

// DB/AI/Vectorizeの例外が縮退・削除・再試行の各経路へ届く場合を先に列挙して検証する。
function providerFailure(f) { return new Error(`injected-provider-failure ${f.env.MEMORY_API_KEY} ${"x".repeat(2_500)}`); }
function boundedDiagnostics(f, logs, event, count = 1) {
  const matches = logs.filter((entry) => entry.event === event);
  assert.equal(matches.length, count, event);
  for (const entry of matches) { assert.match(entry.error, /injected-provider-failure/u); assert.ok(entry.error.includes("[redacted]")); assert.equal(entry.error.length, 2_000); }
  assert.ok(!JSON.stringify(logs).includes(f.env.MEMORY_API_KEY));
}
for (const failure of ["fallbacks", "hydration", "embedding"]) scenario(`search ${failure} failures preserve lexical results and safe diagnostics`, async (f, logs) => {
  const [id] = await f.seed();
  f.db.prepare("UPDATE memories SET embedding_json = ?, vector_status = 'indexed' WHERE id = ?").run(JSON.stringify(embedding), id);
  f.DB.hooks.all = (sql) => {
    if (sql.includes("memories_fts MATCH")) throw providerFailure(f);
    if (failure === "fallbacks" && sql.includes("embedding_json IS NOT NULL")) throw providerFailure(f);
    if (failure === "hydration" && sql.includes("AND id IN (")) throw providerFailure(f);
  };
  f.env.MEMORY_VECTORS.query = async () => {
    if (failure === "fallbacks") throw providerFailure(f);
    return { matches: [{ id, score: 1 }], count: 1 };
  };
  if (failure === "embedding") f.env.AI.run = async () => { throw providerFailure(f); };
  const result = await f.api("/v3/search", { q: "Verified", containerTag: "memories" });
  assert.equal(result.status, 200); assert.ok(result.body.results.some((row) => row.id === id));
  boundedDiagnostics(f, logs, "fts_search_failed");
  if (failure === "fallbacks") { boundedDiagnostics(f, logs, "vector_query_failed"); boundedDiagnostics(f, logs, "semantic_fallback_failed", 2); }
  if (failure === "hydration") boundedDiagnostics(f, logs, "vector_hit_hydration_failed");
  if (failure === "embedding") boundedDiagnostics(f, logs, "semantic_search_failed");
});
for (const hardDelete of [false, true]) scenario(`${hardDelete ? "hard delete" : "forget"} accepts deletion despite safe vector failure log`, async (f, logs) => {
  const [id] = await f.seed(); f.env.MEMORY_VECTORS.deleteByIds = async () => { throw providerFailure(f); };
  const result = hardDelete ? await f.api(`/v3/documents/${id}`, undefined, "DELETE")
    : await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE");
  assert.equal(result.status, hardDelete ? 204 : 200); await f.drain();
  const row = f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id);
  assert.equal(hardDelete ? row : row.is_forgotten, hardDelete ? undefined : 1);
  boundedDiagnostics(f, logs, "vector_delete_failed");
});
for (const failure of ["lookup", "delete", "upsert"]) scenario(`scheduled vector ${failure} failure leaves retry state and safe diagnostics`, async (f, logs) => {
  const [id] = await f.seed();
  if (failure === "upsert") {
    f.db.prepare("UPDATE memories SET embedding_json = ?, vector_status = 'pending' WHERE id = ?").run(JSON.stringify(embedding), id);
    f.env.MEMORY_VECTORS.upsert = async () => { throw providerFailure(f); };
  } else {
    assert.equal((await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE")).status, 200); await f.drain();
    f.env.AI_ENRICHMENT_MODE = "off";
    f.env.MEMORY_VECTORS.getByIds = async () => { if (failure === "lookup") throw providerFailure(f); return [{ id }]; };
    if (failure === "delete") f.env.MEMORY_VECTORS.deleteByIds = async () => { throw providerFailure(f); };
  }
  worker.scheduled({ cron: "*/15 * * * *", scheduledTime: Date.now() }, f.env, f.ctx); await f.drain();
  const row = f.db.prepare("SELECT vector_status, enrichment_error FROM memories WHERE id = ?").get(id);
  assert.equal(row.vector_status, failure === "upsert" ? "failed" : "pending");
  boundedDiagnostics(f, logs, failure === "upsert" ? "vector_reconcile_failed" : `forgotten_vector_${failure}_failed`);
  if (failure === "upsert") { assert.equal(row.enrichment_error.length, 2_000); assert.ok(!row.enrichment_error.includes(f.env.MEMORY_API_KEY)); }
});
scenario("late enrichment upsert deletion failure remains retryable with safe diagnostics", async (f, logs) => {
  const [id] = await f.seed();
  f.env.MEMORY_VECTORS.upsert = async () => { f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(id); return {}; };
  f.env.MEMORY_VECTORS.deleteByIds = async () => { throw providerFailure(f); };
  assert.equal((await f.api("/v4/enrich", { id })).status, 202); await f.drain();
  const row = f.db.prepare("SELECT is_forgotten, vector_status FROM memories WHERE id = ?").get(id);
  assert.equal(row.is_forgotten, 1); assert.equal(row.vector_status, "pending");
  boundedDiagnostics(f, logs, "late_vector_delete_failed");
});
scenario("restored vector repair failure persists safe diagnostics and permits later retry", async (f, logs) => {
  const [id] = await f.seed();
  f.env.MEMORY_VECTORS.deleteByIds = async () => { f.db.prepare("UPDATE memories SET is_forgotten = 0, embedding_json = ? WHERE id = ?").run(JSON.stringify(embedding), id); return {}; };
  f.env.MEMORY_VECTORS.upsert = async () => { throw providerFailure(f); };
  assert.equal((await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE")).status, 200); await f.drain();
  const row = f.db.prepare("SELECT is_forgotten, vector_status, vector_attempted_at, enrichment_error FROM memories WHERE id = ?").get(id);
  assert.equal(row.is_forgotten, 0); assert.equal(row.vector_status, "failed"); assert.equal(row.vector_attempted_at, null);
  assert.equal(row.enrichment_error.length, 2_000); assert.ok(!row.enrichment_error.includes(f.env.MEMORY_API_KEY));
  boundedDiagnostics(f, logs, "vector_restore_after_delete_failed");
});

const report = { command: "node --experimental-transform-types server/tests/log-regressions.mjs", node: process.version, migrations, scope: "authenticated Worker API and scheduled entry points with migrated real SQLite and deterministic local AI/vector fixtures", results: [] };
for (const { name, run } of cases) {
  const f = fixture(); const logs = []; const original = { log: console.log, error: console.error, warn: console.warn };
  const capture = (...values) => { for (const value of values) { try { logs.push(JSON.parse(value)); } catch { /* 非 JSON ログは成果物へ転記しない。 */ } } };
  console.log = capture; console.error = capture; console.warn = capture;
  try { await run(f, logs); report.results.push({ name, status: "passed", events: logs.map((entry) => ({ event: entry.event, ok: entry.ok, cron: entry.cron, facts: entry.facts, topics: entry.topics, attempt: entry.attempt, finishReason: entry.finishReason, sourceCount: entry.sourceCount, retrying: entry.retrying, errors: entry.errors, error: entry.error })) }); }
  catch (error) { report.results.push({ name, status: "failed", error: String(error), events: logs.map((entry) => ({ event: entry.event, error: entry.error })) }); }
  finally { await f.drain(); console.log = original.log; console.error = original.error; console.warn = original.warn; f.db.close(); }
}
report.passed = report.results.filter((result) => result.status === "passed").length;
report.failed = report.results.length - report.passed;
mkdirSync(resolve(artifactPath, ".."), { recursive: true }); writeFileSync(artifactPath, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify({ artifact: artifactPath, passed: report.passed, failed: report.failed }));
if (report.failed) process.exitCode = 1;
