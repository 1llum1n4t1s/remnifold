#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { collectPackageFiles, REPOSITORY_ROOT } from "./package-plugin.mjs";

// アップロードするアーカイブ自身を、同じcheckoutの配布対象と照合する。
const archive = path.resolve(process.argv[2] ?? "dist/remnifold.tar.gz");
const entries = execFileSync("tar", ["-tzf", archive], { encoding: "utf8" }).trim().split("\n");
const files = entries.filter(entry => !entry.endsWith("/"));
const expected = (await collectPackageFiles()).map(file => `remnifold/${file}`).sort();
assert.deepEqual([...files].sort(), expected, "Archive must contain exactly the release files");
for (const entry of files) {
  const bytes = execFileSync("tar", ["-xOf", archive, entry], { maxBuffer: 4 * 1024 * 1024 });
  const source = await readFile(path.join(REPOSITORY_ROOT, entry.slice("remnifold/".length)));
  assert.ok(bytes.equals(source), `Archive/source mismatch: ${entry}`);
}
const primary = JSON.parse(await readFile(path.join(REPOSITORY_ROOT, "package.json"), "utf8"));
for (const file of [".codex-plugin/plugin.json", "server/package.json"]) {
  const manifest = JSON.parse(execFileSync("tar", ["-xOf", archive, `remnifold/${file}`], { encoding: "utf8" }));
  assert.equal(manifest.version, primary.version, `Release version mismatch: ${file}`);
}
console.log(JSON.stringify({ version: primary.version, files: files.length,
  sha256: createHash("sha256").update(await readFile(archive)).digest("hex"), sourceSha: process.env.GITHUB_SHA ?? null }));
