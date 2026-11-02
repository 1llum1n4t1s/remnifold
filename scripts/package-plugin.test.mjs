import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
import assert from "node:assert/strict";
import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { buildPackage, isAllowedPackagePath, REPOSITORY_ROOT } from "./package-plugin.mjs";
import { validatePlugin } from "./validate-plugin.mjs";
import { NATIVE_TARGETS, nativeHooks, nativeJson, nativeMcp, sha256, verifyNativeFiles } from "./native-distribution.mjs";

test("native packages have platform-specific launchers and reject modified executables", () => {
  const sourceHooks = { hooks: Object.fromEntries(["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"].map(event => [event, [{ hooks: [{ type: "command", command: "development launcher", timeout: 12 }] }]])) };
  for (const [target, info] of Object.entries(NATIVE_TARGETS)) {
    const files = new Map([
      [".codex-plugin/plugin.json", Buffer.from(nativeJson({ name: "remnifold", version: "1.0.13" }))],
      [".mcp.json", Buffer.from(nativeJson(nativeMcp(target)))],
      ["hooks/hooks.json", Buffer.from(nativeJson(nativeHooks(target, sourceHooks)))],
      [info.executable, Buffer.from("native fixture executable")],
    ]);
    const sourceCommit = "a".repeat(40);
    files.set("native-build.json", Buffer.from(nativeJson({ schema: 1, target, version: "1.0.13", sourceCommit, files: Object.fromEntries([...files].map(([name, bytes]) => [name, sha256(bytes)])) })));
    assert.equal(verifyNativeFiles(files, { sourceCommit, target }).target, target);
    assert.ok(!files.get("hooks/hooks.json").includes("node"));
    assert.equal(JSON.parse(files.get(".mcp.json")).mcpServers.remnifold.command, `./${info.executable}`);
    files.set(info.executable, Buffer.from("modified native fixture"));
    assert.throws(() => verifyNativeFiles(files), /hash mismatch/u);
  }
});

function testOutput(name) {
  return path.join(REPOSITORY_ROOT, "dist", `.test-${name}-${process.pid}`);
}

test("package allowlist accepts runtime assets and rejects local state", () => {
  assert.equal(isAllowedPackagePath("client/mcp-server.mjs"), true);
  assert.equal(isAllowedPackagePath("server/src/index.ts"), true);
  assert.equal(isAllowedPackagePath("server/migrations/0001_initial.sql"), true);
  assert.equal(isAllowedPackagePath("scripts/setup-server.mjs"), true);
  assert.equal(isAllowedPackagePath("plugin.json"), false);
  assert.equal(isAllowedPackagePath("mcp.json"), false);
  assert.equal(isAllowedPackagePath("client/memory-client.test.mjs"), false);
  assert.equal(isAllowedPackagePath("scripts/setup-server.test.mjs"), false);
  assert.equal(isAllowedPackagePath("server/tests/smoke.mjs"), false);
  assert.equal(isAllowedPackagePath("server/node_modules/tool/index.js"), false);
  assert.equal(isAllowedPackagePath("server/.wrangler/state.json"), false);
  assert.equal(isAllowedPackagePath("server/wrangler.jsonc"), false);
  assert.equal(isAllowedPackagePath("server/.dev.vars"), false);
  assert.equal(isAllowedPackagePath("server/.dev.vars.production"), false);
  assert.equal(isAllowedPackagePath(".env.local"), false);
});

test("buildPackage creates a clean, valid consumer package", async () => {
  const output = testOutput("package");
  try {
    const first = await buildPackage({ output });
    for (const required of [
      ".codex-plugin/plugin.json",
      ".mcp.json",
      "CHANGELOG.md",
      "client/mcp-server.mjs",
      "hooks/hooks.json",
      "scripts/setup-client.ps1",
      "scripts/setup-server.mjs",
      "scripts/setup-server.ps1",
      "scripts/setup-topics.mjs",
      "server/pnpm-lock.yaml",
      "server/wrangler.example.jsonc",
      "server/migrations/0001_initial.sql",
      "server/src/index.ts",
    ]) {
      assert.ok(first.files.includes(required), `${required} should be packaged`);
      await access(path.join(output, required));
    }
    assert.ok(first.files.every(isAllowedPackagePath));
    assert.ok(!first.files.some((file) => /(?:node_modules|\.wrangler|\.test\.|\.dev\.vars)/u.test(file)));

    await writeFile(path.join(output, "stale-secret.txt"), "must be removed", "utf8");
    await buildPackage({ output });
    await assert.rejects(access(path.join(output, "stale-secret.txt")));

    const validation = await validatePlugin(output);
    assert.equal(validation.root, output);
  } finally {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(output, { allowedRoot: path.join(REPOSITORY_ROOT, "dist"), recursive: true, force: true });
  }
});

test("validator requires installed-root MCP cwd and rejects machine-specific paths", async () => {
  const output = testOutput("portable");
  try {
    await buildPackage({ output });
    const manifestPath = path.join(output, ".mcp.json");
    const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    delete manifest.mcpServers.remnifold.cwd;
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
    await assert.rejects(validatePlugin(output), /cwd must be \./u);

    manifest.mcpServers.remnifold.cwd = ".";
    manifest.mcpServers.remnifold.args.push("C:\\Users\\example\\plugin.mjs");
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
    await assert.rejects(validatePlugin(output), /machine-specific or non-portable path/u);
  } finally {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(output, { allowedRoot: path.join(REPOSITORY_ROOT, "dist"), recursive: true, force: true });
  }
});
