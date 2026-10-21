// 失敗条件: SQL/JSの文字数差・NULで本文/ellipsis/AI入力が変わる、scope/同時刻順序/40件境界が変わる、
// metadata/embeddingが欠落する、PATCHの生成物/競合ガードが弱まる、取得量が減らない。
// 再強化のembedding/facts/topics入力が変わる、非同期中の版更新/forget/除外で古い結果を保存する、保存全文を切り詰める。
// 既存の長いcheckpoint引継ぎでAIのpreviousCheckpoint/累積membership/置換動作が変わる。
// 実行: node --experimental-transform-types server/tests/opop-projection-e2e.mjs
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { isAbsolute, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { pathToFileURL } from "node:url";

const root = resolve(import.meta.dirname, "../..");
const source = readFileSync(resolve(root, "server/src/index.ts"), "utf8").replace(/\r\n/gu, "\n");
const timestamp = "2026-10-21T01:00:00.000Z";
const embedding = JSON.stringify(Array.from({ length: 1024 }, (_, index) => Math.sin(index + 1)));
const migrations = readdirSync(resolve(root, "server/migrations")).filter((name) => name.endsWith(".sql")).sort();
const checks = [];
const metrics = [];
const privateFilters = { AND: [{ key: "sm_scope", filterType: "metadata", value: "private" }] };

async function bounded(promise, description) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${description} exceeded 10 seconds`)), 10_000); })]);
  } finally { clearTimeout(timer); }
}

function replaceOnce(text, before, after) {
  assert.equal(text.split(before).length, 2, `source anchor changed: ${before.slice(0, 100)}`);
  return text.replace(before, after);
}

function comparisonSource() {
  let before = replaceOnce(source,
    "m.container_tag AS containerTag,\n            CASE WHEN instr(m.content, char(0)) > 0 THEN m.content ELSE substr(m.content, 1, 5000) END AS content,\n            m.metadata_json AS metadataJson,",
    "m.container_tag AS containerTag, m.content, m.metadata_json AS metadataJson,");
  const start = before.indexOf('  const { clause, parameters } = searchConditions(containerTag, scopeFilter(body), false);');
  const end = before.indexOf('\n  return json({ profile: { static: [], dynamic } });', start);
  assert.ok(start > 0 && end > start, "profile projection boundaries changed");
  before = before.slice(0, start) + '  const rows = await recentMemories(env, containerTag, DEFAULT_LIMIT, scopeFilter(body));\n  const dynamic = rows.map((row) => excerpt(row.content, "", 1_200));' + before.slice(end);
  before = replaceOnce(before,
    '"SELECT container_tag, content, metadata_json, updated_at, topic_revision FROM memories WHERE id = ? AND is_forgotten = 0"',
    '"SELECT * FROM memories WHERE id = ? AND is_forgotten = 0"');
  before = replaceOnce(before,
    'SELECT id, container_tag,\n            CASE WHEN instr(content, char(0)) > 0 THEN content ELSE substr(content, 1, ${MAX_EMBEDDING_CONTENT_LENGTH}) END AS content,\n            metadata_json, updated_at, topic_revision',
    'SELECT id, container_tag, content, metadata_json, updated_at, topic_revision');
  return replaceOnce(before,
    'SELECT consolidation.id, consolidation.memory_id AS memoryId,\n            CASE WHEN instr(memory.content, char(0)) > 0 THEN memory.content ELSE substr(memory.content, 1, 20000) END AS content,',
    'SELECT consolidation.id, consolidation.memory_id AS memoryId, memory.content,');
}

async function loadWorker(text) {
  const imported = replaceOnce(text, '"../../client/memory-index.mjs"', JSON.stringify(pathToFileURL(resolve(root, "client/memory-index.mjs")).href));
  // DB/公開JSON全体を比較するため、時計と新規IDだけを両Workerで同じ値にする。
  const deterministic = `let sequence = 0;
const Date = class extends globalThis.Date { constructor(...args) { super(...(args.length ? args : [${JSON.stringify(timestamp)}])); } static now() { return globalThis.Date.parse(${JSON.stringify(timestamp)}); } };
const crypto = { subtle: globalThis.crypto.subtle, randomUUID: () => "00000000-0000-4000-8000-" + String(++sequence).padStart(12, "0") };
export function resetFixtureIds() { sequence = 0; }
`;
  return import(`data:text/javascript;base64,${Buffer.from(stripTypeScriptTypes(deterministic + imported, { mode: "transform" })).toString("base64")}`);
}

function adapter(db) {
  const reads = [];
  let beforeBatch;
  function prepare(sql) {
    const bind = (values = []) => {
      const select = () => {
        const rows = db.prepare(sql).all(...values);
        reads.push({ sql, rows: rows.length, rowBytes: rows.reduce((sum, row) => sum + Buffer.byteLength(JSON.stringify(row)), 0),
          embeddings: rows.filter((row) => typeof row.embedding_json === "string").length });
        return rows;
      };
      return { bind: (...next) => bind(next), first: async () => select()[0], all: async () => ({ results: select() }),
        run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
        execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
          ? { results: select(), meta: { changes: 0 } }
          : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
      };
    };
    return bind();
  }
  return { reads, prepare, beforeBatch(callback) { beforeBatch = callback; }, batch: async (operations) => {
    const callback = beforeBatch; beforeBatch = undefined; callback?.();
    db.exec("BEGIN");
    try { const results = operations.map((operation) => operation.execute()); db.exec("COMMIT"); return results; }
    catch (error) { db.exec("ROLLBACK"); throw error; }
  } };
}

function fixture(module) {
  module.resetFixtureIds();
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  for (const migration of migrations) db.exec(readFileSync(resolve(root, "server/migrations", migration), "utf8"));
  const DB = adapter(db);
  const calls = [];
  const aiCalls = [];
  const vectorCalls = [];
  const waits = [];
  const checkpoint = { title: "Checkpoint", overview: "Preserved evidence.", verified: ["Source is recorded."], unverified: [], unresolved: [], nextActions: [] };
  const env = { DB, MEMORY_API_KEY: "projection-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      aiCalls.push({ model: _model, input });
      if (input.text) return { data: [JSON.parse(embedding)] };
      if (input.response_format?.json_schema?.name === "memory_consolidation") calls.push(input);
      return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify(input.response_format?.json_schema?.name === "memory_consolidation" ? checkpoint : { facts: [], topics: ["Evidence"] }) } }] };
    } },
    MEMORY_VECTORS: { upsert: async (vectors) => { vectorCalls.push({ operation: "upsert", vectors }); return { mutationId: "fixture-vector" }; },
      deleteByIds: async (ids) => { vectorCalls.push({ operation: "delete", ids }); return {}; }, getByIds: async () => [] },
  };
  const ctx = { waitUntil(promise) { waits.push(promise); } };
  const api = async (path, body, method = "POST", authenticated = true) => {
    const start = DB.reads.length;
    const response = await bounded(module.default.fetch(new Request(`https://fixture.example${path}`, { method,
      headers: { ...(authenticated ? { Authorization: "Bearer projection-fixture" } : {}), "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), env, ctx), `${method} ${path}`);
    const result = { status: response.status, body: await response.json() };
    while (waits.length) await bounded(Promise.all(waits.splice(0)), `waitUntil ${path}`);
    return { ...result, reads: DB.reads.slice(start) };
  };
  function seed(id, content, { container = "memories", metadata = {}, forgotten = 0, revision = `revision-${id}` } = {}) {
    db.prepare("INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)").run(container, container, timestamp, timestamp);
    db.prepare(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, created_at, updated_at,
      topic_revision, embedding_json, embedding_status, fact_status, topic_status, vector_status, entity_context, is_forgotten)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'done', 'done', 'done', 'pending', ?, ?)`).run(
      id, id, container, content, JSON.stringify(metadata), timestamp, timestamp, revision, embedding, "large context ".repeat(1000), forgotten);
  }
  return { db, DB, env, calls, aiCalls, vectorCalls, api, seed };
}

const baselineSource = comparisonSource();
const before = await loadWorker(baselineSource);
const after = await loadWorker(source);

async function compare(name, run) {
  const oldFixture = fixture(before);
  const currentFixture = fixture(after);
  try {
    const old = await run(oldFixture);
    const current = await run(currentFixture);
    assert.deepEqual(current.public, old.public, name);
    checks.push({ name, passed: true });
    if (old.reads && current.reads) {
      assert.equal(old.reads.length, 1, `${name}: baseline SELECT`);
      assert.equal(current.reads.length, 1, `${name}: current SELECT`);
      assert.equal(current.reads[0].rows, old.reads[0].rows, `${name}: row count`);
      assert.ok(current.reads[0].rowBytes < old.reads[0].rowBytes, `${name}: transferred row bytes`);
      metrics.push({ name, rows: old.reads[0].rows, beforeBytes: old.reads[0].rowBytes, afterBytes: current.reads[0].rowBytes,
        beforeEmbeddingRows: old.reads[0].embeddings, afterEmbeddingRows: current.reads[0].embeddings });
    }
  } finally { oldFixture.db.close(); currentFixture.db.close(); }
}

const contents = ["short", "a".repeat(1200), "界".repeat(1201), "a".repeat(1199) + "😀tail", "😀".repeat(600),
  "😀".repeat(601), "界".repeat(200_000), "a".repeat(4999) + "😀" + "z".repeat(20_000), "before\0" + "界".repeat(10_000),
  "a".repeat(5500) + "\0tail", "q".repeat(200_000)];

for (const [index, content] of contents.entries()) await compare(`profile UTF-16/NUL boundary ${index}`, async (f) => {
  f.seed("profile", content, { metadata: { padding: "m".repeat(20_000), sm_scope: "private" } });
  const result = await f.api("/v4/profile", { containerTag: "memories", q: "  ", filters: privateFilters });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  return { public: result.body, reads: result.reads.filter((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)) };
});

await compare("profile scope, forgotten filtering and tied order", async (f) => {
  for (let i = 0; i < 9; i++) f.seed(`tie-${i}`, `${i}:` + "evidence ".repeat(1000),
    { container: i === 8 ? "other" : "memories", forgotten: i === 7 ? 1 : 0,
      metadata: { sm_scope: i === 0 ? "public" : "private", padding: "m".repeat(5000) } });
  const result = await f.api("/v4/profile", { containerTag: "memories", filters: privateFilters });
  assert.equal(result.body.profile.dynamic.length, 5);
  assert.ok(result.body.profile.dynamic.every((value) => /^[1-5]:/u.test(value)));
  return { public: result.body, reads: result.reads.filter((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)) };
});

await compare("profile facts and query branches remain unchanged", async (f) => {
  f.seed("fact-source", "Searchable technical evidence.", { metadata: { sm_scope: "private" } });
  f.db.prepare(`INSERT INTO facts(id, container_tag, source_memory_id, subject, predicate, object, subject_key, predicate_key,
    object_key, is_exclusive, confidence, status, created_at, updated_at) VALUES ('fact', 'memories', 'fact-source', 'Subject', 'has', 'evidence', 'subject', 'has', 'evidence', 0, 1, 'active', ?, ?)`).run(timestamp, timestamp);
  const facts = await f.api("/v4/profile", { containerTag: "memories" });
  assert.deepEqual(facts.body.profile.dynamic, ["Subject has evidence"]);
  assert.ok(!facts.reads.some((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)));
  const search = await f.api("/v4/profile", { containerTag: "memories", q: "technical" });
  const { timing: _timing, ...searchResults } = search.body.searchResults;
  return { public: { facts: facts.body, search: { ...search.body, searchResults } } };
});

for (const count of [4, 41]) await compare(`consolidation ${count} source AI input, revisions and membership`, async (f) => {
  for (let i = 0; i < count; i++) f.seed(`source-${String(i).padStart(2, "0")}`, contents[(i + 6) % contents.length],
    { container: i % 2 ? "legacy" : "memories", metadata: { sm_project_id: "projection-project", title: `Source ${i}` } });
  f.seed("unrelated", "Do not include", { metadata: { sm_project_id: "other-project" } });
  f.seed("forgotten", "Do not include", { forgotten: 1, metadata: { sm_project_id: "projection-project" } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "projection-project", force: true });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(f.calls.length, 1);
  const records = JSON.parse(f.calls[0].messages[1].content).newMemoryRecords;
  assert.equal(records.length, Math.min(40, count));
  const membership = f.db.prepare("SELECT memory_id, source_revision FROM memory_consolidation_sources ORDER BY memory_id").all();
  assert.deepEqual(membership.map((row) => row.memory_id), records.map((record) => record.id));
  const persisted = f.db.prepare("SELECT content, embedding_json FROM memories WHERE id LIKE 'source-%' ORDER BY id").all();
  assert.ok(persisted.every((row) => row.embedding_json === embedding));
  return { public: { response: result.body, calls: f.calls, membership, persisted },
    reads: result.reads.filter((read) => read.sql.includes("AS sourceRevision") && read.sql.includes("LIMIT ?")) };
});

await compare("consolidation rejects a source revised during AI generation", async (f) => {
  f.seed("source", "界".repeat(20_000), { metadata: { sm_project_id: "racing-project" } });
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    if (input.response_format?.json_schema?.name === "memory_consolidation") {
      f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'source'").run();
    }
    return run(model, input);
  };
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "racing-project", force: true });
  assert.equal(result.status, 409, JSON.stringify(result.body));
  assert.equal(result.body.error.message, "Source memories changed during consolidation");
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 0);
  return { public: { response: result.body, calls: f.calls } };
});

const priorContents = ["Previous evidence.", "x".repeat(20_000), "界".repeat(20_001), "x".repeat(19_999) + "😀tail",
  "😀".repeat(10_001), "界".repeat(120_000), "before\0" + "x".repeat(100_000), "x".repeat(25_000) + "\0tail"];
for (const [index, content] of priorContents.entries()) await compare(`prior checkpoint AI input and cumulative membership boundary ${index}`, async (f) => {
  const metadata = { sm_project_id: "prior-project", sm_scope: "private" };
  f.seed("old-source", "Previously covered evidence.", { metadata });
  f.seed("old-checkpoint", content, { metadata: { ...metadata, sm_consolidation: true } });
  f.db.prepare(`INSERT INTO memory_consolidation_projects(project_key, project_id, source_container_tag, revision,
    active_consolidation_id, last_success_at, created_at, updated_at)
    VALUES ('project:prior-project', 'prior-project', 'memories', 1, 'prior-consolidation', ?, ?, ?)`).run(timestamp, timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
    VALUES ('prior-consolidation', 'project:prior-project', 'old-checkpoint', 1, 'active', ?, ?)`).run(timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision)
    VALUES ('prior-consolidation', 'old-source', 'revision-old-source')`).run();
  f.seed("new-source", "Next evidence.", { metadata });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "prior-project", force: true });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(f.calls.length, 1);
  const input = JSON.parse(f.calls[0].messages[1].content);
  assert.ok(input.previousCheckpoint.length <= 20_000);
  assert.deepEqual(input.newMemoryRecords.map((record) => record.id), ["new-source"]);
  const membership = f.db.prepare(`SELECT source.memory_id, source.source_revision FROM memory_consolidation_sources AS source
    JOIN memory_consolidations AS consolidation ON consolidation.id = source.consolidation_id WHERE consolidation.status = 'active'
    ORDER BY source.memory_id`).all();
  assert.deepEqual(membership.map((row) => row.memory_id), ["new-source", "old-source"]);
  const previous = f.db.prepare("SELECT content, is_forgotten, status FROM memories WHERE id = 'old-checkpoint'").get();
  assert.equal(previous.content, content);
  assert.equal(previous.is_forgotten, 1);
  return { public: { response: result.body, calls: f.calls, membership, previous },
    ...(!content.includes("\0") && [...content].length > 20_000
      ? { reads: result.reads.filter((read) => /^SELECT consolidation.id, consolidation.memory_id AS memoryId,/u.test(read.sql)) } : {}) };
});

for (const [name, body] of [["metadata-only", { metadata: { title: "Updated", sm_project_id: "project", padding: "p".repeat(30_000) } }],
  ["content-only", { content: "Updated 😀 evidence." }]]) await compare(`PATCH ${name} preserves response and stored state`, async (f) => {
  f.seed("patch", "Original 😀 content.", { metadata: { title: "Original", sm_project_id: "project", topics: ["Original"], padding: "m".repeat(30_000) } });
  const result = await f.api("/v3/documents/patch", body, "PATCH");
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(result.body.content, body.content ?? "Original 😀 content.");
  const stored = f.db.prepare("SELECT * FROM memories WHERE id = 'patch'").get();
  assert.equal(stored.embedding_json, null);
  return { public: { response: result.body, stored }, reads: result.reads.filter((read) => /^SELECT (?:\*|container_tag, content)/u.test(read.sql)) };
});

await compare("PATCH generated checkpoint, stale revision, missing and unauthenticated guards", async (f) => {
  f.seed("patch", "Original guarded content.");
  const unauthorized = await f.api("/v3/documents/patch", { content: "Denied" }, "PATCH", false);
  const missing = await f.api("/v3/documents/missing", { content: "Missing" }, "PATCH");
  f.DB.beforeBatch(() => f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'patch'").run());
  const stale = await f.api("/v3/documents/patch", { content: "Stale replacement" }, "PATCH");
  assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = 'patch'").get().content, "Original guarded content.");
  f.seed("checkpoint", "Generated content.");
  f.db.prepare(`INSERT INTO memory_consolidation_projects(project_key, source_container_tag, created_at, updated_at)
    VALUES ('container:memories', 'memories', ?, ?)`).run(timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
    VALUES ('generated', 'container:memories', 'checkpoint', 1, 'active', ?, ?)`).run(timestamp, timestamp);
  const generated = await f.api("/v3/documents/checkpoint", { metadata: {} }, "PATCH");
  assert.deepEqual([unauthorized.status, missing.status, stale.status, generated.status], [401, 404, 409, 409]);
  return { public: [unauthorized, missing, stale, generated].map(({ status, body }) => ({ status, body })) };
});

const enrichmentContents = ["short", "x".repeat(60_000), "x".repeat(60_001), "x".repeat(59_999) + "😀tail",
  "x".repeat(23_999) + "😀" + "tail".repeat(10_000), "😀".repeat(30_001), "😀".repeat(70_000),
  "界".repeat(200_000), "x".repeat(200_000), "before\0" + "x".repeat(100_000), "x".repeat(70_000) + "\0tail"];
for (const [index, content] of enrichmentContents.entries()) await compare(`enrichment AI input and stored content boundary ${index}`, async (f) => {
  f.seed("enrich", content, { metadata: { sm_project_id: "enrichment-project", padding: "m".repeat(5000) } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    const result = await run(model, input);
    if (input.response_format?.json_schema?.name === "durable_memory_enrichment") {
      return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify({
        facts: [{ subject: "Project", predicate: "has", object: "evidence", exclusive: false, confidence: 1 }], topics: ["Evidence"],
      }) } }] };
    }
    return result;
  };
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  assert.equal(f.aiCalls.length, 2);
  assert.ok(f.aiCalls[0].input.text[0].length <= 60_000);
  assert.ok(f.aiCalls[1].input.messages[1].content.length <= 24_000);
  assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = 'enrich'").get().content, content);
  const facts = f.db.prepare("SELECT * FROM facts ORDER BY id").all();
  const topics = f.db.prepare("SELECT * FROM memory_topics ORDER BY topic_key").all();
  assert.equal(facts.length, 1);
  assert.equal(topics.length, 1);
  assert.equal(f.vectorCalls.filter((call) => call.operation === "upsert").length, 1);
  const document = await f.api("/v3/documents/enrich", undefined, "GET");
  return { public: { response: result.body, document: document.body, aiCalls: f.aiCalls, vectorCalls: f.vectorCalls, facts, topics },
    ...(!content.includes("\0") && [...content].length > 60_000
      ? { reads: result.reads.filter((read) => /^SELECT id, container_tag,/u.test(read.sql)) } : {}) };
});

for (const mode of ["explicit topics", "checkpoint"]) await compare(`enrichment ${mode} branch`, async (f) => {
  f.seed("enrich", "界".repeat(80_000), { metadata: mode === "checkpoint" ? { sm_consolidation: true } : { topics: ["Explicit"] } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  const topics = f.db.prepare("SELECT * FROM memory_topics ORDER BY topic_key").all();
  assert.equal(topics[0].topic, mode === "checkpoint" ? "Evidence" : "Explicit");
  const document = await f.api("/v3/documents/enrich", undefined, "GET");
  return { public: { response: result.body, document: document.body, aiCalls: f.aiCalls, vectorCalls: f.vectorCalls, topics } };
});

for (const mutation of ["revision", "forgotten", "excluded"]) await compare(`enrichment discards results after concurrent ${mutation}`, async (f) => {
  f.seed("enrich", "x".repeat(100_000));
  f.env.AI_ENRICHMENT_MODE = "on";
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    if (input.text) {
      if (mutation === "revision") f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'enrich'").run();
      if (mutation === "forgotten") f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = 'enrich'").run();
      if (mutation === "excluded") f.db.prepare("UPDATE memories SET metadata_json = ? WHERE id = 'enrich'").run(JSON.stringify({ memoryIndex: { recallable: false } }));
    }
    return run(model, input);
  };
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  assert.deepEqual(f.vectorCalls, []);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_topics").get().count, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM facts").get().count, 0);
  const stored = f.db.prepare("SELECT * FROM memories WHERE id = 'enrich'").get();
  assert.equal(stored.embedding_json, embedding);
  return { public: { response: result.body, stored, aiCalls: f.aiCalls } };
});

await compare("enrichment rejects a revision changed before its guarded update", async (f) => {
  f.seed("enrich", "x".repeat(100_000));
  f.env.AI_ENRICHMENT_MODE = "on";
  f.DB.beforeBatch(() => f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'enrich'").run());
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 409, JSON.stringify(result.body));
  assert.deepEqual(f.aiCalls, []);
  return { public: { response: result.body, stored: f.db.prepare("SELECT * FROM memories WHERE id = 'enrich'").get() } };
});

const outputRoot = process.env.REMNIFOLD_VALIDATION_DIR;
if (outputRoot !== undefined) assert.ok(isAbsolute(outputRoot), "REMNIFOLD_VALIDATION_DIR must be absolute");
const artifactPath = resolve(outputRoot ?? resolve(root, "dist"), "opop-projection-validation.json");
mkdirSync(resolve(artifactPath, ".."), { recursive: true });
const hash = (value) => createHash("sha256").update(value).digest("hex");
writeFileSync(artifactPath, JSON.stringify({ passed: true, checks, metrics, migrations,
  sourceHashes: { worker: hash(source), comparisonWorker: hash(baselineSource), test: hash(readFileSync(import.meta.filename)) },
  baseline: "current Worker with only the five read projections reversed in memory; deterministic fixture clock and IDs",
  measurement: "JSON bytes of actual SQLite SELECT rows at the D1 adapter boundary; not wire size or deployed latency",
}, null, 2) + "\n");
console.log(JSON.stringify({ passed: true, checks: checks.length, metrics, artifactPath }));
