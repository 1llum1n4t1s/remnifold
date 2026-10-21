import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { api as clientApi } from "./memory-client.mjs";
import { loadConfig, normalizeEndpoint } from "./Import-RemnifoldHistory.mjs";

const environmentNames = ["REMNIFOLD_API_URL", "REMNIFOLD_API_KEY", "KAGAYOI_MEMORY_API_URL", "KAGAYOI_MEMORY_API_KEY", "SUPERMEMORY_API_URL", "SUPERMEMORY_CODEX_API_KEY", "CLOUDFLARE_MEMORY_API_KEY"];

function isolatedEnvironment(t) {
  const original = new Map(environmentNames.map((name) => [name, process.env[name]]));
  for (const name of environmentNames) delete process.env[name];
  t.after(() => {
    for (const [name, value] of original) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  });
}

function temporaryHome(t, config) {
  const home = mkdtempSync(join(tmpdir(), "memory-config-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  if (config !== undefined) writeFileSync(join(home, "remnifold.json"), JSON.stringify(config));
  return home;
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

function runSetup(args, environment) {
  const script = resolve("scripts/setup-client.ps1");
  return new Promise((resolveRun, reject) => {
    const child = spawn("pwsh", ["-NoProfile", "-NonInteractive", "-File", script, ...args], {
      cwd: resolve("."),
      env: { ...process.env, ...environment },
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

test("設定ファイルと環境変数を項目単位で統合し利用者設定を保つ", (t) => {
  isolatedEnvironment(t);
  const home = temporaryHome(t, {
    baseUrl: "https://file.example/api/",
    apiKey: "file-key",
    recallMode: "off",
    readContainerTags: ["shared-team"],
  });
  process.env.REMNIFOLD_API_URL = "https://override.example/api/";
  const fromUrlOverride = loadConfig(home);
  assert.equal(fromUrlOverride.baseUrl, "https://override.example/api");
  assert.equal(fromUrlOverride.apiKey, "file-key");
  assert.equal(fromUrlOverride.recallMode, "off");
  assert.deepEqual(fromUrlOverride.readContainerTags, ["shared-team"]);

  delete process.env.REMNIFOLD_API_URL;
  process.env.REMNIFOLD_API_KEY = "environment-key";
  const fromKeyOverride = loadConfig(home);
  assert.equal(fromKeyOverride.baseUrl, "https://file.example/api");
  assert.equal(fromKeyOverride.apiKey, "environment-key");
});

test("完全な環境設定があれば壊れたローカル接続ファイルへ依存しない", (t) => {
  isolatedEnvironment(t);
  const home = temporaryHome(t);
  writeFileSync(join(home, "remnifold.json"), "{invalid");
  process.env.REMNIFOLD_API_URL = "https://environment.example/";
  process.env.REMNIFOLD_API_KEY = "environment-key";
  assert.deepEqual(loadConfig(home), { baseUrl: "https://environment.example", apiKey: "environment-key" });
});

test("1.xの接続設定を読み込み、セットアップ時にRemnifold設定へ移行する", async (t) => {
  isolatedEnvironment(t);
  const baseUrl = await listen(t, (_request, response) => {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ user: { id: "local-user" } }));
  });
  const home = temporaryHome(t);
  const legacyPath = join(home, "supermemory.json");
  const currentPath = join(home, "remnifold.json");
  writeFileSync(legacyPath, JSON.stringify({ baseUrl, apiKey: "legacy-key", recallMode: "off" }));
  assert.equal(loadConfig(home).recallMode, "off");

  const result = await runSetup(["-CodexHome", home], {});
  assert.equal(result.code, 0, result.stderr);
  assert.equal(JSON.parse(readFileSync(currentPath, "utf8")).apiKey, "legacy-key");
  assert.equal(existsSync(legacyPath), true);
});

test("接続先はHTTPSまたはloopbackのHTTPだけを受け付ける", () => {
  assert.equal(normalizeEndpoint("https://worker.example/path/"), "https://worker.example/path");
  assert.equal(normalizeEndpoint("http://localhost:8787/"), "http://localhost:8787");
  assert.equal(normalizeEndpoint("http://127.0.0.1:8787/"), "http://127.0.0.1:8787");
  assert.equal(normalizeEndpoint("http://[::1]:8787/"), "http://[::1]:8787");
  for (const endpoint of [
    "ftp://localhost:21/",
    "http://worker.example/",
    "https://user:password@worker.example/",
    "https://worker.example/?target=other",
    "https://worker.example/#fragment",
    "https://supermemory.ai/",
  ]) assert.throws(() => normalizeEndpoint(endpoint), /Refusing/);
});

test("APIクライアントは認証付きリクエストのredirectを追跡しない", async () => {
  let options;
  await clientApi("/v3/session", {
    config: { baseUrl: "https://worker.example", apiKey: "secret" },
    fetchImpl: async (_url, requestOptions) => {
      options = requestOptions;
      return { ok: true, status: 200, json: async () => ({ user: { id: "user" } }) };
    },
  });
  assert.equal(options.redirect, "error");
  assert.equal(options.headers.Authorization, "Bearer secret");
});

test("セットアップ再実行は接続だけを更新し既存の検索設定を保持する", async (t) => {
  const received = [];
  const baseUrl = await listen(t, (request, response) => {
    received.push(request.headers.authorization);
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ user: { id: "local-user" } }));
  });
  const home = temporaryHome(t, {
    baseUrl: "https://old.example",
    apiKey: "old-key",
    recallMode: "off",
    maxMemories: 3,
    readContainerTags: ["team"],
    futureOption: { enabled: true },
  });
  const secret = "replacement-test-secret";
  const result = await runSetup(["-BaseUrl", baseUrl, "-CodexHome", home, "-Force"], {
    REMNIFOLD_API_KEY: secret,
  });
  assert.equal(result.code, 0, result.stderr);
  assert.doesNotMatch(result.stdout + result.stderr, new RegExp(secret));
  assert.deepEqual(received, [`Bearer ${secret}`]);
  const config = JSON.parse(readFileSync(join(home, "remnifold.json"), "utf8"));
  assert.equal(config.baseUrl, baseUrl);
  assert.equal(config.apiKey, secret);
  assert.equal(config.recallMode, "off");
  assert.equal(config.maxMemories, 3);
  assert.deepEqual(config.readContainerTags, ["team"]);
  assert.deepEqual(config.futureOption, { enabled: true });
});

test("セットアップはCODEX_HOMEを使い明示引数を優先する", async (t) => {
  const baseUrl = await listen(t, (_request, response) => {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ user: { id: "local-user" } }));
  });
  const environmentHome = temporaryHome(t);
  const explicitHome = temporaryHome(t);
  const environment = { CODEX_HOME: environmentHome, REMNIFOLD_API_KEY: "home-test-key" };
  const inherited = await runSetup(["-BaseUrl", baseUrl], environment);
  assert.equal(inherited.code, 0, inherited.stderr);
  assert.equal(JSON.parse(readFileSync(join(environmentHome, "remnifold.json"), "utf8")).baseUrl, baseUrl);
  const explicit = await runSetup(["-BaseUrl", baseUrl, "-CodexHome", explicitHome], environment);
  assert.equal(explicit.code, 0, explicit.stderr);
  assert.equal(JSON.parse(readFileSync(join(explicitHome, "remnifold.json"), "utf8")).baseUrl, baseUrl);
});

test("セットアップは認証ヘッダーをredirect先へ送らない", async (t) => {
  let redirectedRequests = 0;
  let redirectedAuthorization;
  const target = await listen(t, (request, response) => {
    redirectedRequests += 1;
    redirectedAuthorization = request.headers.authorization;
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ user: { id: "redirected" } }));
  });
  const redirector = await listen(t, (_request, response) => {
    response.writeHead(302, { Location: `${target}/v3/session` });
    response.end();
  });
  const home = temporaryHome(t);
  const secret = "redirect-test-secret";
  const result = await runSetup(["-BaseUrl", redirector, "-CodexHome", home], {
    REMNIFOLD_API_KEY: secret,
  });
  assert.notEqual(result.code, 0);
  assert.equal(redirectedRequests, 0);
  assert.equal(redirectedAuthorization, undefined);
  assert.doesNotMatch(result.stdout + result.stderr, new RegExp(secret));
});
