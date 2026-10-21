import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
// 失敗条件を先に固定する: 短い設定キー漏れ、UTF-16境界破壊、旧ID変化、本文欠落、
// 旧receipt/旧canonical再送、履歴stream全件同時生成、重複候補/sort/limitの変化、検索query破壊。
// 実CLIと実hookだけをauthenticated loopback APIへ接続し、架空のfixtureだけを保存する。
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const phase = process.argv[2] || "after";
if (!["before", "after"].includes(phase) || process.argv.length > 3) throw new Error("Usage: node scripts/client-repair-e2e.mjs [before|after]");
const home = await mkdtemp(join(tmpdir(), "remnifold-client-repair-e2e-"));
const artifact = join(root, "dist", `client-repair-${phase}-validation.json`);
const checks = [], documents = [], searches = [];
const check = (name, passed, details = {}) => checks.push({ name, passed, ...details });
const hash = (value) => createHash("sha256").update(value).digest("hex");
let configuredKey = "local-fixture-memory-key", listed = [], searchRows = [];
const server = createServer(async (request, response) => {
  let raw = "";
  for await (const chunk of request) raw += chunk;
  if (request.headers.authorization !== `Bearer ${configuredKey}`) { response.writeHead(401); response.end(); return; }
  const body = raw ? JSON.parse(raw) : {};
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v3/documents/list") response.end(JSON.stringify({ documents: listed, pagination: { totalPages: 1 } }));
  else if (request.url === "/v3/documents") { documents.push(body); response.end(JSON.stringify({ id: `fixture-${documents.length}` })); }
  else if (request.url === "/v4/search") { searches.push(body); response.end(JSON.stringify({ searchScope: "all-containers", searchedContainers: ["memories"], spaceDiscoveryComplete: true, results: searchRows })); }
  else { response.writeHead(404); response.end("{}"); }
});
let environment, report;
async function run(args, { stdin, env = environment } = {}) {
  return new Promise((done, reject) => {
    const child = spawn(process.execPath, args, { cwd: root, env, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    const timer = setTimeout(() => { child.kill(); reject(new Error("Fixture subprocess exceeded 30 seconds")); }, 30_000);
    child.stdout.on("data", (chunk) => stdout += chunk);
    child.stderr.on("data", (chunk) => stderr += chunk);
    child.once("error", (error) => { clearTimeout(timer); reject(error); });
    child.once("close", (code) => { clearTimeout(timer); done({ code, stdout, stderr }); });
    child.stdin.end(stdin ? JSON.stringify(stdin) : "");
  });
}
async function transcript(path, id, user, assistant, extra = {}) {
  await writeFile(path, [
    { type: "session_meta", payload: { id, timestamp: "2020-01-01T00:00:00Z", cwd: root, ...extra } },
    { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: user }] } },
    { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text: assistant }] } },
  ].map(JSON.stringify).join("\n") + "\n");
}
// 実装helperを使わず、仕様の旧固定幅分割から期待IDを独立計算する。
function legacy(title, id, user, assistant, maxChars) {
  const oldTitle = title.slice(0, 200);
  const body = `### User request\n${user}\n\n### Final assistant response\n${assistant}`;
  const width = maxChars - `# ${oldTitle}\n\nSession: ${id}\nTurn: 1\n\n`.length - 80;
  const identity = `${id}:1:${hash(body)}`;
  const keys = [];
  for (let start = 0; start < body.length; start += width) keys.push(`${identity}:${keys.length + 1}:${hash(body.slice(start, start + width)).slice(0, 16)}`);
  return { body, width, keys, ids: keys.map((key) => `codex-turn-v2:${key}`) };
}
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  environment = { ...process.env, CODEX_HOME: home, REMNIFOLD_API_URL: `http://127.0.0.1:${server.address().port}`, REMNIFOLD_API_KEY: configuredKey };
  const sessions = join(home, "sessions");
  await mkdir(sessions);
  const id = "000-unicode", title = "T".repeat(199) + "😀𠮷 title";
  const width = legacy(title, id, "", "", 10_000).width;
  const user = "a".repeat(width - "### User request\n".length - 1) + "😀" + "b".repeat(width - 2) + "𠮷" + "c".repeat(width) + " preserved ending";
  const assistant = "完了しました 😀𠮷";
  const expected = legacy(title, id, user, assistant, 10_000);
  await transcript(join(sessions, `${id}.jsonl`), id, "smaller duplicate", "old response");
  await mkdir(join(home, "archived_sessions"));
  await transcript(join(home, "archived_sessions", `${id}.jsonl`), id, user, assistant);
  for (let index = 1; index <= 96; index++) await transcript(join(sessions, `candidate-${index}.jsonl`), `z-${index}`, "ordinary request", "ordinary result", { timestamp: `2020-02-${String(index % 28 + 1).padStart(2, "0")}T00:00:00Z` });
  await transcript(join(sessions, "subagent.jsonl"), "subagent", "subagent request", "result", { parent_thread_id: id });
  await writeFile(join(sessions, "malformed.jsonl"), "not json\n");
  await writeFile(join(home, "session_index.jsonl"), JSON.stringify({ id, thread_name: title }) + "\n");
  const observer = join(home, "observe-streams.mjs"), metrics = join(home, "streams.json");
  await writeFile(observer, `import fs from 'node:fs'; import { syncBuiltinESMExports } from 'node:module';\nconst original = fs.createReadStream; let active = 0, maximum = 0, opened = 0;\nfs.createReadStream = function(...args) { const stream = original.apply(this,args); active++; opened++; maximum=Math.max(maximum,active); stream.once('close',()=>active--); if(process.env.FIXTURE_STREAM_FAILURE && opened===1) setImmediate(()=>stream.destroy(new Error('Fixture metadata stream failure'))); return stream; }; syncBuiltinESMExports();\nprocess.once('exit',()=>fs.writeFileSync(process.env.FIXTURE_STREAM_METRICS,JSON.stringify({active,maximum,opened})));\n`);
  const historyArgs = [join(root, "client", "Import-RemnifoldHistory.mjs"), "--codex-home", home, "--state-path", join(home, "history-state.json"), "--include-recent", "--root-only", "--limit", "1", "--max-document-chars", "10000", "--apply"];
  const first = await run(["--import", pathToFileURL(observer).href, ...historyArgs], { env: { ...environment, FIXTURE_STREAM_METRICS: metrics } });
  check("actual history CLI succeeds", first.code === 0, { stderr: first.stderr });
  if (first.code !== 0) throw new Error("History CLI failed");
  const summary = JSON.parse(first.stdout), streamMetrics = JSON.parse(await readFile(metrics, "utf8"));
  check("metadata streams are bounded and fully released", streamMetrics.maximum <= 8 && streamMetrics.active === 0, streamMetrics);
  check("all candidates scanned before limit; duplicate largest and sorted earliest selected", summary.sourceFiles === 100 && summary.discoveredSessions === 97 && summary.duplicateTranscriptFiles === 1 && summary.selectedSessions === 1 && documents.every((doc) => doc.metadata.sessionId === id), { summary });
  const unicodeDocuments = documents.slice();
  check("Unicode title and each part remain well formed", unicodeDocuments.length === expected.ids.length && unicodeDocuments.every((doc) => doc.content.isWellFormed() && doc.metadata.title.isWellFormed() && doc.metadata.memoryIndex.title.isWellFormed()));
  check("concatenated Unicode parts preserve every source character", unicodeDocuments.map((doc) => doc.content.slice(doc.content.indexOf("\n\n") + 2)).join("") === expected.body);
  check("Unicode custom IDs and capture keys retain old fixed-width hashes", unicodeDocuments.map((doc) => doc.customId).join() === expected.ids.join() && unicodeDocuments.map((doc) => doc.metadata.captureKey).join() === expected.keys.join(), { ids: expected.ids, nominalWidth: width });
  listed = expected.keys.map((captureKey, index) => ({ id: `old-canonical-${index}`, metadata: { captureVersion: 2, captureKey } }));
  const oldCanonical = await run(historyArgs);
  check("old canonical capture rows suppress history upload", oldCanonical.code === 0 && documents.length === unicodeDocuments.length && JSON.parse(oldCanonical.stdout).unchanged === expected.keys.length);
  const failure = await run(["--import", pathToFileURL(observer).href, ...historyArgs], { env: { ...environment, FIXTURE_STREAM_METRICS: metrics, FIXTURE_STREAM_FAILURE: "1" } });
  const failureMetrics = JSON.parse(await readFile(metrics, "utf8"));
  check("metadata read failure rejects import without upload and releases streams", failure.code !== 0 && failure.stderr.includes("Fixture metadata stream failure") && documents.length === unicodeDocuments.length && failureMetrics.active === 0 && failureMetrics.maximum <= 8, failureMetrics);
  listed = [];
  const asciiHome = join(home, "ascii");
  await mkdir(join(asciiHome, "sessions"), { recursive: true });
  const asciiId = "ascii-compatibility", asciiTitle = "Ordinary ASCII title", asciiUser = "a".repeat(25000), asciiAssistant = "ordinary ASCII answer";
  const asciiExpected = legacy(asciiTitle, asciiId, asciiUser, asciiAssistant, 10000);
  await transcript(join(asciiHome, "sessions", "ascii.jsonl"), asciiId, asciiUser, asciiAssistant);
  await writeFile(join(asciiHome, "session_index.jsonl"), JSON.stringify({ id: asciiId, thread_name: asciiTitle }) + "\n");
  const beforeAscii = documents.length;
  const ascii = await run([join(root, "client", "Import-RemnifoldHistory.mjs"), "--codex-home", asciiHome, "--state-path", join(asciiHome, "state.json"), "--include-recent", "--max-document-chars", "10000", "--apply"]);
  const asciiDocuments = documents.slice(beforeAscii);
  check("ASCII output and every old ID remain identical", ascii.code === 0 && asciiDocuments.length === asciiExpected.ids.length && asciiDocuments.every((doc, index) => doc.customId === asciiExpected.ids[index] && doc.content === `# ${asciiTitle}\n\n${asciiExpected.body.slice(index * asciiExpected.width, (index + 1) * asciiExpected.width)}`));
  const stopPath = join(home, "stop-unicode.jsonl"), stopId = "stop-unicode";
  const stopExpectedWidth = legacy(title, stopId, "", "", 120_000).width;
  const stopUser = "s".repeat(stopExpectedWidth - "### User request\n".length - 1) + "😀" + "v".repeat(stopExpectedWidth - 2) + "𠮷 tail";
  const stopExpected = legacy(title, stopId, stopUser, assistant, 120_000);
  await transcript(stopPath, stopId, stopUser, assistant);
  await writeFile(join(home, "session_index.jsonl"), JSON.stringify({ id: stopId, thread_name: title }) + "\n");
  const payload = { session_id: stopId, transcript_path: stopPath, cwd: root };
  const stopArgs = [join(root, "scripts", "hook-launcher.mjs"), "Stop"];
  const beforeStop = documents.length;
  const stopped = await run(stopArgs, { stdin: payload });
  const stoppedDocuments = documents.slice(beforeStop);
  check("actual Stop preserves Unicode and old IDs", stopped.code === 0 && stoppedDocuments.length === stopExpected.ids.length && stoppedDocuments.every((doc) => doc.content.isWellFormed()) && stoppedDocuments.map((doc) => doc.customId).join() === stopExpected.ids.join() && stoppedDocuments.map((doc) => doc.content.slice(doc.content.indexOf("\n\n") + 2)).join("") === stopExpected.body);
  const retried = await run(stopArgs, { stdin: payload });
  check("acknowledged Stop retry uploads nothing", retried.code === 0 && documents.length === beforeStop + stoppedDocuments.length);
  const receiptHome = join(home, "old-receipts");
  await mkdir(join(receiptHome, "cloudflare-memory", "capture-state"), { recursive: true });
  await writeFile(join(receiptHome, "session_index.jsonl"), JSON.stringify({ id: stopId, thread_name: title }) + "\n");
  const receiptName = (await readdir(join(home, "remnifold", "capture-state"))).find((name) => name.endsWith(".json"));
  await writeFile(join(receiptHome, "cloudflare-memory", "capture-state", receiptName), JSON.stringify(stopExpected.ids));
  const oldReceipt = await run(stopArgs, { stdin: payload, env: { ...environment, CODEX_HOME: receiptHome } });
  check("old JSON receipts suppress Unicode resend", oldReceipt.code === 0 && documents.length === beforeStop + stoppedDocuments.length);
  for (const key of ["Q", "qX", "qX7", "qX7Z", "qX7Z9"]) {
    configuredKey = key;
    const shortHome = join(home, `short-${key.length}`);
    await mkdir(shortHome);
    const shortPath = join(shortHome, "short.jsonl"), shortId = `short-${key.length}`;
    await transcript(shortPath, shortId, `ordinary text ${key} with preserved discussion`, `answer uses ${key} and preserved result`);
    await writeFile(join(shortHome, "session_index.jsonl"), JSON.stringify({ id: shortId, thread_name: `ordinary ${key} title` }) + "\n");
    const count = documents.length;
    const result = await run(stopArgs, { stdin: { session_id: shortId, transcript_path: shortPath, cwd: root }, env: { ...environment, CODEX_HOME: shortHome, REMNIFOLD_API_KEY: key } });
    const doc = documents[count];
    check(`configured key length ${key.length} redacted in ordinary content and title`, result.code === 0 && Boolean(doc) && !doc.content.includes(key) && !doc.metadata.title.includes(key) && doc.content.includes("preserved discussion") && doc.content.includes("[REDACTED]"));
    const unsafeId = legacy(`ordinary ${key} title`, shortId, `ordinary text ${key} with preserved discussion`, `answer uses ${key} and preserved result`, 120000).ids[0];
    check(`redaction changes unsafe prior identity for key length ${key.length}`, Boolean(doc) && doc.customId !== unsafeId);
  }
  configuredKey = environment.REMNIFOLD_API_KEY;
  searchRows = [{ id: "unicode-index", containerTag: "memories", metadata: { memoryIndex: { version: 1, title: "recall " + "t".repeat(92) + "😀 title", description: "recall " + "d".repeat(151) + "𠮷 description", recallable: true } } }];
  const recalled = await run([join(root, "scripts", "hook-launcher.mjs"), "UserPromptSubmit"], { stdin: { session_id: "recall-unicode", cwd: root, prompt: "recall " + "r".repeat(992) + "😀 remaining" } });
  const recalledContext = recalled.stdout ? JSON.parse(recalled.stdout).hookSpecificOutput?.additionalContext : "";
  check("actual recall query and formatted index remain well formed", recalled.code === 0 && searches.length > 0 && searches.at(-1).q.isWellFormed() && Boolean(recalledContext) && recalledContext.isWellFormed(), { queryLength: searches.at(-1)?.q.length, contextLength: recalledContext?.length });
  // 隣接callerも本物のAPI transportを使用する。hookでの先行切断による見逃しを防ぐ。
  const { searchIndex, api } = await import("../client/memory-client.mjs");
  await searchIndex({ query: "recall " + "r".repeat(992) + "😀 tail", settings: { maxMemories: 5, minimumSimilarity: 0.7 }, request: (path, options) => api(path, { ...options, config: { baseUrl: environment.REMNIFOLD_API_URL, apiKey: configuredKey } }) });
  check("direct search query preserves UTF-16 boundary", searches.at(-1).q.isWellFormed() && searches.at(-1).q.length <= 1000);
  const { runSetup } = await import("./setup-server.mjs");
  const fixtureServer = join(home, "setup-server");
  await mkdir(join(fixtureServer, "migrations"), { recursive: true });
  await writeFile(join(fixtureServer, "package.json"), "{}");
  await writeFile(join(fixtureServer, "wrangler.example.jsonc"), "{}");
  const dummyWrangler = join(fixtureServer, "dummy-wrangler.mjs");
  await writeFile(dummyWrangler, "// Never executed\n");
  let setupError;
  try { await runSetup([], { serverDir: fixtureServer, wranglerBin: dummyWrangler, env: { REMNIFOLD_API_KEY: configuredKey }, output: () => {}, runner: async () => ({ code: 1, stderr: "x".repeat(1999) + "😀tail", stdout: "" }) }); }
  catch (error) { setupError = error; }
  check("setup diagnostic caller preserves UTF-16 boundary", Boolean(setupError) && setupError.message.isWellFormed() && setupError.message.includes("x".repeat(1999)) && !setupError.message.includes("tail"));
  const sourceHashes = Object.fromEntries(await Promise.all(["client/Import-RemnifoldHistory.mjs", "client/memory-index.mjs", "client/memory-hooks.mjs", "client/unicode-text.mjs", "client/memory-client.mjs", "scripts/setup-server.mjs", "scripts/client-repair-e2e.mjs"].map(async (path) => [path, hash(await readFile(join(root, path)))])));
  report = { phase, command: `node scripts/client-repair-e2e.mjs ${phase}`, node: process.version, sourceHashes, transports: ["actual history CLI -> authenticated loopback API", "actual Stop/UserPromptSubmit -> authenticated loopback API"], checks, passed: checks.every(({ passed }) => passed) };
} catch (error) { report = { phase, checks, passed: false, error: error.message }; }
finally {
  await new Promise((done) => server.close(done));
  if (process.platform === "win32") {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(home, { allowedRoot: resolve(tmpdir()), recursive: true });
  } else { if (dirname(home) !== resolve(tmpdir()) || (await lstat(home)).isSymbolicLink()) throw new Error("Unexpected cleanup target"); await rm(home, { recursive: true }); }
  await mkdir(dirname(artifact), { recursive: true });
  await writeFile(artifact, JSON.stringify({ ...report, temporaryFixtureRemoved: true }, null, 2) + "\n");
}
console.log(JSON.stringify({ passed: report.passed, artifact, failed: checks.filter(({ passed }) => !passed), error: report.error }));
if (!report.passed) process.exitCode = 1;
