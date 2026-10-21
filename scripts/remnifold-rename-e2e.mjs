// 失敗条件: 旧config読取漏れ、旧receipt再送、新config優先漏れ、改名によるID/来歴変更、旧config削除、MCP名不一致、子プロセス停止漏れ。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const runtimeRoot = process.argv[2] === "--runtime-root" && process.argv[3] ? resolve(process.argv[3]) : root;
if (process.argv.length > 2 && (process.argv.length !== 4 || process.argv[2] !== "--runtime-root")) throw new Error("Usage: remnifold-rename-e2e.mjs [--runtime-root plugin-directory]");
const outputRoot = process.env.REMNIFOLD_VALIDATION_DIR || process.env.KAGAYOI_MEMORY_VALIDATION_DIR || join(root, "dist");
if (!isAbsolute(outputRoot)) throw new Error("Validation output root must be absolute");
const fixture = await mkdtemp(join(tmpdir(), "remnifold-rename-e2e-"));
const children = new Set();
const checks = [];
const documents = [];
const key = "local-rename-fixture-key";
const credentialVariables = ["REMNIFOLD_API_URL", "REMNIFOLD_API_KEY", "KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY", "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY", "NODE_OPTIONS"];
const environment = { ...process.env, NODE_NO_WARNINGS: "1" };
for (const name of credentialVariables) delete environment[name];
const diagnosticSecrets = [key, "obsolete-fixture-key", ...credentialVariables
  .filter(name => /(?:KEY|TOKEN|SECRET)/u.test(name)).map(name => process.env[name])]
  .filter(value => typeof value === "string" && value);
function diagnosticText(value, limit = 3000) {
  let text = String(value ?? "");
  for (const secret of diagnosticSecrets) text = text.replaceAll(secret, "[REDACTED]");
  for (const [directory, label] of [[fixture, "<fixture>"], [runtimeRoot, "<runtime>"], [root, "<source>"]]) text = text.replaceAll(directory, label);
  return text.replace(/\bBearer\s+[^\s"']+/giu, "Bearer [REDACTED]")
    .replace(/((?:api[_-]?key|api[_-]?token|password|secret)["']?\s*[:=]\s*["']?)[^\s,"'}]+/giu, "$1[REDACTED]").slice(0, limit);
}
let lastSubprocess;
const server = createServer(async (request, response) => {
  let body = "";
  for await (const chunk of request) body += chunk;
  if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end(); return; }
  response.setHeader("Content-Type", "application/json");
  if (request.url === "/v3/session") response.end(JSON.stringify({ user: { id: "rename-fixture-user" } }));
  else if (request.method === "POST" && request.url === "/v3/documents") {
    documents.push(JSON.parse(body));
    response.end(JSON.stringify({ id: `rename-document-${documents.length}` }));
  } else { response.writeHead(404); response.end("{}"); }
});

async function run(command, args, { home = fixture, input = "", env = {} } = {}) {
  return await new Promise((done, reject) => {
    const child = spawn(command, args, { cwd: runtimeRoot, env: { ...environment, ...env, CODEX_HOME: home }, stdio: ["pipe", "pipe", "pipe"] });
    children.add(child);
    let stdout = "", stderr = "", timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 20_000);
    child.stdout.on("data", chunk => stdout += chunk);
    child.stderr.on("data", chunk => stderr += chunk);
    child.stdin.on("error", () => {});
    child.once("error", error => { clearTimeout(timer); children.delete(child); reject(error); });
    child.once("close", code => {
      clearTimeout(timer); children.delete(child);
      lastSubprocess = { command: command === process.execPath ? "node" : command,
        code, timedOut, stdout: diagnosticText(stdout), stderr: diagnosticText(stderr) };
      if (timedOut) reject(new Error("Fixture subprocess exceeded 20 seconds"));
      else done({ code, stdout, stderr });
    });
    child.stdin.end(typeof input === "string" ? input : JSON.stringify(input));
  });
}
const hook = (event, input, home) => run(process.execPath, [join(runtimeRoot, "scripts", "hook-launcher.mjs"), event], { input, home });
async function check(name, operation) { await operation(); checks.push({ name, passed: true }); }
async function cleanFixture() {
  const base = await realpath(tmpdir());
  const target = await realpath(fixture);
  const within = relative(base, target);
  assert.ok(within && within !== ".." && !within.startsWith(`..${sep}`) && !isAbsolute(within));
  assert.equal(dirname(target), base);
  assert.ok(target.startsWith(join(base, "remnifold-rename-e2e-")));
  const pending = [fixture];
  while (pending.length) {
    const current = pending.pop(), stat = await lstat(current);
    assert.equal(stat.isSymbolicLink(), false);
    if (stat.isDirectory()) for (const entry of await readdir(current)) pending.push(join(current, entry));
  }
  await rm(fixture, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}

let failed = false;
let currentCheck = "initialization";
try {
  await new Promise((done, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", done); });
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  const { runSetup } = await import(pathToFileURL(join(runtimeRoot, "scripts", "setup-server.mjs")));
  const wranglerBin = join(fixture, "wrangler-fixture.cjs");
  await writeFile(wranglerBin, "");
  currentCheck = "server resources are reused in new-to-old priority without remote mutations";
  await check(currentCheck, async () => {
    for (const names of [["remnifold", "kagayoi-memory", "cloudflare-supermemory"], ["kagayoi-memory", "cloudflare-supermemory"], ["cloudflare-supermemory"]]) {
      const calls = [], lines = [];
      await runSetup([], {
        serverDir: join(runtimeRoot, "server"), wranglerBin, env: {}, output: line => lines.push(line),
        runner: async ({ args }) => {
          calls.push(args);
          if (args[0] === "whoami") return { code: 0, stdout: "{}", stderr: "" };
          if (args[0] === "d1" && args[1] === "list") return { code: 0, stdout: JSON.stringify(names.map(name => ({ name, uuid: "11111111-1111-1111-1111-111111111111" }))), stderr: "" };
          if (args[0] === "vectorize" && args[1] === "list") return { code: 0, stdout: JSON.stringify(names.map(name => ({ name }))), stderr: "" };
          if (args[0] === "vectorize" && args[1] === "get") return { code: 0, stdout: JSON.stringify({ config: { dimensions: 1024, metric: "cosine" } }), stderr: "" };
          if (args[0] === "secret" && args[1] === "list") return { code: names.includes(args[3]) ? 0 : 1, stdout: '[{"name":"MEMORY_API_KEY"}]', stderr: "" };
          throw new Error("Unexpected setup mutation");
        },
      });
      assert.ok(lines.includes(`D1 database: reuse ${names[0]}`));
      assert.ok(lines.some(line => line.startsWith(`Vectorize index: reuse ${names[0]} `)));
      assert.equal(calls.filter(args => args[0] === "secret").at(-1)[3], names[0]);
      assert.equal(calls.some(args => args.includes("create") || args.includes("deploy")), false);
    }
  });
  const importer = await import(pathToFileURL(join(runtimeRoot, "client", "Import-RemnifoldHistory.mjs")));
  const projectRoot = join(fixture, "repository");
  await mkdir(join(projectRoot, ".git", "objects"), { recursive: true });
  await mkdir(join(projectRoot, ".git", "refs", "heads"), { recursive: true });
  await writeFile(join(projectRoot, ".git", "HEAD"), "ref: refs/heads/main\n");
  const setRemote = async slug => writeFile(join(projectRoot, ".git", "config"), `[core]\n\trepositoryformatversion = 0\n\tbare = false\n[remote "origin"]\n\turl = https://github.com/1llum1n4t1s/${slug}.git\n`);
  await setRemote("kagayoi-memory");
  const oldProject = importer.getProjectContext(projectRoot);
  const session = "rename-fixture-session";
  const transcript = join(fixture, "rollout.jsonl");
  await writeFile(transcript, [
    { type: "session_meta", payload: { id: session, cwd: projectRoot } },
    { type: "response_item", payload: { role: "user", content: [{ type: "input_text", text: "Keep the persistent project identity when renaming this repository." }] } },
    { type: "response_item", payload: { role: "assistant", channel: "final", content: [{ type: "output_text", text: "The existing capture identities and provenance remain stable." }] } },
  ].map(JSON.stringify).join("\n") + "\n");
  const payload = { session_id: session, transcript_path: transcript, cwd: projectRoot };
  const candidate = { filePath: transcript, meta: await importer.readSessionMeta(transcript) };
  const parsed = await importer.parseTaskTranscript(transcript, [key]);
  const original = importer.buildTurnDocuments(candidate, parsed, oldProject, "");

  currentCheck = "renamed repository preserves persisted identity";
  await check(currentCheck, async () => {
    assert.equal(parsed.turns.length, 1, "Fixture transcript must yield one complete turn");
    await setRemote("remnifold");
    const project = importer.getProjectContext(projectRoot);
    assert.deepEqual(project, oldProject, "Repository rename must preserve project provenance");
    assert.equal(project.containerTag, "repo_cloudflare_supermemory__57a9aa7a5083510a", "Fixture must resolve the shipped project container");
    assert.deepEqual(importer.buildTurnDocuments(candidate, parsed, project, ""), original, "Repository rename must preserve complete capture documents");
    assert.ok(original.length > 0, "Fixture must produce capture documents");
    assert.ok(original.every(doc => doc.metadata.sm_client === "codex-kagayoi-memory" && doc.metadata.captureVersion === 2));
  });

  for (const [configName, stateName] of [["kagayoi-memory.json", "kagayoi-memory"], ["supermemory.json", "cloudflare-memory"]]) {
    currentCheck = `${configName} and ${stateName} receipts suppress acknowledged capture`;
    const home = join(fixture, stateName);
    await mkdir(home);
    const configText = JSON.stringify({ baseUrl, apiKey: key, maxMemories: 7 });
    await writeFile(join(home, configName), configText);
    const receiptDirectory = join(home, stateName, "capture-state");
    await mkdir(receiptDirectory, { recursive: true });
    const receipt = createHash("sha256").update(`${baseUrl}:${oldProject.containerTag}:${session}`).digest("hex");
    await writeFile(join(receiptDirectory, `${receipt}.json`), JSON.stringify(original.map(doc => doc.customId)));
    await check(currentCheck, async () => {
      const before = documents.length, result = await hook("Stop", payload, home);
      assert.equal(result.code, 0); assert.equal(result.stderr, "");
      assert.equal(documents.length, before);
    });
    currentCheck = `setup migrates ${configName} without removing it`;
    await check(currentCheck, async () => {
      const result = await run("pwsh", ["-NoProfile", "-NonInteractive", "-File", join(runtimeRoot, "scripts", "setup-client.ps1"), "-CodexHome", home], { home });
      assert.equal(result.code, 0); assert.equal(result.stderr, "");
      assert.equal(await readFile(join(home, configName), "utf8"), configText);
      const migrated = JSON.parse(await readFile(join(home, "remnifold.json"), "utf8"));
      assert.equal(migrated.maxMemories, 7);
      assert.equal(migrated.baseUrl, baseUrl);
      const before = documents.length;
      assert.equal((await hook("Stop", payload, home)).code, 0);
      assert.equal(documents.length, before);
    });
  }

  currentCheck = "new configuration wins over both legacy files";
  await check(currentCheck, async () => {
    const home = join(fixture, "priority");
    await mkdir(home);
    for (const name of ["kagayoi-memory.json", "supermemory.json"]) await writeFile(join(home, name), JSON.stringify({ baseUrl, apiKey: "obsolete-fixture-key", maxMemories: 9 }));
    await writeFile(join(home, "remnifold.json"), JSON.stringify({ baseUrl, apiKey: key, maxMemories: 3 }));
    const probe = join(fixture, "settings-probe.mjs");
    await writeFile(probe, `import { readSettings } from ${JSON.stringify(pathToFileURL(join(runtimeRoot, "client", "memory-client.mjs")).href)}; console.log(JSON.stringify({ maxMemories: readSettings(process.argv[2]).maxMemories }));`);
    const settings = await run(process.execPath, [probe, home], { home });
    assert.equal(settings.code, 0); assert.equal(JSON.parse(settings.stdout).maxMemories, 3);
    const before = documents.length, result = await hook("Stop", payload, home);
    assert.equal(result.code, 0); assert.equal(documents.length - before, original.length);
    assert.deepEqual(documents.slice(before).map(doc => doc.customId), original.map(doc => doc.customId));
  });

  currentCheck = "Kagayoi configuration takes priority over the earlier Supermemory file";
  await check(currentCheck, async () => {
    const home = join(fixture, "legacy-priority");
    await mkdir(home);
    await writeFile(join(home, "kagayoi-memory.json"), JSON.stringify({ baseUrl, apiKey: key }));
    await writeFile(join(home, "supermemory.json"), JSON.stringify({ baseUrl, apiKey: "obsolete-fixture-key" }));
    const before = documents.length, result = await hook("Stop", payload, home);
    assert.equal(result.code, 0); assert.equal(documents.length - before, original.length);
  });

  currentCheck = "legacy environment variables remain usable and Remnifold variables win";
  await check(currentCheck, async () => {
    for (const [index, variables] of [
      { KAGAYOI_MEMORY_API_URL: baseUrl, KAGAYOI_MEMORY_API_KEY: key },
      { SUPERMEMORY_API_URL: baseUrl, SUPERMEMORY_CODEX_API_KEY: key },
      { SUPERMEMORY_API_URL: baseUrl, CLOUDFLARE_MEMORY_API_KEY: key },
      { REMNIFOLD_API_URL: baseUrl, REMNIFOLD_API_KEY: key, KAGAYOI_MEMORY_API_URL: "https://obsolete.invalid", KAGAYOI_MEMORY_API_KEY: "obsolete-fixture-key" },
    ].entries()) {
      const home = join(fixture, `environment-${index}`);
      await mkdir(home);
      const before = documents.length;
      const result = await run(process.execPath, [join(runtimeRoot, "scripts", "hook-launcher.mjs"), "Stop"], { input: payload, home, env: variables });
      assert.equal(result.code, 0); assert.equal(documents.length - before, original.length);
      assert.deepEqual(documents.slice(before).map(doc => doc.customId), original.map(doc => doc.customId));
    }
  });

  currentCheck = "Remnifold MCP namespace and initialization";
  await check(currentCheck, async () => {
    const manifest = JSON.parse(await readFile(join(runtimeRoot, ".mcp.json"), "utf8"));
    assert.deepEqual(Object.keys(manifest.mcpServers), ["remnifold"]);
    const result = await run(process.execPath, [join(runtimeRoot, "scripts", "mcp-launcher.mjs")], { input: `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } })}\n` });
    assert.equal(result.code, 0); assert.equal(JSON.parse(result.stdout).result.serverInfo.name, "remnifold");
    const permission = await hook("PreToolUse", { tool_name: "mcp__remnifold__getDocument", tool_input: { id: "fixture" } });
    assert.equal(JSON.parse(permission.stdout).hookSpecificOutput.permissionDecision, "allow");
    assert.equal((await hook("PreToolUse", { tool_name: "mcp__kagayoi_memory__getDocument" })).stdout, "");
  });
} catch (error) {
  failed = true;
  const failure = { name: error?.name || "Error", message: diagnosticText(error?.message || error),
    stack: diagnosticText(error?.stack || "", 5000),
    ...(error instanceof assert.AssertionError ? { operator: error.operator,
      actual: diagnosticText(JSON.stringify(error.actual)), expected: diagnosticText(JSON.stringify(error.expected)) } : {}),
    ...(lastSubprocess ? { lastSubprocess } : {}) };
  checks.push({ name: currentCheck, passed: false, failure });
  console.error(JSON.stringify({ check: currentCheck, failure }));
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  assert.equal(children.size, 0, "No fixture subprocess may remain during cleanup");
  await cleanFixture();
}
const artifact = join(outputRoot, runtimeRoot === root ? "remnifold-rename-validation.json" : "remnifold-rename-installed-validation.json");
await mkdir(dirname(artifact), { recursive: true });
await writeFile(artifact, JSON.stringify({ node: process.version, checks, passed: !failed, temporaryFixtureRemoved: true }, null, 2) + "\n");
console.log(JSON.stringify({ passed: !failed, checks: checks.length, artifact }));
if (failed) process.exitCode = 1;
