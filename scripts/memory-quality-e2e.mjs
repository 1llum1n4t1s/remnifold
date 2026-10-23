// 失敗条件: skill混入、capture ID/part変更、ACK再送、追問の主題誤認、改訂重複、来歴欠落、子プロセス停止漏れ。
// 主題復元: 1MiBより前の主題消失、UTF-8/途中行の誤読、巨大行の保持、別session混入、8MiB/256KiB/期限の超過。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  const name = process.argv[i];
  if (!["--runtime-root", "--history-root"].includes(name) || !process.argv[i + 1] || options[name]) throw new Error("Usage: memory-quality-e2e.mjs [--runtime-root directory] [--history-root codex-home]");
  options[name] = resolve(process.argv[i + 1]);
}
const runtimeRoot = options["--runtime-root"] || root;
const { parseTaskTranscript, loadConfig } = await import(pathToFileURL(join(runtimeRoot, "client", "Import-RemnifoldHistory.mjs")));
const { projectRequestText } = await import(pathToFileURL(join(runtimeRoot, "client", "request-projection.mjs")));
const outputRoot = process.env.REMNIFOLD_VALIDATION_DIR || process.env.KAGAYOI_MEMORY_VALIDATION_DIR || join(root, "dist");
if (!isAbsolute(outputRoot)) throw new Error("Validation output root must be absolute");
const temporary = await mkdtemp(join(tmpdir(), "remnifold-quality-e2e-"));
const home = join(temporary, "home");
await mkdir(home);
const key = "local-memory-quality-fixture-key";
const environment = { ...process.env, CODEX_HOME: home, REMNIFOLD_API_KEY: key };
const children = new Set();
const checks = [];
let currentCheck = "initialization";
const metrics = { searches: 0, captures: 0, historySessions: 0 };
let searchRows = [];
let lastQuery = "";
const captured = [];
const stored = new Map();
const hash = (value) => createHash("sha256").update(value).digest("hex");
const skill = (content = "skill-only-canary SnowRunner Realtime VPS") => `<skill>\n<name>fixture</name>\n<path>D:/skills/fixture/SKILL.md</path>\n${content}\n</skill>`;
const link = "[$fixture](D:/skills/fixture/SKILL.md)";
const user = (text) => ({ type: "response_item", payload: { role: "user", content: [{ type: "input_text", text }] } });
const assistant = (text) => ({ type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text }] } });
async function transcript(session, turns) {
  const file = join(home, `${session}.jsonl`);
  await writeFile(file, [{ type: "session_meta", payload: { id: session, cwd: root } }, ...turns].map(JSON.stringify).join("\n") + "\n");
  return file;
}
const server = createServer(async (request, response) => {
  try {
    if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end(); return; }
    let body = "";
    for await (const chunk of request) { body += chunk; if (body.length > 2_000_000) throw new Error("Fixture request too large"); }
    let result;
    if (request.url === "/v4/search") {
      metrics.searches++; lastQuery = JSON.parse(body).q;
      result = { searchScope: "all-containers", searchedContainers: ["fixture-space"], spaceDiscoveryComplete: true, results: searchRows };
    } else if (request.url === "/v3/documents/list" && JSON.parse(body).recentTasks === true) {
      result = { recentTasks: true, documents: [] };
    } else if (request.method === "POST" && request.url === "/v3/documents") {
      const document = JSON.parse(body); captured.push(document); metrics.captures++;
      result = { id: `capture-${captured.length}` };
    } else if (request.url.startsWith("/v3/documents/")) {
      result = stored.get(decodeURIComponent(request.url.slice("/v3/documents/".length)));
      if (!result) { response.writeHead(404); response.end("{}"); return; }
    } else if (request.url === "/v3/container-tags") result = { spaces: [{ containerTag: "fixture-space" }] };
    else { response.writeHead(404); response.end("{}"); return; }
    response.writeHead(200, { "Content-Type": "application/json" }); response.end(JSON.stringify(result));
  } catch { response.writeHead(500); response.end("{}"); }
});
server.requestTimeout = 5_000;
server.headersTimeout = 5_000;

function run(args, input, mcp = false) {
  return new Promise((done, reject) => {
    const child = spawn(process.execPath, args, { cwd: root, env: environment, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let stdout = "", timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 30_000);
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      if (stdout.length > 2_000_000) child.kill("SIGKILL");
      if (mcp && stdout.includes("\n")) child.stdin.end();
    });
    // 子の診断に履歴や秘密が含まれる可能性があるため、失敗時も転記しない。
    child.stderr.resume();
    child.once("error", (error) => { clearTimeout(timeout); children.delete(child); reject(new Error(`Fixture subprocess failed: ${error.code || "spawn"}`)); });
    child.once("close", (code) => {
      clearTimeout(timeout); children.delete(child);
      if (timedOut || code !== 0) { reject(new Error(timedOut ? "Fixture subprocess exceeded 30 seconds" : `Fixture subprocess exit ${code}`)); return; }
      try { done(mcp ? JSON.parse(stdout.trim().split("\n")[0]).result : stdout ? JSON.parse(stdout) : {}); }
      catch { reject(new Error("Fixture subprocess returned invalid JSON")); }
    });
    if (mcp) child.stdin.write(JSON.stringify(input) + "\n");
    else child.stdin.end(JSON.stringify(input));
  });
}
const hook = (event, payload) => run([join(runtimeRoot, "scripts", "hook-launcher.mjs"), event], payload);
const tool = (name, args) => run([join(runtimeRoot, "client", "mcp-server.mjs")], { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, true);
const context = (output) => output.hookSpecificOutput?.additionalContext || "";
async function check(name, operation) { currentCheck = name; const details = await operation(); checks.push({ name, passed: true, ...details }); }
function legacyIds(session, turns, title = "Quality fixture", max = 120_000) {
  return turns.flatMap((turn, index) => {
    const body = `### User request\n${turn.user}\n\n### Final assistant response\n${turn.assistant}`;
    const size = max - `# ${title.slice(0, 200)}\n\nSession: ${session}\nTurn: ${index + 1}\n\n`.length - 80;
    const ids = [];
    for (let offset = 0, part = 1; offset < body.length; offset += size, part++) {
      const captureKey = `${session}:${index + 1}:${hash(body)}:${part}:${hash(body.slice(offset, offset + size)).slice(0, 16)}`;
      ids.push({ customId: `codex-turn-v2:${captureKey}`, captureKey });
    }
    return ids;
  });
}
function row(id, turn = 1, part = 1, date = "2026-10-19T00:00:00Z") {
  return { id, containerTag: "fixture-space", createdAt: date, updatedAt: date, topics: ["SnowRunner"],
    metadata: { sessionId: "revisions", turn, part, parts: 2, sm_project_id: "quality-project", project: "Quality", sourceTimestamp: date,
      memoryIndex: { version: 1, title: `SnowRunner ${id}`, description: "SnowRunner vehicle settings", sections: [], recallable: true } } };
}

// 清掃範囲の親・子双方を検査し、リンクや別作業へ辿らない。
async function safeCleanup(target, allowedRoot) {
  const resolved = resolve(target), boundary = resolve(allowedRoot);
  const inside = relative(boundary, resolved);
  if (!inside || inside === ".." || inside.startsWith(`..${sep}`) || isAbsolute(inside)) throw new Error("Cleanup target outside allowed root");
  for (let current = resolved; ; current = dirname(current)) {
    const stat = await lstat(current);
    if (stat.isSymbolicLink()) throw new Error("Cleanup path contains a link");
    if (dirname(current) === current) break;
  }
  const pending = [resolved];
  while (pending.length) {
    const current = pending.pop(), stat = await lstat(current);
    if (stat.isSymbolicLink()) throw new Error("Cleanup tree contains a link");
    if (stat.isDirectory()) for (const entry of await readdir(current)) pending.push(join(current, entry));
  }
  assert.equal(await realpath(resolved), resolved);
  await rm(resolved, { recursive: true, force: true });
}

let failure;
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  environment.REMNIFOLD_API_URL = `http://127.0.0.1:${server.address().port}`;
  await writeFile(join(home, "remnifold.json"), JSON.stringify({ maxMemories: 5 }));
  await check("skill projection and multipart legacy identities through Stop", async () => {
    const session = "quality-capture";
    const request = `${link}\nSnowRunner truck settings\n${skill("skill-only-canary ".repeat(15_000))}\nKeep the suspension soft.`;
    const answer = `Result includes literal ${skill("assistant-skill-example")}.`;
    const file = await transcript(session, [user(request), assistant(answer)]);
    await writeFile(join(home, "session_index.jsonl"), JSON.stringify({ id: session, thread_name: "Quality fixture" }) + "\n");
    const parsed = await parseTaskTranscript(file, [key]);
    const expected = legacyIds(session, parsed.turns);
    const before = captured.length;
    await hook("Stop", { session_id: session, transcript_path: file, cwd: root });
    const docs = captured.slice(before);
    assert.deepEqual(docs.map((doc) => ({ customId: doc.customId, captureKey: doc.metadata.captureKey })), expected);
    assert.ok(docs.length >= 3);
    const content = docs.map((doc) => doc.content).join("\n");
    assert.ok(!content.includes("skill-only-canary"));
    assert.equal((content.match(/SKILL\.md/gu) || []).length, 1);
    assert.ok(content.includes("SnowRunner") && content.includes(answer) && content.includes("suspension soft"));
    assert.ok(docs.some((doc) => doc.metadata.memoryIndex.recallable === false));
    assert.ok(docs.every((doc, i) => doc.metadata.part === i + 1 && doc.metadata.parts === docs.length));
    await hook("Stop", { session_id: session, transcript_path: file, cwd: root });
    assert.equal(captured.length, before + docs.length);
    return { parts: docs.length, legacyIdsPreserved: true, acknowledgedRetryPosts: 0 };
  });
  await check("skill-only completed work remains recallable without dropping ordinary variables or paths", async () => {
    const session = "quality-skill-only";
    const request = `${link}\n${skill()}`;
    const answer = "## SnowRunner settings\n$PATH and /tmp remain actual evidence.\n".repeat(8).trim();
    const file = await transcript(session, [user(request), assistant(answer)]);
    const before = captured.length;
    await hook("Stop", { session_id: session, transcript_path: file, cwd: root });
    assert.equal(captured.length, before + 1);
    assert.equal(captured.at(-1).metadata.memoryIndex.recallable, true);
    assert.ok(captured.at(-1).content.includes(answer));
    await hook("UserPromptSubmit", { prompt: "Use $PATH in /tmp and /var/cache/widget", cwd: root });
    assert.ok(lastQuery.includes("$PATH") && lastQuery.includes("tmp") && lastQuery.includes("widget"));
  });
  await check("skill links and structured skill text stay outside automatic queries", async () => {
    await hook("UserPromptSubmit", { prompt: `${link}\nSnowRunner truck settings\n${skill()}`, cwd: root });
    assert.match(lastQuery, /SnowRunner/u);
    assert.ok(!/SKILL\.md|skill-only-canary|VPS|Realtime/u.test(lastQuery));
  });
  await check("short followups use only previous user topics", async () => {
    for (const [index, topic] of ["SnowRunner truck settings", "Realtime 音声翻訳"].entries()) {
      const session = `followup-${index}`;
      const file = await transcript(session, [user(topic), assistant("assistant-only-canary Kubernetes"), user("お願い")]);
      const before = metrics.searches;
      await hook("UserPromptSubmit", { prompt: "お願い", session_id: session, transcript_path: file, cwd: root });
      assert.equal(metrics.searches, before + 1);
      assert.ok(lastQuery.includes(topic) && !lastQuery.includes("Kubernetes"));
    }
    for (const turns of [[], [user("お願い"), assistant("SnowRunner assistant-only-canary")], [user(skill()), assistant("SnowRunner")]]) {
      const session = `generic-${metrics.searches}-${turns.length}`;
      const file = await transcript(session, turns);
      const before = metrics.searches;
      await hook("UserPromptSubmit", { prompt: "お願い", session_id: session, transcript_path: file, cwd: root });
      assert.equal(metrics.searches, before);
    }
    const session = "bounded-followups";
    const file = await transcript(session, [user("SnowRunner truck settings"), ...Array.from({ length: 40 }, () => user("お願い"))]);
    const before = metrics.searches;
    await hook("UserPromptSubmit", { prompt: "お願い", session_id: session, transcript_path: file, cwd: root });
    assert.equal(metrics.searches, before, "Repeated followups must still count toward the history bound");
  });
  await check("followup scans bounded chunks across large tool rows and UTF-8 boundaries", async () => {
    const topic = "Realtime 音声翻訳";
    const largeTool = { type: "response_item", payload: { role: "tool", content: "Kubernetes".repeat(130_000) } };
    const file = await transcript("chunk-followup", [user(topic + " 語".repeat(40_000)), largeTool,
      user(skill()), user("[$Kubernetes](thread://other-task)"), user("Message Type: MESSAGE\nKubernetes"),
      user("<subagent_notification>Kubernetes</subagent_notification>"), user("お願い")]);
    // 改行のない末尾は、たとえJSONとして読めても主題として採用しない。
    await writeFile(file, await readFile(file, "utf8") + JSON.stringify(user("Kubernetes incomplete-tail-canary")));
    const before = metrics.searches;
    await hook("UserPromptSubmit", { prompt: "お願い", session_id: "chunk-followup", transcript_path: file, cwd: root });
    assert.equal(metrics.searches, before + 1);
    assert.ok(lastQuery.includes(topic) && !lastQuery.includes("Kubernetes") && !lastQuery.includes("\ufffd"));
    await hook("UserPromptSubmit", { prompt: "お願い", session_id: "wrong-session", transcript_path: file, cwd: root });
    assert.equal(metrics.searches, before + 1, "Header mismatch must reject even a recoverable topic");
    return { topicBeyondBytes: Buffer.byteLength(JSON.stringify(largeTool)), maxScanBytes: 8 * 1024 * 1024, maxRowBytes: 256 * 1024 };
  });
  await check("followup byte and row limits reject unavailable topics and deadline stops scanning", async () => {
    const tooOld = await transcript("too-old-followup", [user("SnowRunner truck settings"),
      { type: "response_item", payload: { role: "tool", content: "x".repeat(9 * 1024 * 1024) } }, user("お願い")]);
    const oversized = await transcript("oversized-followup", [user("SnowRunner " + "x".repeat(300 * 1024)), user("お願い")]);
    const deadlineFile = await transcript("deadline-followup", [user("SnowRunner truck settings"),
      { type: "response_item", payload: { role: "tool", content: "x".repeat(2 * 1024 * 1024) } }, user("お願い")]);
    const runs = [];
    for (const [session, file, forceDeadline] of [["too-old-followup", tooOld, false], ["oversized-followup", oversized, false], ["deadline-followup", deadlineFile, true]]) {
      const receipt = join(home, `${session}-reads.json`);
      const preload = join(home, `${session}-read-probe.mjs`);
      // 実launcherの対象履歴だけを計測する。期限ケースは2chunk目のread後に時計を進める。
      await writeFile(preload, `import fs from "node:fs"; import { syncBuiltinESMExports } from "node:module";
const open = fs.openSync, read = fs.readSync, now = Date.now; const files = new Set();
const metrics = { bytes: 0, reads: 0, maxRead: 0 };
fs.openSync = function(path, ...args) { const fd = open.call(this, path, ...args); if (path === ${JSON.stringify(file)}) files.add(fd); return fd; };
fs.readSync = function(fd, buffer, offset, length, position) { const count = read.call(this, fd, buffer, offset, length, position); if (files.has(fd)) { metrics.bytes += count; metrics.reads++; metrics.maxRead = Math.max(metrics.maxRead, length); } return count; };
Date.now = () => now() + (${forceDeadline} && metrics.reads >= 3 ? 9000 : 0);
syncBuiltinESMExports(); process.on("exit", () => fs.writeFileSync(${JSON.stringify(receipt)}, JSON.stringify(metrics)));
`);
      const before = metrics.searches;
      const started = performance.now();
      await run(["--import", pathToFileURL(preload).href, join(runtimeRoot, "scripts", "hook-launcher.mjs"), "UserPromptSubmit"],
        { prompt: "お願い", session_id: session, transcript_path: file, cwd: root });
      const elapsedMs = performance.now() - started;
      assert.equal(metrics.searches, before);
      const reads = JSON.parse(await readFile(receipt, "utf8"));
      assert.ok(reads.bytes <= 8 * 1024 * 1024 + 64 * 1024);
      assert.ok(reads.maxRead <= 128 * 1024);
      assert.ok(elapsedMs < 9_000, "Bounded local scan must finish before the hook deadline");
      if (forceDeadline) assert.equal(reads.reads, 3, "Elapsed hook deadline must stop further chunks");
      runs.push({ session, ...reads, elapsedMs });
    }
    return { runs };
  });
  await check("automatic latest revisions preserve separate turns and parts; manual retains history", async () => {
    searchRows = [row("old"), row("latest", 1, 1, "2026-10-20T00:00:00Z"), row("other-turn", 2), row("other-part", 1, 2)];
    const output = context(await hook("UserPromptSubmit", { prompt: "SnowRunner truck settings", cwd: root }));
    assert.ok(!output.includes("id=old ") && !output.includes("id=old\n"));
    for (const id of ["latest", "other-turn", "other-part"]) assert.ok(output.includes(`id=${id}`));
    const manual = await tool("search_memory", { query: "SnowRunner truck settings" });
    assert.ok(!manual.isError);
    const text = JSON.stringify(manual);
    for (const id of ["old", "latest", "other-turn", "other-part"]) assert.ok(text.includes(`\"${id}\"`));
    searchRows = [];
  });
  await check("legacy getDocument projects skills while preserving source identity and provenance", async () => {
    const answer = `Literal ${skill("assistant-skill-example")}.`;
    stored.set("legacy-source", { ...row("legacy-source"), content: `# SnowRunner\n\n### User request\n${link}\nSnowRunner truck settings\n${skill()}\n\n### Final assistant response\n${answer}` });
    const result = await tool("getDocument", { documentId: "legacy-source" });
    assert.ok(!result.isError);
    const document = result.structuredContent.document;
    assert.equal(document.id, "legacy-source");
    assert.equal(document.provenance.projectId, "quality-project");
    assert.equal(document.provenance.containerTag, "fixture-space");
    assert.ok(!document.content.includes("skill-only-canary"));
    assert.equal((document.content.match(/SKILL\.md/gu) || []).length, 1);
    assert.ok(document.content.includes(answer));
  });
  if (options["--history-root"]) {
    await check("optional local history is sanitized and replayed only against loopback", async () => {
      const historyRoot = options["--history-root"];
      const config = loadConfig(historyRoot);
      const scenarios = [
        { prefix: "01a158f7", topic: /SnowRunner/iu },
        { prefix: "01a15913", topic: /SnowRunner/iu },
        { prefix: "01a141bd", topic: /Wave\s*Link/iu, select: /(?:Wave\s*Link[\s\S]*(?:3|３|三|同時)|(?:3|３|三|同時)[\s\S]*Wave\s*Link)/iu },
      ];
      const prefixes = scenarios.map((scenario) => scenario.prefix);
      const found = new Map();
      const pending = [join(historyRoot, "sessions")];
      while (pending.length) {
        const directory = pending.pop();
        for (const entry of await readdir(directory, { withFileTypes: true })) {
          if (entry.isSymbolicLink()) continue;
          const path = join(directory, entry.name);
          if (entry.isDirectory()) pending.push(path);
          else if (entry.name.endsWith(".jsonl")) {
            const prefix = prefixes.find((value) => entry.name.includes(value));
            if (prefix) {
              const parsed = await parseTaskTranscript(path, [config.apiKey]);
              const scenario = scenarios.find((entry) => entry.prefix === prefix);
              if (!parsed.turns.some((entry) => (scenario.select || scenario.topic).test(projectRequestText(entry.user)))) continue;
              const modified = (await lstat(path)).mtimeMs;
              const previous = found.get(prefix);
              // 継続rolloutを含め、更新日時を優先し同値なら完了turn数で選ぶ。
              if (!previous || modified > previous.modified || modified === previous.modified && parsed.turns.length > previous.parsed.turns.length) {
                found.set(prefix, { parsed, modified });
              }
            }
          }
        }
      }
      assert.equal(found.size, prefixes.length, "Requested history sessions are missing");
      const historyMetrics = [];
      for (const scenario of scenarios) {
        const { parsed } = found.get(scenario.prefix);
        // SnowRunner/VPSは最初の具体的な依頼、RealtimeはWave Link 3の同時利用の依頼を使う。
        const matching = parsed.turns.filter((entry) => (scenario.select || scenario.topic).test(projectRequestText(entry.user)));
        const turn = scenario.select
          ? matching.find((entry) => /<skill\b|\bSKILL\.md/iu.test(entry.user)) || matching[0]
          : matching[0];
        assert.ok(turn, "History fixture has no matching completed task");
        const turns = [turn];
        const projected = projectRequestText(turn.user);
        const removed = turn.user.length - projected.length;
        const session = `history-quality-${metrics.historySessions++}`;
        const records = turns.flatMap((turn) => [user(turn.user), assistant(turn.assistant)]);
        const fixture = await transcript(session, records);
        const baseline = captured.length;
        await hook("Stop", { session_id: session, transcript_path: fixture, cwd: root });
        const docs = captured.slice(baseline);
        assert.ok(docs.length);
        assert.ok(!JSON.stringify(docs).includes(config.apiKey));
        const reparsed = await parseTaskTranscript(fixture, [key]);
        const title = reparsed.turns[0]?.user.split("\n")[0] || "Codex task";
        const expected = legacyIds(session, reparsed.turns, title);
        assert.deepEqual(docs.map((doc) => ({ customId: doc.customId, captureKey: doc.metadata.captureKey })), expected);
        await hook("Stop", { session_id: session, transcript_path: fixture, cwd: root });
        assert.equal(captured.length, baseline + docs.length);
        const source = docs.find((doc) => doc.metadata.memoryIndex.recallable !== false);
        assert.ok(source);
        const id = `history-source-${metrics.historySessions}`;
        const latest = { ...source, id, updatedAt: "2026-10-20T00:00:00Z", semanticSimilarity: 0.99,
          metadata: { ...source.metadata, memoryIndex: { ...source.metadata.memoryIndex, sourceUpdatedAt: "2026-10-20T00:00:00Z" } } };
        const old = { ...latest, id: "history-old-revision", updatedAt: "2026-10-01T00:00:00Z",
          metadata: { ...latest.metadata, memoryIndex: { ...latest.metadata.memoryIndex, sourceUpdatedAt: "2026-10-01T00:00:00Z" } } };
        const distractor = { id: "history-distractor", containerTag: "unrelated-space", semanticSimilarity: 0.99,
          topics: ["Astronomy"], metadata: { memoryIndex: { version: 1, title: "Astronomy telescope", description: "Saturn rings and telescope optics", sections: [], recallable: true } } };
        searchRows = [old, distractor, latest];
        const searches = metrics.searches;
        const recalled = context(await hook("UserPromptSubmit", { prompt: "お願い", session_id: session, transcript_path: fixture, cwd: root }));
        assert.equal(metrics.searches, searches + 1);
        assert.match(lastQuery, scenario.topic);
        assert.ok(!lastQuery.includes(config.apiKey));
        assert.ok(recalled.includes(`id=${id}`));
        assert.ok(!recalled.includes("id=history-old-revision") && !recalled.includes("id=history-distractor"));
        const receiptDirectory = join(home, "remnifold", "capture-state");
        const ackIds = new Set();
        for (const name of await readdir(receiptDirectory)) {
          if (!name.endsWith(".json")) continue;
          for (const ack of JSON.parse(await readFile(join(receiptDirectory, name), "utf8"))) ackIds.add(ack);
        }
        const acknowledged = expected.filter((doc) => ackIds.has(doc.customId)).length;
        assert.equal(acknowledged, expected.length);
        historyMetrics.push({ rawUserChars: turn.user.length, projectedUserChars: projected.length,
          removedSkillChars: removed, skillRemovalRatio: turn.user.length ? removed / turn.user.length : 0,
          expectedIds: expected.length, matchingIds: docs.length, acknowledgedIds: acknowledged, retryPosts: 0,
          recalledLatest: 1, recalledOld: 0, recalledDistractor: 0 });
        searchRows = [];
      }
      return { sessions: found.size, measurements: historyMetrics };
    });
  }
} catch (error) {
  // actual/expectedや原文を含む診断文は保存せず、このスクリプトの行番号だけを残す。
  const location = /memory-quality-e2e\.mjs:(\d+):(\d+)/u.exec(error?.stack || "");
  failure = { name: error?.name || "Error", check: currentCheck, ...(location ? { line: Number(location[1]) } : {}) };
  process.exitCode = 1;
} finally {
  for (const child of children) child.kill("SIGKILL");
  await Promise.all([...children].map((child) => new Promise((done) => { const timeout = setTimeout(done, 3_000); child.once("close", () => { clearTimeout(timeout); done(); }); })));
  server.closeAllConnections();
  await new Promise((done) => { const timeout = setTimeout(done, 3_000); server.close(() => { clearTimeout(timeout); done(); }); });
  try { await safeCleanup(temporary, dirname(temporary)); }
  catch { failure ||= { name: "CleanupError", check: checks.length + 1 }; process.exitCode = 1; }
  const report = { passed: !failure, checks, metrics, ...(failure ? { failure } : {}) };
  await mkdir(outputRoot, { recursive: true });
  await writeFile(join(outputRoot, "memory-quality-validation.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report));
}
