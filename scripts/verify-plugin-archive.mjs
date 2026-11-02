#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, readFile } from "node:fs/promises";
import path from "node:path";
import { collectPackageFiles, REPOSITORY_ROOT } from "./package-plugin.mjs";
import { isNativeSourcePath, nativeHooks, nativeJson, nativeTarget, verifyNativeFiles } from "./native-distribution.mjs";
import { tarExecutable } from "./lib/tar.mjs";

// アップロードするアーカイブ自身を、同じcheckoutの配布対象と照合する。
const archive = path.resolve(process.argv[2] ?? "dist/remnifold.tar.gz");
const tar = tarExecutable();
const entries = execFileSync(tar, ["-tzf", archive], { encoding: "utf8" }).trim().split(/\r?\n/u);
const files = entries.filter(entry => !entry.endsWith("/"));
if (files.includes("remnifold/native-build.json")) {
  const bytes = new Map();
  for (const name of files) {
    assert.ok(name.startsWith("remnifold/") && !name.includes("../") && !name.includes("\\"), "Unsafe archive path");
    const relative = name.slice("remnifold/".length);
    assert.ok(!bytes.has(relative), "Duplicate archive entry");
    bytes.set(relative, execFileSync(tar, ["-xOf", archive, name], { maxBuffer: 64 * 1024 * 1024 }));
  }
  const commit = process.env.GITHUB_SHA ?? execFileSync("git", ["rev-parse", "HEAD"], { cwd: REPOSITORY_ROOT, encoding: "utf8" }).trim();
  const manifest = verifyNativeFiles(bytes, { sourceCommit: commit });
  const primary = JSON.parse(await readFile(path.join(REPOSITORY_ROOT, "package.json"), "utf8"));
  const cargo = await readFile(path.join(REPOSITORY_ROOT, "native/Cargo.toml"), "utf8");
  const cargoVersion = cargo.match(/^\[package\]\s*\r?\n([\s\S]*?)(?=^\[|$(?![\s\S]))/mu)?.[1].match(/^version\s*=\s*"([^"]+)"/mu)?.[1];
  assert.equal(manifest.version, primary.version, "Native/root version mismatch");
  assert.equal(cargoVersion, primary.version, "Native Cargo/root version mismatch");
  for (const name of [".codex-plugin/plugin.json", "server/package.json"]) assert.equal(JSON.parse(bytes.get(name).toString("utf8")).version, primary.version, `Release version mismatch: ${name}`);
  const binaryName = nativeTarget(manifest.target).executable;
  assert.ok(process.argv.length <= 3 || (process.argv.length === 5 && process.argv[3] === "--binary"), "Expected an optional --binary <exact-build-output>");
  const targetRoot = process.env.CARGO_TARGET_DIR ? path.resolve(process.env.CARGO_TARGET_DIR) : path.join(REPOSITORY_ROOT, "native/target");
  const expectedBinary = path.resolve(process.argv[4] ?? path.join(targetRoot, manifest.target, "release", path.basename(binaryName)));
  const binaryInfo = await lstat(expectedBinary);
  assert.ok(binaryInfo.isFile() && !binaryInfo.isSymbolicLink(), "Expected native build output must be a regular file");
  assert.ok(bytes.get(binaryName).equals(await readFile(expectedBinary)), "Archive/native build output mismatch");
  const expectedSources = (await collectPackageFiles()).filter(isNativeSourcePath);
  assert.deepEqual([...bytes.keys()].filter(isNativeSourcePath).sort(), expectedSources.sort(), "Native archive source file set mismatch");
  for (const name of expectedSources) assert.ok(bytes.get(name).equals(await readFile(path.join(REPOSITORY_ROOT, name))), `Archive/source mismatch: ${name}`);
  const hooks = JSON.parse(await readFile(path.join(REPOSITORY_ROOT, "hooks/hooks.json"), "utf8"));
  assert.equal(bytes.get("hooks/hooks.json").toString("utf8"), nativeJson(nativeHooks(manifest.target, hooks)));
  console.log(JSON.stringify({ version: manifest.version, target: manifest.target, sourceSha: manifest.sourceCommit, files: bytes.size, sha256: createHash("sha256").update(await readFile(archive)).digest("hex") }));
} else {
const expected = (await collectPackageFiles()).map(file => `remnifold/${file}`).sort();
assert.deepEqual([...files].sort(), expected, "Archive must contain exactly the release files");
for (const entry of files) {
  const bytes = execFileSync(tar, ["-xOf", archive, entry], { maxBuffer: 4 * 1024 * 1024 });
  const source = await readFile(path.join(REPOSITORY_ROOT, entry.slice("remnifold/".length)));
  assert.ok(bytes.equals(source), `Archive/source mismatch: ${entry}`);
}
const primary = JSON.parse(await readFile(path.join(REPOSITORY_ROOT, "package.json"), "utf8"));
for (const file of [".codex-plugin/plugin.json", "server/package.json"]) {
  const manifest = JSON.parse(execFileSync(tar, ["-xOf", archive, `remnifold/${file}`], { encoding: "utf8" }));
  assert.equal(manifest.version, primary.version, `Release version mismatch: ${file}`);
}
console.log(JSON.stringify({ version: primary.version, files: files.length,
  sha256: createHash("sha256").update(await readFile(archive)).digest("hex"), sourceSha: process.env.GITHUB_SHA ?? null }));
}
