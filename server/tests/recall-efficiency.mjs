// 実行: node --experimental-transform-types server/tests/recall-efficiency.mjs
// 失敗条件: 領域数に比例する通信/AI呼出、100領域上限、scope漏れ、障害時の検索消失、
// stale vector混入、原文固有語欠落/semantic自動注入、legacy出典破壊、checkpoint過剰抑制、
// 互換fallback誤判定、空prompt通信、パス内のプロジェクト欠落、識別子の見出し不一致、
// バージョン番号の過剰除外。実APIと実clientを全migration済みSQLiteへ接続する。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import worker from "../src/index.ts";
import { api as clientApi, searchIndex, listIndex, readDocument } from "../../client/memory-client.mjs";
import { buildTurnDocuments, sanitizeText } from "../../client/Import-RemnifoldHistory.mjs";
import { runHook } from "../../client/memory-hooks.mjs";
import { callTool } from "../../client/mcp-server.mjs";
import { buildMemoryIndex } from "../../client/memory-index.mjs";

const artifactPath = resolve(import.meta.dirname, "../../dist/recall-efficiency-validation.json");
const migrations = readdirSync(resolve(import.meta.dirname, "../migrations")).filter((name) => name.endsWith(".sql")).sort();
const embedding = Array.from({ length: 1024 }, (_, index) => index === 0 ? 1 : 0);
const opposite = embedding.map((value) => -value);
const settings = { recallMode: "direct", maxMemories: 5, minimumSimilarity: 0.7 };
const context = { projectId: "fixture-project", projectName: "fixture", containerTag: "memories", projectTags: ["memories"], sharedTags: [], readTags: ["memories"] };
const sourceDate = "2025-03-04T12:00:00.000Z";
const namespace = (tag) => `ct_${createHash("sha256").update(tag).digest("hex").slice(0, 48)}`;
const completion = (value) => ({ choices: [{ finish_reason: "stop", message: { content: JSON.stringify(value) } }] });

function adapter(database) {
  function prepare(sql) {
    const bind = (values = []) => ({ sql, values, bind: (...next) => bind(next),
      first: async () => database.prepare(sql).get(...values),
      all: async () => ({ results: database.prepare(sql).all(...values) }),
      run: async () => ({ meta: { changes: Number(database.prepare(sql).run(...values).changes) } }),
      execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
        ? { results: database.prepare(sql).all(...values), meta: { changes: 0 } }
        : { results: [], meta: { changes: Number(database.prepare(sql).run(...values).changes) } },
    });
    return bind();
  }
  return { prepare, batch: async (operations) => {
    database.exec("BEGIN");
    try { const results = operations.map((operation) => operation.execute()); database.exec("COMMIT"); return results; }
    catch (error) { database.exec("ROLLBACK"); throw error; }
  } };
}

function fixture() {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  for (const migration of migrations) db.exec(readFileSync(resolve(import.meta.dirname, "../migrations", migration), "utf8"));
  const pending = [];
  const ctx = { waitUntil(promise) { pending.push(Promise.resolve(promise)); } };
  const metrics = { requests: [], embeddings: 0, vectorQueries: [], responseBytes: 0 };
  const vectors = new Map();
  let vectorFailure = false;
  let aiFailure = false;
  let transportStatus = 0;
  let legacyServer = false;
  let markerless = false;
  let networkFailure = false;
  const env = { DB: adapter(db), MEMORY_API_KEY: "deterministic-local-key", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      if (input.text) { metrics.embeddings++; if (aiFailure) throw new Error("injected-ai-unavailable"); return { data: [embedding] }; }
      if (input.response_format?.json_schema?.name === "memory_consolidation") return completion({ title: "Orion checkpoint", overview: "Orion implementation state", verified: ["Orion implemented"], unverified: [], unresolved: [], nextActions: [] });
      return completion({ facts: [], topics: ["Orion"] });
    } },
    MEMORY_VECTORS: {
      query: async (_vector, options) => {
        metrics.vectorQueries.push(options);
        if (vectorFailure) throw new Error("injected-vector-unavailable");
        const matches = [...vectors.values()].filter((v) => !options.namespace || v.namespace === options.namespace)
          .map((v) => ({ id: v.id, score: v.score ?? 0.99, metadata: v.metadata })).slice(0, options.topK);
        return { matches, count: matches.length };
      },
      upsert: async (rows) => { for (const row of rows) vectors.set(row.id, row); return { mutationId: "fixture-mutation" }; },
      deleteByIds: async (ids) => { for (const id of ids) vectors.delete(id); return {}; },
      getByIds: async (ids) => ids.map((id) => vectors.get(id)).filter(Boolean),
    },
  };
  async function fetchImpl(url, options = {}) {
    const request = new Request(url, options);
    const path = new URL(request.url).pathname;
    const body = options.body ? JSON.parse(options.body) : undefined;
    metrics.requests.push({ path, method: request.method, ...(body ? { body } : {}) });
    if (path === "/v4/search" && body?.allContainers && networkFailure) throw new Error("injected-network-unavailable");
    let response;
    if (path === "/v4/search" && body?.allContainers && transportStatus) response = Response.json({ error: "injected-transport-error" }, { status: transportStatus });
    else if (path === "/v4/search" && body?.allContainers && legacyServer) {
      // 実Workerのvalidationからnested errorを返す。手製のErrorだけでは互換を検証しない。
      const legacyBody = { ...body }; delete legacyBody.allContainers;
      response = await worker.fetch(new Request(request.url, { method: "POST", headers: request.headers, body: JSON.stringify(legacyBody) }), env, ctx);
    }
    else {
      response = await worker.fetch(request, env, ctx);
      if (markerless && body?.allContainers && response.ok) {
        const result = await response.json(); delete result.searchScope; delete result.spaceDiscoveryComplete; delete result.searchedContainers;
        response = Response.json(result);
      }
      if (legacyServer && path === "/v4/search" && response.ok) {
        const result = await response.json();
        // 1.x検索サーバーは旧文書の派生索引を返さず、詳細GETを必要とする。
        for (const row of result.results) {
          const saved = JSON.parse(db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(row.id).metadata_json);
          if (!saved.memoryIndex) { row.metadata = saved; delete row.searchExcerpt; }
        }
        response = Response.json(result);
      }
    }
    metrics.responseBytes += Buffer.byteLength(await response.clone().text());
    return response;
  }
  const request = (path, options = {}) => clientApi(path, { ...options, config: { baseUrl: "https://local.example", apiKey: env.MEMORY_API_KEY }, fetchImpl });
  const raw = async (path, body, method = "POST") => {
    const response = await fetchImpl(`https://local.example${path}`, { method, headers: { Authorization: `Bearer ${env.MEMORY_API_KEY}`, "Content-Type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    return { status: response.status, body: await response.json() };
  };
  const drain = async () => { while (pending.length) await Promise.allSettled(pending.splice(0)); };
  const seed = async (containerTag, content, metadata = {}, { indexed = true } = {}) => {
    const saved = await raw("/v3/documents", { containerTag, content, metadata });
    assert.equal(saved.status, 201, JSON.stringify(saved.body)); await drain();
    if (indexed) {
      db.prepare("UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'indexed' WHERE id = ?").run(JSON.stringify(embedding), saved.body.id);
      const row = db.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(saved.body.id);
      vectors.set(saved.body.id, { id: saved.body.id, namespace: namespace(containerTag), metadata: { topic_revision: row.topic_revision }, score: 0.99 });
    }
    return saved.body.id;
  };
  const reset = () => { metrics.requests = []; metrics.embeddings = 0; metrics.vectorQueries = []; metrics.responseBytes = 0; env.AI_ENRICHMENT_MODE = "on"; };
  const search = (query, options = {}) => searchIndex({ query, request, settings, context, ...options });
  const hook = (prompt) => runHook("UserPromptSubmit", { prompt }, { request, settings, context });
  return { db, env, request, raw, seed, search, hook, drain, reset, vectors, metrics,
    failVector(value = true) { vectorFailure = value; }, failAI(value = true) { aiFailure = value; },
    markerless(value = true) { markerless = value; }, network(value = true) { networkFailure = value; },
    transport(value) { transportStatus = value; }, legacy(value = true) { legacyServer = value; },
    async checkpoint(projectId, sources) {
      for (const id of sources) db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.sm_project_id', ?) WHERE id = ?").run(projectId, id);
      env.AI_ENRICHMENT_MODE = "on";
      const result = await raw("/v4/consolidate", { projectId, force: true });
      assert.equal(result.status, 200, JSON.stringify(result.body)); assert.equal(result.body.status, "consolidated", JSON.stringify(result.body)); await drain();
      return result.body.memoryId;
    },
  };
}

const cases = [];
const scenario = (name, run) => cases.push({ name, run });
const ids = (result) => result.results.map((row) => row.id);
const index = (title, request = title) => ({ memoryIndex: buildMemoryIndex({ title, request, sourceUpdatedAt: sourceDate }) });

scenario("automatic project-path recall excludes machine prefixes and generic request noise", async (f) => {
  const wanted = await f.seed("other-folder", "# Remnifold\n索引の読込", index("Remnifold", "索引の読込"));
  const noise = [];
  for (const [title, request] of [["Kagayoi.Support", String.raw`C:\Users\IMT\dev\Kagayoi.Support`], ["請求書", "修正してください"], ["Release", "Developer release"]]) {
    noise.push(await f.seed("unrelated-folder", `# ${title}\n${request}`, index(title, request)));
  }
  f.reset(); const hook = await f.hook(String.raw`"C:\Users\IMT\dev\remnifold" メモリー機能側で修正で最適化が出来そうな部分が発見できていたら修正してください`);
  const text = hook.hookSpecificOutput?.additionalContext || "";
  assert.ok(text.includes(wanted)); assert.ok(noise.every((id) => !text.includes(id)));
  assert.equal(f.metrics.requests.length, 1);
  assert.doesNotMatch(f.metrics.requests[0].body.q, /Users|IMT|\bdev\b/u);
  return { selected: 1, excluded: noise.length, requests: f.metrics.requests.length, crossFolderMatch: true };
});
scenario("ASCII word fragments stay out of automatic recall while manual semantic search remains", async (f) => {
  const wanted = await f.seed("development", "# Dev tools\nDevelopment tools", index("Dev tools"));
  const unrelated = await f.seed("release", "# Developer release\nRelease history", index("Developer release"));
  f.reset(); const result = await f.search("dev", { automatic: true });
  assert.ok(ids(result).includes(wanted)); assert.ok(!ids(result).includes(unrelated));
  f.reset(); assert.ok(ids(await f.search("dev")).includes(unrelated));
  const dotnet = await f.seed("dotnet", "# .NET8 runtime\nRuntime behavior", index(".NET8 runtime"));
  const node = await f.seed("node", "# Node.js24 runtime\nRuntime behavior", index("Node.js24 runtime"));
  const wrongVersion = await f.seed("other-version", "# .NET80 runtime\nDifferent version", index(".NET80 runtime"));
  f.reset(); assert.ok(ids(await f.search(".NET", { automatic: true })).includes(dotnet));
  f.reset(); assert.ok(ids(await f.search("Node.js", { automatic: true })).includes(node));
  f.reset(); const versioned = await f.search(".NET8", { automatic: true });
  assert.ok(ids(versioned).includes(dotnet)); assert.ok(!ids(versioned).includes(wrongVersion));
});
scenario("file paths preserve the project without recalling another project README", async (f) => {
  const wanted = await f.seed("other-folder", "# Remnifold\n索引の読込", index("Remnifold", "索引の読込"));
  const unrelated = await f.seed("other-project", "# README.md\nDocumentation changes", index("README.md"));
  f.reset(); const result = await f.hook(String.raw`"C:\Users\IMT\dev\remnifold\client\memory-client.mjs" を修正してください`);
  const text = result.hookSpecificOutput?.additionalContext || "";
  assert.ok(text.includes(wanted)); assert.ok(!text.includes(unrelated));
  assert.match(f.metrics.requests[0].body.q, /remnifold/u);
  assert.doesNotMatch(f.metrics.requests[0].body.q, /Users|IMT|\bdev\b|memory-client/u);
  const support = await f.seed("support-folder", "# Kagayoi.Support\nSupport service", index("Kagayoi.Support"));
  const code = await f.seed("code-folder", "# Code\nCode samples", index("Code"));
  for (const query of [String.raw`"C:\Code\Kagayoi.Support"`, String.raw`"C:\Code\Kagayoi.Support\"`, '"/opt/Kagayoi.Support"']) {
    f.reset(); const result = await f.search(query, { automatic: true });
    assert.ok(ids(result).includes(support)); assert.ok(!ids(result).includes(code));
  }
});
scenario("legacy hydration uses the same identifier matching for the selected heading", async (f) => {
  const wanted = await f.seed("legacy", "## Gardening\nFlower care\n\n## Remnifold\n索引の読込", { sm_project_id: "legacy-source", sourceTimestamp: sourceDate });
  f.legacy(); f.reset(); const result = await f.search("remnifold", { automatic: true });
  const row = result.results.find((candidate) => candidate.id === wanted);
  assert.ok(row); assert.equal(row.title, "Remnifold"); assert.doesNotMatch(row.description, /Flower/u);
  assert.equal(row.provenance.projectId, "legacy-source"); assert.equal(row.sourceUpdatedAt, sourceDate);
  assert.ok(f.metrics.requests.some((request) => request.path === `/v3/documents/${wanted}`));
});
scenario("generic repair wording skips automatic requests but remains manually searchable", async (f) => {
  const wanted = await f.seed("manual-history", "# 最適化の記録\n修正内容", index("最適化の記録"));
  f.reset(); assert.deepEqual(await f.hook("修正して最適化してください"), {});
  assert.equal(f.metrics.requests.length, 0); assert.equal(f.metrics.embeddings, 0);
  f.reset(); assert.ok(ids(await f.search("最適化")).includes(wanted));
});

scenario("59 legacy spaces: search, hook and MCP each use one API/embed/vector and zero document GET", async (f) => {
  const wanted = await f.seed("space-58", "# AbortSignal timeout\n\nAbortSignal timeout implementation evidence", { sm_project_id: "legacy-project", sourceTimestamp: sourceDate });
  for (let i = 0; i < 58; i++) await f.seed(`space-${i}`, `# Gardening ${i}\n\nUnrelated flowers`);
  const runs = [];
  f.metrics.runs = runs;
  for (const [route, run] of [["searchIndex", () => f.search("AbortSignal timeout", { automatic: true })], ["runHook", () => f.hook("AbortSignal timeout")], ["MCP callTool", () => callTool("search_memory", { query: "AbortSignal timeout" }, { request: f.request, settings, context })]]) {
    f.reset(); const start = performance.now(); const result = await run();
    runs.push({ route, elapsedMs: performance.now() - start, requests: f.metrics.requests.length, embeddings: f.metrics.embeddings, vectorQueries: f.metrics.vectorQueries.length, responseBytes: f.metrics.responseBytes });
    const body = route === "runHook" ? JSON.stringify(result) : JSON.stringify(route === "MCP callTool" ? result.structuredContent : result);
    assert.ok(body.includes(wanted), `${route} missed legacy matching record`);
    assert.equal(f.metrics.requests.length, 1, `${route} multiplied API calls`);
    assert.equal(f.metrics.embeddings, 1); assert.equal(f.metrics.vectorQueries.length, 1);
    assert.equal(f.metrics.vectorQueries[0].namespace, undefined);
  }
  return { runs };
});
scenario("101 spaces includes oldest tail beyond discovery limit", async (f) => {
  const wanted = await f.seed("oldest-tail", "# NeedleTail\n\nNeedleTail historical evidence");
  for (let i = 0; i < 100; i++) await f.seed(`recent-${i}`, `# Other ${i}\n\nUnrelated archive`);
  f.db.prepare("UPDATE container_tags SET updated_at = '2000-01-01T00:00:00.000Z' WHERE tag = 'oldest-tail'").run();
  f.reset(); const result = await f.search("NeedleTail", { automatic: true });
  assert.ok(ids(result).includes(wanted)); assert.equal(result.searchScope, "all-containers");
  assert.equal(result.spaceDiscoveryComplete, true); assert.equal(result.searchedContainers.length, 101);
  assert.equal(f.metrics.requests.length, 1);
});
scenario("explicit container and scope retain namespace and reject cross-space hits", async (f) => {
  const wanted = await f.seed("chosen", "# ScopeNeedle\nScopeNeedle allowed", { ...index("ScopeNeedle"), sm_scope: "selected" });
  await f.seed("chosen", "# ScopeNeedle\nScopeNeedle forbidden scope", { ...index("ScopeNeedle"), sm_scope: "other" });
  await f.seed("elsewhere", "# ScopeNeedle\nScopeNeedle forbidden space", index("ScopeNeedle"));
  f.reset(); const result = await f.raw("/v4/search", { containerTag: "chosen", q: "ScopeNeedle", indexOnly: true, filters: { AND: [{ key: "sm_scope", filterType: "metadata", value: "selected" }] } });
  assert.equal(result.status, 200); assert.deepEqual(ids(result.body), [wanted]);
  assert.equal(f.metrics.vectorQueries[0].namespace, namespace("chosen"));
  f.reset(); assert.ok((await f.search("ScopeNeedle", { containerTag: "chosen", automatic: true })).results.every((row) => row.containerTag === "chosen"));
});
scenario("Vectorize failure falls back to current D1 embedding; AI failure preserves lexical search", async (f) => {
  const wanted = await f.seed("failure-space", "# ResilienceNeedle\nResilienceNeedle evidence", index("ResilienceNeedle"));
  f.reset(); f.failVector(); const semantic = await f.raw("/v4/search", { allContainers: true, q: "DifferentSemanticQuery", indexOnly: true });
  assert.equal(semantic.status, 200); assert.ok(ids(semantic.body).includes(wanted)); assert.equal(semantic.body.results.find((row) => row.id === wanted).semanticSimilarity, 1);
  f.reset(); f.failVector(false); f.failAI(); assert.ok(ids(await f.search("ResilienceNeedle", { automatic: true })).includes(wanted));
});
scenario("forgotten and revised sources reject late stale vector hits", async (f) => {
  const forgotten = await f.seed("race", "# ObsoleteNeedle\nObsoleteNeedle", index("ObsoleteNeedle"));
  const changed = await f.seed("race", "# ObsoleteNeedle\nObsoleteNeedle", index("ObsoleteNeedle"));
  const staleForgotten = f.vectors.get(forgotten); const staleChanged = f.vectors.get(changed);
  await f.raw("/v4/memories", { containerTag: "race", documentId: forgotten }, "DELETE");
  await f.raw(`/v3/documents/${changed}`, { content: "# Replacement\nEntirely different evidence", metadata: index("Replacement") }, "PATCH"); await f.drain();
  f.db.prepare("UPDATE memories SET embedding_json = ?, vector_status = 'indexed' WHERE id = ?").run(JSON.stringify(opposite), changed);
  f.vectors.set(forgotten, staleForgotten); f.vectors.set(changed, staleChanged);
  f.reset(); const result = await f.search("ObsoleteNeedle"); assert.ok(!ids(result).includes(forgotten)); assert.ok(!ids(result).includes(changed));
});
scenario("body-only query returns bounded original excerpt, but unrelated semantic records stay out of hook", async (f) => {
  const wanted = await f.seed("answers", `# Request transport\n### User request\nImprove request reliability\n### Final assistant response\n${"Background detail. ".repeat(50)}Use AbortSignal.timeout to bound fetch execution.`, index("Request transport", "Improve request reliability"));
  const unrelated = await f.seed("answers", "# Gardening\nFlower arrangement", index("Gardening"));
  f.reset(); const apiResult = await f.raw("/v4/search", { allContainers: true, q: "AbortSignal.timeout", indexOnly: true });
  assert.equal(apiResult.status, 200); const row = apiResult.body.results.find((row) => row.id === wanted);
  assert.ok(row); assert.match(row.searchExcerpt, /AbortSignal\.timeout/u); assert.ok(row.searchExcerpt.length <= 240);
  assert.equal(row.metadata.memoryIndex.title, "Request transport");
  f.reset(); const hook = await f.hook("AbortSignal.timeout"); const text = hook.hookSpecificOutput?.additionalContext || "";
  assert.ok(text.includes(wanted)); assert.match(text, /原文|original (?:quote|excerpt)/iu); assert.match(text, /AbortSignal\.timeout/u); assert.ok(!text.includes(unrelated));
  assert.equal(f.metrics.requests.length, 1); assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM facts").get().n, 0);
  f.reset(); assert.ok(ids(await f.search("AbortSignal.timeout")).includes(unrelated));
});
scenario("recallable false stays manually discoverable and never automatically injected", async (f) => {
  const id = await f.seed("ack", "# AcknowledgementNeedle\nAcknowledgementNeedle", { memoryIndex: { ...buildMemoryIndex({ title: "AcknowledgementNeedle", request: "AcknowledgementNeedle" }), recallable: false } });
  f.reset(); assert.ok(ids(await f.search("AcknowledgementNeedle")).includes(id));
  f.reset(); assert.deepEqual(await f.hook("AcknowledgementNeedle"), {});
});
scenario("legacy derives query heading/date/provenance without persisting metadata or losing original GET", async (f) => {
  const content = "# Codex saved memory import: archive\nupdated_at: 2025-03-04T12:00:00.000Z\n## Gardening\nFlower care\n## Needle transport\nNeedle transport query-specific evidence";
  const id = await f.seed("legacy", content, { sm_project_id: "legacy-source", filepath: "rollout_summaries/example.md" });
  const before = f.db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(id).metadata_json;
  f.reset(); const result = await f.search("Needle transport", { automatic: true }); const row = result.results.find((row) => row.id === id);
  assert.ok(row); assert.match(row.title, /Needle transport/u); assert.equal(row.sourceUpdatedAt, sourceDate); assert.equal(row.provenance.projectId, "legacy-source");
  assert.equal(f.metrics.requests.filter((r) => r.method === "GET").length, 0);
  assert.equal(f.db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(id).metadata_json, before);
  const document = await readDocument(id, f.request); assert.equal(document.document.content, content);
});
scenario("legacy list projection builds compact index and preserves authoritative empty topics and provenance", async (f) => {
  const id = await f.seed("legacy-list", "# 旧文書\n一覧だけで確認できる要点\n\nOriginal archival detail.", {
    topics: ["Obsolete metadata topic"], sourceTimestamp: sourceDate, sm_project_id: "legacy-list-source", project: "Legacy project", filepath: "archive/source.md",
  });
  // 旧metadataだけにtopicが残り、正本のtopic関連は空である永続状態を再現する。
  f.db.prepare("DELETE FROM memory_topics WHERE memory_id = ?").run(id);
  f.reset(); const result = await listIndex({ containerTag: "legacy-list", request: f.request });
  const row = result.documents.find((row) => row.id === id);
  assert.ok(row); assert.equal(row.title, "旧文書"); assert.equal(row.description, "一覧だけで確認できる要点");
  assert.deepEqual(row.topics, []); assert.equal(row.sourceUpdatedAt, sourceDate);
  // publicIndexは出典を省く簡潔schema。実API/原文/recallの出典契約とは分けて確認する。
  assert.equal(row.provenance, undefined);
  assert.equal(f.metrics.requests.length, 1); assert.equal(f.metrics.requests[0].body.projection, "index");
  const raw = await f.raw("/v3/documents/list", { containerTag: "legacy-list", projection: "index", page: 1, limit: 10 });
  assert.equal(raw.status, 200); const compact = raw.body.documents.find((row) => row.id === id);
  assert.equal(compact.content, undefined); assert.ok(compact.summary.includes("一覧だけで確認できる要点")); assert.deepEqual(compact.topics, []);
  assert.equal(compact.provenance.projectId, "legacy-list-source"); assert.equal(compact.provenance.containerTag, "legacy-list");
  const original = await readDocument(id, f.request);
  assert.equal(original.document.provenance.projectId, "legacy-list-source"); assert.equal(original.document.provenance.filepath, "archive/source.md");
  assert.equal(original.document.index.provenance, undefined);
  const recalled = await f.search("旧文書", { automatic: true });
  assert.equal(recalled.results.find((row) => row.id === id).provenance.projectId, "legacy-list-source");
});
scenario("substantial brief continuation capture is redacted, retry-stable and recalled through its timeout section", async (f) => {
  const secret = "fixture-configured-sensitive-value";
  const assistant = sanitizeText(`## timeoutの修正\nAbortSignal.timeoutで取得期限を設定しました。設定値 ${secret} は秘匿します。\n## 検証\n実際のAPI経路で期限超過を確認しました。`, [secret], { count: 0 });
  const candidate = { meta: { id: "continuation-session", rootSessionId: "continuation-session", isSubagent: false } };
  const project = { projectName: "Transport project", containerTag: "transport-project" };
  const turn = { user: "続けて", assistant, sourceTimestamp: sourceDate };
  const [document] = buildTurnDocuments(candidate, { turns: [turn] }, project, "接続経路の改善");
  const [repeat] = buildTurnDocuments(candidate, { turns: [turn] }, project, "接続経路の改善");
  const [datedAgain] = buildTurnDocuments(candidate, { turns: [{ ...turn, sourceTimestamp: "2026-09-30T00:00:00.000Z" }] }, project, "別タイトル");
  const body = `### User request\n続けて\n\n### Final assistant response\n${assistant}`;
  const digest = createHash("sha256").update(body).digest("hex");
  const captureKey = `continuation-session:1:${digest}:1:${digest.slice(0, 16)}`;
  assert.equal(document.customId, `codex-turn-v2:${captureKey}`); assert.equal(document.metadata.captureKey, captureKey);
  assert.equal(repeat.customId, document.customId); assert.equal(datedAgain.customId, document.customId); assert.equal(datedAgain.metadata.captureKey, captureKey);
  assert.equal(document.metadata.memoryIndex.recallable, true); assert.match(document.metadata.memoryIndex.description, /継続結果/u);
  assert.ok(document.metadata.memoryIndex.sections.includes("timeoutの修正")); assert.ok(!JSON.stringify(document).includes(secret)); assert.match(document.content, /\[REDACTED\]/u);
  const first = await f.raw("/v3/documents", document); assert.equal(first.status, 201);
  const reused = await f.raw("/v3/documents", repeat); assert.equal(reused.status, 201); assert.equal(reused.body.id, first.body.id); assert.equal(reused.body.reusedExistingCapture, true);
  f.reset(); const hook = await f.hook("timeout"); assert.ok(hook.hookSpecificOutput?.additionalContext.includes(first.body.id));
  const original = await readDocument(first.body.id, f.request); assert.equal(original.document.content, document.content);
  assert.ok(!JSON.stringify(original).includes(secret)); assert.equal(original.document.index.recallable, true); assert.equal(original.document.index.sourceUpdatedAt, sourceDate);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memories WHERE custom_id = ?").get(document.customId).n, 1);
});
scenario("checkpoint preserves specific uncovered original terms, suppresses ordinary duplicates and manual retains sources", async (f) => {
  const ordinary = await f.seed("memories", "# Orion implemented\nOrion implemented", index("Orion implemented"));
  const specific = await f.seed("memories", "# Orion AbortSignal.timeout\nOrion AbortSignal.timeout implementation detail", index("Orion AbortSignal.timeout"));
  const summary = await f.checkpoint("orion-project", [ordinary, specific]);
  f.reset(); const detail = await f.search("Orion AbortSignal.timeout", { automatic: true }); assert.ok(ids(detail).includes(summary)); assert.ok(ids(detail).includes(specific));
  f.reset(); const usual = await f.search("Orion implemented", { automatic: true }); assert.ok(ids(usual).includes(summary)); assert.ok(!ids(usual).includes(ordinary));
  f.reset(); const manual = await f.search("Orion implemented"); assert.ok(ids(manual).includes(ordinary));
});
scenario("unselected checkpoint cannot suppress a source that fills the remaining result budget", async (f) => {
  const sources = [];
  for (let i = 0; i < 6; i++) {
    const id = await f.seed("memories", `# Orion project${i}\nOrion implementation`, index(`Orion project${i}`, "Orion implementation")); sources.push(id);
    await f.checkpoint(`budget-project-${i}`, [id]);
  }
  // checkpointを全て検索候補に残した上で、1つの原文だけ具体語を含む。
  const last = sources.at(-1);
  f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.title', 'Orion ExactBudgetNeedle', '$.memoryIndex.description', 'Orion ExactBudgetNeedle') WHERE id = ?").run(last);
  // 特定のsummaryがbudgetから外れるよう、現在の保存日時を固定する。
  const owner = f.db.prepare("SELECT consolidation.memory_id AS id FROM memory_consolidations consolidation JOIN memory_consolidation_sources source ON source.consolidation_id = consolidation.id WHERE source.memory_id = ?").get(last).id;
  f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.consolidationCreatedAt', '2000-01-01T00:00:00.000Z') WHERE id = ?").run(owner);
  f.reset(); const result = await f.search("Orion ExactBudgetNeedle", { automatic: true, limit: 5 });
  assert.ok(ids(result).includes(last), "source with concrete query evidence was suppressed by checkpoint candidates");
  assert.equal(result.results.length, 5);
});
scenario("higher-relevance source survives its lower-relevance unselected checkpoint", async (f) => {
  const a = await f.seed("memories", "# Orion implemented\nOrion implemented", index("Orion implemented"));
  const b = await f.seed("memories", "# Orion implemented\nOrion implemented", index("Orion implemented"));
  const summaryA = await f.checkpoint("selected-project", [a]);
  const summaryB = await f.checkpoint("unselected-project", [b]);
  f.db.prepare("UPDATE memories SET content = '# Orion overview\nOrion', metadata_json = json_set(metadata_json, '$.memoryIndex.title', 'Orion', '$.memoryIndex.description', 'Orion', '$.memoryIndex.sections', json('[]')) WHERE id = ?").run(summaryB);
  f.reset(); const result = await f.search("Orion implemented", { automatic: true, limit: 2 });
  assert.deepEqual(new Set(ids(result)), new Set([summaryA, b])); assert.ok(!ids(result).includes(summaryB));
});
scenario("global checkpoint membership includes only candidate sources but retains full count", async (f) => {
  const sources = [];
  for (let i = 0; i < 25; i++) sources.push(await f.seed("memories", `# Orion source ${i}\nOrion evidence ${i}`, index(`Orion source ${i}`)));
  const summary = await f.checkpoint("membership-project", sources);
  f.reset(); const result = await f.raw("/v4/search", { allContainers: true, q: "Orion", indexOnly: true, limit: 3 });
  assert.equal(result.status, 200); const row = result.body.results.find((r) => r.id === summary); assert.ok(row);
  assert.equal(row.metadata.sourceCount, 25); assert.equal(row.metadata.sourceMemoryIdsTruncated, true);
  const candidates = new Set(ids(result.body)); assert.ok(row.metadata.sourceMemoryIds.every((id) => candidates.has(id))); assert.ok(row.metadata.sourceMemoryIds.length <= 2);
});
scenario("old server 400 uses discovery and legacy hydration", async (f) => {
  const wanted = await f.seed("old", "# CompatibilityNeedle\nCompatibilityNeedle original");
  f.legacy(); f.reset(); const result = await f.search("CompatibilityNeedle", { automatic: true });
  assert.ok(ids(result).includes(wanted)); assert.ok(f.metrics.requests.some((r) => r.body?.allContainers));
  assert.ok(f.metrics.requests.some((r) => r.path === "/v3/container-tags")); assert.ok(f.metrics.requests.some((r) => r.path === `/v3/documents/${wanted}`));
});
scenario("markerless legacy success uses discovery compatibility", async (f) => {
  const wanted = await f.seed("markerless", "# MarkerNeedle\nMarkerNeedle", index("MarkerNeedle"));
  f.reset(); f.markerless(); assert.ok(ids(await f.search("MarkerNeedle", { automatic: true })).includes(wanted));
  assert.ok(f.metrics.requests.some((r) => r.path === "/v3/container-tags"));
});
scenario("network failure does not multiply fallback requests", async (f) => {
  await f.seed("network", "# NetworkNeedle\nNetworkNeedle", index("NetworkNeedle")); f.reset(); f.network();
  await assert.rejects(f.search("NetworkNeedle"), /network-unavailable/u); assert.equal(f.metrics.requests.length, 1);
});
scenario("original excerpt escapes markup before hook injection", async (f) => {
  const id = await f.seed("escape", "# Transport\nTransport reliability\n### Final assistant response\nUse EscapeNeedle <script>alert('archive')</script> & evidence.", index("Transport", "Transport reliability"));
  f.reset(); const hook = await f.hook("EscapeNeedle"); const text = hook.hookSpecificOutput?.additionalContext || "";
  assert.ok(text.includes(id)); assert.match(text, /EscapeNeedle/u); assert.ok(!text.includes("<script>")); assert.ok(text.includes("&amp;"));
});
for (const status of [401, 500]) scenario(`global HTTP ${status} fails without per-space fallback`, async (f) => {
  await f.seed("errors", "# ErrorNeedle\nErrorNeedle", index("ErrorNeedle")); f.reset(); f.transport(status);
  await assert.rejects(f.search("ErrorNeedle")); assert.equal(f.metrics.requests.length, 1); assert.equal(f.metrics.requests[0].body.allContainers, true);
});
scenario("checkpoint crowding cannot evict original covering the concrete query term", async (f) => {
  // 失敗条件: limitより多い半語一致の清書で、両語一致する別space原文が消える。
  for (let i = 0; i < 41; i++) {
    f.env.AI_ENRICHMENT_MODE = "off";
    const source = await f.seed("memories", `# Orion implemented ${i}\nOrion implemented`, index(`Orion implemented ${i}`));
    await f.checkpoint(`crowded-project-${i}`, [source]);
  }
  f.env.AI_ENRICHMENT_MODE = "off";
  const original = await f.seed("specific-original", "# Orion ExactNeedle\nOrion ExactNeedle original evidence", index("Orion ExactNeedle"));
  f.reset(); const result = await f.search("Orion ExactNeedle", { automatic: true });
  assert.ok(ids(result).includes(original), "partial-term checkpoints evicted concrete original evidence");
  assert.equal(f.metrics.requests.length, 1);
});
scenario("index-only topic target survives fifty higher semantic unrelated spaces", async (f) => {
  // 失敗条件: 本文には無い索引語の一致が、semantic上位候補だけで消える。
  for (let i = 0; i < 50; i++) await f.seed(`semantic-${i}`, `# Gardening ${i}\nFlowers`, index(`Gardening ${i}`));
  const target = await f.seed("semantic-target", "# Technical archive\nTransport observations without the queried topic", index("SpecificTopicNeedle"));
  f.vectors.get(target).score = 0.9;
  f.reset(); const result = await f.search("SpecificTopicNeedle", { automatic: true });
  assert.ok(ids(result).includes(target)); assert.equal(result.results[0].id, target);
  assert.equal(f.metrics.embeddings, 1); assert.equal(f.metrics.vectorQueries.length, 1);
  const row = f.db.prepare("SELECT content, topic_revision FROM memories WHERE id = ?").get(target);
  assert.ok(!row.content.includes("SpecificTopicNeedle")); assert.equal(f.vectors.get(target).metadata.topic_revision, row.topic_revision);
});
scenario("fifty lexical-plus-semantic body hits cannot outrank the actual index title match", async (f) => {
  // 失敗条件: RRFの二重一致加点で、索引一致するvector51位の文書が押し出される。
  for (let i = 0; i < 50; i++) await f.seed(`body-${i}`, `# General request ${i}\nBodyCrowdingNeedle implementation detail`, index(`General request ${i}`, "General request"));
  const target = await f.seed("body-index-target", "# Technical archive\nTransport observations", index("BodyCrowdingNeedle"));
  f.vectors.get(target).score = 0.9;
  f.reset(); const result = await f.search("BodyCrowdingNeedle", { automatic: true });
  assert.equal(result.results[0]?.id, target, "body-only reciprocal-rank boost displaced the title match");
});
for (const [query, title] of [["école", "ÉCOLE"], ["σχολή", "ΣΧΟΛΉ"], ["İstanbul", "İSTANBUL"], ["kiriha", "KIRIHA"], ["ωmega", "ΩMEGA"], ["θeta", "ϴETA"]]) scenario(`Unicode index case folding survives lexical crowding with AI off: ${query}`, async (f) => {
  // 失敗条件: SQLite ASCII lowerに依存しUnicode大文字索引が候補検索から落ちる。
  for (let i = 0; i < 50; i++) await f.seed(`unicode-${i}`, `# General request ${i}\n${query} implementation detail`, index(`General request ${i}`, "General request"), { indexed: false });
  const target = await f.seed("unicode-index-target", "# Technical archive\nTransport observations", index(title), { indexed: false });
  f.reset(); f.env.AI_ENRICHMENT_MODE = "off";
  const result = await f.search(query, { automatic: true }); assert.equal(result.results[0]?.id, target);
  assert.equal(f.metrics.embeddings, 0); assert.equal(f.metrics.vectorQueries.length, 0);
});
for (const legacyTitle of [false, true]) scenario(`weighted concrete index term survives fifty short-term matches: ${legacyTitle ? "legacy metadata.title" : "saved memoryIndex"}`, async (f) => {
  // 失敗条件: SQLの一致語数COUNTがgo/uiを優先し、長いExactNeedleをlimit前に落とす。
  for (let i = 0; i < 50; i++) await f.seed(`weighted-${i}`, `# General archive ${i}\nUnrelated transcript`, index("Go UI", "Go UI"));
  const target = await f.seed("weighted-target", "# Technical archive\nUnrelated original transcript", legacyTitle ? { title: "ExactNeedle" } : index("ExactNeedle"));
  f.vectors.get(target).score = 0.9;
  f.reset(); const result = await f.search("go ui ExactNeedle", { automatic: true });
  assert.equal(result.results[0]?.id, target, "short query terms evicted stronger concrete evidence");
  assert.ok(!f.db.prepare("SELECT content FROM memories WHERE id = ?").get(target).content.includes("ExactNeedle"));
  assert.equal(f.metrics.requests.length, 1);
});
scenario("MCP manual body-only search displays the original query excerpt", async (f) => {
  // 失敗条件: 手動候補には残るが原文固有語が表示されず、検索根拠を確認できない。
  const target = await f.seed("manual-excerpt", "# Request reliability\n### User request\nImprove request execution\n### Final assistant response\nUse AbortSignal.timeout for the fetch deadline.", index("Request reliability", "Improve request execution"));
  f.reset(); const result = await callTool("search_memory", { query: "AbortSignal.timeout" }, { request: f.request, settings, context });
  assert.ok(result.structuredContent.results.some((row) => row.id === target));
  const text = result.content.filter((block) => block.type === "text").map((block) => block.text).join("\n");
  assert.match(text, /原文の該当箇所/u); assert.match(text, /AbortSignal\.timeout/u); assert.equal(f.metrics.requests.length, 1);
});
scenario("empty prompts and SessionStart send no API or AI traffic", async (f) => {
  f.reset(); assert.deepEqual(await f.hook(""), {}); assert.deepEqual(await f.hook("この その あの"), {});
  assert.deepEqual(await runHook("SessionStart", {}, { request: f.request, settings, context }), {});
  assert.equal((await f.search(" ")).results.length, 0); assert.equal(f.metrics.requests.length, 0); assert.equal(f.metrics.embeddings, 0);
});

const report = { command: "node --experimental-transform-types server/tests/recall-efficiency.mjs", node: process.version, platform: process.platform, arch: process.arch, migrations,
  scope: "authenticated worker.fetch, real migrated SQLite, deterministic AI/Vectorize, production client API/searchIndex/runHook/MCP callTool; no external requests", results: [] };
for (const { name, run } of cases) {
  const f = fixture(); const original = { log: console.log, error: console.error, warn: console.warn }; const events = [];
  const capture = (...values) => { for (const value of values) { try { const entry = JSON.parse(value); events.push({ event: entry.event, error: entry.error }); } catch { /* 非JSONログは保存しない。 */ } } };
  console.log = capture; console.error = capture; console.warn = capture;
  const start = performance.now(); let result;
  try { const observations = await run(f); result = { name, status: "passed", ...(observations || {}) }; }
  catch (error) { result = { name, status: "failed", error: String(error), stack: error.stack?.split("\n").slice(0, 4) }; }
  finally { await f.drain(); console.log = original.log; console.error = original.error; console.warn = original.warn; f.db.close(); }
  report.results.push({ ...result, elapsedMs: performance.now() - start, metrics: f.metrics, events });
}
report.passed = report.results.filter((result) => result.status === "passed").length;
report.failed = report.results.length - report.passed;
// 初回の改修前観測を、改修後の再実行でも同じ成果物内に残す。
if (existsSync(artifactPath)) {
  const previous = JSON.parse(readFileSync(artifactPath, "utf8"));
  report.baseline = previous.baseline || { node: previous.node, passed: previous.passed, failed: previous.failed, results: previous.results };
}
mkdirSync(resolve(artifactPath, ".."), { recursive: true }); writeFileSync(artifactPath, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify({ artifact: artifactPath, passed: report.passed, failed: report.failed }));
if (report.failed) process.exitCode = 1;
