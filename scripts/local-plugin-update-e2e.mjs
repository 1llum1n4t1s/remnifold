// 失敗条件: 公開ref/version/CI/artifact/hashの不一致、危険なarchive、利用者編集、
// 公式CLI失敗、再開時の重複add、source/cacheの不一致、検証失敗前のローカル変更。
import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { access, copyFile, cp, lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { isNativeSourcePath } from "./native-distribution.mjs";

const execute = promisify(execFile);
const repository = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const updater = path.join(repository, "scripts", "update-local-plugin.mjs");
let sha = "1234567890abcdef1234567890abcdef12345678";
const oldVersion = "0.0.0-e2e";
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const blob = (bytes) => createHash("sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex");
const inside = (root, target) => {
  const relative = path.relative(root, target);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
};

async function safeExternalDirectory(value) {
  assert.ok(path.isAbsolute(value), "An absolute external work root is required");
  const absolute = path.resolve(value);
  assert.ok(!inside(repository, absolute), "Fixture output must be outside the repository");
  if (process.env.CODEX_HOME) assert.ok(!inside(path.resolve(process.env.CODEX_HOME), absolute));
  if (process.env.USERPROFILE) assert.ok(!inside(path.join(process.env.USERPROFILE, ".codex"), absolute));
  if (process.platform === "win32") {
    assert.match(absolute, /^D:\\/iu, "Windows fixture output must be on D:");
    assert.ok(inside(path.resolve("D:/Codex/remnifold"), absolute), "Updater work root must be under D:/Codex/remnifold");
  }
  let current = path.parse(absolute).root;
  let existingParent = current;
  for (const component of absolute.slice(current.length).split(path.sep).filter(Boolean)) {
    assert.notEqual(component.toLowerCase(), "artifacts", "The fixture must not create an artifacts directory");
    current = path.join(current, component);
    try {
      const stats = await lstat(current);
      assert.equal(stats.isSymbolicLink(), false, `Linked work path: ${current}`);
      assert.ok(stats.isDirectory(), `Work path is not a directory: ${current}`);
      existingParent = current;
    }
    catch (error) { if (error.code !== "ENOENT") throw error; }
  }
  // macOS/Linuxのルートが読取専用でも、実際の作成先の親が書ければよい。
  await access(existingParent, constants.W_OK);
  await mkdir(absolute, { recursive: true });
  await access(absolute, constants.W_OK);
  return absolute;
}

async function filesAt(root) {
  const result = {};
  async function walk(directory, prefix = "") {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name);
      const relative = prefix + entry.name;
      assert.equal((await lstat(absolute)).isSymbolicLink(), false);
      if (entry.isDirectory()) await walk(absolute, relative + "/");
      else result[relative] = hash(await readFile(absolute));
    }
  }
  await walk(root);
  return Object.fromEntries(Object.entries(result).sort(([left], [right]) => left.localeCompare(right, "en")));
}

// 別プロセスで動くCLI fixture。updater自体のAPIやarchive readerは再利用しない。
async function fakeCli() {
  const fs = await import("node:fs/promises");
  const p = await import("node:path");
  const [kind, statePath, ...args] = process.argv.slice(2);
  const state = JSON.parse(await fs.readFile(statePath, "utf8"));
  await fs.appendFile(state.logPath, JSON.stringify({ kind, args, temp: process.env.TEMP, tmp: process.env.TMP }) + "\n");
  const output = (value) => process.stdout.write(JSON.stringify(value) + "\n");
  if (kind === "gh" && args[0] === "api") {
    const prefix = `repos/${state.repository}/`;
    if (!args[1]?.startsWith(prefix)) throw new Error("Unexpected repository");
    const endpoint = args[1].slice(prefix.length);
    if (state.historyUnavailable && endpoint.startsWith("actions/runs")) {
      output({ workflow_runs: [], artifacts: [] });
      return;
    }
    if (endpoint === `git/ref/heads/release/${state.version}`) {
      output({ ref: `refs/heads/release/${state.version}`, object: { type: "commit", sha: state.fault === "ref" ? "not-a-commit" : state.sha } });
    } else if (endpoint === `contents/.codex-plugin/plugin.json?ref=${state.sha}`) {
      const manifest = { ...state.manifest, version: state.fault === "version" ? "999.0.0" : state.version };
      output({ encoding: "base64", content: Buffer.from(JSON.stringify(manifest)).toString("base64") });
    } else if (endpoint === `actions/runs?head_sha=${state.sha}&event=push&per_page=100`) {
      const run = { id: 100, run_attempt: 1, name: "CI", head_sha: state.sha, event: "push", head_branch: `release/${state.version}`, status: "completed", conclusion: "success" };
      if (state.fault === "ci-sha") run.head_sha = "f".repeat(40);
      if (state.fault === "ci") run.conclusion = "failure";
      output({ workflow_runs: [run] });
    } else if (endpoint === "actions/runs/100/artifacts?per_page=100") {
      output({ artifacts: state.fault === "artifact" ? [] : [{ id: 200, name: state.artifactName || "remnifold", digest: state.fault === "digest" ? `sha256:${"0".repeat(64)}` : state.artifactDigest, expired: state.fault === "expired", expires_at: "2999-01-01T00:00:00Z" }] });
    } else if (endpoint === "actions/artifacts/200/zip" && state.zipPath) {
      process.stdout.write(state.fault === "archive" ? Buffer.from("invalid zip") : await fs.readFile(state.zipPath));
    } else if (endpoint === `git/trees/${state.sha}?recursive=1`) {
      const tree = state.tree.map((entry, index) => ({ ...entry, sha: state.fault === "hash" && index === 0 ? "0".repeat(40) : entry.sha }));
      output({ truncated: state.fault === "tree", tree });
    } else throw new Error(`Unexpected GitHub API: ${endpoint}`);
  } else if (kind === "gh" && args[0] === "run" && args[1] === "download") {
    if (state.historyUnavailable) throw new Error("Published workflow artifact is no longer retained");
    if (JSON.stringify(args.slice(0, 7)) !== JSON.stringify(["run", "download", "100", "--repo", state.repository, "--name", "remnifold"]) || args[7] !== "--dir" || !p.isAbsolute(args[8])) throw new Error("Unexpected artifact download");
    const target = p.join(args[8], "remnifold.tar.gz");
    if (state.fault === "archive") await fs.writeFile(target, "not a gzip archive");
    else await fs.copyFile(state.archive, target);
    if (state.fault === "extra-file") await fs.writeFile(p.join(args[8], "unexpected.txt"), "unexpected");
  } else if (kind === "codex" && JSON.stringify(args) === JSON.stringify(["plugin", "list", "--marketplace", "personal", "--json"])) {
    output({ installed: [{ pluginId: "remnifold@personal", installed: true, enabled: true, version: state.installedVersion, source: { source: "local", path: state.source } }] });
  } else if (kind === "codex" && JSON.stringify(args) === JSON.stringify(["plugin", "add", "remnifold@personal", "--json"])) {
    if (state.failAdd) { process.stderr.write("Official install fixture failed\n"); process.exitCode = 42; return; }
    const version = JSON.parse(await fs.readFile(p.join(state.source, ".codex-plugin", "plugin.json"), "utf8")).version;
    await fs.cp(state.source, p.join(state.codexHome, "plugins", "cache", "personal", "remnifold", version), { recursive: true });
    state.installedVersion = version;
    await fs.writeFile(statePath, JSON.stringify(state));
    output({ installed: true });
  } else throw new Error(`Unexpected fixture command: ${kind} ${JSON.stringify(args)}`);
}

async function main() {
  const args = {};
  for (let index = 2; index < process.argv.length; index += 2) {
    const flag = process.argv[index];
    assert.ok(["--archive", "--work-root", "--updater-binary", "--legacy-root"].includes(flag) && process.argv[index + 1] && !args[flag], "Usage: node scripts/local-plugin-update-e2e.mjs --archive <published.tar.gz> --work-root <external directory> [--updater-binary <native executable>] [--legacy-root <source package>]");
    args[flag] = process.argv[index + 1];
  }
  assert.ok(args["--archive"] && path.isAbsolute(args["--archive"]), "--archive must name an absolute published plugin archive");
  const output = await safeExternalDirectory(args["--work-root"] || process.env.REMNIFOLD_VALIDATION_DIR || "");
  const root = path.join(output, `local-plugin-e2e-${randomUUID()}`);
  await mkdir(root);
  const temp = path.join(root, "temp");
  await mkdir(temp);
  const env = { ...process.env, TEMP: temp, TMP: temp };
  const command = (file, parameters, options = {}) => execute(file, parameters, { cwd: repository, env, windowsHide: true, timeout: 90_000, maxBuffer: 4 * 1024 * 1024, ...options });
  const archive = path.join(root, "published.tar.gz");
  await copyFile(args["--archive"], archive);
  const archiveHash = hash(await readFile(archive));
  const listing = (await command("tar", ["-tzf", archive])).stdout.trim().split(/\r?\n/u);
  assert.ok(listing.length > 1);
  for (const entry of listing) {
    assert.ok(/^remnifold(?:\/|$)/u.test(entry) && !entry.includes("\\") && !entry.includes(":"));
    assert.ok(!entry.replace(/\/$/u, "").split("/").some((part) => !part || part === "." || part === ".."));
  }
  const verboseListing = (await command("tar", ["-tvzf", archive])).stdout.trim().split(/\r?\n/u);
  for (const entry of verboseListing) assert.match(entry, /^[-d]/u, "The published fixture must contain only files and directories");
  const extracted = path.join(root, "published");
  await mkdir(extracted);
  await command("tar", ["-xzf", archive, "-C", extracted]);
  const packageRoot = path.join(extracted, "remnifold");
  const expected = await filesAt(packageRoot);
  const manifest = JSON.parse(await readFile(path.join(packageRoot, ".codex-plugin", "plugin.json"), "utf8"));
  const native = await readFile(path.join(packageRoot, "native-build.json"), "utf8").then(JSON.parse, () => null);
  if (native) { assert.ok(args["--updater-binary"], "Native archive requires --updater-binary"); sha = native.sourceCommit; }
  const tree = await Promise.all(Object.keys(expected).filter(name => !native || isNativeSourcePath(name)).map(async (name) => ({ path: name, type: "blob", mode: "100644", sha: blob(await readFile(path.join(packageRoot, name))) })));
  if (native) tree.push({ path: "native/Cargo.toml", type: "blob", mode: "100644", sha: "a".repeat(40) });
  let zipPath;
  let artifactDigest;
  if (native) {
    const payload = await readFile(archive);
    const name = Buffer.from(`remnifold-${native.target}.tar.gz`);
    let crc = 0xffffffff;
    for (const byte of payload) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0); }
    crc = (crc ^ 0xffffffff) >>> 0;
    const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4); local.writeUInt32LE(crc, 14); local.writeUInt32LE(payload.length, 18); local.writeUInt32LE(payload.length, 22); local.writeUInt16LE(name.length, 26);
    const central = Buffer.alloc(46); central.writeUInt32LE(0x02014b50); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6); central.writeUInt32LE(crc, 16); central.writeUInt32LE(payload.length, 20); central.writeUInt32LE(payload.length, 24); central.writeUInt16LE(name.length, 28);
    const end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50); end.writeUInt16LE(1, 8); end.writeUInt16LE(1, 10); end.writeUInt32LE(central.length + name.length, 12); end.writeUInt32LE(local.length + name.length + payload.length, 16);
    const zip = Buffer.concat([local, name, payload, central, name, end]);
    zipPath = path.join(root, "published.zip"); await writeFile(zipPath, zip); artifactDigest = `sha256:${hash(zip)}`;
  }
  const cliPath = path.join(root, "fake-cli.mjs");
  await writeFile(cliPath, `(${fakeCli.toString()})().catch(error => { console.error(error.message); process.exitCode = 1; });\n`);
  const scenarios = [];
  async function fixture(name, changes = {}, initialRoot = packageRoot, runFromSource = false, sourceName = "marketplace-source") {
    const directory = path.join(root, name);
    const source = path.join(directory, sourceName);
    const codexHome = path.join(directory, "codex-home");
    const work = path.join(directory, "work");
    await mkdir(directory);
    await cp(initialRoot, source, { recursive: true });
    await writeFile(path.join(source, ".codex-plugin", "plugin.json"), JSON.stringify({ ...manifest, version: oldVersion }));
    await cp(source, path.join(codexHome, "plugins", "cache", "personal", "remnifold", oldVersion), { recursive: true });
    const statePath = path.join(directory, "state.json");
    const logPath = path.join(directory, "calls.jsonl");
    const configPath = path.join(directory, "config.json");
    const state = { repository: "fixture/remnifold", version: manifest.version, manifest, sha, tree, source, codexHome, archive, logPath, zipPath, artifactDigest, artifactName: native ? `remnifold-${native.target}` : "remnifold", installedVersion: oldVersion, ...changes };
    await writeFile(statePath, JSON.stringify(state));
    await writeFile(logPath, "");
    await writeFile(configPath, JSON.stringify({ localPlugin: { id: "remnifold@personal", repository: state.repository, workflow: "CI", artifact: "remnifold", sourcePath: source }, commands: { gh: [process.execPath, cliPath, "gh", statePath], codex: [process.execPath, cliPath, "codex", statePath] } }));
    const calls = async () => (await readFile(logPath, "utf8")).split("\n").filter(Boolean).map((line) => JSON.parse(line));
    const snapshot = async () => ({ source: await filesAt(source), cache: await filesAt(path.join(codexHome, "plugins", "cache")) });
    async function run(mode = "--apply", workRoot = work) {
      let result;
      let exitCode = 0;
      try { result = await command(runFromSource ? path.join(source, native.target.includes("windows") ? "bin/remnifold.exe" : "bin/remnifold") : (args["--updater-binary"] || process.execPath), [ ...(args["--updater-binary"] ? ["plugin-update"] : [updater]), "--version", manifest.version, "--work-root", workRoot, "--config", configPath, mode], { env: { ...env, CODEX_HOME: codexHome } }); }
      catch (error) { assert.equal(typeof error.code, "number", `Updater process failed to launch: ${error.message}`); result = error; exitCode = error.code; }
      assert.ok(result.stderr === "" || (args["--updater-binary"] && /LOCAL_PLUGIN_UPDATE_FAILED/u.test(result.stderr)), "Updater must use structured diagnostics");
      const value = JSON.parse(result.stdout.trim());
      for (const call of await calls()) {
        assert.equal(path.resolve(call.temp), path.resolve(workRoot));
        assert.equal(path.resolve(call.tmp), path.resolve(workRoot));
      }
      return { ...value, exitCode };
    }
    const addCount = async () => (await calls()).filter((entry) => entry.kind === "codex" && entry.args[1] === "add").length;
    async function assertInstalled() {
      const current = JSON.parse(await readFile(statePath, "utf8"));
      assert.equal(current.installedVersion, manifest.version);
      assert.deepEqual(await filesAt(source), expected);
      assert.deepEqual(await filesAt(path.join(codexHome, "plugins", "cache", "personal", "remnifold", manifest.version)), expected);
    }
    return { source, codexHome, statePath, configPath, snapshot, run, calls, addCount, assertInstalled };
  }
  async function success(fixture, mode, status) {
    const result = await fixture.run(mode);
    assert.equal(result.exitCode, 0);
    assert.equal(result.status, status);
    assert.equal(result.version, manifest.version);
    assert.equal(result.sha, sha);
    return result;
  }
  const updated = await fixture("success");
  const beforeReady = await updated.snapshot();
  await success(updated, "--check-published", "published");
  assert.deepEqual(await updated.snapshot(), beforeReady);
  assert.equal(await updated.addCount(), 0);
  scenarios.push({ name: "published-preflight", passed: true });
  await success(updated, "--apply", "updated");
  await updated.assertInstalled();
  assert.equal(await updated.addCount(), 1);
  scenarios.push({ name: "published-install-source-cache", passed: true });
  await success(updated, "--apply", "already-current");
  await success(updated, "--verify-only", "verified");
  await updated.assertInstalled();
  assert.equal(await updated.addCount(), 1);
  scenarios.push({ name: "idempotent-install-and-verify", passed: true });
  const installedState = JSON.parse(await readFile(updated.statePath, "utf8"));
  await writeFile(updated.statePath, JSON.stringify({ ...installedState, historyUnavailable: true }));
  const callsBeforeRetainedVerify = (await updated.calls()).length;
  const beforeRetainedVerify = await updated.snapshot();
  await success(updated, "--verify-only", "verified");
  await updated.assertInstalled();
  assert.deepEqual(await updated.snapshot(), beforeRetainedVerify);
  assert.equal(await updated.addCount(), 1);
  const verifyCalls = (await updated.calls()).slice(callsBeforeRetainedVerify);
  const verifyGitHubCalls = verifyCalls.filter((entry) => entry.kind === "gh");
  assert.equal(verifyGitHubCalls.length, 3, "Verification needs only release ref, manifest and Git tree");
  assert.deepEqual(verifyGitHubCalls.map((entry) => entry.args), [
    ["api", `repos/${installedState.repository}/git/ref/heads/release/${manifest.version}`],
    ["api", `repos/${installedState.repository}/contents/.codex-plugin/plugin.json?ref=${sha}`],
    ["api", `repos/${installedState.repository}/git/trees/${sha}?recursive=1`],
  ]);
  assert.equal(verifyCalls.filter((entry) => entry.kind === "codex").length, 1, "Verification only lists the installed plugin");
  scenarios.push({ name: "verify-without-retained-ci-or-artifact", githubCalls: verifyGitHubCalls.length, additionalAdds: 0, passed: true });
  await writeFile(path.join(updated.codexHome, "plugins", "cache", "personal", "remnifold", manifest.version, "README.md"), "Corrupted installed cache\n");
  const beforeVerify = await updated.snapshot();
  const rejectedVerify = await updated.run("--verify-only");
  assert.equal(rejectedVerify.exitCode, 1);
  assert.equal(rejectedVerify.code, "INSTALLED_PLUGIN_CONTENT_MISMATCH");
  assert.deepEqual(await updated.snapshot(), beforeVerify);
  assert.equal(await updated.addCount(), 1);
  scenarios.push({ name: "verify-rejects-corrupted-cache-without-repair", passed: true });

  const retry = await fixture("official-failure-resume", { failAdd: true });
  const beforeFailure = await retry.snapshot();
  const failed = await retry.run();
  assert.equal(failed.exitCode, 1);
  assert.equal(failed.code, "PLUGIN_ADD_FAILED");
  assert.deepEqual((await retry.snapshot()).cache, beforeFailure.cache);
  assert.deepEqual((await retry.snapshot()).source, expected);
  const failedState = JSON.parse(await readFile(retry.statePath, "utf8"));
  await writeFile(retry.statePath, JSON.stringify({ ...failedState, failAdd: false }));
  await success(retry, "--apply", "updated");
  await retry.assertInstalled();
  assert.equal(await retry.addCount(), 2);
  scenarios.push({ name: "official-failure-resume", passed: true });

  if (native) {
    const contained = await fixture("reject-work-inside-source");
    const aliasedSource = contained.source.toUpperCase();
    const caseAliasExists = await access(aliasedSource).then(() => true, () => false);
    if (process.platform === "win32") assert.ok(caseAliasExists, "Windows must exercise mixed-case aliases");
    if (caseAliasExists) {
      const config = JSON.parse(await readFile(contained.configPath, "utf8"));
      config.localPlugin.sourcePath = aliasedSource;
      await writeFile(contained.configPath, JSON.stringify(config));
    }
    const containedBefore = await contained.snapshot();
    const nestedWork = path.join(contained.source, "must-not-be-created");
    const containedResult = await contained.run("--apply", nestedWork);
    assert.equal(containedResult.code, "INVALID_WORK_ROOT"); assert.equal(containedResult.exitCode, 1);
    assert.deepEqual(await contained.snapshot(), containedBefore);
    await assert.rejects(access(nestedWork)); assert.equal((await contained.calls()).length, 0);
    scenarios.push({ name: "reject-work-inside-source-before-any-write", caseAliasTested: caseAliasExists, passed: true });
    const unicode = await fixture("reject-unicode-case-alias", {}, packageRoot, false, "Ä");
    const unicodeAlias = path.join(path.dirname(unicode.source), "ä");
    const unicodeAliasExists = await access(unicodeAlias).then(() => true, () => false);
    if (process.platform === "win32") assert.ok(unicodeAliasExists, "Windows must exercise Unicode case aliases");
    const unicodeBefore = await unicode.snapshot();
    const unicodeWork = path.join(unicodeAliasExists ? unicodeAlias : unicode.source, "must-not-be-created");
    const unicodeResult = await unicode.run("--apply", unicodeWork);
    assert.equal(unicodeResult.code, "INVALID_WORK_ROOT"); assert.equal(unicodeResult.exitCode, 1);
    assert.deepEqual(await unicode.snapshot(), unicodeBefore);
    await assert.rejects(access(unicodeWork)); assert.equal((await unicode.calls()).length, 0);
    scenarios.push({ name: "reject-unicode-case-aliased-work-before-any-write", caseAliasTested: unicodeAliasExists, passed: true });
    const self = await fixture("running-from-installed-source", {}, packageRoot, true);
    await success(self, "--apply", "updated"); await self.assertInstalled();
    scenarios.push({ name: "running-from-installed-source", passed: true });
    if (args["--legacy-root"]) {
      const legacyRoot = path.resolve(args["--legacy-root"]);
      await filesAt(legacyRoot); await access(path.join(legacyRoot, "scripts/mcp-launcher.mjs"));
      const migration = await fixture("legacy-js-migration", {}, legacyRoot);
      const sentinel = path.join(migration.codexHome, "connection-and-capture-receipt.json");
      const sentinelContent = JSON.stringify({ connection: "existing-connection", captureReceipt: "existing-receipt" });
      await writeFile(sentinel, sentinelContent);
      await success(migration, "--apply", "updated"); await migration.assertInstalled();
      assert.equal(await readFile(sentinel, "utf8"), sentinelContent);
      await success(migration, "--verify-only", "verified");
      scenarios.push({ name: "legacy-js-migration-preserves-external-connection-and-receipts", passed: true });
    }
  }

  for (const [fault, code] of [["ref", "RELEASE_REF_MISMATCH"], ["version", "RELEASE_VERSION_MISMATCH"], ["ci", "RELEASE_CI_NOT_SUCCESSFUL"], ["ci-sha", "RELEASE_CI_NOT_SUCCESSFUL"], ["artifact", "RELEASE_ARTIFACT_UNAVAILABLE"], ["expired", "RELEASE_ARTIFACT_UNAVAILABLE"], ["hash", "PACKAGE_HASH_MISMATCH"], ["tree", "INCOMPLETE_RELEASE_TREE"], ...(!native ? [["archive", "INVALID_ARCHIVE"], ["extra-file", "UNEXPECTED_ARTIFACT_CONTENTS"]] : [["digest", "ARTIFACT_DIGEST_MISMATCH"]]), ["source-edit", "LOCAL_SOURCE_MODIFIED"]]) {
    const isolated = await fixture(`reject-${fault}`, { fault });
    if (fault === "source-edit") await writeFile(path.join(isolated.source, "README.md"), "User-maintained content that must survive.\n");
    const before = await isolated.snapshot();
    const result = await isolated.run();
    assert.equal(result.exitCode, 1, fault);
    assert.equal(result.status, "blocked", fault);
    assert.equal(result.code, code, fault);
    assert.deepEqual(await isolated.snapshot(), before, `${fault}: source/cache changed before validation`);
    assert.equal(await isolated.addCount(), 0, fault);
    scenarios.push({ name: `reject-${fault}-before-mutation`, code, passed: true });
  }
  const report = { passed: true, version: manifest.version, archiveSha256: archiveHash, files: Object.keys(expected).length, scenarios, workRoot: root };
  await writeFile(path.join(output, "local-plugin-update-validation.json"), JSON.stringify(report, null, 2) + "\n");
  process.stdout.write(JSON.stringify(report) + "\n");
}

main().catch((error) => { console.error(error.stack); process.exitCode = 1; });
