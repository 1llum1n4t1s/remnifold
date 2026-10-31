// 失敗条件: 公開JSON/キー順・AI/Vectorize入力・DB更新順・revisionガード・診断順が変わる、
// 再調整の探索/digestや検索のexcerpt/pattern/小文字化が減らない、index投影が本文を評価する。
// 実行: node server/tests/structure-performance-e2e.mjs [--comparison-root <修正前のrepo>]
// 保存済み比較元を省略したCIでは、現在の実Workerの契約と処理回数を確認する。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { isAbsolute, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { resolveWorkerImports } from "./worker-imports.mjs";

const root = resolve(import.meta.dirname, "../..");
const args = process.argv.slice(2);
assert.ok(args.length === 0 || args.length === 2 && args[0] === "--comparison-root", "expected --comparison-root <repository>");
const comparisonRoot = args.length ? resolve(args[1]) : undefined;
const outputRoot = process.env.REMNIFOLD_VALIDATION_DIR;
if (outputRoot) assert.ok(isAbsolute(outputRoot), "REMNIFOLD_VALIDATION_DIR must be absolute");
const artifactPath = resolve(outputRoot ?? resolve(root, "dist"), "structure-performance-validation.json");
const timestamp = "2026-10-21T01:00:00.000Z";
const embedding = Array.from({ length: 1024 }, (_, index) => index === 0 ? 1 : 0);
const migrations = readdirSync(resolve(root, "server/migrations")).filter((name) => name.endsWith(".sql")).sort();
const checks = [];
const metrics = [];

async function bounded(promise, description) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${description} exceeded 10 seconds`)), 10_000);
    })]);
  } finally { clearTimeout(timer); }
}

async function loadWorker(sourceRoot) {
  const source = readFileSync(resolve(sourceRoot, "server/src/index.ts"), "utf8").replace(/\r\n/gu, "\n");
  let observed = resolveWorkerImports(source, sourceRoot);
  // 実際の評価点だけを数える。探索候補・判定・結果は置き換えない。
  for (const name of ["excerpt", "indexQueryPatterns"]) {
    const declaration = new RegExp(`(function ${name}\\([^\\n]+\\{\\n)`, "u");
    assert.ok(declaration.test(observed), `${name}: instrumentation anchor`);
    observed = observed.replace(declaration, `$1  fixtureCounts.${name}++;\n`);
  }
  observed = observed.replaceAll("text.toLowerCase()", "(fixtureCounts.candidateLower++, text.toLowerCase())")
    .replaceAll("result.results.find((candidate) => candidate.id === vector.id)",
      "result.results.find((candidate) => (fixtureCounts.rowComparisons++, candidate.id === vector.id))")
    .replaceAll("rowsById.get(vector.id)", "(fixtureCounts.rowLookups++, rowsById.get(vector.id))");
  const deterministic = `let fixtureSequence = 0;
const fixtureCounts = { excerpt: 0, indexQueryPatterns: 0, candidateLower: 0, rowComparisons: 0, rowLookups: 0, digests: 0 };
export function resetFixture() { fixtureSequence = 0; resetCounts(); }
export function resetCounts() { for (const key of Object.keys(fixtureCounts)) fixtureCounts[key] = 0; }
export function counts() { return { ...fixtureCounts }; }
const Date = class extends globalThis.Date { constructor(...args) { super(...(args.length ? args : [${JSON.stringify(timestamp)}])); } static now() { return globalThis.Date.parse(${JSON.stringify(timestamp)}); } };
const crypto = { randomUUID: () => "00000000-0000-4000-8000-" + String(++fixtureSequence).padStart(12, "0"),
  subtle: { digest(...args) { fixtureCounts.digests++; return globalThis.crypto.subtle.digest(...args); } } };
`;
  const module = await import(`data:text/javascript;base64,${Buffer.from(stripTypeScriptTypes(deterministic + observed, { mode: "transform" })).toString("base64")}`);
  return { module, sourceHash: createHash("sha256").update(source).digest("hex") };
}

function fixture(module) {
  module.resetFixture();
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  for (const migration of migrations) db.exec(readFileSync(resolve(root, "server/migrations", migration), "utf8"));
  const events = [];
  const waits = [];
  const vectors = new Map();
  const controls = { vectorFailure: false, embeddingFailure: false, enrichmentFailure: false, afterAi: undefined };
  function prepare(sql) {
    const bind = (values = []) => {
      const execute = () => {
        events.push({ operation: "sql", sql, values });
        if (/^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)) return { results: db.prepare(sql).all(...values), meta: { changes: 0 } };
        return { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } };
      };
      return { bind: (...next) => bind(next), first: async () => execute().results[0], all: async () => execute(), run: async () => execute(), execute };
    };
    return bind();
  }
  const DB = { prepare, batch: async (operations) => {
    db.exec("BEGIN");
    try { const result = operations.map((operation) => operation.execute()); db.exec("COMMIT"); return result; }
    catch (error) { db.exec("ROLLBACK"); throw error; }
  } };
  const env = { DB, MEMORY_API_KEY: "structure-fixture-key", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (model, input) => {
      events.push({ operation: "ai", model, input });
      if (input.text && controls.embeddingFailure) throw new Error("fixture embedding failure");
      if (!input.text && controls.enrichmentFailure) throw new Error("fixture enrichment failure");
      if (controls.afterAi) { const callback = controls.afterAi; controls.afterAi = undefined; callback(); }
      if (input.text) return { data: [embedding] };
      return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify({ facts: [], topics: ["Evidence"] }) } }] };
    } },
    MEMORY_VECTORS: {
      upsert: async (rows) => {
        events.push({ operation: "upsert", rows });
        if (controls.vectorFailure) throw new Error("fixture vector failure");
        for (const row of rows) vectors.set(row.id, row);
        return { mutationId: "fixture-mutation" };
      },
      getByIds: async (ids) => { events.push({ operation: "getByIds", ids }); return ids.flatMap((id) => vectors.has(id) ? [vectors.get(id)] : []); },
      deleteByIds: async (ids) => { events.push({ operation: "deleteByIds", ids }); for (const id of ids) vectors.delete(id); return {}; },
      query: async (_values, options) => { events.push({ operation: "query", options }); return { matches: [], count: 0 }; },
    },
  };
  const ctx = { waitUntil(promise) { waits.push(promise); } };
  async function drain() { while (waits.length) await bounded(Promise.all(waits.splice(0)), "Worker waitUntil"); }
  async function api(path, body, method = "POST", authenticated = true) {
    const response = await bounded(module.default.fetch(new Request(`https://fixture.example${path}`, { method,
      headers: { ...(authenticated ? { Authorization: `Bearer ${env.MEMORY_API_KEY}` } : {}), "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), env, ctx), `${method} ${path}`);
    const text = await response.text();
    await drain();
    return { status: response.status, body: JSON.parse(text) };
  }
  function seed(id, content, { container = "memories", metadata = {}, indexed = false, invalid = false } = {}) {
    db.prepare("INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)").run(container, container, timestamp, timestamp);
    db.prepare(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, created_at, updated_at,
      topic_revision, embedding_json, embedding_status, fact_status, topic_status, vector_status, entity_context)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'done', 'done', 'done', ?, ?)`)
      .run(id, id, container, content, JSON.stringify(metadata), timestamp, timestamp, `revision-${id}`,
        JSON.stringify(invalid ? [1, 2] : embedding), indexed ? "indexed" : "pending", "fixture context");
  }
  async function reconcile() {
    env.AI_ENRICHMENT_MODE = "on";
    module.default.scheduled({ cron: "*/10 * * * *" }, env, ctx);
    await drain();
  }
  const state = () => db.prepare("SELECT * FROM memories ORDER BY id").all();
  const reset = () => { events.length = 0; module.resetCounts(); };
  return { db, env, api, seed, reconcile, controls, events, state, reset };
}

const after = await loadWorker(root);
const before = comparisonRoot ? await loadWorker(comparisonRoot) : undefined;

async function scenario(name, run, verify) {
  const outputs = [];
  for (const worker of [before, after].filter(Boolean)) {
    const f = fixture(worker.module);
    try { outputs.push(await run(f, worker.module)); }
    finally { f.db.close(); }
  }
  const current = outputs.at(-1);
  if (before) {
    assert.deepEqual(current.public, outputs[0].public, `${name}: public/side-effect contract`);
    // JSON文字列としても比較し、DTOのキー列挙順を保つ。
    assert.equal(JSON.stringify(current.public), JSON.stringify(outputs[0].public), `${name}: JSON key ordering`);
  }
  verify?.(current, before ? outputs[0] : undefined);
  checks.push({ name, passed: true, compared: Boolean(before) });
}

for (const [name, tags, failure, invalid] of [
  ["reconcile same tag 100 rows", 1, false, false],
  ["reconcile three tags 100 rows", 3, false, false],
  ["reconcile failed upsert", 1, true, false],
  ["reconcile invalid embedding", 1, false, true],
]) await scenario(name, async (f, module) => {
  for (let index = 0; index < 100; index++) f.seed(`vector-${String(index).padStart(3, "0")}`, "vector evidence", {
    container: `tag-${index % tags}`, indexed: index >= 90, invalid: invalid && index === 0,
  });
  f.controls.vectorFailure = failure;
  f.reset();
  await f.reconcile();
  const upserts = f.events.filter((event) => event.operation === "upsert");
  assert.equal(upserts.length, 1);
  assert.equal(upserts[0].rows.length, invalid ? 99 : 100);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memories WHERE vector_status = ?").get(failure ? "failed" : "queued").n, invalid ? 99 : 100);
  return { public: { state: f.state(), events: f.events }, counts: module.counts() };
}, (current, old) => {
  const count = invalid ? 99 : 100;
  assert.equal(current.counts.digests, tags);
  assert.equal(current.counts.rowComparisons, 0);
  assert.equal(current.counts.rowLookups, count);
  if (old) {
    assert.equal(old.counts.digests, count);
    assert.ok(old.counts.rowComparisons >= count * (count + 1) / 2);
  }
  metrics.push({ name, before: old?.counts, after: current.counts });
});

for (const projection of ["full", "index", "ids", "capture"]) await scenario(`document ${projection} and GET DTO`, async (f) => {
  f.seed("document", "Unicode 😀 before\0after " + "evidence ".repeat(80), { metadata: { title: "保存した題名", sm_project_id: "project", sm_scope: "private", filepath: "fixture.md" } });
  const list = await f.api("/v3/documents/list", { containerTag: "memories", projection });
  const get = await f.api("/v3/documents/document", undefined, "GET");
  const denied = await f.api("/v3/documents/document", undefined, "GET", false);
  assert.equal(list.status, 200); assert.equal(get.status, 200); assert.equal(denied.status, 401);
  assert.equal(get.body.content, "Unicode 😀 before\0after " + "evidence ".repeat(80));
  assert.equal(get.body.enrichment.embeddingStatus, "done");
  if (projection === "index") assert.ok(!Object.hasOwn(list.body.documents[0], "content"));
  return { public: { list, get, denied, events: f.events } };
});

for (const indexOnly of [false, true]) await scenario(`search ${indexOnly ? "index" : "full"} Unicode terms and stable ties`, async (f, module) => {
  const metadata = { title: "Stored heading", memoryIndex: { version: 1, title: "Stored heading", description: "Existing description", sections: [], recallable: true, sourceKind: "conversation" } };
  for (let index = 0; index < 3; index++) f.seed(`search-${index}`, "prefix ".repeat(45) + "EVIDENCE café 東京 😀 alpha before\0after evidence café 東京 alpha tail", { metadata });
  f.reset();
  const result = await f.api("/v4/search", { q: "evidence café 東京 alpha", allContainers: true, limit: 3, indexOnly });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(result.body.results.length, 3);
  assert.deepEqual(result.body.results.map((row) => row.id), ["search-0", "search-1", "search-2"]);
  for (const row of result.body.results) {
    if (indexOnly) { assert.ok(!Object.hasOwn(row, "content")); assert.ok(row.searchExcerpt.includes("EVIDENCE")); }
    else assert.equal(row.memory, row.content);
  }
  const { timing: _timing, ...body } = result.body;
  return { public: { status: result.status, body, events: f.events }, counts: module.counts() };
}, (current, old) => {
  // index投影はFTS順位用の独立した1回も必要。indexedMemoriesの重複だけが減る。
  assert.equal(current.counts.indexQueryPatterns, indexOnly ? 2 : 1);
  assert.equal(current.counts.excerpt, indexOnly ? 0 : 3);
  assert.equal(current.counts.candidateLower, 12);
  if (old) {
    assert.equal(old.counts.indexQueryPatterns, indexOnly ? 3 : 2);
    if (!indexOnly) assert.equal(old.counts.excerpt - current.counts.excerpt, 3);
    else assert.equal(current.counts.excerpt, old.counts.excerpt);
    assert.ok(current.counts.candidateLower > 0);
    assert.equal(old.counts.candidateLower, current.counts.candidateLower * 4);
  }
  metrics.push({ name: `search ${indexOnly ? "index" : "full"}`, before: old?.counts, after: current.counts });
});

for (const mode of ["success", "vector failure", "embedding and enrichment failure", "stale revision"]) await scenario(`enrichment ${mode}`, async (f) => {
  f.seed("enrich", "Evidence 😀 before\0after " + "preserved ".repeat(20), { metadata: { title: "Original", sm_project_id: "project" } });
  f.env.AI_ENRICHMENT_MODE = "on";
  f.controls.vectorFailure = mode === "vector failure";
  f.controls.embeddingFailure = mode === "embedding and enrichment failure";
  f.controls.enrichmentFailure = mode === "embedding and enrichment failure";
  if (mode === "stale revision") f.controls.afterAi = () => f.db.prepare("UPDATE memories SET updated_at = ?, topic_revision = 'new-revision' WHERE id = 'enrich'").run("2026-10-21T02:00:00.000Z");
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  const row = f.state()[0];
  if (mode === "success") { assert.equal(row.embedding_status, "done"); assert.equal(row.vector_status, "queued"); }
  if (mode === "vector failure") { assert.equal(row.vector_status, "failed"); assert.match(row.enrichment_error, /^vector index:/u); }
  if (mode === "embedding and enrichment failure") { assert.equal(row.embedding_status, "failed"); assert.match(row.enrichment_error, /^embedding:/u); }
  if (mode === "stale revision") { assert.equal(row.topic_revision, "new-revision"); assert.equal(f.events.filter((event) => event.operation === "upsert").length, 0); }
  return { public: { result, state: f.state(), events: f.events } };
});

mkdirSync(resolve(artifactPath, ".."), { recursive: true });
writeFileSync(artifactPath, JSON.stringify({ passed: true, checks, metrics, currentSourceHash: after.sourceHash,
  ...(before ? { comparisonSourceHash: before.sourceHash } : {}), artifactPath }, null, 2) + "\n");
console.log(JSON.stringify({ passed: true, checks: checks.length, compared: Boolean(before), artifactPath, metrics }));
