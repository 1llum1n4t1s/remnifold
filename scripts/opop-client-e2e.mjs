import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
// 失敗条件を先に固定: 非object設定/入力による例外、旧設定fallback喪失、
// 一部文書取得失敗の隠蔽、全成功/全失敗通知の変化、既読・順位・サイズ契約の変化、
// PreToolUseでの不要なclient読込み、Stop診断/権限/秘密の漏れ。実データは使わない。
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { mkdir, mkdtemp, readFile, writeFile, lstat, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const baseline = process.argv.includes("--baseline");
const artifact = join(root, "dist", "opop-client-validation.json");
const fixture = await mkdtemp(join(tmpdir(), "remnifold-opop-client-e2e-"));
const key = "fixture-opop-key-do-not-print";
const marker = "fixture-sensitive-error-do-not-print";
const checks = [];
const children = new Set();
const requests = [];
let mode = "partial";
let report;
let metrics;
const good = { id: "good-index", containerTag: "fixture-space", lexicalSimilarity: 1,
  metadata: { memoryIndex: { version: 1, title: "Fixture orchard", description: "orchard indexed source", recallable: true } } };
const bad = { id: marker, containerTag: "fixture-space", lexicalSimilarity: 0.9 };
const server = createServer(async (request, response) => {
  let text = "";
  for await (const chunk of request) text += chunk;
  if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end(marker); return; }
  requests.push({ path: request.url, body: text ? JSON.parse(text) : undefined });
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v4/search") response.end(JSON.stringify({ searchScope: "all-containers", searchedContainers: ["fixture-space"], spaceDiscoveryComplete: true,
    results: mode === "all-failed" ? [bad] : mode === "success" ? [good] : [good, bad] }));
  else { response.writeHead(503); response.end(JSON.stringify({ error: marker })); }
});

function check(name, condition, detail = {}) {
  checks.push({ name, passed: Boolean(condition), ...detail });
  if (!baseline) assert.ok(condition, name);
}
async function run(event, input, home, extra = []) {
  const env = { ...process.env, CODEX_HOME: home, NODE_NO_WARNINGS: "1", REMNIFOLD_API_URL: `http://127.0.0.1:${server.address().port}`, REMNIFOLD_API_KEY: key };
  for (const name of ["KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY", "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY", "NODE_OPTIONS"]) delete env[name];
  return new Promise((done, reject) => {
    const child = spawn(process.execPath, [...extra, join(root, "scripts", "hook-launcher.mjs"), event], { cwd: root, env, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let stdout = "", stderr = "";
    const timeout = setTimeout(() => { child.kill(); }, 15_000);
    child.stdout.on("data", chunk => stdout += chunk);
    child.stderr.on("data", chunk => stderr += chunk);
    child.once("error", reject);
    child.once("close", code => { clearTimeout(timeout); children.delete(child); done({ code, stdout, stderr }); });
    child.stdin.end(typeof input === "string" ? input : JSON.stringify(input));
  });
}
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  const home = join(fixture, "home");
  await mkdir(home);
  const current = join(home, "remnifold.json");
  const legacy = join(home, "supermemory.json");
  const settingsProbe = join(fixture, "settings-probe.mjs");
  await writeFile(settingsProbe, `import { readSettings } from ${JSON.stringify(pathToFileURL(join(root, "client", "memory-client.mjs")).href)}; console.log(JSON.stringify(readSettings(process.argv[2])));`);
  function settings() {
    try { return JSON.parse(execFileSync(process.execPath, [settingsProbe, home], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] })); }
    catch { return null; }
  }
  for (const value of [null, [], "scalar", 42, true]) {
    await writeFile(current, JSON.stringify(value));
    await writeFile(legacy, JSON.stringify({ recallMode: "off", maxMemories: 2 }));
    const result = settings();
    check(`invalid current ${JSON.stringify(value)} falls back`, result?.recallMode === "off" && result.maxMemories === 2);
    await writeFile(legacy, JSON.stringify(value));
    const defaults = settings();
    check(`invalid legacy ${JSON.stringify(value)} defaults`, defaults?.recallMode === "direct" && defaults.maxMemories === 3);
    const started = await run("SessionStart", {}, home);
    check(`SessionStart invalid settings ${JSON.stringify(value)}`, started.code === 0 && started.stdout === "" && started.stderr === "");
  }
  await writeFile(current, JSON.stringify({ recallMode: "direct", maxMemories: 3, similarityThreshold: 0.8, readContainerTags: ["extra"] }));
  const valid = settings();
  check("valid current overrides legacy and preserves settings", valid?.maxMemories === 3 && valid.minimumSimilarity === 0.8 && valid.readContainerTags[0] === "extra");
  for (const event of ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop", "Unknown"]) {
    for (const value of ["{", "null", "[]", '"scalar"', "42", "true"]) {
      const result = await run(event, value, home);
      check(`${event} rejects nonobject/malformed ${value}`, result.code === 0 && result.stdout === "" && result.stderr === "");
    }
  }
  const transcript = join(home, "recall.jsonl");
  await writeFile(transcript, "");
  const payload = { prompt: "orchard", session_id: "fixture-recall", transcript_path: transcript };
  const partial = await run("UserPromptSubmit", payload, home);
  const context = JSON.parse(partial.stdout).hookSpecificOutput.additionalContext;
  check("partial hydration count is visible", context.includes("Incomplete search: 0 spaces and 1 document indexes were unavailable."));
  check("partial context preserves source and size", context.includes("good-index") && context.length <= 3000 && partial.code === 0 && partial.stderr === "");
  check("partial failure does not expose IDs or error/key", ![marker, key].some(secret => context.includes(secret)));
  const statePath = join(home, "remnifold", "recall-state", `${createHash("sha256").update(payload.session_id).digest("hex")}.json`);
  const state = JSON.parse(await readFile(statePath, "utf8"));
  check("only selected document saved as read", Object.keys(state.documents).length === 1);
  const repeated = await run("UserPromptSubmit", payload, home);
  check("read suppression keeps existing partial system message", JSON.parse(repeated.stdout).systemMessage === "◪ Remnifold のトピック索引検索は一部の保存先を取得できませんでした。");
  mode = "success";
  const success = await run("UserPromptSubmit", { prompt: "orchard" }, home);
  const completeContext = JSON.parse(success.stdout).hookSpecificOutput.additionalContext;
  check("successful recall has no failure annotation", success.code === 0 && success.stderr === "" && !/Incomplete search|Partial search/.test(completeContext));
  mode = "all-failed";
  const failure = await run("UserPromptSubmit", { prompt: "orchard" }, home);
  check("all failed keeps existing system message", failure.code === 0 && failure.stderr === "" && JSON.parse(failure.stdout).systemMessage === "◪ Remnifold のトピック索引検索は一部の保存先を取得できませんでした。");
  check("real transport uses authenticated global index search and raw document GET", requests.some(row => row.body?.allContainers && row.body.indexOnly) && requests.some(row => row.path.startsWith("/v3/documents/")));
  const loader = join(fixture, "module-probe.mjs");
  const loadedPath = join(fixture, "loaded.json");
  await writeFile(loader, `import { registerHooks } from "node:module"; import { writeFileSync } from "node:fs"; const loaded = []; registerHooks({ load(url, context, nextLoad) { const result = nextLoad(url, context); if (url.startsWith("file:")) loaded.push(url); return result; } }); process.on("exit", () => writeFileSync(${JSON.stringify(loadedPath)}, JSON.stringify(loaded)));`);
  const permission = await run("PreToolUse", { tool_name: "mcp__remnifold__getDocument", tool_input: { id: "fixture" } }, home, ["--import", pathToFileURL(loader).href]);
  check("trusted read-only tool permission preserved", permission.code === 0 && permission.stderr === "" && JSON.parse(permission.stdout).hookSpecificOutput.permissionDecision === "allow" && JSON.parse(permission.stdout).hookSpecificOutput.updatedInput.id === "fixture", { code: permission.code, diagnostic: permission.stderr.replaceAll(fixture, "<fixture>") });
  const files = JSON.parse(await readFile(loadedPath, "utf8"));
  const clientFiles = files.filter(url => url.includes("/client/"));
  const bytes = (await Promise.all(clientFiles.map(async url => (await readFile(fileURLToPath(url))).length))).reduce((sum, count) => sum + count, 0);
  metrics = { preToolUse: { clientModulesLoaded: clientFiles.length, clientSourceBytes: bytes, modules: clientFiles.map(url => fileURLToPath(url).slice(root.length + 1).replaceAll("\\", "/")) } };
  check("PreToolUse avoids client graph", clientFiles.length === 0);
  const unknown = await run("Unknown", {}, home, ["--import", pathToFileURL(loader).href]);
  check("unknown event avoids client graph", unknown.code === 0 && unknown.stdout === "" && unknown.stderr === "" && !JSON.parse(await readFile(loadedPath, "utf8")).some(url => url.includes("/client/")));
  const writeTool = await run("PreToolUse", { tool_name: "mcp__remnifold__deleteDocument" }, home);
  check("write tool does not receive allow", writeTool.code === 0 && writeTool.stdout === "" && writeTool.stderr === "");
  const stop = await run("Stop", { session_id: "missing-transcript", transcript_path: join(home, "missing.jsonl") }, home);
  check("Stop retains fixed transcript failure classification", stop.code === 1 && stop.stdout === "" && stop.stderr.includes("[stage=transcript; cause=unavailable; action=repair-required]") && !stop.stderr.includes(home));
  const hashes = {};
  for (const file of ["client/memory-client.mjs", "client/memory-hooks.mjs", "scripts/hook-launcher.mjs", "scripts/opop-client-e2e.mjs"]) hashes[file] = createHash("sha256").update(await readFile(join(root, file))).digest("hex");
  report = { node: process.version, command: `node scripts/opop-client-e2e.mjs${baseline ? " --baseline" : ""}`, sourceHashes: hashes, checks, metrics, passed: checks.every(row => row.passed) };
} catch (error) {
  report = { node: process.version, checks, metrics, passed: false, failure: error instanceof assert.AssertionError ? error.message : "Fixture execution failed; inspect last completed scenario." };
} finally {
  await new Promise(done => server.close(done));
  assert.equal(children.size, 0, "No fixture subprocess may remain during cleanup");
  if (process.platform === "win32") {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(fixture, { allowedRoot: resolve(tmpdir()), recursive: true });
  } else {
    if (dirname(fixture) !== resolve(tmpdir()) || (await lstat(fixture)).isSymbolicLink()) throw new Error("Unexpected cleanup target");
    await rm(fixture, { recursive: true });
  }
  let previous = {};
  try { previous = JSON.parse(await readFile(artifact, "utf8")); } catch { /* 初回の成果物。 */ }
  const phase = baseline ? "before" : "after";
  const result = { ...previous, [phase]: { ...report, fixturePath: fixture, temporaryFixtureRemoved: true } };
  if (!baseline && result.before?.metrics && metrics) result.improvement = { clientModulesAvoided: result.before.metrics.preToolUse.clientModulesLoaded - metrics.preToolUse.clientModulesLoaded, clientSourceBytesAvoided: result.before.metrics.preToolUse.clientSourceBytes - metrics.preToolUse.clientSourceBytes, latencyMeasured: false };
  await mkdir(dirname(artifact), { recursive: true });
  await writeFile(artifact, JSON.stringify(result, null, 2) + "\n");
}
console.log(JSON.stringify({ phase: baseline ? "before" : "after", passed: report.passed, checks: checks.length, metrics, artifact }));
if (!baseline && !report.passed) process.exitCode = 1;
