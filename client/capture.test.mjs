import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync, appendFileSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { cleanUserRequest, parseTaskTranscript, buildTurnDocuments, readSessionMeta, getProjectContext } from "./Import-RemnifoldHistory.mjs";
import { capture, findTranscript } from "./capture.mjs";

const user = (text, timestamp) => ({ ...(timestamp ? { timestamp } : {}), type: "response_item", payload: { role: "user", content: [{ type: "input_text", text }] } });
const assistant = (text, channel = "final", timestamp) => ({ ...(timestamp ? { timestamp } : {}), type: "response_item", payload: { role: "assistant", channel, content: [{ type: "output_text", text }] } });

test("製品リポジトリ改名後も1.xのprovenance IDを維持する", (t) => {
  const root = mkdtempSync(join(tmpdir(), "kagayoi-memory-project-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  execFileSync("git", ["init", "--quiet", root]);
  execFileSync("git", ["-C", root, "remote", "add", "origin", "git@github.com:1llum1n4t1s/kagayoi-memory.git"]);
  const project = getProjectContext(root);
  assert.equal(project.projectName, "kagayoi_memory");
  assert.equal(project.containerTag, "repo_cloudflare_supermemory__57a9aa7a5083510a");
});

function fixture(t, rows = []) {
  const home = mkdtempSync(join(tmpdir(), "memory-capture-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const directory = join(home, "sessions", "2020", "01", "01");
  mkdirSync(directory, { recursive: true });
  const path = join(directory, "rollout-task-a.jsonl");
  const meta = { id: "task-a", session_id: "task-a", cwd: home };
  writeFileSync(path, [ { type: "session_meta", payload: meta }, ...rows ].map(JSON.stringify).join("\n") + "\n");
  writeFileSync(join(home, "session_index.jsonl"), JSON.stringify({ id: "task-a", thread_name: "保存テスト" }) + "\n");
  return { home, path, meta, payload: { session_id: "task-a", transcript_path: path },
    options: { codexHome: home, config: { baseUrl: "https://example.invalid", apiKey: "test-only-secret" } } };
}

async function listen(t, handler) {
  const server = createServer(handler);
  await new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  t.after(() => new Promise((resolveClose) => server.close(resolveClose)));
  return `http://127.0.0.1:${server.address().port}`;
}

function runHistoryImport(args) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("./Import-RemnifoldHistory.mjs", import.meta.url)), ...args], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (code) => resolveRun({ code, stdout, stderr }));
  });
}

test("自動注入文を除き、同じメッセージ内の実要求は保持する", () => {
  assert.equal(cleanUserRequest("# AGENTS.md instructions\n\n<INSTRUCTIONS>legacy rules</INSTRUCTIONS>"), "");
  assert.equal(cleanUserRequest("# AGENTS.md instructions for C:\\test\n<INSTRUCTIONS>rules</INSTRUCTIONS>"), "");
  assert.equal(cleanUserRequest("<recommended_plugins>plugins</recommended_plugins>\n# AGENTS.md instructions for C:\\test\n<INSTRUCTIONS>rules</INSTRUCTIONS>\n<environment_context>env</environment_context>"), "");
  for (const tag of ["recommended_plugins", "environment_context", "codex_internal_context", "remnifold-context", "remnifold-recall", "remnifold-index", "kagayoi-memory-context", "kagayoi-memory-recall", "kagayoi-memory-index", "supermemory-context", "supermemory-recall", "supermemory-index"]) {
    assert.equal(cleanUserRequest(`<${tag}>injected\nnoise</${tag}>\n直して`), "直して");
  }
  assert.equal(cleanUserRequest("## Referenced chats with Codex:\nwrapper\n## My request:\nタスクを探して"), "タスクを探して");
  assert.equal(cleanUserRequest("AGENTS.md の手順を直して"), "AGENTS.md の手順を直して");
});

test("response annotationは利用者のコメントと参照箇所だけを依頼として保持する", () => {
  const source = {
    messageId: "internal-message-id",
    startOffset: 10,
    endOffset: 20,
  };
  const annotations = JSON.stringify([
    { text: "以前の回答", annotation: "この方針で対応してください。", source },
    { text: "別の回答", annotation: "Codex公式メモリは見ないでください。", source },
  ]);
  const cleaned = cleanUserRequest(`# Response annotations:\n自動案内と内部metadata\n<response-annotations>${annotations}</response-annotations>\n\n## My request:\n保存情報も整理してください。`);
  assert.equal(cleaned, [
    "### Feedback on earlier response 1\n\nReferenced assistant response:\n以前の回答\n\nUser comment:\nこの方針で対応してください。",
    "### Feedback on earlier response 2\n\nReferenced assistant response:\n別の回答\n\nUser comment:\nCodex公式メモリは見ないでください。",
    "### Current user request\n保存情報も整理してください。",
  ].join("\n\n"));
  assert.doesNotMatch(cleaned, /messageId|startOffset|endOffset|自動案内と内部metadata/);
  assert.equal(cleanUserRequest(`<supermemory-recall><response-annotations>${annotations}</response-annotations></supermemory-recall>\n直して`), "直して");
  assert.equal(cleanUserRequest("# Response annotations:\n案内\n<response-annotations>{invalid}</response-annotations>\n## My request:\n本文だけ"), "本文だけ");
});

test("完了応答のみ抽出し、ツール・分析・進捗・未完了要求・秘密値を保存しない", async (t) => {
  const f = fixture(t, [user("<environment_context>noise</environment_context>"), user("依頼 test-only-secret"),
    assistant("analysis", "analysis"), assistant("progress", "commentary"),
    { type: "response_item", payload: { type: "function_call_output", output: "secret output" } },
    assistant("完了 <private>private detail</private>"), user("未完了"), assistant("working", "commentary")]);
  const parsed = await parseTaskTranscript(f.path, ["test-only-secret"]);
  assert.deepEqual(parsed.turns, [{ user: "依頼 [REDACTED]", assistant: "完了 [REDACTED]" }]);
});

test("旧形式の重複イベントを一つにまとめる", async (t) => {
  const f = fixture(t, [{ type: "event_msg", payload: { type: "user_message", message: "依頼" } }, user("依頼"),
    { type: "event_msg", payload: { type: "assistant_output_text", text: "完了" } }, assistant("完了")]);
  assert.equal((await parseTaskTranscript(f.path, [])).turns.length, 1);
});

test("重複の行間隔は採用イベントから数え、除外イベントの伏字数と原文時刻を保つ", async (t) => {
  const sourceTimestamp = "2026-09-11T01:02:03.456Z";
  const f = fixture(t, [
    assistant("依頼前の応答"),
    user("依頼 test-only-secret", sourceTimestamp),
    user("依頼 test-only-secret", "2026-09-11T01:03:00.000Z"),
    null,
    {},
    assistant("途中経過", "commentary"),
    user("依頼 test-only-secret"),
    user("依頼 test-only-secret"),
    assistant("完了 test-only-secret"),
    user("未完了の依頼"),
  ]);
  appendFileSync(f.path, "{invalid\n");
  assert.deepEqual(await parseTaskTranscript(f.path, ["test-only-secret"]), {
    turns: [{ user: "依頼 [REDACTED]\n\n依頼 [REDACTED]", assistant: "完了 [REDACTED]", sourceTimestamp }],
    redactions: 5,
  });
});

test("実機のphase形式で進捗を完了応答と誤認しない", async (t) => {
  const progress = assistant("進捗");
  delete progress.payload.channel;
  progress.payload.phase = "commentary";
  const final = assistant("完了");
  delete final.payload.channel;
  final.payload.phase = "final_answer";
  const f = fixture(t, [user("完了した依頼"), final, user("実行中の依頼"), progress]);
  assert.deepEqual((await parseTaskTranscript(f.path, [])).turns, [{ user: "完了した依頼", assistant: "完了" }]);
});

test("turnの原文イベント時刻をmetadataへ保持するが文書IDには含めない", async (t) => {
  const sourceTimestamp = "2026-09-11T01:02:03.456Z";
  const f = fixture(t, [user("依頼", sourceTimestamp), assistant("完了", "final", "2026-09-11T01:03:04.567Z")]);
  const transcript = await parseTaskTranscript(f.path, []);
  assert.deepEqual(transcript.turns, [{ user: "依頼", assistant: "完了", sourceTimestamp }]);
  const project = getProjectContext(f.home);
  const [document] = buildTurnDocuments({ meta: f.meta }, transcript, project, "タイトル");
  const [withoutTimestamp] = buildTurnDocuments({ meta: f.meta }, { turns: [{ user: "依頼", assistant: "完了" }] }, project, "タイトル");
  assert.equal(document.metadata.sourceTimestamp, sourceTimestamp);
  assert.deepEqual(document.metadata.memoryIndex, {
    version: 1,
    title: "タイトル",
    description: "依頼",
    sections: [],
    recallable: true,
    sourceKind: "conversation",
    sourceUpdatedAt: sourceTimestamp,
  });
  assert.equal(document.customId, withoutTimestamp.customId);
  assert.equal(document.metadata.captureKey, withoutTimestamp.metadata.captureKey);
});

test("assistant回答を索引上の確認済み事実へ昇格しない", () => {
  const transcript = { turns: [{ user: "保存内容を整理して", assistant: "接続は確認済みです。" }] };
  const project = { projectName: "project", containerTag: "repo_project" };
  const [document] = buildTurnDocuments({ meta: { id: "task", rootSessionId: "task", isSubagent: false } }, transcript, project, "記憶の整理");
  assert.equal(document.customId, "codex-turn-v2:task:1:f83103950ddfeda11140b10b05c9fc15e1fb9944159f7045d87744b821bf0414:1:f83103950ddfeda1");
  assert.equal(document.metadata.captureKey, "task:1:f83103950ddfeda11140b10b05c9fc15e1fb9944159f7045d87744b821bf0414:1:f83103950ddfeda1");
  assert.equal(document.containerTag, "memories");
  assert.equal(document.reuseExistingCapture, true);
  assert.equal(document.metadata.project, "project");
  assert.equal(document.metadata.sm_project_id, "repo_project");
  assert.equal(document.metadata.memoryIndex.description, "保存内容を整理して");
  assert.doesNotMatch(JSON.stringify(document.metadata.memoryIndex), /確認済み|verified|evidence/i);
  assert.equal(document.content, "# 記憶の整理\n\n### User request\n保存内容を整理して\n\n### Final assistant response\n接続は確認済みです。");
  assert.doesNotMatch(document.content, /^Session:|\nSession:|^Turn:|\nTurn:|^Part:|\nPart:/m);
});

test("明示した保存containerもproject provenanceと決定的IDを変えない", () => {
  const transcript = { turns: [{ user: "保存内容を整理して", assistant: "接続は確認済みです。" }] };
  const project = { projectName: "project", containerTag: "repo_project" };
  const [common] = buildTurnDocuments({ meta: { id: "task", rootSessionId: "task", isSubagent: false } }, transcript, project, "記憶の整理");
  const [explicit] = buildTurnDocuments({ meta: { id: "task", rootSessionId: "task", isSubagent: false } }, transcript, project, "記憶の整理", 120_000, "intentional-space");
  assert.equal(explicit.containerTag, "intentional-space");
  assert.equal(explicit.reuseExistingCapture, undefined);
  assert.equal(explicit.metadata.sm_project_id, "repo_project");
  assert.equal(explicit.customId, common.customId);
  assert.equal(explicit.metadata.captureKey, common.metadata.captureKey);
});

test("project pathのない履歴も共通containerへ保存しprovenanceを捏造しない", () => {
  const [document] = buildTurnDocuments({ meta: { id: "old-task", rootSessionId: "old-task", isSubagent: false } },
    { turns: [{ user: "古い依頼", assistant: "古い結果" }] }, {}, "古い履歴");
  assert.equal(document.containerTag, "memories");
  assert.equal(document.reuseExistingCapture, undefined);
  assert.equal(document.metadata.sm_scope, "shared");
  assert.equal(document.metadata.project, undefined);
  assert.equal(document.metadata.sm_project_id, undefined);
});

test("履歴importは全space一覧から旧containerの同じcaptureKeyを検出して再作成しない", async (t) => {
  const f = fixture(t, [user("古い依頼"), assistant("古い結果")]);
  const transcript = await parseTaskTranscript(f.path, []);
  const [planned] = buildTurnDocuments({ meta: f.meta }, transcript, getProjectContext(f.home), "保存テスト");
  const listBodies = [];
  const baseUrl = await listen(t, (request, response) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      assert.equal(request.url, "/v3/documents/list");
      listBodies.push(JSON.parse(body));
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({
        documents: [{ id: "legacy-id", containerTags: [planned.metadata.sm_project_id], metadata: planned.metadata }],
        pagination: { currentPage: 1, totalPages: 1 },
      }));
    });
  });
  writeFileSync(join(f.home, "remnifold.json"), JSON.stringify({ baseUrl, apiKey: "history-test-key" }));
  const result = await runHistoryImport(["--codex-home", f.home, "--include-recent", "--session-id", "task-a"]);
  assert.equal(result.code, 0, result.stderr);
  assert.deepEqual(listBodies, [{ page: 1, limit: 50, projection: "capture" }]);
  const report = JSON.parse(result.stdout);
  assert.deepEqual(report.containerTags, ["memories"]);
  assert.equal(report.plannedDocuments, 1);
  assert.equal(report.unchanged, 1);
  assert.equal(report.created, 0);
});

test("作業中の補足要求と、別ターンで繰り返された同じ要求を失わない", async (t) => {
  const f = fixture(t, [user("直して"), assistant("確認中", "commentary"), user("Safariです"),
    assistant("修正済み"), user("直して"), assistant("再修正済み")]);
  assert.deepEqual((await parseTaskTranscript(f.path, [])).turns, [
    { user: "直して\n\nSafariです", assistant: "修正済み" },
    { user: "直して", assistant: "再修正済み" },
  ]);
});

test("別ターンを追加しても前の文書は保持され、再実行は送信しない", async (t) => {
  const f = fixture(t, [user("最初の依頼"), assistant("最初の結果")]);
  const remote = new Map();
  let calls = 0;
  const options = { ...f.options, send: async (doc) => { calls++; remote.set(doc.customId, doc); return { id: doc.customId }; } };
  assert.equal((await capture(f.payload, options)).saved, 1);
  const original = [...remote.values()][0];
  appendFileSync(f.path, [user("次の依頼"), assistant("次の結果")].map(JSON.stringify).join("\n") + "\n");
  assert.equal((await capture(f.payload, options)).saved, 1);
  assert.equal((await capture(f.payload, options)).saved, 0);
  assert.equal(calls, 2);
  assert.equal(remote.size, 2);
  assert.deepEqual(remote.get(original.customId), original);
  assert.equal(original.metadata.title, "保存テスト");
  assert.equal(original.metadata.sessionId, "task-a");
  assert.equal(original.metadata.sm_scope, "project");
  assert.equal(original.containerTag, "memories");
  assert.match(original.metadata.sm_project_id, /^repo_memory_capture_.*__[a-f0-9]{16}$/);
});

test("通信結果不明の再送は同じIDで、未確認文書を既送にしない", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("結果")]);
  const ids = [];
  const options = { ...f.options, send: async (doc) => { ids.push(doc.customId); throw new Error("lost response"); } };
  await assert.rejects(capture(f.payload, options), /lost response/);
  options.send = async (doc) => { ids.push(doc.customId); return { id: "stored" }; };
  assert.equal((await capture(f.payload, options)).saved, 1);
  assert.equal(ids[0], ids[1]);
});

test("APIが成功を確認しない応答では再試行できる", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("結果")]);
  await assert.rejects(capture(f.payload, { ...f.options, send: async () => ({ error: "failed" }) }), /acknowledge/);
  assert.equal((await capture(f.payload, { ...f.options, send: async () => ({ id: "ok" }) })).saved, 1);
});

test("途中失敗後は成功分を保持して残りを送る", async (t) => {
  const f = fixture(t, [user("一"), assistant("結果一"), user("二"), assistant("結果二")]);
  let count = 0;
  await assert.rejects(capture(f.payload, { ...f.options, send: async () => {
    if (++count === 2) throw new Error("offline"); return { id: "first" };
  } }), /offline/);
  const remaining = [];
  await capture(f.payload, { ...f.options, send: async (doc) => { remaining.push(doc); return { id: "second" }; } });
  assert.equal(remaining.length, 1);
  assert.match(remaining[0].content, /結果二/);
});

test("1.xの保存レシートを新しいturnと統合し、名称変更後も旧文書を再送しない", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("結果")]);
  await capture(f.payload, { ...f.options, send: async (document) => ({ id: document.customId }) });
  const currentDirectory = join(f.home, "remnifold", "capture-state");
  const legacyDirectory = join(f.home, "cloudflare-memory", "capture-state");
  const receiptName = readdirSync(currentDirectory).find((name) => name.endsWith(".json"));
  mkdirSync(legacyDirectory, { recursive: true });
  writeFileSync(join(legacyDirectory, receiptName), readFileSync(join(currentDirectory, receiptName)));
  rmSync(join(f.home, "remnifold"), { recursive: true, force: true });
  appendFileSync(f.path, [user("次の依頼"), assistant("次の結果")].map(JSON.stringify).join("\n") + "\n");

  const sent = [];
  const result = await capture(f.payload, { ...f.options, send: async (document) => { sent.push(document); return { id: document.customId }; } });
  assert.deepEqual(result, { saved: 1, pending: 0, completedTurns: 2 });
  assert.equal(sent.length, 1);
  const migratedReceipts = JSON.parse(readFileSync(join(currentDirectory, receiptName), "utf8"));
  assert.equal(migratedReceipts.length, 2);

  let retried = 0;
  const retry = await capture(f.payload, { ...f.options, send: async () => { retried += 1; return { id: "unexpected" }; } });
  assert.deepEqual(retry, { saved: 0, pending: 0, completedTurns: 2 });
  assert.equal(retried, 0);
});

test("レシート更新失敗でもSQLite transactionを解放して次回再試行できる", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("結果")]);
  const project = getProjectContext(f.home);
  const stateDirectory = join(f.home, "remnifold", "capture-state");
  const stateId = createHash("sha256").update(`${f.options.config.baseUrl}:${project.containerTag}:${f.meta.id}`).digest("hex");
  const statePath = join(stateDirectory, `${stateId}.json`);
  await assert.rejects(capture(f.payload, { ...f.options, send: async (document) => {
    mkdirSync(stateDirectory, { recursive: true });
    writeFileSync(statePath, "{invalid", "utf8");
    return { id: document.customId };
  } }), /JSON/);
  const databasePath = join(stateDirectory, "capture-coordination.sqlite3");
  assert.equal(existsSync(databasePath), true);
  assert.equal(existsSync(`${statePath}.lock`), false);
  writeFileSync(statePath, "[]", "utf8");
  const retry = await capture(f.payload, { ...f.options, send: async (document) => ({ id: document.customId }) });
  assert.deepEqual(retry, { saved: 1, pending: 0, completedTurns: 1 });
});

test("古い日付の継続タスクを探索し、別タスクの明示パスを拒否する", async (t) => {
  const f = fixture(t);
  assert.equal((await findTranscript({ session_id: "task-a" }, f.home)).filePath, f.path);
  await assert.rejects(findTranscript({ ...f.payload, session_id: "task-b" }, f.home), /mismatch/);
});

test("session_idのない履歴も自身のidを親タスクIDとして読める", async (t) => {
  const f = fixture(t);
  writeFileSync(f.path, JSON.stringify({ type: "session_meta", payload: { id: "old-task", cwd: f.home } }) + "\n");
  const meta = await readSessionMeta(f.path);
  assert.equal(meta.rootSessionId, "old-task");
  assert.equal(meta.isSubagent, false);
});

test("自動保存と復元は同じIDを使い、分割しても本文を欠落させない", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("長い結果".repeat(4000))]);
  const transcript = await parseTaskTranscript(f.path, []);
  const docs = buildTurnDocuments({ meta: f.meta }, transcript, getProjectContext(f.home), "タイトル", 10000);
  assert.ok(docs.length > 1);
  assert.ok(docs.every((doc) => doc.content.length <= 10000));
  assert.deepEqual(docs, buildTurnDocuments({ meta: f.meta }, transcript, getProjectContext(f.home), "タイトル", 10000));
  const body = docs.map((doc) => doc.content.slice(doc.content.indexOf("\n\n") + 2)).join("");
  assert.equal(body, `### User request\n依頼\n\n### Final assistant response\n${"長い結果".repeat(4000)}`);
  const largerParts = buildTurnDocuments({ meta: f.meta }, transcript, getProjectContext(f.home), "タイトル", 12000);
  assert.notEqual(docs[0].customId, largerParts[0].customId);
});

test("時間枠を超えた未送信分を明示して次回へ残す", async (t) => {
  const f = fixture(t, [user("依頼"), assistant("結果")]);
  const result = await capture(f.payload, { ...f.options, budgetMs: 0, send: async () => assert.fail("must not send") });
  assert.deepEqual(result, { saved: 0, pending: 1, completedTurns: 1 });
});
