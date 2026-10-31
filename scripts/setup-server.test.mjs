import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { parseArgs, runSetup } from "./setup-server.mjs";

const API_KEY = "test-only-api-key-that-must-not-appear";

function verifiedResponse(url, init) {
  if (!url.endsWith("/mcp")) return { ok: true, status: 200 };
  const message = JSON.parse(init.body);
  const result = message.method === "initialize"
    ? { protocolVersion: "2025-06-18" }
    : { tools: ["search_memory", "getDocument", "add_memory"].map((name) => ({ name })) };
  return { ok: true, status: 200, json: async () => ({ jsonrpc: "2.0", id: message.id, result }) };
}

async function fixture(existingConfig) {
  const root = await mkdtemp(path.join(os.tmpdir(), "remnifold-setup-"));
  const serverDir = path.join(root, "server");
  const wranglerBin = path.join(serverDir, "node_modules", "wrangler", "bin", "wrangler.js");
  await mkdir(path.dirname(wranglerBin), { recursive: true });
  await mkdir(path.join(serverDir, "migrations"), { recursive: true });
  await writeFile(path.join(serverDir, "package.json"), "{}\n");
  await writeFile(wranglerBin, "// mock\n");
  await writeFile(path.join(serverDir, "migrations", "0001.sql"), "SELECT 1;\n");
  await writeFile(
    path.join(serverDir, "wrangler.example.jsonc"),
    JSON.stringify({
      $schema: "./node_modules/wrangler/config-schema.json",
      name: "remnifold",
      main: "src/index.ts",
      compatibility_date: "2026-09-02",
      workers_dev: true,
      preview_urls: false,
      triggers: { crons: ["*/15 * * * *", "0 18 * * *"] },
      vars: { AI_ENRICHMENT_MODE: "on" },
      ai: { binding: "AI" },
      vectorize: [],
      d1_databases: [],
    }),
  );
  if (existingConfig) await writeFile(path.join(serverDir, "wrangler.jsonc"), existingConfig);
  return { root, serverDir, wranglerBin };
}

function mockCloudflare(overrides = {}) {
  const state = {
    d1: overrides.d1 ?? undefined,
    vector: overrides.vector ?? undefined,
    secretVerifiable: overrides.secretVerifiable ?? false,
    secretPresent: overrides.secretPresent ?? false,
    d1CreateCode: overrides.d1CreateCode ?? 0,
    vectorCreateCode: overrides.vectorCreateCode ?? 0,
    secretPutCode: overrides.secretPutCode ?? 0,
    resourceName: overrides.resourceName ?? "remnifold",
    workerName: overrides.workerName ?? "remnifold",
  };
  const calls = [];
  const runner = async ({ args, input }) => {
    calls.push({ args: [...args], input });
    const command = args.join(" ");
    if (command.startsWith("whoami --json")) return { code: 0, stdout: '{"email":"user@example.test"}', stderr: "" };
    if (command.startsWith("d1 list --json")) {
      return { code: 0, stdout: JSON.stringify(state.d1 ? [{ name: state.resourceName, uuid: state.d1 }] : []), stderr: "" };
    }
    if (command.startsWith("d1 create remnifold")) {
      state.d1 = state.d1 ?? "11111111-1111-1111-1111-111111111111";
      return { code: state.d1CreateCode, stdout: "created", stderr: state.d1CreateCode ? "already exists" : "" };
    }
    if (command.startsWith("vectorize list --json")) {
      return { code: 0, stdout: JSON.stringify(state.vector ? [{ name: state.resourceName }] : []), stderr: "" };
    }
    if (command.startsWith("vectorize create remnifold")) {
      state.vector = state.vector ?? { dimensions: 1024, metric: "cosine" };
      return { code: state.vectorCreateCode, stdout: "{}", stderr: state.vectorCreateCode ? "already exists" : "" };
    }
    if (command.startsWith(`vectorize get ${state.resourceName} --json`)) {
      return { code: 0, stdout: JSON.stringify({ name: state.resourceName, config: state.vector }), stderr: "" };
    }
    if (command.startsWith("secret list --name ")) {
      if (args[3] !== state.workerName || !state.secretVerifiable) return { code: 1, stdout: "", stderr: "Worker does not exist" };
      return {
        code: 0,
        stdout: JSON.stringify(state.secretPresent ? [{ name: "MEMORY_API_KEY", type: "secret_text" }] : []),
        stderr: "",
      };
    }
    if (command.startsWith("secret put MEMORY_API_KEY")) {
      assert.equal(input, `${API_KEY}\n`);
      if (state.secretPutCode) {
        return { code: state.secretPutCode, stdout: "", stderr: `failed with input ${input}` };
      }
      state.secretPresent = true;
      state.secretVerifiable = true;
      return { code: 0, stdout: "secret updated", stderr: "" };
    }
    if (command.startsWith("d1 migrations apply DB --remote")) return { code: 0, stdout: "migrated", stderr: "" };
    if (command.startsWith("deploy --minify")) {
      return { code: 0, stdout: "Deployed https://remnifold.example.workers.dev", stderr: "" };
    }
    throw new Error(`Unexpected mock Wrangler command: ${command}`);
  };
  return { state, calls, runner };
}

test("argument validation rejects unsafe names and conflicting D1 placement", () => {
  assert.throws(() => parseArgs(["--worker-name", "Not Valid"]), /lowercase letters/);
  assert.throws(() => parseArgs(["--location", "apac", "--jurisdiction", "eu"]), /either/);
  assert.throws(() => parseArgs(["--base-url", "http://example.com"]), /HTTPS/);
});

test("default dry run performs discovery without creating resources or writing config", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare();
  const lines = [];
  const result = await runSetup([], {
    ...files,
    runner: cloudflare.runner,
    output: (line) => lines.push(line),
    nodeVersion: "24.0.0",
  });

  assert.equal(result.mode, "dry-run");
  assert.equal(cloudflare.calls.some(({ args }) => args.includes("create") || args.includes("deploy")), false);
  await assert.rejects(readFile(path.join(files.serverDir, "wrangler.jsonc")), { code: "ENOENT" });
  assert.match(lines.join("\n"), /pass --apply/);
  assert.match(lines.join("\n"), /set REMNIFOLD_API_KEY/);
});

test("apply creates missing resources, writes config, migrates, deploys, and verifies", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare();
  const lines = [];
  const requests = [];
  const result = await runSetup(["--apply", "--location", "apac"], {
    ...files,
    env: { REMNIFOLD_API_KEY: API_KEY },
    runner: cloudflare.runner,
    output: (line) => lines.push(line),
    fetchImpl: async (url, init) => {
      requests.push({ url, authorization: init.headers?.Authorization });
      return verifiedResponse(url, init);
    },
    sleep: async () => {},
    nodeVersion: "24.0.0",
  });

  assert.equal(result.mode, "apply");
  const config = JSON.parse(await readFile(path.join(files.serverDir, "wrangler.jsonc"), "utf8"));
  assert.equal(config.d1_databases[0].database_id, "11111111-1111-1111-1111-111111111111");
  assert.equal(config.vectorize[0].index_name, "remnifold");
  assert.equal("secrets" in config, false);
  assert.ok(cloudflare.calls.some(({ args }) => args.join(" ").includes("d1 create remnifold --location apac")));
  assert.ok(cloudflare.calls.some(({ args }) => args.join(" ").startsWith("d1 migrations apply DB --remote")));
  assert.ok(cloudflare.calls.some(({ args }) => args.join(" ").startsWith("deploy --minify")));
  assert.deepEqual(requests, [
    { url: "https://remnifold.example.workers.dev/health", authorization: undefined },
    { url: "https://remnifold.example.workers.dev/v3/session", authorization: `Bearer ${API_KEY}` },
    { url: "https://remnifold.example.workers.dev/mcp", authorization: `Bearer ${API_KEY}` },
    { url: "https://remnifold.example.workers.dev/mcp", authorization: `Bearer ${API_KEY}` },
  ]);
  assert.ok(cloudflare.calls.findIndex(({ args }) => args[0] === "d1" && args[1] === "migrations") < cloudflare.calls.findIndex(({ args }) => args[0] === "deploy"));
  assert.match(lines.join("\n"), /Remote MCP migration: verified/);
  assert.doesNotMatch(lines.join("\n"), new RegExp(API_KEY));
});

test("existing resources and secret are reused while custom config fields are preserved", async (t) => {
  const existing = `{
    // user-owned routing stays intact
    "name": "old-name",
    "main": "old.ts",
    "routes": [{ "pattern": "memory.example.test", "custom_domain": true }],
    "vars": { "CUSTOM": "kept" },
    "vectorize": [{ "binding": "OTHER_VECTOR", "index_name": "other" }],
    "d1_databases": [{ "binding": "OTHER_DB", "database_name": "other", "database_id": "other-id" }],
  }`;
  const files = await fixture(existing);
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({
    d1: "22222222-2222-2222-2222-222222222222",
    vector: { dimensions: 1024, metric: "cosine" },
    secretVerifiable: true,
    secretPresent: true,
  });
  const requests = [];
  await runSetup(["--apply", "--worker-name", "remnifold"], {
    ...files,
    env: {},
    runner: cloudflare.runner,
    output: (line) => requests.push({ line }),
    fetchImpl: async (url, init) => {
      requests.push({ url, headers: init.headers });
      return { ok: true, status: 200 };
    },
    nodeVersion: "24.0.0",
  });

  const config = JSON.parse(await readFile(path.join(files.serverDir, "wrangler.jsonc"), "utf8"));
  assert.deepEqual(config.routes, [{ pattern: "memory.example.test", custom_domain: true }]);
  assert.equal(config.vars.CUSTOM, "kept");
  assert.equal(config.vars.AI_ENRICHMENT_MODE, "on");
  assert.deepEqual(config.triggers.crons, ["*/15 * * * *", "0 18 * * *"]);
  assert.equal(config.compatibility_date, "2026-09-02");
  assert.equal(config.vectorize.length, 2);
  assert.equal(config.d1_databases.length, 2);
  assert.equal(cloudflare.calls.some(({ args }) => args[0] === "secret" && args[1] === "put"), false);
  assert.equal(requests.filter(({ url }) => url).length, 1);
  assert.ok(requests.some(({ url }) => url?.endsWith("/health")));
  assert.ok(requests.some(({ line }) => line?.includes("MCP verification skipped")));
});

test("existing custom configuration selects the same Worker and storage, and rejects a mismatched D1 ID", async (t) => {
  const id = "22222222-2222-2222-2222-222222222222";
  const config = JSON.stringify({ name: "custom-memory", d1_databases: [{ binding: "DB", database_name: "custom-memory", database_id: id }], vectorize: [{ binding: "MEMORY_VECTORS", index_name: "custom-memory" }] });
  const files = await fixture(config);
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({ resourceName: "custom-memory", workerName: "custom-memory", d1: id, vector: { dimensions: 1024, metric: "cosine" }, secretVerifiable: true, secretPresent: true });
  await runSetup(["--apply"], { ...files, env: {}, runner: cloudflare.runner, output: () => {}, fetchImpl: async () => ({ ok: true, status: 200 }), nodeVersion: "24.0.0" });
  const updated = JSON.parse(await readFile(path.join(files.serverDir, "wrangler.jsonc"), "utf8"));
  assert.equal(updated.name, "custom-memory");
  assert.equal(updated.d1_databases[0].database_id, id);
  assert.equal(updated.vectorize[0].index_name, "custom-memory");
  assert.equal(cloudflare.calls.some(({ args }) => args.includes("create")), false);
  const wrongAccount = mockCloudflare({ resourceName: "custom-memory", d1: "33333333-3333-3333-3333-333333333333", vector: { dimensions: 1024, metric: "cosine" } });
  await assert.rejects(runSetup(["--apply"], { ...files, env: {}, runner: wrongAccount.runner, output: () => {}, nodeVersion: "24.0.0" }), /Configured D1 database was not found with its existing ID/);
  assert.equal(wrongAccount.calls.some(({ args }) => args.includes("create") || args[0] === "deploy"), false);
});

test("1.x resource names are reused instead of creating an empty replacement store", async (t) => {
  for (const legacyName of ["kagayoi-memory", "cloudflare-supermemory"]) {
    const files = await fixture();
    t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
    const cloudflare = mockCloudflare({
      resourceName: legacyName,
      workerName: legacyName,
      d1: "44444444-4444-4444-4444-444444444444",
      vector: { dimensions: 1024, metric: "cosine" },
      secretVerifiable: true,
      secretPresent: true,
    });

    await runSetup(["--apply"], {
      ...files,
      env: {},
      runner: cloudflare.runner,
      output: () => {},
      fetchImpl: async () => ({ ok: true, status: 200 }),
      nodeVersion: "24.0.0",
    });

    const config = JSON.parse(await readFile(path.join(files.serverDir, "wrangler.jsonc"), "utf8"));
    assert.equal(config.d1_databases[0].database_name, legacyName);
    assert.equal(config.vectorize[0].index_name, legacyName);
    assert.equal(config.name, legacyName);
    assert.equal(cloudflare.calls.some(({ args }) => args.includes("create")), false);
  }
});

test("explicit Remnifold resource names are not replaced by discovered 1.x resources", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({
    resourceName: "cloudflare-supermemory",
    workerName: "cloudflare-supermemory",
    d1: "44444444-4444-4444-4444-444444444444",
    vector: { dimensions: 1024, metric: "cosine" },
    secretVerifiable: true,
    secretPresent: true,
  });

  const lines = [];
  const result = await runSetup([
    "--worker-name", "remnifold",
    "--database-name", "remnifold",
    "--vector-index-name", "remnifold",
  ], {
    ...files,
    env: {},
    runner: cloudflare.runner,
    output: (line) => lines.push(line),
    nodeVersion: "24.0.0",
  });

  assert.equal(result.d1, false);
  assert.equal(result.vector, false);
  assert.equal(result.secret.verifiable, false);
  assert.match(lines.join("\n"), /D1 database: create remnifold/);
  assert.match(lines.join("\n"), /Vectorize index: create remnifold/);
});

test("apply fails before mutations when no existing or supplied Worker secret is available", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare();

  await assert.rejects(
    runSetup(["--apply"], {
      ...files,
      env: {},
      runner: cloudflare.runner,
      output: () => {},
      nodeVersion: "24.0.0",
    }),
    /Set REMNIFOLD_API_KEY/,
  );
  assert.equal(cloudflare.calls.some(({ args }) => args.includes("create") || args.includes("deploy")), false);
});

test("an incompatible existing Vectorize index is rejected before changes", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({
    d1: "33333333-3333-3333-3333-333333333333",
    vector: { dimensions: 768, metric: "cosine" },
  });

  await assert.rejects(
    runSetup(["--apply"], {
      ...files,
      env: { REMNIFOLD_API_KEY: API_KEY },
      runner: cloudflare.runner,
      output: () => {},
      nodeVersion: "24.0.0",
    }),
    /expected 1024 and cosine/,
  );
  await assert.rejects(readFile(path.join(files.serverDir, "wrangler.jsonc")), { code: "ENOENT" });
});

test("rerun-safe discovery accepts the 1.x key alias and recovers when create reports a conflict", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({ d1CreateCode: 1 });
  const lines = [];
  await runSetup(["--apply"], {
    ...files,
    env: { CLOUDFLARE_MEMORY_API_KEY: API_KEY },
    runner: cloudflare.runner,
    output: (line) => lines.push(line),
    fetchImpl: async (url, init) => verifiedResponse(url, init),
    sleep: async () => {},
    nodeVersion: "24.0.0",
  });
  assert.match(lines.join("\n"), /conflict or interruption/);
  assert.doesNotMatch(lines.join("\n"), new RegExp(API_KEY));
});

test("MCP tool discovery failure does not report a completed migration or expose credentials", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare();
  const lines = [];
  await assert.rejects(runSetup(["--apply"], {
    ...files,
    env: { REMNIFOLD_API_KEY: API_KEY },
    runner: cloudflare.runner,
    output: (line) => lines.push(line),
    fetchImpl: async (url, init) => {
      if (url.endsWith("/mcp") && JSON.parse(init.body).method === "tools/list") {
        return { ok: true, json: async () => ({ jsonrpc: "2.0", id: 2, result: { tools: [] } }) };
      }
      return verifiedResponse(url, init);
    },
    sleep: async () => {},
    nodeVersion: "24.0.0",
  }), /Remote MCP tool discovery verification failed/);
  assert.doesNotMatch(lines.join("\n"), /Remote MCP migration: verified/);
  assert.doesNotMatch(lines.join("\n"), new RegExp(API_KEY));
});

test("a failed secret update never includes the secret value in the error", async (t) => {
  const files = await fixture();
  t.after(() => ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(files.root, { allowedRoot: path.resolve(os.tmpdir()), recursive: true, force: true }));
  const cloudflare = mockCloudflare({ secretPutCode: 1 });
  let failure;
  try {
    await runSetup(["--apply"], {
      ...files,
      env: { REMNIFOLD_API_KEY: API_KEY },
      runner: cloudflare.runner,
      output: () => {},
      sleep: async () => {},
      nodeVersion: "24.0.0",
    });
  } catch (error) {
    failure = error;
  }
  assert.ok(failure);
  assert.match(failure.message, /setting MEMORY_API_KEY/);
  assert.doesNotMatch(failure.message, new RegExp(API_KEY));
});
