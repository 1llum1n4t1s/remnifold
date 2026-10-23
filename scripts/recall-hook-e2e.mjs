import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
// 実hookプロセスとローカルHTTP APIを接続する再現用E2E。秘密・外部APIは使わない。
import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { documentIndex, formatIndexItem } from "../client/memory-index.mjs";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { appendFile, lstat, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
if (process.argv.length > 2 && (process.argv[2] !== "--runtime-root" || process.argv.length !== 4)) {
  throw new Error("Usage: node scripts/recall-hook-e2e.mjs [--runtime-root plugin-directory]");
}
const runtimeRoot = process.argv[3] ? resolve(process.argv[3]) : root;
const temporary = await mkdtemp(join(tmpdir(), "remnifold-recall-e2e-"));
const artifact = join(root, "dist", runtimeRoot === root ? "recall-hook-validation.json" : "recall-hook-installed-validation.json");
const checks = [];
// 失敗条件: 重複注入、更新/compact見逃し、出力超過、Git/API過剰呼出し、子の停止漏れ、fixtureの認証漏れ。
// 直近作業の失敗条件: 話題への依存、旧サーバーの誤採用、現在セッションの除外漏れ、既読channelの干渉、片側障害による正常側の欠落。
const children = new Set();
const configuredKey = "local-fixture-key";
let revision = "2026-09-30T00:00:00.000Z";
let failure = false;
let huge = false;
let empty = false;
let fixtureRows = null;
let recentRows = [];
let recentFailure = false;
let recentMarker = true;
const recentRequests = [];
const documentFetches = [];
let lastSearchQuery = "";
let report;
let requests = 0;
const rows = () => fixtureRows || Array.from({ length: huge ? 5 : 1 }, (_, index) => ({
  id: `doc-${index}`, containerTag: "fixture-space", updatedAt: revision, createdAt: "2026-09-01T00:00:00.000Z",
  topics: huge ? Array.from({ length: 500 }, (_, topic) => `Chrome-${topic}-${"&".repeat(100)}`) : ["Chrome"],
  searchExcerpt: huge ? "Chrome " + "😀&".repeat(100) : undefined,
  metadata: { project: huge ? "fixture-project " + "p".repeat(160) : "fixture-project", sm_project_id: huge ? "q".repeat(160) : undefined, memoryIndex: { version: 1, title: huge ? "Chrome " + "t".repeat(100) : "Chrome extension history", description: huge ? "Chrome extension " + "😀&".repeat(1000) : "Chrome extension implementation", sections: huge ? ["s".repeat(60), "u".repeat(60)] : [], recallable: true } },
}));
const server = createServer(async (request, response) => {
  if (request.headers.authorization !== `Bearer ${configuredKey}`) {
    response.writeHead(401); response.end(); return;
  }
  let body = "";
  for await (const chunk of request) body += chunk;
  const input = body ? JSON.parse(body) : {};
  if (request.url === "/v4/search") { lastSearchQuery = input.q; }
  requests += 1;
  if (request.url === "/v3/documents/list" && input.recentTasks === true) {
    recentRequests.push(input);
    response.writeHead(recentFailure ? 503 : 200, { "Content-Type": "application/json" });
    response.end(JSON.stringify(recentFailure ? { error: "private-recent-error-do-not-expose" }
      : { documents: recentRows.slice(0, input.limit), ...(recentMarker ? { recentTasks: true } : {}) }));
    return;
  }
  if (/^\/v3\/documents\//u.test(request.url)) documentFetches.push(request.url);
  response.writeHead(failure ? 503 : 200, { "Content-Type": "application/json" });
  response.end(JSON.stringify(failure ? { error: "fixture unavailable" } : {
    user: { id: "fixture-user" }, searchScope: "all-containers", searchedContainers: ["fixture-space"], spaceDiscoveryComplete: true,
    spaces: [{ containerTag: "fixture-space", memoryCount: 5 }], results: empty ? [] : rows(),
  }));
});
const environment = { ...process.env, CODEX_HOME: temporary,
  REMNIFOLD_API_KEY: configuredKey };
const transcript = join(temporary, "rollout.jsonl");
const payload = { prompt: "Chrome extension implementation", cwd: root, session_id: "fixture-session", transcript_path: transcript };
const count = (output) => output.hookSpecificOutput?.additionalContext?.length || 0;
async function hook(input = payload, event = "UserPromptSubmit", extraEnvironment = {}) {
  const args = [join(runtimeRoot, "scripts", "hook-launcher.mjs"), event];
  return await new Promise((done, reject) => {
    const child = spawn(process.execPath, args, { cwd: root, env: { ...environment, ...extraEnvironment }, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let output = "", errors = "";
    let timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 20_000);
    child.stdout.on("data", (chunk) => output += chunk);
    child.stderr.on("data", (chunk) => errors += chunk);
    child.once("error", (error) => { clearTimeout(timeout); reject(error); });
    child.once("close", (code) => {
      clearTimeout(timeout); children.delete(child);
      if (timedOut || code !== 0) { reject(new Error(timedOut ? "hook exceeded 20 seconds and was stopped" : `hook exit ${code}: ${errors}`)); return; }
      try { done(output ? JSON.parse(output) : {}); } catch (error) { reject(error); }
    });
    child.stdin.end(JSON.stringify(input));
  });
}
async function check(name, operation) {
  const detail = await operation();
  checks.push({ name, passed: true, ...detail });
}
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  environment.REMNIFOLD_API_URL = `http://127.0.0.1:${server.address().port}`;
  await writeFile(transcript, `${JSON.stringify({ type: "session_meta", payload: { id: "fixture-session" } })}\n`);
  await check("default three and explicit five through launcher and setup", async () => {
    fixtureRows = Array.from({ length: 5 }, (_, index) => ({ ...rows()[0], id: `small-${index}` }));
    const input = { ...payload, session_id: undefined };
    const items = (output) => (output.hookSpecificOutput.additionalContext.match(/\n  id=/gu) || []).length;
    const defaultItems = items(await hook(input));
    assert.equal(defaultItems, 3);
    const configPath = join(temporary, "remnifold.json");
    const setup = () => promisify(execFile)("pwsh", ["-NoProfile", "-File", join(runtimeRoot, "scripts", "setup-client.ps1"), "-CodexHome", temporary, "-BaseUrl", environment.REMNIFOLD_API_URL, "-Force"], { env: environment, windowsHide: true, timeout: 20_000 });
    await setup();
    assert.equal(JSON.parse(await readFile(configPath, "utf8")).maxMemories, 3);
    assert.equal(items(await hook(input)), 3);
    await writeFile(configPath, JSON.stringify({ maxMemories: 5 }));
    await setup();
    assert.equal(JSON.parse(await readFile(configPath, "utf8")).maxMemories, 5);
    const explicitItems = items(await hook(input));
    assert.equal(explicitItems, 5);
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(configPath, { allowedRoot: temporary });
    fixtureRows = null;
    return { defaultItems, setupDefault: 3, explicitItems, setupPreservesExplicit: 5 };
  });
  await check("duplicate injection suppression", async () => {
    const after = [count(await hook()), count(await hook())];
    assert.ok(after[0]); assert.equal(after[1], 0);
    return { characters: after };
  });
  await check("UI envelopes do not enter automatic search queries", async () => {
    const input = { ...payload, session_id: undefined };
    await hook({ ...input, prompt: '<in-app-browser-context source="ambient-ui-state">\nThis block is automatically supplied ambient UI state. unrelated-ui-marker\n</in-app-browser-context>\nChrome extension implementation' });
    assert.match(lastSearchQuery, /Chrome/u);
    assert.doesNotMatch(lastSearchQuery, /ambient|unrelated-ui-marker/u);
    await hook({ ...input, prompt: `<send_user_message_question_reply>${JSON.stringify([{ questionItemId: '["request_user_input_async","call_fixture","0"]', question: "Chrome extension implementation?", answer: "Keep retries bounded" }])}</send_user_message_question_reply>` });
    assert.match(lastSearchQuery, /Chrome/u);
    assert.match(lastSearchQuery, /retries/u);
    assert.doesNotMatch(lastSearchQuery, /questionItemId|request_user_input_async|call_fixture/u);
  });
  await check("legacy UI descriptions are projected without extra document fetches", async () => {
    const original = rows()[0];
    const broken = '[{"questionItemId":"[\\"request_user_input_async\\",\\"call_fixture';
    fixtureRows = [
      { ...original, id: "legacy-with-source", content: '### User request\n<in-app-browser-context source="ambient-ui-state">This block is automatically supplied ambient UI state.</in-app-browser-context>\nChrome extension implementation\n\n### Final assistant response\nCompleted', metadata: { ...original.metadata, memoryIndex: { ...original.metadata.memoryIndex, description: "This block is automatically supplied ambient UI state, not part of the user's request." } } },
      { ...original, id: "legacy-index-only", metadata: { ...original.metadata, memoryIndex: { ...original.metadata.memoryIndex, description: broken } } },
      { ...original, id: "legacy-opt-out", metadata: { ...original.metadata, memoryIndex: { ...original.metadata.memoryIndex, description: broken, recallable: false } } },
    ];
    try {
      const before = requests;
      const output = (await hook({ ...payload, session_id: "legacy-ui-session" })).hookSpecificOutput.additionalContext;
      assert.match(output, /id=legacy-with-source/u);
      assert.match(output, /id=legacy-index-only/u);
      assert.doesNotMatch(output, /id=legacy-opt-out|questionItemId|request_user_input_async|ambient UI state/u);
      assert.equal(requests - before, 2);
      return { requests: requests - before, detailsFetched: 0 };
    } finally { fixtureRows = null; }
  });
  await check("document update date", async () => {
    revision = "2026-09-30T01:00:00.000Z";
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
  });
  await check("session start and topic-less requests avoid Git discovery", async () => {
    const trace = join(temporary, "git-activity.log");
    environment.GIT_TRACE = trace;
    try {
      await writeFile(trace, "");
      const requestsBefore = requests;
      await hook(payload, "SessionStart");
      assert.equal(requests, requestsBefore);
      await hook({ ...payload, prompt: "修正して最適化してください" });
      const after = (await readFile(trace, "utf8")).match(/built-in: git/gu)?.length || 0;
      assert.equal(after, 0); assert.equal(requests, requestsBefore + 1);
      return { gitProcesses: after, apiRequests: requests - requestsBefore };
    } finally { delete environment.GIT_TRACE; }
  });
  await check("project identifier excludes account, action and partial-word noise", async () => {
    const row = (id, title, description, searchExcerpt) => ({ id, containerTag: "other-folder-space", updatedAt: revision, semanticSimilarity: 0.99,
      metadata: { memoryIndex: { version: 1, title, description, sections: [], recallable: true } }, ...(searchExcerpt ? { searchExcerpt } : {}) });
    fixtureRows = [row("wanted-memory", "Remnifold", "索引の読込"),
      row("support-noise", "Kagayoi.Support", String.raw`C:\Users\IMT\dev\Kagayoi.Support`),
      row("invoice-noise", "請求書", "修正してください"), row("release-noise", "Release", "Developer release"),
      row("excerpt-noise", "Other task", "別の話題", String.raw`C:\Users\IMT\dev\Other を修正してください`)];
    try {
      const prompt = String.raw`"C:\Users\IMT\dev\remnifold" メモリー機能側で修正で最適化が出来そうな部分が発見できていたら修正してください`;
      const input = { ...payload, prompt, session_id: "topic-noise-session" };
      const after = await hook(input);
      const text = after.hookSpecificOutput.additionalContext;
      assert.match(text, /id=wanted-memory/u);
      assert.doesNotMatch(text, /id=(?:support|invoice|release|excerpt)-noise/u);
      assert.doesNotMatch(lastSearchQuery, /Users|IMT|\bdev\b/u);
      assert.match(lastSearchQuery, /remnifold/u);
      return { indexItems: (text.match(/\n  id=/gu) || []).length, characters: count(after), crossFolderMatch: true };
    } finally { fixtureRows = null; }
  });
  await check("POSIX, UNC and nested file paths preserve their project identifier", async () => {
    fixtureRows = [{ id: "wanted-path", containerTag: "other-folder-space", updatedAt: revision,
      metadata: { memoryIndex: { version: 1, title: "Remnifold", description: "索引の読込", sections: [], recallable: true } } },
      { id: "unrelated-readme", containerTag: "other-project", updatedAt: revision,
        metadata: { memoryIndex: { version: 1, title: "README.md", description: "Documentation", sections: [], recallable: true } } }];
    try {
      for (const [session_id, path] of [["posix-path", '"/Users/IMT/dev/remnifold"'], ["unc-path", String.raw`"\\server\share\remnifold"`],
        ["windows-file", String.raw`"C:\Users\IMT\dev\remnifold\README.md"`],
        ["nested-file", String.raw`C:\Users\IMT\dev\remnifold\client\memory-client.mjs`],
        ["home-file", '"/home/imt/projects/remnifold/client/memory-client.mjs"']]) {
        const output = await hook({ ...payload, session_id, prompt: `${path} を最適化してください` });
        assert.match(output.hookSpecificOutput.additionalContext, /id=wanted-path/u);
        assert.doesNotMatch(output.hookSpecificOutput.additionalContext, /id=unrelated-readme/u);
        assert.doesNotMatch(lastSearchQuery, /Users|IMT|server|share/u);
      }
    } finally { fixtureRows = null; }
  });
  await check("dotted directory names outside known workspaces remain intact", async () => {
    fixtureRows = [{ id: "wanted-support", containerTag: "other-folder-space", updatedAt: revision,
      metadata: { memoryIndex: { version: 1, title: "Kagayoi.Support", description: "Support service", sections: [], recallable: true } } },
      { id: "unrelated-code", containerTag: "other-project", updatedAt: revision,
        metadata: { memoryIndex: { version: 1, title: "Code", description: "Code samples", sections: [], recallable: true } } }];
    try {
      for (const [session_id, path] of [["dotted-directory", String.raw`"C:\Code\Kagayoi.Support"`],
        ["dotted-trailing", String.raw`"C:\Code\Kagayoi.Support\"`], ["dotted-posix", '"/opt/Kagayoi.Support"']]) {
        const output = await hook({ ...payload, session_id, prompt: `${path} を最適化してください` });
        assert.match(output.hookSpecificOutput.additionalContext, /id=wanted-support/u);
        assert.doesNotMatch(output.hookSpecificOutput.additionalContext, /id=unrelated-code/u);
        assert.match(lastSearchQuery, /Kagayoi\.Support/u);
      }
    } finally { fixtureRows = null; }
  });
  await check("different session and unknown session", async () => {
    assert.ok(count(await hook({ ...payload, session_id: "other-session" })));
    for (let turn = 0; turn < 2; turn++) assert.ok(count(await hook({ ...payload, session_id: undefined })));
  });
  await check("transcript compaction and SessionStart compact", async () => {
    await appendFile(transcript, `${JSON.stringify({ type: "compacted", payload: { message: "fixture" } })}\n`);
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
    assert.deepEqual(await hook({ ...payload, source: "compact" }, "SessionStart"), {});
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
    await appendFile(transcript, `${JSON.stringify({ type: "event_msg", payload: { type: "context_compacted" } })}\n`);
    assert.ok(count(await hook()));
  });
  const stateDirectory = join(temporary, "remnifold", "recall-state");
  const stateFile = join(stateDirectory, `${createHash("sha256").update(payload.session_id).digest("hex")}.json`);
  await check("large complete compaction row resets without a following event", async () => {
    assert.equal(count(await hook()), 0);
    await appendFile(transcript, `${JSON.stringify({ type: "compacted", payload: { message: "x".repeat(200 * 1024) } })}\n`);
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
  });
  await check("incomplete transcript row persists no conversation text", async () => {
    const marker = "private-fixture-do-not-copy-秘密";
    const row = JSON.stringify({ type: "compacted", payload: { message: `${marker}${"x".repeat(200 * 1024)}` } });
    const start = (await readFile(transcript)).length;
    await appendFile(transcript, row.slice(0, 200));
    assert.ok(count(await hook()));
    const saved = await readFile(stateFile, "utf8");
    assert.ok(!saved.includes(marker)); assert.ok(!saved.includes("remainder"));
    assert.equal(JSON.parse(saved).offset, start);
    await appendFile(transcript, `${row.slice(200)}\n`);
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
  });
  await check("missing transcript retains recall on every prompt", async () => {
    const input = { ...payload, session_id: "no-transcript-session", transcript_path: undefined };
    assert.ok(count(await hook(input))); assert.ok(count(await hook(input)));
    assert.deepEqual(await hook({ ...input, source: "compact" }, "SessionStart"), {});
    assert.ok(count(await hook(input)));
    const path = join(stateDirectory, `${createHash("sha256").update(input.session_id).digest("hex")}.json`);
    await assert.rejects(readFile(path), { code: "ENOENT" });
  });
  await check("corrupt state and unreadable transcript fail open", async () => {
    await writeFile(stateFile, "broken fixture");
    assert.ok(count(await hook()));
    assert.ok(count(await hook({ ...payload, transcript_path: join(temporary, "missing.jsonl") })));
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(stateFile, { allowedRoot: temporary });
  });
  await check("unwritable state path fail open", async () => {
    // 他ケースの復元記録が参照する親ディレクトリを通常ファイルに置き換えない。
    const blockedHome = join(temporary, "unwritable-home");
    await mkdir(join(blockedHome, "remnifold"), { recursive: true });
    await writeFile(join(blockedHome, "remnifold", "recall-state"), "fixture file blocks directory");
    const blockedEnvironment = { CODEX_HOME: blockedHome };
    assert.ok(count(await hook(payload, "UserPromptSubmit", blockedEnvironment)));
    assert.ok(count(await hook(payload, "UserPromptSubmit", blockedEnvironment)));
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(blockedHome, { allowedRoot: temporary, recursive: true });
  });
  await check("API failure remains visible and does not mark seen", async () => {
    failure = true; assert.ok((await hook()).systemMessage); failure = false;
    assert.ok(count(await hook()));
  });
  await check("empty results remain silent and do not mark documents seen", async () => {
    const input = { ...payload, session_id: "empty-session" };
    empty = true; assert.deepEqual(await hook(input), {}); empty = false;
    assert.ok(count(await hook(input))); assert.equal(count(await hook(input)), 0);
  });
  const recentDocument = (id, overrides = {}) => ({
    id, containerTag: "other-project-space", createdAt: "2026-09-01T00:00:00.000Z", updatedAt: "2026-10-22T21:00:00.000Z",
    metadata: { project: "OrionCrossProject", type: "conversation", sessionId: `completed-${id}`,
      memoryIndex: { version: 1, title: "Invoice export completed", description: "CSV export now preserves Japanese names",
        sourceUpdatedAt: "2026-10-20T12:34:56.000Z", sections: [], recallable: true } },
    ...overrides,
  });
  const recentHeading = /直近の作業（話題との一致は未判定）/u;
  const context = (output) => output.hookSpecificOutput?.additionalContext || "";
  await check("recent work crosses project and topic without fetching documents", async () => {
    recentRows = [recentDocument("recent-cross-project")];
    const beforeRequests = recentRequests.length, beforeFetches = documentFetches.length;
    try {
      const input = { ...payload, session_id: "recent-cross-project-session" };
      const text = context(await hook(input));
      assert.match(text, recentHeading);
      assert.match(text, /id=recent-cross-project/u);
      assert.match(text, /OrionCrossProject/u);
      assert.match(text, /Invoice export completed/u);
      assert.match(text, /2026-10-20/u);
      assert.match(text, /id=doc-0/u);
      const request = recentRequests[beforeRequests];
      assert.equal(recentRequests.length - beforeRequests, 1);
      assert.deepEqual(request, { projection: "index", recentTasks: true, limit: 2, page: 1, excludeSessionId: input.session_id });
      assert.match(text, /\n  id=doc-0[^\n]*container=fixture-space/u,
        "An older topic match must retain its topic index rather than becoming a recent task");
      assert.equal(documentFetches.length, beforeFetches);
      assert.ok(text.length <= 3000);
      return { recentRequests: 1, detailsFetched: 0, characters: text.length, sourceDate: "2026-10-20" };
    } finally { recentRows = []; }
  });
  await check("topic-less prompts receive recent work while empty and slash requests remain silent", async () => {
    recentRows = [recentDocument("recent-topicless")];
    try {
      for (const prompt of ["続けて", "ありがとう"]) {
        const text = context(await hook({ ...payload, prompt, session_id: `recent-topicless-${prompt}` }));
        assert.match(text, recentHeading); assert.match(text, /id=recent-topicless/u);
        assert.doesNotMatch(text, /id=doc-0/u);
      }
      const before = requests;
      for (const prompt of ["", "   ", "/help"]) assert.deepEqual(await hook({ ...payload, prompt }), {});
      assert.equal(requests, before);
    } finally { recentRows = []; }
  });
  await check("recent revisions deduplicate independently and compact restores them", async () => {
    const input = { ...payload, prompt: "ありがとう", session_id: "recent-revisions" };
    recentRows = [recentDocument("recent-revision")];
    try {
      assert.match(context(await hook(input)), /id=recent-revision/u);
      assert.equal(count(await hook(input)), 0);
      const original = recentRows[0];
      recentRows = [{ ...original, updatedAt: "2026-10-23T12:00:00.000Z", metadata: { ...original.metadata,
        memoryIndex: { ...original.metadata.memoryIndex, sourceUpdatedAt: "2026-10-21T12:34:56.000Z", description: "Invoice export and retry completed" } } }];
      assert.match(context(await hook(input)), /id=recent-revision/u);
      assert.equal(count(await hook(input)), 0);
      recentRows = [recentDocument("recent-arrival"), ...recentRows];
      const arrival = context(await hook(input));
      assert.match(arrival, /id=recent-arrival/u);
      assert.doesNotMatch(arrival, /id=recent-revision/u);
      assert.equal(count(await hook(input)), 0);
      assert.deepEqual(await hook({ ...input, source: "compact" }, "SessionStart"), {});
      const compacted = context(await hook(input));
      assert.match(compacted, /id=recent-revision/u); assert.match(compacted, /id=recent-arrival/u);
      assert.equal(count(await hook(input)), 0);
    } finally { recentRows = []; }
  });
  await check("recent visibility does not suppress a later topic index and same-turn overlap appears once", async () => {
    const original = rows()[0];
    const shared = recentDocument("recent-topic-shared", { topics: ["Chrome"], metadata: { ...original.metadata,
      type: "conversation", sessionId: "completed-shared", memoryIndex: { ...original.metadata.memoryIndex, sourceUpdatedAt: revision } } });
    fixtureRows = [shared]; recentRows = [shared]; empty = true;
    try {
      const input = { ...payload, session_id: "recent-topic-independent" };
      assert.match(context(await hook(input)), recentHeading);
      empty = false;
      const topic = context(await hook(input));
      assert.match(topic, /id=recent-topic-shared/u);
      assert.doesNotMatch(topic, recentHeading);
      assert.equal(count(await hook(input)), 0);
      const overlap = context(await hook({ ...input, session_id: "recent-topic-overlap" }));
      assert.equal((overlap.match(/id=recent-topic-shared/gu) || []).length, 1);
    } finally { recentRows = []; fixtureRows = null; empty = false; }
  });
  await check("legacy recent response marker is required and does not mark unseen documents", async () => {
    const input = { ...payload, prompt: "ありがとう", session_id: "recent-legacy-marker" };
    recentRows = [recentDocument("recent-legacy")]; recentMarker = false;
    try {
      assert.equal(count(await hook(input)), 0);
      recentMarker = true;
      assert.match(context(await hook(input)), /id=recent-legacy/u);
    } finally { recentRows = []; recentMarker = true; }
  });
  await check("topic and recent failures preserve the healthy channel and hide exception text", async () => {
    recentRows = [recentDocument("recent-partial")];
    try {
      failure = true;
      const topicFailed = await hook({ ...payload, session_id: "recent-topic-failed" });
      assert.match(context(topicFailed), /id=recent-partial/u);
      assert.ok(topicFailed.systemMessage);
      failure = false; recentFailure = true;
      const recentFailed = await hook({ ...payload, session_id: "recent-request-failed" });
      assert.match(context(recentFailed), /id=doc-0/u);
      assert.ok(recentFailed.systemMessage);
      assert.doesNotMatch(JSON.stringify([topicFailed, recentFailed]), /private-recent-error-do-not-expose|fixture unavailable/u);
      recentFailure = false;
      assert.match(context(await hook({ ...payload, session_id: "recent-request-failed" })), /id=recent-partial/u);
    } finally { recentRows = []; failure = false; recentFailure = false; }
  });
  await check("recent settings disable requests and clamp the independent count", async () => {
    const configPath = join(temporary, "remnifold.json");
    recentRows = Array.from({ length: 4 }, (_, index) => recentDocument(`recent-limit-${index}`));
    try {
      await writeFile(configPath, JSON.stringify({ maxRecentTasks: 0 }));
      const before = recentRequests.length;
      const disabled = context(await hook({ ...payload, session_id: "recent-disabled" }));
      assert.match(disabled, /id=doc-0/u); assert.doesNotMatch(disabled, recentHeading);
      assert.equal(recentRequests.length, before);
      await writeFile(configPath, JSON.stringify({ maxMemories: 1, maxRecentTasks: 2 }));
      const separate = context(await hook({ ...payload, session_id: "recent-separate-limit" }));
      assert.match(separate, /id=doc-0/u);
      assert.equal((separate.match(/id=recent-limit-/gu) || []).length, 2);
      await writeFile(configPath, JSON.stringify({ maxRecentTasks: 99 }));
      const capped = context(await hook({ ...payload, prompt: "ありがとう", session_id: "recent-capped" }));
      assert.equal(recentRequests.at(-1).limit, 3);
      assert.equal((capped.match(/id=recent-limit-/gu) || []).length, 3);
    } finally { recentRows = []; await writeFile(configPath, "{}"); }
  });
  await check("recent slots survive large topic indexes within the combined context budget", async () => {
    huge = true;
    const original = recentDocument("recent-budget-0");
    recentRows = [original, recentDocument("recent-budget-1", { metadata: { ...original.metadata,
      memoryIndex: { ...original.metadata.memoryIndex, title: "Japanese output " + "😀&".repeat(500), description: "CSV " + "😀&".repeat(1000) } } })];
    try {
      const text = context(await hook({ ...payload, session_id: "recent-combined-budget" }));
      assert.match(text, recentHeading);
      assert.match(text, /id=recent-budget-0/u); assert.match(text, /id=recent-budget-1/u);
      assert.ok(text.length <= 3000);
      return { characters: text.length, maximumCharacters: 3000, recentItems: 2 };
    } finally { recentRows = []; huge = false; }
  });
  await check("concurrent hooks leave valid state", async () => {
    const input = { ...payload, session_id: "concurrent-session" };
    const output = await Promise.all([hook(input), hook(input)]);
    assert.ok(output.some((item) => count(item)));
    assert.equal(count(await hook(input)), 0);
    for (const name of await readdir(stateDirectory)) JSON.parse(await readFile(join(stateDirectory, name), "utf8"));
    return { simultaneousInjectionCount: output.filter(count).length, semantics: "concurrent calls may both recall; persisted state remains valid" };
  });
  await check("topic count and escaped output size bound", async () => {
    huge = true;
    const input = { ...payload, session_id: "large-session" };
    const formattedLengths = rows().slice(0, 3).map((row) => formatIndexItem(documentIndex(row)).length);
    assert.ok(formattedLengths.reduce((sum, size) => sum + size, 0) > 3000);
    const output = await hook(input);
    const text = output.hookSpecificOutput.additionalContext;
    assert.ok(text.length <= 3000);
    assert.match(text, /id=doc-0/); assert.match(text, /container=fixture-space/); assert.match(text, /provenance=fixture-project/); assert.match(text, /2026-09-30/);
    assert.doesNotMatch(text, /Chrome-3-/);
    const firstIds = [...text.matchAll(/\n  id=(doc-\d+)/gu)].map((match) => match[1]);
    assert.ok(firstIds.length > 0 && firstIds.length < 3);
    const next = (await hook(input)).hookSpecificOutput.additionalContext;
    const nextIds = [...next.matchAll(/\n  id=(doc-\d+)/gu)].map((match) => match[1]);
    assert.ok(nextIds.length);
    assert.ok(nextIds.every((id) => !firstIds.includes(id)));
    assert.ok(next.length <= 3000);
    return { characters: text.length, nextCharacters: next.length, firstIds, nextIds, formattedLengths, maximumCharacters: 3000, fixtureTopicCharacters: rows()[0].topics.join("").length };
  });
  await check("oversized middle index does not hide a later fitting index", async () => {
    const large = rows().slice(0, 3);
    fixtureRows = [large[0], large[1], { ...large[2], topics: ["Chrome"], searchExcerpt: undefined,
      metadata: { ...large[2].metadata, project: "fixture-project", sm_project_id: undefined,
        memoryIndex: { ...large[2].metadata.memoryIndex, description: "Chrome extension", sections: [] } } }];
    try {
      const input = { ...payload, prompt: "Chrome extension", session_id: "fit-later-session" };
      const text = (await hook(input)).hookSpecificOutput.additionalContext;
      assert.match(text, /id=doc-0/u); assert.match(text, /id=doc-2/u); assert.doesNotMatch(text, /id=doc-1/u);
      assert.ok(text.length <= 3000);
      const next = (await hook(input)).hookSpecificOutput.additionalContext;
      assert.match(next, /id=doc-1/u); assert.doesNotMatch(next, /id=doc-(?:0|2)/u);
      assert.equal(count(await hook(input)), 0);
      return { firstIds: ["doc-0", "doc-2"], nextIds: ["doc-1"], characters: text.length, maximumCharacters: 3000 };
    } finally { fixtureRows = null; }
  });
  await check("large transcript gap resets safely and next prompt deduplicates", async () => {
    huge = false;
    await appendFile(transcript, `${"x".repeat(1100 * 1024)}\n${JSON.stringify({ type: "compacted" })}\n`);
    assert.ok(count(await hook())); assert.equal(count(await hook()), 0);
  });
  await check("unknown launcher event remains a no-op", async () => {
    const before = requests;
    assert.deepEqual(await hook(payload, "UnknownEvent"), {});
    assert.equal(requests, before);
  });
  report = { command: `node scripts/recall-hook-e2e.mjs${runtimeRoot === root ? "" : ` --runtime-root ${JSON.stringify(runtimeRoot)}`}`, runtimeRoot, checks, requests, fixture: "local HTTP API + real hook subprocess; no remote memory", temporary, temporaryRemoved: false };
} finally {
  await Promise.all([...children].map((child) => new Promise((done) => { child.once("close", done); child.kill("SIGKILL"); })));
  await new Promise((done) => server.close(done));
  // mkdtempが今回作った専用ディレクトリだけを清掃する。
  assert.ok(temporary.startsWith(join(tmpdir(), "remnifold-recall-e2e-")));
  if (process.platform === "win32") await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(temporary, { allowedRoot: resolve(tmpdir()), recursive: true });
  else {
    assert.equal(dirname(temporary), resolve(tmpdir()));
    async function verify(path) {
      const item = await lstat(path);
      assert.ok(!item.isSymbolicLink(), "Linked temporary item");
      if (item.isDirectory()) for (const name of await readdir(path)) await verify(join(path, name));
    }
    await verify(temporary);
    await rm(temporary, { recursive: true });
    await assert.rejects(lstat(temporary), { code: "ENOENT" });
  }
}
report.temporaryRemoved = true;
report.pendingChildren = children.size;
await mkdir(dirname(artifact), { recursive: true });
await writeFile(artifact, JSON.stringify(report, null, 2));
process.stdout.write(`${JSON.stringify({ passed: checks.length, artifact })}\n`);
