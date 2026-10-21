import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
// 失敗条件を先に固定: 設定不備、transcript不一致、現行/旧receipt破損、read/write障害、
// transport障害、不正ack、秘密/パス/応答漏れ、exit/retry/ID契約の変化。実データは使わない。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { getProjectContext } from "../client/Import-RemnifoldHistory.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = await mkdtemp(join(tmpdir(), "remnifold-capture-diagnostics-e2e-"));
const artifact = join(root, "dist", "capture-diagnostics-validation.json");
const key = "fixture-diagnostics-key-do-not-print";
const marker = "fixture-sensitive-response-do-not-print";
const checks = [];
const documents = [];
let mode = "success";
const server = createServer(async (request, response) => {
  let body = "";
  for await (const chunk of request) body += chunk;
  if (request.url !== "/v3/documents" || request.headers.authorization !== `Bearer ${key}`) {
    response.writeHead(401); response.end(marker); return;
  }
  documents.push(JSON.parse(body));
  if (mode === "disconnect") { request.socket.destroy(); return; }
  response.writeHead(mode === "http-failure" ? 500 : 200, { "Content-Type": "application/json" });
  response.end(JSON.stringify(mode === "success" ? { id: "fixture-document" } : { message: marker }));
});
const cases = [
  ["configuration", "configuration", "invalid", "repair-required"],
  ["transcript", "transcript", "unavailable", "repair-required"],
  ["receipt-malformed", "receipt", "invalid", "repair-required"],
  ["receipt-shape", "receipt", "invalid", "repair-required"],
  ["legacy-receipt-malformed", "receipt", "invalid", "repair-required"],
  ["receipt-read", "receipt", "read-failed", "repair-required"],
  ["receipt-write", "receipt", "write-failed", "repair-required"],
  ["http-failure", "transport", "request-failed", "retry-or-repair"],
  ["disconnect", "transport", "request-failed", "retry-or-repair"],
  ["acknowledgement", "transport", "invalid-acknowledgement", "retry-or-repair"],
];
let report;
let activeScenario;
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  for (const caller of ["CLI", "Stop"]) {
    for (const [name, stage, cause, action] of [...cases, ["success"]]) {
      activeScenario = { caller, scenario: name };
      mode = ["http-failure", "disconnect", "acknowledgement"].includes(name) ? name : "success";
      const home = join(fixture, `${caller}-${name}`);
      await mkdir(home);
      const session = "diagnostics-e2e";
      const transcript = join(home, `${marker}.jsonl`);
      await writeFile(transcript, [
        { type: "session_meta", payload: { id: session, cwd: root } },
        { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: "Fixture user turn" }] } },
        { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text: "Fixture completed turn" }] } },
      ].map(JSON.stringify).join("\n") + "\n");
      const environment = { ...process.env, CODEX_HOME: home, NODE_NO_WARNINGS: "1",
        REMNIFOLD_API_URL: baseUrl, REMNIFOLD_API_KEY: key };
      for (const variable of ["KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY", "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY"]) delete environment[variable];
      if (name === "configuration") { delete environment.REMNIFOLD_API_KEY; }
      const payload = { session_id: name === "transcript" ? "wrong-session" : session, transcript_path: transcript, cwd: root };
      const stateId = createHash("sha256").update(`${baseUrl}:${getProjectContext(root).containerTag}:${session}`).digest("hex");
      const receiptDirectory = join(home, name === "legacy-receipt-malformed" ? "cloudflare-memory" : "remnifold", "capture-state");
      const receiptPath = join(receiptDirectory, `${stateId}.json`);
      if (name.startsWith("receipt-") || name === "legacy-receipt-malformed") {
        await mkdir(receiptDirectory, { recursive: true });
        if (name === "receipt-read") await mkdir(receiptPath);
        else if (name === "receipt-write") await mkdir(join(receiptDirectory, "capture-coordination.sqlite3"));
        else await writeFile(receiptPath, name === "receipt-shape" ? JSON.stringify({ secret: marker }) : `broken ${marker}`);
      }
      async function run() {
        return new Promise((done, reject) => {
          const args = caller === "Stop" ? [join(root, "scripts", "hook-launcher.mjs"), "Stop"] : [join(root, "client", "capture.mjs")];
          const child = spawn(process.execPath, args, { cwd: root, env: environment, stdio: ["pipe", "pipe", "pipe"] });
          let stdout = "", stderr = "";
          const timeout = setTimeout(() => { child.kill(); reject(new Error("Capture subprocess exceeded 20 seconds")); }, 20_000);
          child.stdout.on("data", (chunk) => stdout += chunk);
          child.stderr.on("data", (chunk) => stderr += chunk);
          child.once("error", (error) => { clearTimeout(timeout); reject(error); });
          child.once("close", (code) => { clearTimeout(timeout); done({ code, stdout, stderr }); });
          child.stdin.end(JSON.stringify(payload));
        });
      }
      const countBefore = documents.length;
      const result = await run();
      if (name === "success") {
        assert.equal(result.code, 0); assert.equal(result.stderr, "");
        assert.equal(documents.length, countBefore + 1);
        const files = (await readdir(receiptDirectory)).filter((file) => file.endsWith(".json"));
        assert.equal(files.length, 1);
        assert.deepEqual(JSON.parse(await readFile(receiptPath, "utf8")), [documents.at(-1).customId]);
        const retry = await run();
        assert.equal(retry.code, 0); assert.equal(documents.length, countBefore + 1);
        checks.push({ caller, scenario: name, passed: true, retrySuppressed: true });
      } else {
        assert.equal(result.code, 1); assert.equal(result.stdout, "");
        const expected = `Remnifold capture failed; unacknowledged records will be retried. [stage=${stage}; cause=${cause}; action=${action}]\n`;
        assert.equal(result.stderr, expected);
        for (const sensitive of [key, marker, home, transcript, baseUrl]) assert.ok(!result.stderr.includes(sensitive));
        const preSend = ["configuration", "transcript", "receipt-malformed", "receipt-shape", "legacy-receipt-malformed", "receipt-read"].includes(name);
        assert.equal(documents.length, countBefore + (preSend ? 0 : 1));
        const retry = await run();
        assert.equal(retry.code, 1); assert.equal(retry.stderr, expected);
        if (!preSend) assert.equal(documents.at(-1).customId, documents.at(-2).customId);
        checks.push({ caller, scenario: name, stage, cause, action, passed: true, stableRetry: true });
      }
    }
  }
  assert.equal(new Set(documents.map(({ customId }) => customId)).size, 1);
  report = { command: "node scripts/capture-diagnostics-e2e.mjs", node: process.version,
    transport: "Stop and standalone capture subprocess -> authenticated loopback API", checks, deterministicIds: true, passed: true };
} catch {
  report = { command: "node scripts/capture-diagnostics-e2e.mjs", node: process.version, checks, passed: false,
    activeScenario,
    failure: "A fixture assertion failed; inspect the last completed scenario and rerun locally." };
} finally {
  await new Promise((done) => server.close(done));
  if (process.platform === "win32") {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(fixture, { allowedRoot: resolve(tmpdir()), recursive: true });
  } else {
    if (dirname(fixture) !== resolve(tmpdir()) || (await lstat(fixture)).isSymbolicLink()) throw new Error("Unexpected cleanup target");
    await rm(fixture, { recursive: true });
  }
  await mkdir(dirname(artifact), { recursive: true });
  await writeFile(artifact, JSON.stringify({ ...report, temporaryFixtureRemoved: true }, null, 2) + "\n");
}
console.log(JSON.stringify({ passed: report.passed, artifact, checks: checks.length }));
if (!report.passed) process.exitCode = 1;
