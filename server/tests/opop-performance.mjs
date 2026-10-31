// 実行: node --experimental-transform-types server/tests/opop-performance.mjs
// 失敗条件: 表示列/状態/出典の欠落、内部embeddingのNULL化、同点順序/checkpoint優先の変化、
// 段階間の古いキー再利用、同ID別rowの混同、scope/revision/limit/membershipの変化、
// metadata再解析の残存、UTF-16補正後の同一抜粋windowの再採点。
// 現在の実Workerから旧表示SQLと旧比較器だけをメモリ内で復元し、同じ全migration済みDBで比較する。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { resolveWorkerImports } from "./worker-imports.mjs";
import { buildMemoryIndex } from "../../client/memory-index.mjs";

const root = resolve(import.meta.dirname, "../..");
const sourcePath = resolve(root, "server/src/index.ts");
const sourceBytes = readFileSync(sourcePath);
const source = sourceBytes.toString("utf8").replace(/\r\n/gu, "\n");
const comparisonArgument = process.argv.indexOf("--comparison-root");
assert.ok(comparisonArgument < 0 || process.argv[comparisonArgument + 1], "--comparison-root requires a repository path");
const comparisonRoot = comparisonArgument < 0 ? undefined : resolve(process.argv[comparisonArgument + 1]);
const optimizationBaselineSource = comparisonRoot
  ? readFileSync(resolve(comparisonRoot, "server/src/index.ts"), "utf8").replace(/\r\n/gu, "\n") : undefined;
const hash = (value) => createHash("sha256").update(value).digest("hex");
const migrations = readdirSync(resolve(root, "server/migrations")).filter((name) => name.endsWith(".sql")).sort();
const timestamp = "2026-03-04T12:00:00.000Z";
const embedding = Array.from({ length: 1024 }, (_, i) => Math.sin(i + 1) / 32);
const embeddingJson = JSON.stringify(embedding);
const oldSort = `function sortByQueryCoverage(rows, topics, terms, compareIndexCoverage = false, preferCheckpoints = false) {
  return rows.sort((left, right) => {
    __counts.comparisons++;
    return queryCoverage(right, topics.get(right.id) ?? [], terms) - queryCoverage(left, topics.get(left.id) ?? [], terms) ||
      (compareIndexCoverage ? queryCoverage(right, topics.get(right.id) ?? [], terms, false) - queryCoverage(left, topics.get(left.id) ?? [], terms, false) : 0) ||
      (preferCheckpoints ? Number(parseMetadata(right.metadata_json).sm_consolidation === true) - Number(parseMetadata(left.metadata_json).sm_consolidation === true) : 0);
  });
}`;

function replaceOnce(text, before, after) {
  assert.equal(text.split(before).length, 2, `source anchor changed: ${before.slice(0, 80)}`);
  return text.replace(before, after);
}

function comparisonSource() {
  let before = replaceOnce(source, "NULL AS embedding_json, m.vector_status", "m.embedding_json, m.vector_status");
  before = replaceOnce(before, "  const scoredStarts = new Set<number>();\n", "");
  before = replaceOnce(before, "    if (scoredStarts.has(start)) continue;\n    scoredStarts.add(start);\n", "");
  before = replaceOnce(before,
    "`SELECT ${DOCUMENT_DISPLAY_COLUMNS} FROM memories AS m WHERE m.id = ? AND m.is_forgotten = 0`",
    '"SELECT * FROM memories WHERE id = ? AND is_forgotten = 0"');
  const start = before.indexOf("function sortByQueryCoverage(");
  const end = before.indexOf("\nasync function searchMemories(", start);
  assert.ok(start > 0 && end > start, "sort helper boundaries changed");
  return before.slice(0, start) + oldSort + "\n" + before.slice(end);
}

async function loadWorker(text, optimized) {
  let instrumented = resolveWorkerImports(text, root);
  const coverageSignature = instrumented.match(/^function queryCoverage\([^\n]+\): number \{/mu)?.[0];
  assert.ok(coverageSignature, "queryCoverage signature changed");
  instrumented = replaceOnce(instrumented, coverageSignature, `${coverageSignature} __counts.coverage++;`);
  instrumented = replaceOnce(instrumented,
    "function parseMetadata(value: string): JsonObject {",
    "function parseMetadata(value: string): JsonObject { __counts.metadataParses++;");
  instrumented = replaceOnce(instrumented,
    "const text = truncateUtf16(content.slice(start), 236);",
    "__counts.excerptWindows++; const text = truncateUtf16(content.slice(start), 236);");
  if (optimized) instrumented = replaceOnce(instrumented,
    "ranked.sort((left, right) => right.coverage - left.coverage ||\n    right.indexCoverage - left.indexCoverage || right.checkpoint - left.checkpoint);",
    "ranked.sort((left, right) => { __counts.comparisons++; return right.coverage - left.coverage ||\n    right.indexCoverage - left.indexCoverage || right.checkpoint - left.checkpoint; });");
  instrumented = `const __counts = { coverage: 0, comparisons: 0, metadataParses: 0, excerptWindows: 0 };\n` + instrumented + `
export { sortByQueryCoverage as sortCoverage, querySearchExcerpt as searchExcerpt, excerpt as sourceExcerpt };
export function resetCounts() { for (const key of Object.keys(__counts)) __counts[key] = 0; }
export function counts() { return { ...__counts }; }
`;
  const javascript = stripTypeScriptTypes(instrumented, { mode: "transform" });
  return import(`data:text/javascript;base64,${Buffer.from(javascript).toString("base64")}`);
}

// D1の返却直前に、実SELECTのJSONサイズとembedding取得数を測る。WireサイズやD1課金の推定ではない。
function adapter(db) {
  const reads = [];
  let beforeRead;
  function prepare(sql) {
    const bind = (values = []) => {
      const select = () => {
        beforeRead?.(sql, values);
        const rows = db.prepare(sql).all(...values);
        reads.push({ sql, rows: rows.length, rowBytes: rows.reduce((sum, row) => sum + Buffer.byteLength(JSON.stringify(row)), 0),
          embeddings: rows.filter((row) => typeof row.embedding_json === "string").length });
        return rows;
      };
      return { sql, values, bind: (...next) => bind(next),
        first: async () => select()[0], all: async () => ({ results: select() }),
        run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
        execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
          ? { results: select(), meta: { changes: 0 } }
          : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
      };
    };
    return bind();
  }
  return { prepare, reads, setBeforeRead(callback) { beforeRead = callback; }, batch: async (operations) => {
    db.exec("BEGIN");
    try { const results = operations.map((operation) => operation.execute()); db.exec("COMMIT"); return results; }
    catch (error) { db.exec("ROLLBACK"); throw error; }
  } };
}

const baselineSource = comparisonSource();
const before = await loadWorker(baselineSource, false);
const after = await loadWorker(source, true);
const optimizationBaseline = optimizationBaselineSource ? await loadWorker(optimizationBaselineSource, true) : undefined;
const db = new DatabaseSync(":memory:");
const checks = [];
const metrics = { selectRows: [], search: [], microbenchmark: [], optimizations: [] };
let artifact;
try {
  db.exec("PRAGMA foreign_keys = ON");
  for (const migration of migrations) db.exec(readFileSync(resolve(root, "server/migrations", migration), "utf8"));
  const DB = adapter(db);
  const pending = [];
  const ctx = { waitUntil(promise) { pending.push(Promise.resolve(promise)); } };
  const env = { DB, MEMORY_API_KEY: "opop-local-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      assert.ok(input.text, "this fixture does not perform generated enrichment");
      return { data: [embedding] };
    } },
    MEMORY_VECTORS: { query: async () => ({ matches: [], count: 0 }) },
  };
  async function request(module, path, body, method = "POST", authenticated = true) {
    DB.reads.length = 0; module.resetCounts();
    const response = await module.default.fetch(new Request(`https://local.example${path}`, { method,
      headers: { ...(authenticated ? { Authorization: `Bearer ${env.MEMORY_API_KEY}` } : {}), "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), env, ctx);
    const result = { status: response.status, body: await response.json(), reads: [...DB.reads], counts: module.counts() };
    while (pending.length) await Promise.all(pending.splice(0));
    return result;
  }
  async function compare(name, path, body, method = "POST", expectedStatus = 200, beforeRead) {
    DB.setBeforeRead(beforeRead?.());
    const old = await request(before, path, body, method);
    DB.setBeforeRead(beforeRead?.());
    const current = await request(after, path, body, method);
    DB.setBeforeRead(undefined);
    assert.equal(old.status, expectedStatus, `${name}: before status`);
    assert.equal(current.status, expectedStatus, `${name}: after status`);
    // timingのみ非決定値。results・順序・scope・provenance・membershipは丸ごと照合する。
    const stable = ({ timing: _timing, ...value }) => value;
    assert.deepEqual(stable(current.body), stable(old.body), name);
    assert.equal(current.reads.length, old.reads.length, `${name}: SELECT count changed`);
    assert.equal(current.counts.comparisons, old.counts.comparisons, `${name}: comparator ordering changed`);
    if (optimizationBaseline) {
      DB.setBeforeRead(beforeRead?.());
      const previous = await request(optimizationBaseline, path, body, method);
      DB.setBeforeRead(undefined);
      assert.equal(previous.status, expectedStatus, `${name}: optimization baseline status`);
      assert.deepEqual(stable(current.body), stable(previous.body), `${name}: optimization public JSON`);
      assert.deepEqual(current.reads, previous.reads, `${name}: optimization SELECT reads`);
      assert.equal(current.counts.coverage, previous.counts.coverage, `${name}: coverage entry count`);
      assert.equal(current.counts.comparisons, previous.counts.comparisons, `${name}: optimization comparator count`);
      assert.ok(current.counts.metadataParses <= previous.counts.metadataParses, `${name}: metadata parse count`);
      assert.ok(current.counts.excerptWindows <= previous.counts.excerptWindows, `${name}: excerpt window count`);
      metrics.optimizations.push({ name, before: previous.counts, after: current.counts });
    }
    checks.push({ name, passed: true });
    return { old, current };
  }
  async function seed(customId, containerTag, content, metadata = {}, topics = []) {
    const saved = await request(after, "/v3/documents", { customId, containerTag, content, metadata, topics });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    db.prepare(`UPDATE memories SET embedding_json = ?, embedding_status = 'done', embedding_model = 'fixture-embedding',
      fact_status = 'done', fact_model = 'fixture-facts', topic_status = 'done', topic_model = 'fixture-topics',
      embedded_at = ?, facts_extracted_at = ?, topics_extracted_at = ?, vector_attempted_at = ?,
      vector_status = 'pending', vector_mutation_id = 'fixture-mutation', enrichment_error = 'fixture-diagnostic',
      created_at = ?, updated_at = ? WHERE id = ?`).run(embeddingJson, ...Array(6).fill(timestamp), saved.body.id);
    return saved.body.id;
  }
  const index = (title, description = title) => buildMemoryIndex({ title, request: description, sourceUpdatedAt: timestamp });
  const displayIds = [];
  for (let i = 0; i < 50; i++) displayIds.push(await seed(`display-${i}`, "display", `Stored display evidence ${i}`,
    { title: `Evidence ${i}`, filepath: `fixture/${i}.md`, sm_project_id: "display-project", sm_scope: "private", memoryIndex: index(`Evidence ${i}`) }, ["Evidence"]));
  assert.equal((await request(after, `/v3/documents/${displayIds[0]}`, undefined, "GET", false)).status, 401);
  checks.push({ name: "unauthenticated display read rejected", passed: true });

  const readOne = await compare("GET retains all public fields", `/v3/documents/${displayIds[0]}`, undefined, "GET");
  const listFull = await compare("full list retains all public fields", "/v3/documents/list", { containerTag: "display", projection: "full", limit: 50 });
  for (const [name, result, match] of [["GET", readOne, (read) => read.sql.includes("WHERE") && read.sql.includes("embedding") || read.sql.startsWith("SELECT * FROM memories")],
    ["list-full", listFull, (read) => read.sql.includes("ORDER BY m.updated_at DESC LIMIT")]]) {
    const old = result.old.reads.filter(match); const current = result.current.reads.filter(match);
    assert.equal(old.length, 1); assert.equal(current.length, 1);
    assert.equal(old[0].rows, name === "GET" ? 1 : 50); assert.equal(current[0].rows, old[0].rows);
    assert.equal(old[0].embeddings, old[0].rows); assert.equal(current[0].embeddings, 0);
    assert.ok(current[0].rowBytes < old[0].rowBytes / 8, "display SELECT must omit large vectors");
    metrics.selectRows.push({ operation: name, rows: old[0].rows, beforeBytes: old[0].rowBytes, afterBytes: current[0].rowBytes,
      removedBytes: old[0].rowBytes - current[0].rowBytes });
  }
  for (const projection of ["index", "ids", "capture"]) await compare(`list ${projection} unchanged`, "/v3/documents/list", { containerTag: "display", projection, limit: 50 });
  await compare("full list topic filter and pagination", "/v3/documents/list", { containerTag: "display", topic: "Evidence", page: 2, limit: 7 });
  await compare("missing document still returns 404", "/v3/documents/missing", undefined, "GET", 404);

  const searchIds = [];
  for (let i = 0; i < 75; i++) {
    const title = i % 3 === 0 ? "Orion transport" : i % 3 === 1 ? "Orion" : "Transport";
    searchIds.push(await seed(`search-${i}`, i % 2 ? "search-b" : "search-a", `# ${title}\nVerified source ${i}${i % 4 ? "" : " AbortSignal.timeout transport"}`,
      { title, memoryIndex: i % 5 ? index(title) : { version: "legacy" }, padding: "x".repeat(10_000),
        sm_project_id: "search-project", sm_scope: i % 2 ? "public" : "private" }, i % 7 ? [title] : []));
  }
  const checkpointIds = [];
  for (let i = 0; i < 6; i++) {
    const id = await seed(`checkpoint-${i}`, "search-a", "# Orion transport\nCheckpoint evidence",
      { title: "Orion transport", memoryIndex: index("Orion transport"), sm_consolidation: true, sourceCount: 2,
        sourceMemoryIds: [searchIds[i], searchIds[i + 6]], sm_project_id: `project-${i}`, sm_scope: "private" }, ["Orion"]);
    const projectKey = `fixture-project-${i}`;
    db.prepare(`INSERT INTO memory_consolidation_projects (project_key, source_container_tag, revision, created_at, updated_at)
      VALUES (?, 'search-a', 1, ?, ?)`).run(projectKey, timestamp, timestamp);
    db.prepare(`INSERT INTO memory_consolidations (id, project_key, memory_id, revision, status, created_at, updated_at)
      VALUES (?, ?, ?, 1, 'active', ?, ?)`).run(`consolidation-${i}`, projectKey, id, timestamp, timestamp);
    for (const sourceId of [searchIds[i], searchIds[i + 6]]) db.prepare(`INSERT INTO memory_consolidation_sources
      (consolidation_id, memory_id, source_revision) SELECT ?, id, topic_revision FROM memories WHERE id = ?`).run(`consolidation-${i}`, sourceId);
    checkpointIds.push(id);
  }
  const scopes = [undefined, "private", "public"];
  for (const indexOnly of [true, false]) for (const allContainers of [true, false]) for (const scope of scopes) {
    const body = { q: "Orion transport AbortSignal.timeout", indexOnly, limit: 12,
      ...(allContainers ? { allContainers: true } : { containerTag: "search-a" }),
      ...(scope ? { filters: { AND: [{ key: "sm_scope", filterType: "metadata", value: scope }] } } : {}),
    };
    const name = `search index=${indexOnly} global=${allContainers} scope=${scope ?? "all"}`;
    const result = await compare(name, "/v4/search", body);
    assert.ok(result.current.body.results.length <= 12);
    for (const row of result.current.body.results) {
      const stored = JSON.parse(db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(row.id).metadata_json);
      if (scope) assert.equal(stored.sm_scope, scope);
      if (!allContainers) assert.equal(row.containerTag, "search-a");
    }
    metrics.search.push({ name, results: result.current.body.results.length, comparisons: result.current.counts.comparisons,
      coverageBefore: result.old.counts.coverage, coverageAfter: result.current.counts.coverage });
  }
  const checkpointResult = await compare("checkpoint preference and source membership", "/v4/search", { q: "Orion transport", containerTag: "search-a", indexOnly: true, limit: 50 });
  assert.ok(checkpointResult.current.body.results.some((row) => checkpointIds.includes(row.id)), "checkpoint stage not exercised");
  for (const row of checkpointResult.current.body.results.filter((row) => checkpointIds.includes(row.id))) assert.equal(row.metadata.sourceMemoryIds.length, 2);
  await compare("empty query recent sorting", "/v4/search", { q: "", allContainers: true, indexOnly: true, limit: 7 });
  await compare("excerpt-specific query with legacy index hydration", "/v4/search", { q: "AbortSignal.timeout", allContainers: true, indexOnly: true, limit: 50 });
  await compare("no matching term", "/v4/search", { q: "missing-term-xyz", allContainers: true, indexOnly: true, limit: 5 });

  // 原文再取得の直前に一度だけ改訂をずらす。比較Workerごとに同じ変更を行う。
  const staleId = searchIds[0];
  const revision = db.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(staleId).topic_revision;
  const revisionRace = () => {
    db.prepare("UPDATE memories SET topic_revision = ? WHERE id = ?").run(revision, staleId);
    let changed = false;
    return (sql, values) => {
      if (!changed && sql.includes("SELECT id, container_tag, content, metadata_json, created_at, updated_at, topic_revision") && values.includes(staleId)) {
        changed = true;
        db.prepare("UPDATE memories SET topic_revision = 'fixture-late-revision' WHERE id = ?").run(staleId);
      }
    };
  };
  const stale = await compare("source revision recheck after candidate selection", "/v4/search", { q: "Orion", containerTag: "search-a", indexOnly: true, limit: 50 }, "POST", 200, revisionRace);
  assert.ok(!stale.current.body.results.some((row) => row.id === staleId));
  db.prepare("UPDATE memories SET topic_revision = ? WHERE id = ?").run(revision, staleId);

  const semanticId = await seed("semantic-only", "semantic", "Different evidence without the lookup phrase", { title: "Distinct", memoryIndex: index("Distinct") });
  env.AI_ENRICHMENT_MODE = "on";
  const semantic = await compare("semantic fallback keeps stored embeddings", "/v4/search", { q: "neutrino", containerTag: "semantic", limit: 5 });
  assert.ok(semantic.current.body.results.some((row) => row.id === semanticId && row.semanticSimilarity > 0.99));
  assert.ok(semantic.current.reads.some((read) => read.embeddings > 0));
  env.AI_ENRICHMENT_MODE = "off";

  const excerptId = await seed("shared-excerpt-window", "excerpt-window", "# Evidence\n\nalpha beta gamma delta\n" + "原文😀".repeat(100),
    { title: "Evidence", memoryIndex: index("Evidence") });
  const sharedExcerpt = await compare("shared excerpt window through authenticated search", "/v4/search",
    { q: "alpha beta gamma delta", containerTag: "excerpt-window", indexOnly: true, limit: 5 });
  assert.ok(sharedExcerpt.current.body.results.some((row) => row.id === excerptId && row.searchExcerpt));
  assert.ok(sharedExcerpt.current.counts.excerptWindows > 0);
  assert.ok(sharedExcerpt.old.counts.excerptWindows > sharedExcerpt.current.counts.excerptWindows);
  metrics.optimizations.push({ name: "authenticated shared excerpt window", before: sharedExcerpt.old.counts, after: sharedExcerpt.current.counts });

  const unicodeContent = "# Evidence\n\n" + "İ".repeat(100) + " targetneedle answer " + "原文😀".repeat(100);
  const unicodeId = await seed("unicode-excerpt-offset", "unicode-excerpt", unicodeContent,
    { title: "Evidence", memoryIndex: index("Evidence") });
  const unicodeQuery = { q: "targetneedle", containerTag: "unicode-excerpt", indexOnly: true, limit: 5 };
  const unicodeResult = await request(after, "/v4/search", unicodeQuery);
  assert.equal(unicodeResult.status, 200);
  const unicodeExcerpt = unicodeResult.body.results.find((row) => row.id === unicodeId)?.searchExcerpt;
  assert.match(unicodeExcerpt, /targetneedle/u);
  assert.ok(unicodeContent.includes(unicodeExcerpt.replace(/^…|…$/gu, "")));
  if (optimizationBaseline) {
    const original = await request(optimizationBaseline, "/v4/search", unicodeQuery);
    assert.equal(original.status, 200);
    const originalRow = original.body.results.find((row) => row.id === unicodeId);
    assert.ok(originalRow);
    assert.ok(!originalRow.searchExcerpt, "pre-opop source reproduces the Unicode offset defect");
  }
  checks.push({ name: "authenticated excerpt maps expanded lowercase offsets back to original source", passed: true });

  const longUnicodeContent = "İ".repeat(4_000) + " targetneedle answer";
  const fullUnicodeId = await seed("unicode-full-excerpt-offset", "unicode-full-excerpt", longUnicodeContent,
    { title: "Evidence", memoryIndex: index("Evidence") });
  const fullUnicodeQuery = { q: "targetneedle", containerTag: "unicode-full-excerpt", indexOnly: false, limit: 5 };
  const fullUnicodeResult = await request(after, "/v4/search", fullUnicodeQuery);
  assert.equal(fullUnicodeResult.status, 200);
  const fullUnicodeRow = fullUnicodeResult.body.results.find((row) => row.id === fullUnicodeId);
  assert.match(fullUnicodeRow?.content, /targetneedle/u);
  assert.equal(fullUnicodeRow.memory, fullUnicodeRow.content);
  assert.ok(longUnicodeContent.includes(fullUnicodeRow.content.replace(/^…|…$/gu, "")));
  if (optimizationBaseline) {
    const original = await request(optimizationBaseline, "/v4/search", fullUnicodeQuery);
    assert.equal(original.status, 200);
    assert.equal(original.body.results.find((row) => row.id === fullUnicodeId)?.content, "…");
  }
  checks.push({ name: "authenticated manual full-search excerpt maps locale lowercase offsets", passed: true });

  // 比較器のCPUはsynthetic microbenchmarkのみ。APIの待時間短縮やD1の時間短縮を主張しない。
  const topics = new Map();
  const candidates = Array.from({ length: 200 }, (_, i) => ({
    id: `sort-${i % 197}`, content: "", container_tag: "synthetic", created_at: timestamp, updated_at: timestamp,
    metadata_json: JSON.stringify({ memoryIndex: index(i % 6 ? "Orion" : "Orion transport"), padding: "x".repeat(10_000), sm_consolidation: i % 11 === 0 }),
    ...(i % 4 ? {} : { searchExcerpt: "transport AbortSignal.timeout" }),
  }));
  for (const row of candidates) topics.set(row.id, Number(row.id.slice(5)) % 7 ? [] : ["Transport"]);
  const terms = ["orion", "transport", "abortsignal.timeout"];
  for (const [stage, secondary, checkpoint] of [["active-checkpoints", false, false], ["checkpoint-only", false, true], ["index-only", true, true], ["global-full", true, false]]) {
    before.resetCounts(); after.resetCounts(); optimizationBaseline?.resetCounts();
    const orderedBefore = before.sortCoverage([...candidates], topics, terms, secondary, checkpoint);
    const orderedAfter = after.sortCoverage([...candidates], topics, terms, secondary, checkpoint);
    assert.deepEqual(orderedAfter, orderedBefore, `${stage}: exact row ordering, including duplicate IDs and ties`);
    orderedAfter.forEach((row, i) => assert.equal(row, orderedBefore[i], `${stage}: row identity`));
    assert.equal(after.counts().coverage, candidates.length * (secondary ? 2 : 1));
    assert.equal(after.counts().metadataParses, candidates.length, `${stage}: exactly one metadata parse per row`);
    assert.ok(before.counts().coverage > after.counts().coverage);
    if (optimizationBaseline) {
      assert.deepEqual(orderedAfter, optimizationBaseline.sortCoverage([...candidates], topics, terms, secondary, checkpoint));
      assert.equal(optimizationBaseline.counts().metadataParses, candidates.length * (1 + Number(secondary) + Number(checkpoint)));
      metrics.optimizations.push({ name: stage, before: optimizationBaseline.counts(), after: after.counts() });
    }
    // 新たなstageでmetadata/excerptを変えても古いキーを再利用しない。
    const changed = candidates.map((row, i) => i % 3 ? row : { ...row, metadata_json: JSON.stringify({ memoryIndex: index("transport") }), searchExcerpt: "orion" });
    assert.deepEqual(after.sortCoverage([...changed], topics, terms, secondary, checkpoint), before.sortCoverage([...changed], topics, terms, secondary, checkpoint));
    const measure = (module) => {
      module.resetCounts();
      const started = performance.now();
      for (let iteration = 0; iteration < 30; iteration++) module.sortCoverage([...candidates], topics, terms, secondary, checkpoint);
      return { milliseconds: performance.now() - started, ...module.counts() };
    };
    // JITを揃えるため両方を同回数warm-upする。
    for (let iteration = 0; iteration < 3; iteration++) { before.sortCoverage([...candidates], topics, terms, secondary, checkpoint); after.sortCoverage([...candidates], topics, terms, secondary, checkpoint); }
    const old = measure(before); const current = measure(after);
    assert.equal(current.comparisons, old.comparisons);
    metrics.microbenchmark.push({ stage, candidates: candidates.length, iterations: 30, metadataPaddingBytes: 10_000, before: old, after: current });
    checks.push({ name: `${stage} stable ties, distinct same-ID rows and fresh stage keys`, passed: true });
  }

  for (let i = 0; i < 1_000; i++) {
    const content = "前😀".repeat(i % 31) + "alpha beta gamma alpha\n" + "後𠮷😀".repeat(i % 97);
    const terms = i % 4 === 0 ? [] : i % 4 === 1 ? ["missing"]
      : i % 4 === 2 ? ["alpha", "beta", "gamma", "alpha"] : ["😀alpha", "beta", "後", "𠮷"];
    assert.equal(after.searchExcerpt(content, terms), before.searchExcerpt(content, terms), `Unicode excerpt ${i}`);
    if (optimizationBaseline) assert.equal(after.searchExcerpt(content, terms), optimizationBaseline.searchExcerpt(content, terms));
  }
  const sharedWindowContent = "abcdefghijklmnopqrstuvwxyz0123456789" + "😀原文".repeat(100);
  const sharedWindowTerms = Array.from({ length: 30 }, (_, i) => sharedWindowContent.slice(i, i + 2));
  before.resetCounts(); after.resetCounts();
  assert.equal(after.searchExcerpt(sharedWindowContent, sharedWindowTerms), before.searchExcerpt(sharedWindowContent, sharedWindowTerms));
  assert.equal(before.counts().excerptWindows, 30);
  assert.equal(after.counts().excerptWindows, 1);
  metrics.optimizations.push({ name: "30 terms sharing one corrected start", before: before.counts(), after: after.counts() });
  // 補正前は62/63の別位置でも、サロゲートペアの補正後は両方63になる。
  const correctedCollision = "x".repeat(61) + "😀" + "x".repeat(59) + "abc" + "後".repeat(300);
  before.resetCounts(); after.resetCounts();
  assert.equal(after.searchExcerpt(correctedCollision, ["ab", "bc"]), before.searchExcerpt(correctedCollision, ["ab", "bc"]));
  assert.equal(before.counts().excerptWindows, 2);
  assert.equal(after.counts().excerptWindows, 1);
  metrics.optimizations.push({ name: "distinct starts merging after UTF-16 correction", before: before.counts(), after: after.counts() });
  checks.push({ name: "1000 Unicode excerpts, duplicate terms, suffixes and shared corrected starts", passed: true });
  for (const content of ["İ".repeat(100), "ΟΣ.İ😀".repeat(50), "\uD800İ\uDC00".repeat(80)]) {
    const text = content + " targetneedle answer " + "後😀".repeat(100);
    const excerpt = after.searchExcerpt(text, ["targetneedle"]);
    assert.match(excerpt, /targetneedle/u);
    assert.ok(text.includes(excerpt.replace(/^…|…$/gu, "")));
  }
  assert.match(after.searchExcerpt("İ".repeat(100) + " ΟΣ.", ["ος"]), /ΟΣ/u);
  assert.match(after.searchExcerpt("x".repeat(100) + "İ", ["i\u0307"]), /İ/u);
  assert.match(after.searchExcerpt("İ".repeat(100) + "😀", ["😀"]), /😀/u);
  checks.push({ name: "expanded lowercase offsets retain contextual sigma, surrogate positions and original text", passed: true });
  const originalLocaleLower = String.prototype.toLocaleLowerCase;
  try {
    for (const locale of ["en", "tr", "lt"]) {
      String.prototype.toLocaleLowerCase = function () { return Reflect.apply(originalLocaleLower, this, [locale]); };
      for (const prefix of ["İ".repeat(4_000), "I\u0307".repeat(3_000), "I\u0323\u0307".repeat(2_000), "I\u0301😀".repeat(1_500)]) {
        const content = prefix + " targetneedle answer";
        const excerpt = after.sourceExcerpt(content, "targetneedle", 4_000);
        assert.match(excerpt, /targetneedle/u);
        assert.ok(content.includes(excerpt.replace(/^…|…$/gu, "")));
      }
    }
  } finally { String.prototype.toLocaleLowerCase = originalLocaleLower; }
  assert.equal(String.prototype.toLocaleLowerCase, originalLocaleLower);
  checks.push({ name: "locale source excerpts preserve Turkish contractions and Lithuanian context", passed: true });
  artifact = { schemaVersion: 1, command: "node --experimental-transform-types server/tests/opop-performance.mjs", node: process.version,
    sourceHashes: { "server/src/index.ts": hash(sourceBytes), comparisonWorker: hash(baselineSource),
      ...(optimizationBaselineSource ? { optimizationBaseline: hash(optimizationBaselineSource) } : {}),
      "server/tests/opop-performance.mjs": hash(readFileSync(import.meta.filename)) },
    baseline: "current working source with precomputed comparator, display projection and excerpt deduplication reversed in memory; no Git HEAD dependency",
    ...(comparisonRoot ? { optimizationComparisonRoot: comparisonRoot } : {}),
    migrations: migrations.map((name) => ({ name, sha256: hash(readFileSync(resolve(root, "server/migrations", name))) })),
    assumptions: { authenticatedActualWorker: true, database: "migration-backed in-memory SQLite via D1 adapter", syntheticEmbeddingDimensions: 1024,
      embeddingJsonBytes: Buffer.byteLength(embeddingJson), displayRows: 50, searchRows: 75, activeCheckpoints: 6,
      deterministicAiAndEmptyVectorFixtures: true, rowBytes: "sum of UTF-8 JSON serialized actual SELECT rows; excludes D1 wire framing",
      cpu: "synthetic sort microbenchmark only; timings are observational and not pass/fail thresholds", excludedComparisonField: "API timing" },
    passed: true, checks, metrics };
} finally {
  db.close();
}
mkdirSync(resolve(root, "dist"), { recursive: true });
writeFileSync(resolve(root, "dist/opop-performance-validation.json"), JSON.stringify(artifact, null, 2) + "\n");
console.log(JSON.stringify({ passed: artifact.checks.length, selectRows: metrics.selectRows, microbenchmark: metrics.microbenchmark,
  artifact: "dist/opop-performance-validation.json" }, null, 2));
