// 失敗条件を先に固定: 取得前の採点を取得後へ流用する、同点順・意味閾値・取得上限が変わる、
// 自動/手動検索・最新capture・清書の検索語被覆が変わる、計測の組込みprototype変更が漏れる、子処理が停止しない。
// 明示したプロジェクトの出典を見落とす、短いexcerptに無い主題語を失う、無関係な出典や自動想起まで広げる。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const script = fileURLToPath(import.meta.url);
const root = resolve(dirname(script), "..");
const key = "ranking-fixture-key";

async function probe() {
  const runtimeRoot = resolve(process.argv[3]);
  const baseUrl = process.argv[4];
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  const scenarios = JSON.parse(input);
  const { searchIndex, api } = await import(pathToFileURL(join(runtimeRoot, "client", "memory-client.mjs")));
  const results = [];
  for (const scenario of scenarios) {
    const metrics = { normalizations: 0, matchingNormalizations: 0, matchingCalls: 0 };
    const originalNormalize = String.prototype.normalize;
    const originalReplace = String.prototype.replace;
    let result;
    try {
      String.prototype.normalize = function (...args) {
        metrics.normalizations++;
        return Reflect.apply(originalNormalize, this, args);
      };
      String.prototype.replace = function (pattern, replacement) {
        if (pattern instanceof RegExp && pattern.source === "[._-]+" && replacement === " ") {
          metrics.matchingNormalizations++;
          // matchingTopicTermsの本文正規化だけを数え、各検索語の正規化と区別する。
          if (new Error().stack.split("\n")[3]?.includes("at matchingTopicTerms (")) metrics.matchingCalls++;
        }
        return Reflect.apply(originalReplace, this, [pattern, replacement]);
      };
      result = await searchIndex({ ...scenario.input,
        settings: { maxMemories: 20, minimumSimilarity: 0.7 },
        request: (path, options) => api(path, { ...options, config: { baseUrl: `${baseUrl}/${scenario.name}`, apiKey: key } }),
      });
    } finally {
      String.prototype.normalize = originalNormalize;
      String.prototype.replace = originalReplace;
    }
    assert.equal(String.prototype.normalize, originalNormalize);
    assert.equal(String.prototype.replace, originalReplace);
    results.push({ name: scenario.name, result, metrics });
  }
  process.stdout.write(JSON.stringify(results));
}

async function main() {
  const args = process.argv.slice(2);
  const comparisonIndex = args.indexOf("--comparison-root");
  assert.ok(args.length === 0 || comparisonIndex === 0 && args.length === 2, "Use --comparison-root <source root>");
  const comparisonRoot = comparisonIndex < 0 ? null : resolve(args[comparisonIndex + 1]);
  const outputRoot = process.env.REMNIFOLD_VALIDATION_DIR || join(root, "dist");
  assert.ok(isAbsolute(outputRoot), "REMNIFOLD_VALIDATION_DIR must be absolute");
  const output = join(outputRoot, "client-ranking-validation.json");
  const { buildMemoryIndex } = await import("../client/memory-index.mjs");
  const date = "2026-10-01T00:00:00.000Z";
  const indexed = (id, title, metadata = {}, extra = {}) => ({ id, containerTag: "memories", createdAt: date, updatedAt: date,
    metadata: { memoryIndex: buildMemoryIndex({ title, request: title, sourceUpdatedAt: metadata.sourceUpdatedAt || date }), ...metadata }, ...extra });
  const legacy = (id, title = "alpha beta", extra = {}) => ({ id, containerTag: "memories", title, semanticSimilarity: 0.9, ...extra });
  const detail = (row, title = row.title) => ({ ...row, title, content: `# ${title}\n### User request\n${title}\n### Final assistant response\nVerified source details.` });
  const ranking = [
    indexed("covered", "alpha beta"), indexed("checkpoint", "alpha beta", { sm_consolidation: true, sourceMemoryIds: ["covered"], consolidationCreatedAt: date }),
    indexed("uncovered", "beta gamma"), indexed("narrow-checkpoint", "alpha", { sm_consolidation: true, sourceMemoryIds: ["uncovered"], consolidationCreatedAt: date }),
    indexed("revision-old", "alpha beta", { sessionId: "session", turn: 1, part: 1, sourceUpdatedAt: date }),
    indexed("revision-new", "alpha beta", { sessionId: "session", turn: 1, part: 1, sourceUpdatedAt: "2026-10-02T00:00:00.000Z" }),
    ...Array.from({ length: 12 }, (_, i) => indexed(`rank-${i}`, i % 3 ? "alpha" : "alpha beta", {}, { semanticSimilarity: i % 2 ? 0.8 : 0.7 })),
  ];
  ranking.push(ranking[6]);
  const hydration = [legacy("disappears"), legacy("promotes", "zeta"), legacy("unavailable"), indexed("stable", "alpha")];
  const semantic = [indexed("semantic-edge", "zeta", {}, { semanticSimilarity: 0.7 }),
    indexed("semantic-below", "zeta", {}, { semanticSimilarity: 0.699 }),
    indexed("lexical", "zeta", {}, { lexicalSimilarity: 0 }), indexed("unrelated", "zeta")];
  const capped = Array.from({ length: 30 }, (_, i) => legacy(`bounded-${i}`));
  const spaces = Array.from({ length: 5 }, (_, i) => `space-${i}`);
  const many = spaces.flatMap((containerTag) => Array.from({ length: 20 }, (_, i) => legacy(`${containerTag}-${i}`, "alpha beta", { containerTag })));
  const projectRows = [
    indexed("old-release", "ReplaceTranslator release", { sm_project_id: "repo_replacetranslator__1234567890abcdef", sourceUpdatedAt: date }, { semanticSimilarity: 0.99 }),
    indexed("wrong-project", "ReplaceTranslator instructions", { project: "another", sm_project_id: "repo_another__1234567890abcdef" }, { semanticSimilarity: 0.98 }),
    indexed("recent-project", "依存更新", { project: "replacetranslator", sm_project_id: "repo_replacetranslator__1234567890abcdef", sourceUpdatedAt: "2026-10-21T00:00:00.000Z" },
      { semanticSimilarity: 0.71, topics: ["ESLint", "Puppeteer"], searchExcerpt: "依存更新 ESLint Puppeteer" }),
  ];
  const scenarios = [
    { name: "manual-project-hydration-cap", rows: [
      ...Array.from({ length: 25 }, (_, i) => ({ ...projectRows[0], id: `old-project-${i}` })), projectRows[2],
    ], input: { query: "ReplaceTranslator", limit: 3 } },
    { name: "manual-project-recency", rows: projectRows, input: { query: "ReplaceTranslator", limit: 3 } },
    { name: "manual-project-topic", rows: projectRows, input: { query: "ReplaceTranslator 依存更新 ESLint Puppeteer", limit: 3 } },
    { name: "automatic-project-isolation", rows: projectRows, input: { query: "ReplaceTranslator", automatic: true, limit: 3 } },
    { name: "automatic-ranking", rows: ranking, input: { query: "alpha beta", automatic: true, limit: 20 } },
    { name: "manual-ranking", rows: ranking, input: { query: "alpha beta", limit: 20 } },
    { name: "automatic-hydration", rows: hydration, input: { query: "alpha beta", automatic: true } },
    { name: "manual-hydration", rows: hydration, input: { query: "alpha beta" } },
    { name: "semantic-manual", rows: semantic, input: { query: "alpha" } },
    { name: "semantic-automatic", rows: semantic, input: { query: "alpha", automatic: true } },
    { name: "minimum-hydration-budget", rows: capped, input: { query: "alpha beta", limit: 1, automatic: true } },
    { name: "maximum-hydration-budget", rows: many, tags: spaces, fallback: true, input: { query: "alpha beta", limit: 20, automatic: true } },
    { name: "partial-space-failure", rows: [indexed("available", "alpha beta")], tags: ["memories", "unavailable"], fallback: true, failedTag: "unavailable", input: { query: "alpha beta", automatic: true } },
    { name: "explicit-container", rows: capped.slice(0, 20), input: { query: "alpha beta", containerTag: "memories", limit: 2 } },
    { name: "stable-ties", rows: [indexed("tie-a", "alpha beta"), indexed("tie-b", "alpha beta")], input: { query: "alpha beta" } },
    { name: "excerpt-order", rows: [indexed("excerpt", "alpha", {}, { searchExcerpt: "beta" }), indexed("full", "alpha beta")], input: { query: "alpha beta", automatic: true } },
    { name: "unicode-terms", rows: [indexed("unicode", "ＡＬＰＨＡ .NET8 日本語")], input: { query: "alpha .NET 日本語", automatic: true } },
    { name: "empty-query", rows: ranking, input: { query: "  " } },
    { name: "no-topic", rows: ranking, input: { query: "続けて", automatic: true } },
  ];
  const byName = new Map(scenarios.map((scenario) => [scenario.name, scenario]));
  const requests = [];
  const children = new Set();
  const server = createServer(async (request, response) => {
    response.setHeader("Content-Type", "application/json");
    if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end("{}"); return; }
    const [, name, ...parts] = request.url.split("/");
    const route = `/${parts.join("/")}`;
    const scenario = byName.get(name);
    if (!scenario) { response.writeHead(404); response.end("{}"); return; }
    let text = "";
    for await (const chunk of request) text += chunk;
    const body = text ? JSON.parse(text) : {};
    requests.push({ name, route, body });
    if (route === "/v3/container-tags") response.end(JSON.stringify({ spaces: scenario.tags.map((containerTag) => ({ containerTag, memoryCount: 20 })) }));
    else if (route === "/v4/search") {
      if (scenario.fallback && body.allContainers) { response.writeHead(404); response.end("{}"); }
      else if (scenario.failedTag && scenario.failedTag === body.containerTag) { response.writeHead(503); response.end("{}"); }
      else {
        const rows = body.containerTag ? scenario.rows.filter((row) => row.containerTag === body.containerTag) : scenario.rows;
        response.end(JSON.stringify(body.allContainers ? { results: rows, searchScope: "all-containers", searchedContainers: ["memories"], spaceDiscoveryComplete: true } : { results: rows }));
      }
    } else if (route.startsWith("/v3/documents/")) {
      const id = decodeURIComponent(route.slice("/v3/documents/".length));
      const row = scenario.rows.find((candidate) => candidate.id === id);
      if (!row || id === "unavailable") { response.writeHead(503); response.end("{}"); }
      else response.end(JSON.stringify(detail(row, id === "disappears" ? "zeta" : id === "promotes" ? "alpha beta" : row.title)));
    } else { response.writeHead(404); response.end("{}"); }
  });
  async function run(runtimeRoot, baseUrl) {
    const start = requests.length;
    const results = await new Promise((done, reject) => {
      const child = spawn(process.execPath, [script, "--probe", runtimeRoot, baseUrl], { windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
      children.add(child);
      let stdout = "", stderr = "", timedOut = false;
      const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 30_000);
      child.stdout.on("data", (chunk) => stdout += chunk);
      child.stderr.on("data", (chunk) => stderr += chunk);
      child.stdin.on("error", (error) => { if (error.code !== "EPIPE") reject(error); });
      child.once("error", (error) => { clearTimeout(timer); children.delete(child); reject(error); });
      child.once("close", (code) => {
        clearTimeout(timer); children.delete(child);
        if (timedOut) reject(new Error("Ranking client exceeded 30 seconds"));
        else if (code !== 0) reject(new Error(`Ranking client exited ${code}: ${stderr}`));
        else { try { done(JSON.parse(stdout)); } catch (error) { reject(error); } }
      });
      child.stdin.end(JSON.stringify(scenarios.map(({ name, input }) => ({ name, input }))));
    });
    return results.map((result) => {
      const calls = requests.slice(start).filter((request) => request.name === result.name);
      return { ...result, requests: calls, documentFetches: calls.filter((request) => request.route.startsWith("/v3/documents/")).length };
    });
  }
  try {
    await new Promise((done, reject) => {
      const timer = setTimeout(() => reject(new Error("Ranking fixture startup exceeded 5 seconds")), 5_000);
      server.once("error", (error) => { clearTimeout(timer); reject(error); });
      server.listen(0, "127.0.0.1", () => { clearTimeout(timer); done(); });
    });
    const baseUrl = `http://127.0.0.1:${server.address().port}`;
    const current = await run(root, baseUrl);
    const find = (name) => current.find((scenario) => scenario.name === name);
    const ids = (name) => find(name).result.results.map((index) => index.id);
    assert.equal(ids("manual-project-recency")[0], "recent-project");
    assert.equal(ids("manual-project-hydration-cap")[0], "recent-project");
    assert.equal(ids("manual-project-topic")[0], "recent-project");
    assert.ok(!ids("automatic-project-isolation").includes("recent-project"));
    assert.ok(ids("automatic-ranking").includes("checkpoint"));
    assert.ok(ids("automatic-ranking").includes("uncovered"));
    assert.ok(!ids("automatic-ranking").includes("covered"));
    assert.ok(!ids("automatic-ranking").includes("revision-old"));
    assert.ok(ids("automatic-ranking").includes("revision-new"));
    assert.ok(ids("manual-ranking").includes("covered"));
    assert.ok(ids("manual-ranking").includes("revision-old"));
    assert.deepEqual(ids("automatic-hydration"), ["promotes", "stable"]);
    assert.deepEqual(find("automatic-hydration").result.failedDocuments, ["unavailable"]);
    assert.ok(ids("manual-hydration").includes("disappears"));
    assert.ok(ids("manual-hydration").includes("unavailable"));
    assert.deepEqual(ids("semantic-manual"), ["semantic-edge", "lexical"]);
    assert.deepEqual(ids("semantic-automatic"), []);
    assert.equal(find("minimum-hydration-budget").documentFetches, 20);
    assert.equal(find("maximum-hydration-budget").documentFetches, 80);
    assert.equal(find("explicit-container").documentFetches, 20);
    assert.deepEqual(find("partial-space-failure").result.failedContainers, ["unavailable"]);
    assert.deepEqual(ids("stable-ties"), ["tie-a", "tie-b"]);
    assert.deepEqual(ids("excerpt-order"), ["full", "excerpt"]);
    assert.deepEqual(ids("unicode-terms"), ["unicode"]);
    for (const name of ["empty-query", "no-topic"]) { assert.deepEqual(ids(name), []); assert.equal(find(name).requests.length, 0); }
    for (const scenario of current.filter((scenario) => scenario.requests.length)) {
      assert.ok(scenario.metrics.normalizations > 0);
      assert.ok(scenario.metrics.matchingCalls > 0);
    }
    let comparison = null;
    if (comparisonRoot) {
      comparison = await run(comparisonRoot, baseUrl);
      for (let i = 0; i < current.length; i++) {
        assert.deepEqual(current[i].result, comparison[i].result, `${current[i].name}: public result/order`);
        // 同時HTTP要求の到着順は非決定的なので、要求内容と重複数を比較する。
        const calls = (scenario) => scenario.requests.map((request) => JSON.stringify(request)).sort();
        assert.deepEqual(calls(current[i]), calls(comparison[i]), `${current[i].name}: API calls/fetch counts`);
        assert.equal(current[i].documentFetches, comparison[i].documentFetches);
      }
      for (const name of ["automatic-ranking", "manual-ranking", "maximum-hydration-budget"]) {
        const before = comparison.find((scenario) => scenario.name === name).metrics;
        const after = find(name).metrics;
        assert.ok(after.normalizations < before.normalizations, `${name}: normalization reduction`);
        assert.ok(after.matchingCalls < before.matchingCalls, `${name}: matching reduction`);
      }
    }
    const report = { passed: true, runtime: process.version, comparisonRoot, current, comparison };
    await mkdir(outputRoot, { recursive: true });
    await writeFile(output, `${JSON.stringify(report, null, 2)}\n`);
    process.stdout.write(`${JSON.stringify({ passed: true, scenarios: current.length, comparison: Boolean(comparison), output })}\n`);
  } finally {
    for (const child of children) child.kill("SIGKILL");
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
  }
}

if (process.argv[2] === "--probe") await probe();
else await main();
