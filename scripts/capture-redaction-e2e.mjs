import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
// 実Stop hookからローカルHTTPまでの秘匿・再送を検証する。値はすべて架空。
import { spawn } from "node:child_process";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const phase = process.argv[2] || "after";
if (!["before", "after"].includes(phase) || process.argv.length > 3) throw new Error("Usage: node scripts/capture-redaction-e2e.mjs [before|after]");
// 失敗条件: secret漏れ、説明消失、ID変化、receipt消失/重複、並行Stopの未到達、子/待機の終了漏れ。
const home = await mkdtemp(join(tmpdir(), "remnifold-capture-redaction-e2e-"));
const artifact = join(root, "dist", `capture-redaction-${phase}-validation.json`);
const documents = [];
const children = new Set();
let barrier = null;
function rendezvous(milliseconds = 20_000) {
  let arrive, fail, release;
  const arrived = new Promise((done, reject) => { arrive = done; fail = reject; });
  const released = new Promise((done) => { release = done; });
  const gate = { count: 0, timer: null, arrived, released,
    release() { clearTimeout(gate.timer); gate.timer = null; release(); },
    enter() { if (++gate.count === 2) { arrive(); gate.release(); } return released; } };
  gate.timer = setTimeout(() => { gate.timer = null; fail(new Error("Stop rendezvous exceeded deadline")); }, milliseconds);
  return gate;
}
async function stop(payload, environment) {
  return new Promise((done, reject) => {
    const child = spawn(process.execPath, [join(root, "scripts", "hook-launcher.mjs"), "Stop"], { cwd: root, env: environment, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let stdout = "", stderr = "", timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 20_000);
    child.stdout.on("data", (chunk) => stdout += chunk);
    child.stderr.on("data", (chunk) => stderr += chunk);
    child.once("error", (error) => { clearTimeout(timeout); reject(error); });
    child.once("close", (code) => {
      clearTimeout(timeout); children.delete(child);
      if (timedOut) reject(new Error("Stop hook exceeded 20 seconds and was stopped"));
      else done({ code, stdout, stderr });
    });
    child.stdin.end(JSON.stringify(payload));
  });
}
const configuredKey = "local-fixture-memory-key";
const assignments = [
  ["AWS_SECRET_ACCESS_KEY", "fixtureAwsUnprefixedValue", "="],
  ["OPENAI_API_KEY", "fixtureOpenaiUnprefixedValue", "="],
  ["AZURE_CLIENT_SECRET", "fixtureAzureUnprefixedValue", " : "],
  ["api_key", "fixtureGenericValue", "="],
];
const discussion = "AWS_SECRET_ACCESS_KEY と OPENAI_API_KEY の設定名を説明してください。";
const jsonAssignments = { AWS_SECRET_ACCESS_KEY: "fixtureJsonAwsValue", OPENAI_API_KEY: "fixtureJsonOpenaiValue" };
const quotedSecrets = ["fixture space secret", "fixture,comma,secret", 'fixture escaped "double" secret', "fixture escaped 'single' secret"];
const ordinaryLongKey = `${"ordinary-".repeat(4000)}setting`;
const text = [discussion, "$env:OPENAI_API_KEY=fixturePowerShellValue", "https://local.example/?OPENAI_API_KEY=fixtureUrlValue", ...assignments.map(([name, value, separator]) => `${name}${separator}"${value}"`),
  JSON.stringify(jsonAssignments), "{'AZURE_CLIENT_SECRET': 'fixtureSingleQuotedValue'}", `${ordinaryLongKey}=ordinary-setting-value`,
  `password="${quotedSecrets[0]}"`, `secret='${quotedSecrets[1]}'`, `client_secret=${JSON.stringify(quotedSecrets[2])}`, `password='fixture escaped \\'single\\' secret'`,
  "password='fixture doubled ''single'' value'", '$env:OPENAI_API_KEY="fixture backtick `"quoted`" value"',
  JSON.stringify({ password: "fixture trailing backtick`", discussion: "keep neighboring ordinary discussion" }),
  'password="fixture generic backtick`" ordinary="keep generic ordinary discussion"',
  `configured key ${configuredKey}`, "prefixed key sk-proj-fixture0123456789abcdef"].join("\n");
const server = createServer(async (request, response) => {
  let body = "";
  for await (const chunk of request) body += chunk;
  if (request.method !== "POST" || request.url !== "/v3/documents" || request.headers.authorization !== `Bearer ${configuredKey}`) {
    response.writeHead(400); response.end(); return;
  }
  documents.push(JSON.parse(body));
  if (barrier && barrier.count < 2) await barrier.enter();
  response.writeHead(200, { "Content-Type": "application/json" });
  response.end(JSON.stringify({ id: "fixture-document" }));
});
const checks = [];
const check = (name, passed, details = {}) => checks.push({ name, passed, ...details });
let report;
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  const environment = { ...process.env, CODEX_HOME: home,
    REMNIFOLD_API_URL: `http://127.0.0.1:${server.address().port}`, REMNIFOLD_API_KEY: configuredKey };
  const transcript = join(home, "rollout.jsonl");
  await writeFile(transcript, [
    { type: "session_meta", payload: { id: "redaction-e2e", cwd: root } },
    { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text }] } },
    { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text }] } },
  ].map(JSON.stringify).join("\n") + "\n");
  await writeFile(join(home, "session_index.jsonl"), JSON.stringify({ id: "redaction-e2e", thread_name: 'OPENAI_API_KEY=fixtureTitleUnprefixedValue password="fixture title space secret" secret=\'fixture title,comma\'' }) + "\n");
  const payload = { session_id: "redaction-e2e", transcript_path: transcript, cwd: root };
  const first = await stop(payload, environment);
  check("Stop process succeeds", first.code === 0 && documents.length === 1);
  if (documents.length !== 1) throw new Error("Expected one captured document");
  const stored = JSON.stringify(documents[0]);
  check("PowerShell environment assignment redacted", !stored.includes("fixturePowerShellValue"));
  check("prefixed URL query assignment redacted", !stored.includes("fixtureUrlValue"));
  for (const [name, value] of assignments) check(`${name} assignment redacted`, !stored.includes(value));
  for (const [name, value] of Object.entries(jsonAssignments)) check(`${name} JSON assignment redacted`, !stored.includes(value));
  check("single quoted assignment redacted", !stored.includes("fixtureSingleQuotedValue"));
  for (let index = 0; index < quotedSecrets.length; index++) check(`quoted complete value ${index + 1} redacted`, !documents[0].content.includes(quotedSecrets[index]) && !documents[0].content.includes(quotedSecrets[index].replaceAll('"', '\\"').replaceAll("'", "\\'")));
  check("quoted whitespace/comma/escape assignment wrappers preserved", documents[0].content.includes('password="[REDACTED]"') && documents[0].content.includes("secret='[REDACTED]'"));
  check("PowerShell doubled single quotes redacted completely", !stored.includes("fixture doubled") && !stored.includes("single'' value"));
  check("PowerShell backtick escaped double quotes redacted completely", !stored.includes("fixture backtick") && !stored.includes("quoted`"));
  check("JSON backtick is ordinary text and neighboring discussion survives", !stored.includes("fixture trailing backtick") && documents[0].content.includes('"discussion":"keep neighboring ordinary discussion"'));
  check("generic assignment backtick preserves neighboring quotation", !stored.includes("fixture generic backtick") && documents[0].content.includes('ordinary="keep generic ordinary discussion"'));
  check("assignment quotation preserved", documents[0].content.includes('"AWS_SECRET_ACCESS_KEY":"[REDACTED]"') && documents[0].content.includes("'AZURE_CLIENT_SECRET': '[REDACTED]'"));
  check("ordinary long identifier preserved", documents.map(({ content }) => content).join("").includes(`${ordinaryLongKey}=ordinary-setting-value`));
  check("title assignment redacted", !stored.includes("fixtureTitleUnprefixedValue"));
  check("quoted complete title values redacted", !stored.includes("fixture title space secret") && !stored.includes("fixture title,comma") && documents[0].metadata.title.includes('password="[REDACTED]"') && documents[0].metadata.title.includes("secret='[REDACTED]'"));
  check("configured and prefix keys redacted", !stored.includes(configuredKey) && !stored.includes("sk-proj-fixture0123456789abcdef"));
  check("discussion identifiers preserved", documents[0].content.includes(discussion));
  const receiptDirectory = join(home, "remnifold", "capture-state");
  const receiptFiles = (await readdir(receiptDirectory)).filter((name) => name.endsWith(".json"));
  check("acknowledged ID recorded", receiptFiles.length === 1 && JSON.parse(await readFile(join(receiptDirectory, receiptFiles[0]), "utf8")).includes(documents[0].customId));
  const retry = await stop(payload, environment);
  check("acknowledged retry sends nothing", retry.code === 0 && documents.length === 1);
  // 空の別Codex homeから同じtranscriptを再送し、状態によらないID決定性を確認する。
  const secondHome = join(home, "fresh-home");
  await mkdir(secondHome);
  await writeFile(join(secondHome, "session_index.jsonl"), await readFile(join(home, "session_index.jsonl")));
  environment.CODEX_HOME = secondHome;
  const fresh = await stop(payload, environment);
  check("fresh capture keeps deterministic IDs", fresh.code === 0 && documents.length === 2 && documents[0].customId === documents[1].customId && documents[0].metadata.captureKey === documents[1].metadata.captureKey);
  const concurrentHome = join(home, "concurrent-home");
  await mkdir(concurrentHome);
  const concurrentTranscript = join(concurrentHome, "rollout.jsonl");
  await writeFile(concurrentTranscript, [
    { type: "session_meta", payload: { id: "concurrent-e2e", cwd: root } },
    ...["one", "two"].flatMap((turn) => [
      { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: `request ${turn}` }] } },
      { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text: `result ${turn}` }] } },
    ]),
  ].map(JSON.stringify).join("\n") + "\n");
  const concurrentPayload = { session_id: "concurrent-e2e", transcript_path: concurrentTranscript, cwd: root };
  const concurrentEnvironment = { ...environment, CODEX_HOME: concurrentHome };
  const start = documents.length;
  barrier = rendezvous();
  const concurrentStops = Promise.all([stop(concurrentPayload, concurrentEnvironment), stop(concurrentPayload, concurrentEnvironment)]);
  // barrier待機中の子失敗も未処理rejectionにしない。
  concurrentStops.catch(() => {});
  let results;
  try { await Promise.race([barrier.arrived, concurrentStops.then(() => { throw new Error("Stops exited before rendezvous"); })]); results = await concurrentStops; }
  finally { barrier.release(); await concurrentStops; barrier = null; }
  assert.ok(results.every(({ code, stdout }) => code === 0 && /saved 2 documents; pending 0/u.test(stdout)));
  const sent = documents.slice(start);
  assert.equal(sent.length, 4);
  assert.ok(sent.every(({ customId }) => customId.startsWith("codex-turn-v2:")));
  assert.equal(sent[0].customId, sent[1].customId);
  assert.equal(sent[2].customId, sent[3].customId);
  assert.notEqual(sent[0].customId, sent[2].customId);
  const concurrentDirectory = join(concurrentHome, "remnifold", "capture-state");
  const concurrentReceipts = (await readdir(concurrentDirectory)).filter((name) => name.endsWith(".json"));
  assert.equal(concurrentReceipts.length, 1);
  const receipt = JSON.parse(await readFile(join(concurrentDirectory, concurrentReceipts[0]), "utf8"));
  assert.deepEqual(receipt, [...new Set(sent.map(({ customId }) => customId))].sort());
  const concurrentRetry = await stop(concurrentPayload, concurrentEnvironment);
  assert.equal(concurrentRetry.code, 0); assert.equal(concurrentRetry.stdout, ""); assert.equal(documents.length, start + 4);
  check("two concurrent Stop processes preserve canonical receipt union and suppress retry", true,
    { savedPerProcess: [2, 2], postedDocuments: 4, uniqueIds: receipt.length, receiptFiles: concurrentReceipts.length, retryPosts: 0 });
  // 実Stopを1本だけ到達させ、欠けた参加者が期限内に失敗し子とtimerを残さないことを確認する。
  const canaryHome = join(home, "canary-home");
  await mkdir(canaryHome);
  barrier = rendezvous(1000);
  const canaryGate = barrier;
  const canaryStart = Date.now();
  const canaryStop = stop(concurrentPayload, { ...environment, CODEX_HOME: canaryHome });
  canaryStop.catch(() => {});
  try { await assert.rejects(canaryGate.arrived, /rendezvous exceeded deadline/u); assert.equal(canaryGate.count, 1); }
  finally { canaryGate.release(); barrier = null; await canaryStop; }
  assert.equal(children.size, 0); assert.equal(canaryGate.timer, null);
  check("missing rendezvous participant fails within bound and leaves no child or timer", true,
    { deadlineMilliseconds: 1000, elapsedMilliseconds: Date.now() - canaryStart, pendingChildren: children.size, pendingBarrierTimers: 0 });
  const projectionHome = join(home, "projection-home");
  await mkdir(projectionHome);
  const projectionTranscript = join(projectionHome, "rollout.jsonl");
  const projectionRequests = [
    '<in-app-browser-context source="ambient-ui-state">\nThis block is automatically supplied ambient UI state, not part of the user\'s request.\n</in-app-browser-context>\nChrome extension implementationを確認してください。',
    `<send_user_message_question_reply>${JSON.stringify([{ questionItemId: '["request_user_input_async","call_fixture","0"]', question: "Chrome extensionの保存方針は？" + "長い補足です。".repeat(40), answer: "失敗時だけ再送してください" }])}</send_user_message_question_reply>`,
  ];
  await writeFile(projectionTranscript, [
    { type: "session_meta", payload: { id: "projection-e2e", cwd: root } },
    ...projectionRequests.flatMap((request) => [
      { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: request }] } },
      { type: "response_item", payload: { role: "assistant", phase: "final_answer", content: [{ type: "output_text", text: "Chrome extensionの保存動作を確認しました。" }] } },
    ]),
  ].map(JSON.stringify).join("\n") + "\n");
  const projectionPayload = { session_id: "projection-e2e", transcript_path: projectionTranscript, cwd: root };
  const projectionEnvironment = { ...environment, CODEX_HOME: projectionHome };
  const projectionStart = documents.length;
  const projectionResult = await stop(projectionPayload, projectionEnvironment);
  const projected = documents.slice(projectionStart);
  assert.equal(projectionResult.code, 0); assert.equal(projected.length, 2);
  check("ambient UI state is omitted from new capture content while the actual request is retained",
    projected[0].metadata.memoryIndex.description === "Chrome extension implementationを確認してください。" &&
    !projected[0].content.includes("automatically supplied ambient UI state") &&
    projected[0].content.includes("Chrome extension implementationを確認してください。"));
  check("question replies retain the actual answer without internal routing IDs in the index",
    projected[1].metadata.memoryIndex.description.includes("失敗時だけ再送してください") &&
    !/questionItemId|request_user_input_async|call_fixture/u.test(projected[1].metadata.memoryIndex.description) &&
    projected[1].content.includes("questionItemId"));
  const projectionRetry = await stop(projectionPayload, projectionEnvironment);
  check("projected indexes retain v2 capture and acknowledged retry suppression",
    projected.every((document) => document.customId.startsWith("codex-turn-v2:") && document.metadata.captureVersion === 2) &&
    projectionRetry.code === 0 && projectionRetry.stdout === "" && documents.length === projectionStart + 2);
  report = { phase, command: `node scripts/capture-redaction-e2e.mjs ${phase}`, node: process.version,
    transport: "Stop subprocess -> authenticated loopback HTTP /v3/documents", checks, temporaryFixture: home, pendingChildren: children.size,
    postedDocuments: documents.length, sanitizedContentSha256: createHash("sha256").update(documents[0].content).digest("hex"),
    passed: checks.every(({ passed }) => passed) };
} catch (error) {
  report = { phase, checks, passed: false, error: error.message };
} finally {
  barrier?.release();
  await Promise.all([...children].map((child) => new Promise((done) => { child.once("close", done); child.kill("SIGKILL"); })));
  await new Promise((done) => server.close(done));
  // この実行で作った絶対パスだけを確認し、PowerShellで一括清掃する。
  if (process.platform === "win32") {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(home, { allowedRoot: resolve(tmpdir()), recursive: true });
  } else {
    if (dirname(home) !== resolve(tmpdir()) || !/^remnifold-capture-redaction-e2e-[A-Za-z0-9]{6}$/u.test(home.slice(home.lastIndexOf("/") + 1))) throw new Error("Unexpected cleanup target");
    async function verify(path) {
      const item = await lstat(path);
      assert.ok(!item.isSymbolicLink(), "Linked temporary item");
      if (item.isDirectory()) for (const name of await readdir(path)) await verify(join(path, name));
    }
    await verify(home);
    await rm(home, { recursive: true });
    await assert.rejects(lstat(home), { code: "ENOENT" });
  }
  await mkdir(dirname(artifact), { recursive: true });
  await writeFile(artifact, JSON.stringify({ ...report, temporaryFixtureRemoved: true }, null, 2) + "\n");
}
console.log(JSON.stringify({ passed: report.passed, artifact, checks }));
if (!report.passed) process.exitCode = 1;
