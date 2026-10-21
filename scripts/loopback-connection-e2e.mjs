// 失敗条件を先に固定: setup保存後にMCP/capture/importが127/8を拒否する、
// mapped IPv6の既存受理範囲を失う、DNS似せhost/非loopback HTTPや既存の禁止URLを許す、
// 環境変数が保存設定の検証を迂回する、子プロセスまたはfixture待機が無期限になる。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeEndpoint } from "../client/Import-RemnifoldHistory.mjs";
import { assertCleanupPath } from "./lib/cleanup.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = await mkdtemp(join(tmpdir(), "remnifold-loopback-e2e-"));
const artifact = join(process.env.REMNIFOLD_VALIDATION_DIR || join(root, "dist"), "loopback-connection-validation.json");
const key = "fixture-loopback-key";
const checks = [];
const requests = [];
const documents = [];
const children = new Set();
const environment = { ...process.env, CODEX_HOME: fixture, REMNIFOLD_API_KEY: key };
for (const name of ["REMNIFOLD_API_URL", "KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY",
  "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY"]) delete environment[name];

function run(command, args, input = "", env = environment) {
  return new Promise((done, reject) => {
    const child = spawn(command, args, { cwd: fixture, env, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let stdout = "", stderr = "", timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 20_000);
    child.stdout.on("data", (chunk) => stdout += chunk);
    child.stderr.on("data", (chunk) => stderr += chunk);
    child.stdin.on("error", (error) => { if (error.code !== "EPIPE") reject(error); });
    child.once("error", (error) => { clearTimeout(timer); children.delete(child); reject(error); });
    child.once("close", (code) => {
      clearTimeout(timer); children.delete(child);
      if (timedOut) reject(new Error("Loopback fixture child exceeded 20 seconds"));
      else done({ code, stdout, stderr });
    });
    child.stdin.end(input);
  });
}
const server = createServer(async (request, response) => {
  if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end(); return; }
  let body = "";
  for await (const chunk of request) body += chunk;
  requests.push(request.url);
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v3/session") response.end(JSON.stringify({ user: { id: "fixture-user" } }));
  else if (request.url === "/v3/container-tags") response.end(JSON.stringify({ spaces: [{ containerTag: "memories" }] }));
  else if (request.url === "/v3/documents/list") response.end(JSON.stringify({ documents: [], pagination: { totalPages: 1 } }));
  else if (request.url === "/v4/search") response.end(JSON.stringify({ results: [], searchedContainers: ["memories"], searchScope: "all-containers", spaceDiscoveryComplete: true }));
  else if (request.url === "/v3/documents" && request.method === "POST") {
    documents.push(JSON.parse(body)); response.end(JSON.stringify({ id: `fixture-${documents.length}` }));
  } else { response.writeHead(404); response.end(JSON.stringify({ error: "fixture route missing" })); }
});
let report;
try {
  const accepted = [
    ["http://localhost:8787/", "http://localhost:8787"],
    ["http://127.0.0.1:8787/", "http://127.0.0.1:8787"],
    ["http://127.0.0.2:8787/", "http://127.0.0.2:8787"],
    ["http://127.255.255.255/", "http://127.255.255.255"],
    ["http://[::1]:8787/", "http://[::1]:8787"],
    ["http://[::ffff:127.0.0.1]/", "http://[::ffff:7f00:1]"],
    ["http://127.2/", "http://127.0.0.2"],
    ["http://2130706434/", "http://127.0.0.2"],
    ["http://0x7f000002/", "http://127.0.0.2"],
    ["http://[0:0:0:0:0:0:0:1]/", "http://[::1]"],
    ["https://worker.example/path/", "https://worker.example/path"],
  ];
  for (const [input, expected] of accepted) assert.equal(normalizeEndpoint(input), expected);
  const rejected = ["http://128.0.0.1", "http://126.255.255.255", "http://192.168.1.1", "http://0.0.0.0",
    "http://127.evil.example", "http://127.0.0.1.evil.example", "http://localhost.evil.example",
    "http://[::ffff:127.0.0.2]", "http://[::2]", "http://[::127.0.0.1]",
    "http://user:password@127.0.0.2", "http://127.0.0.2?query=1", "http://127.0.0.2#fragment",
    "https://supermemory.ai", "https://api.supermemory.ai", "ftp://127.0.0.2"];
  for (const endpoint of rejected) assert.throws(() => normalizeEndpoint(endpoint), /Refusing/);
  checks.push({ name: "normalized loopback and unchanged endpoint guards", passed: true, accepted: accepted.length, rejected: rejected.length });

  await new Promise((done, reject) => {
    const timer = setTimeout(() => reject(new Error("Loopback fixture startup exceeded 5 seconds")), 5_000);
    server.once("error", (error) => { clearTimeout(timer); reject(error); });
    server.listen(0, "127.0.0.2", () => { clearTimeout(timer); done(); });
  });
  const baseUrl = `http://127.0.0.2:${server.address().port}`;
  const setup = await run("pwsh", ["-NoProfile", "-File", join(root, "scripts", "setup-client.ps1"), "-CodexHome", fixture, "-BaseUrl", `${baseUrl}/`, "-Force"]);
  assert.equal(setup.code, 0, setup.stderr);
  assert.equal(JSON.parse(await readFile(join(fixture, "remnifold.json"), "utf8")).baseUrl, baseUrl);
  delete environment.REMNIFOLD_API_KEY;
  checks.push({ name: "real PowerShell setup saves authenticated 127.0.0.2 endpoint", passed: true });

  const mcp = await run(process.execPath, [join(root, "client", "mcp-server.mjs")], [
    { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "whoAmI", arguments: {} } },
    { jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "search_memory", arguments: { query: "fixture loopback" } } },
  ].map(JSON.stringify).join("\n") + "\n");
  assert.equal(mcp.code, 0, mcp.stderr);
  const replies = mcp.stdout.trim().split("\n").map((line) => JSON.parse(line));
  assert.equal(replies.length, 2);
  for (const reply of replies) { assert.ok(!reply.error); assert.ok(!reply.result.isError, JSON.stringify(reply)); }
  assert.equal(replies.find(({ id }) => id === 1).result.structuredContent.userId, "fixture-user");
  assert.ok(requests.includes("/v4/search"));
  checks.push({ name: "MCP identity and search use saved configuration", passed: true });

  const sessions = join(fixture, "sessions");
  await mkdir(sessions);
  const transcript = join(sessions, "loopback.jsonl");
  await writeFile(transcript, [
    { type: "session_meta", payload: { id: "loopback-session", cwd: fixture } },
    { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: "Fixture loopback request" }] } },
    { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text: "Fixture loopback completed" }] } },
  ].map(JSON.stringify).join("\n") + "\n");
  const stop = await run(process.execPath, [join(root, "scripts", "hook-launcher.mjs"), "Stop"], JSON.stringify({ session_id: "loopback-session", transcript_path: transcript, cwd: fixture }));
  assert.equal(stop.code, 0, stop.stderr);
  assert.equal(documents.length, 1);
  assert.ok(documents[0].customId);
  checks.push({ name: "Stop saves through setup configuration", passed: true });
  const imported = await run(process.execPath, [join(root, "client", "Import-RemnifoldHistory.mjs"), "--apply", "--include-recent", "--codex-home", fixture, "--state-path", join(fixture, "history-state.json")]);
  assert.equal(imported.code, 0, imported.stderr);
  assert.equal(documents.length, 2);
  assert.equal(documents[1].customId, documents[0].customId);
  checks.push({ name: "history import saves with unchanged deterministic identity", passed: true });
  report = { command: "node scripts/loopback-connection-e2e.mjs", node: process.version, checks };
} catch (error) {
  report = { checks, error: error instanceof Error ? error.message : String(error) };
  throw error;
} finally {
  for (const child of children) child.kill("SIGKILL");
  server.closeAllConnections();
  await new Promise((done) => server.close(done));
  await mkdir(dirname(artifact), { recursive: true });
  await writeFile(artifact, JSON.stringify(report, null, 2) + "\n");
  await rm(assertCleanupPath(fixture, tmpdir()), { recursive: true, force: true });
}
console.log(JSON.stringify(report, null, 2));
