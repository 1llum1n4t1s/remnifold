// 実行: Node 24.18+ --experimental-transform-types scripts/native-client-e2e.mjs --binary <absolute binary>
// 実Rust CLIと実Workerを接続し、mock APIでは見落とすJSON/認証/保存/索引契約を検証する。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile, lstat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { DatabaseSync } from "node:sqlite";
import { pathToFileURL } from "node:url";
import worker from "../server/src/index.ts";
import { assertCleanupPath } from "./lib/cleanup.mjs";

const args = process.argv.slice(2);
assert.equal(args.length, 2, "Expected --binary <absolute native executable>");
assert.equal(args[0], "--binary");
assert.ok(isAbsolute(args[1]), "Native binary path must be absolute");
const binary = resolve(args[1]);
assert.ok((await lstat(binary)).isFile(), "Native executable must exist");
const root = resolve(import.meta.dirname, "..");
const artifact = join(process.env.REMNIFOLD_VALIDATION_DIR || join(root, "dist"), "native-client-validation.json");
assert.ok(isAbsolute(artifact));
const tempRoot = resolve(tmpdir());
assertCleanupPath(join(tempRoot, "remnifold-native-client-validation"), tempRoot);
const fixture = await mkdtemp(join(tempRoot, "remnifold-native-client-"));
const home = join(fixture, "codex");
const project = join(fixture, "project");
await mkdir(home); await mkdir(project);
const key = "native-worker-local-fixture-key";
const embedding = Array.from({ length: 1024 }, (_, i) => i === 0 ? 1 : 0);
const pending = [];
const vectors = new Map();
const requests = [];
const checks = [];
const children = new Map();
const metrics = { embeddings: 0, vectorQueries: 0 };
const db = new DatabaseSync(":memory:");
db.exec("PRAGMA foreign_keys = ON");
for (const name of (await readdir(join(root, "server/migrations"))).filter(name => name.endsWith(".sql")).sort()) {
  db.exec(await readFile(join(root, "server/migrations", name), "utf8"));
}

// recall-efficiency.mjsと同じD1/SQLite境界。privateな既存fixtureをimportすると全試験が走るため最小adapterだけ再利用。
function prepare(sql) {
  const bind = (values = []) => ({ sql, values, bind: (...next) => bind(next),
    first: async () => db.prepare(sql).get(...values),
    all: async () => ({ results: db.prepare(sql).all(...values) }),
    run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
    execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
      ? { results: db.prepare(sql).all(...values), meta: { changes: 0 } }
      : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
  }); return bind();
}
const env = {
  DB: { prepare, batch: async operations => {
    db.exec("BEGIN");
    try { const result = operations.map(operation => operation.execute()); db.exec("COMMIT"); return result; }
    catch (error) { db.exec("ROLLBACK"); throw error; }
  } },
  MEMORY_API_KEY: key, AI_ENRICHMENT_MODE: "on",
  AI: { run: async (_model, input) => {
    if (input.text) { metrics.embeddings++; return { data: [embedding] }; }
    return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify({ facts: [], topics: ["NativeIntegration"] }) } }] };
  } },
  MEMORY_VECTORS: {
    query: async (_vector, options) => {
      metrics.vectorQueries++;
      const matches = [...vectors.values()].filter(row => !options.namespace || row.namespace === options.namespace)
        .slice(0, options.topK).map(row => ({ id: row.id, score: 0.99, metadata: row.metadata }));
      return { matches, count: matches.length };
    },
    upsert: async rows => { for (const row of rows) vectors.set(row.id, row); return { mutationId: "native-fixture" }; },
    deleteByIds: async ids => { for (const id of ids) vectors.delete(id); return {}; },
    getByIds: async ids => ids.map(id => vectors.get(id)).filter(Boolean),
  },
};
const context = { waitUntil(promise) { pending.push(Promise.resolve(promise)); } };
const bounded = (promise, label, ms = 20_000) => {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms}ms`)), ms))])
    .finally(() => clearTimeout(timer));
};
async function drain() { while (pending.length) await bounded(Promise.all(pending.splice(0)), "Worker enrichment"); }
const server = createServer(async (request, response) => {
  try {
    let body = "";
    for await (const chunk of request) { body += chunk; if (Buffer.byteLength(body) > 2 * 1024 * 1024) throw new Error("HTTP request too large"); }
    const headers = new Headers();
    for (const [name, value] of Object.entries(request.headers)) if (value !== undefined) headers.set(name, Array.isArray(value) ? value.join(",") : value);
    const result = await worker.fetch(new Request(`https://local.example${request.url}`, {
      method: request.method, headers, ...(!["GET", "HEAD"].includes(request.method) ? { body } : {}),
    }), env, context);
    await drain();
    requests.push({ path: new URL(request.url, "http://fixture").pathname, method: request.method, status: result.status });
    response.writeHead(result.status, Object.fromEntries(result.headers)); response.end(Buffer.from(await result.arrayBuffer()));
  } catch {
    response.writeHead(500, { "Content-Type": "application/json" }); response.end('{"error":"local Worker fixture failed"}');
  }
});
server.requestTimeout = 15_000; server.headersTimeout = 10_000;
await bounded(new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); }), "HTTP listen");
const endpoint = `http://127.0.0.1:${server.address().port}`;
await writeFile(join(home, "remnifold.json"), JSON.stringify({ baseUrl: endpoint, apiKey: key, maxRecentTasks: 0 }));
const environment = { ...process.env, CODEX_HOME: home, HOME: fixture, USERPROFILE: fixture };
for (const name of ["REMNIFOLD_API_URL", "REMNIFOLD_API_KEY", "KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY",
  "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY"]) delete environment[name];
function launch(args) {
  const child = spawn(binary, args, { cwd: project, env: environment, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
  const closed = new Promise((done, reject) => { child.once("error", reject); child.once("close", code => done(code)); });
  children.set(child, closed); closed.finally(() => children.delete(child)).catch(() => {});
  return { child, closed };
}
async function runHook(event, payload) {
  const { child, closed } = launch(["hook", event]);
  let stdout = "", stderr = "";
  child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
  child.stdout.on("data", chunk => stdout += chunk); child.stderr.on("data", chunk => stderr += chunk);
  child.stdin.end(JSON.stringify(payload));
  try { assert.equal(await bounded(closed, `${event} hook`), 0, stderr); }
  catch (error) { child.kill("SIGKILL"); throw error; }
  assert.equal(stderr, ""); return stdout ? JSON.parse(stdout) : {};
}
function startMcp() {
  const { child, closed } = launch(["mcp"]); const calls = new Map(); let next = 0; let stderr = "";
  child.stderr.setEncoding("utf8"); child.stderr.on("data", chunk => stderr += chunk);
  const send = value => child.stdin.write(`${JSON.stringify(value)}\n`);
  const lines = createInterface({ input: child.stdout });
  lines.on("line", line => {
    const message = JSON.parse(line);
    if (message.method === "roots/list") { send({ jsonrpc: "2.0", id: message.id, result: { roots: [{ uri: pathToFileURL(project).href, name: "native-fixture" }] } }); return; }
    calls.get(message.id)?.(message); calls.delete(message.id);
  });
  const exchange = async (method, params) => {
    const id = ++next; const reply = new Promise(done => calls.set(id, done));
    send({ jsonrpc: "2.0", id, method, params });
    const response = await bounded(reply, `MCP ${method}`); assert.equal(response.error, undefined, JSON.stringify(response.error)); return response.result;
  };
  return { exchange, send, tool: async (name, args) => {
    const result = await exchange("tools/call", { name, arguments: args });
    assert.notEqual(result.isError, true, JSON.stringify(result)); return result.structuredContent;
  }, close: async () => { child.stdin.end(); assert.equal(await bounded(closed, "MCP shutdown"), 0, stderr); assert.equal(stderr, ""); lines.close(); } };
}
let report;
try {
  assert.equal((await fetch(`${endpoint}/v3/documents/list`, { method: "POST", body: "{}" })).status, 401);
  checks.push("real Worker rejects unauthenticated requests");
  const mcp = startMcp();
  const initialized = await mcp.exchange("initialize", { protocolVersion: "2025-06-18", capabilities: { roots: { listChanged: false } }, clientInfo: { name: "native-worker-fixture", version: "1" } });
  assert.equal(initialized.serverInfo.name, "remnifold"); mcp.send({ jsonrpc: "2.0", method: "notifications/initialized" });
  const saved = await mcp.tool("add_memory", { content: "OrionNative migration checklist preserves Unicode 😀 and project provenance.", sourceFolder: project });
  assert.ok(saved.id); assert.equal(saved.success, true);
  const found = await mcp.tool("search_memory", { query: "OrionNative migration checklist", sourceFolder: project });
  assert.ok(found.results.some(row => row.id === saved.id), JSON.stringify(found));
  const listed = await mcp.tool("listDocuments", { limit: 10 });
  assert.ok(listed.documents.some(row => row.id === saved.id));
  const read = await mcp.tool("getDocument", { documentId: saved.id });
  assert.ok(JSON.stringify(read.document).includes("OrionNative migration checklist"));
  const provenance = JSON.parse(db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(saved.id).metadata_json);
  assert.equal(provenance.sm_scope, "project"); assert.ok(provenance.sm_project_id);
  checks.push("native MCP save/search/list/get use real Worker JSON and project provenance");
  const session = "native-capture-worker-session";
  await mkdir(join(home, "sessions"));
  const transcript = join(home, "sessions", `rollout-${session}.jsonl`);
  const rows = [
    { type: "session_meta", payload: { id: session, cwd: project, timestamp: "2026-01-01T00:00:00Z" } },
    { type: "event_msg", timestamp: "2026-01-01T00:00:00Z", payload: { type: "user_message", message: `OrionNative capture request API_KEY='local-test-secret' ${key} 😀\0` } },
    { type: "response_item", payload: { role: "assistant", phase: "commentary", content: [{ type: "output_text", text: "progress stays local" }] } },
    { type: "response_item", payload: { role: "assistant", phase: "final_answer", content: [{ type: "output_text", text: "OrionNative finished the worker bridge and verified Unicode 😀." }] } },
  ];
  await writeFile(transcript, rows.map(row => JSON.stringify(row)).join("\n"));
  const payload = { session_id: session, transcript_path: transcript, cwd: project };
  const captured = await runHook("Stop", payload);
  assert.match(captured.systemMessage, /saved 1 documents; pending 0/);
  const stored = db.prepare("SELECT * FROM memories WHERE custom_id LIKE 'codex-turn-v2:%'").all();
  assert.equal(stored.length, 1); const capture = stored[0];
  assert.ok(capture.content.includes("😀\0")); assert.ok(capture.content.includes("[REDACTED]"));
  assert.ok(!capture.content.includes(key)); assert.ok(!capture.content.includes("local-test-secret")); assert.ok(!capture.content.includes("progress stays local"));
  const postsBeforeRetry = requests.filter(row => row.method === "POST" && row.path === "/v3/documents").length;
  await runHook("Stop", payload);
  assert.equal(requests.filter(row => row.method === "POST" && row.path === "/v3/documents").length, postsBeforeRetry);
  checks.push("native Stop capture persists sanitized final-only Unicode/NUL content and suppresses acknowledged retry");
  const recall = await runHook("UserPromptSubmit", { session_id: "native-other-session", cwd: project, prompt: "OrionNative capture request worker bridge" });
  const recallText = recall.hookSpecificOutput?.additionalContext || "";
  assert.ok(recallText.includes(capture.id), JSON.stringify(recall));
  assert.ok(!recallText.includes(key)); assert.ok(!recallText.includes("local-test-secret"));
  assert.ok(recallText.length < 4000);
  checks.push("native automatic recall finds captured Worker index without exposing configured key");
  const forgotten = await mcp.tool("add_memory", { action: "forget", documentId: saved.id });
  assert.equal(forgotten.success, true);
  assert.equal(db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(saved.id).is_forgotten, 1);
  assert.equal(vectors.has(saved.id), false);
  const afterForget = await mcp.tool("listDocuments", { limit: 10 });
  assert.ok(!afterForget.documents.some(row => row.id === saved.id));
  checks.push("native MCP forget persists Worker tombstone and removes document from indexes");
  await mcp.close();
  assert.ok(metrics.embeddings > 0); assert.ok(metrics.vectorQueries > 0);
  assert.ok(!requests.some(row => row.status >= 500));
  report = { passed: true, checks, worker: "server/src/index.ts", database: "migration-backed SQLite", binary, metrics, requests };
} catch (error) {
  report = { passed: false, checks, message: error.message, binary, metrics, requests }; throw error;
} finally {
  for (const child of children.keys()) child.kill("SIGKILL");
  await bounded(Promise.allSettled([...children.values()]), "native child cleanup", 10_000);
  server.closeAllConnections();
  await bounded(new Promise(done => server.close(done)), "HTTP shutdown", 10_000);
  await drain(); db.close();
  if (process.platform === "win32") {
    // Windows作業規約: 標準APIで削除せず、呼出元のPrunlet最終清掃へ対象を返す。
    report.fixtureDirectory = fixture; report.fixtureCleanup = "Prunlet required on Windows";
  } else { assertCleanupPath(fixture, tempRoot); await rm(fixture, { recursive: true, force: true }); }
  await mkdir(dirname(artifact), { recursive: true }); await writeFile(artifact, `${JSON.stringify(report, null, 2)}\n`);
}
console.log(JSON.stringify({ passed: true, checks: checks.length, artifact }));
