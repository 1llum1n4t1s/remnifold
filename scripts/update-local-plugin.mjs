import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash, randomUUID } from "node:crypto";
import { access, copyFile, lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import { gunzipSync } from "node:zlib";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { isAllowedPackagePath, REPOSITORY_ROOT } from "./package-plugin.mjs";
import { validatePlugin } from "./validate-plugin.mjs";

const execute = promisify(execFile);
const fail = (code) => { throw Object.assign(new Error(code), { updateCode: code }); };
const expand = (value) => path.resolve(value.startsWith("~/") ? path.join(os.homedir(), value.slice(2)) : value);
const inside = (parent, child) => { const relative = path.relative(parent, child); return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative)); };
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const blobHash = (bytes) => createHash("sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex");

async function noLinks(target, missing = false) {
  const absolute = path.resolve(target);
  let current = path.parse(absolute).root;
  for (const part of absolute.slice(current.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    try { if ((await lstat(current)).isSymbolicLink()) fail("LINK_PATH_REJECTED"); }
    catch (error) { if (missing && error.code === "ENOENT") return; throw error; }
  }
}

async function filesAt(root, hash = digest) {
  await noLinks(root);
  const files = new Map();
  async function walk(directory, prefix = "") {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const relative = prefix + entry.name;
      const absolute = path.join(directory, entry.name);
      const stat = await lstat(absolute);
      if (stat.isSymbolicLink()) fail("LINK_PATH_REJECTED");
      if (entry.isDirectory()) {
        if (entry.name === ".git") fail("SOURCE_MUST_NOT_BE_GIT");
        await walk(absolute, relative + "/");
      } else if (entry.isFile() && isAllowedPackagePath(relative)) files.set(relative, hash(await readFile(absolute)));
      else fail("UNKNOWN_LOCAL_FILE");
    }
  }
  await walk(root);
  return files;
}

// tarを展開ツールへ渡さず、通常ファイルとディレクトリだけを受け入れる。
export function readArchive(compressed) {
  let archive;
  try { archive = gunzipSync(compressed, { maxOutputLength: 64 * 1024 * 1024 }); }
  catch { fail("INVALID_ARCHIVE"); }
  const files = new Map();
  const names = new Set();
  for (let offset = 0; offset + 512 <= archive.length;) {
    const header = archive.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) {
      if (!archive.subarray(offset).every((byte) => byte === 0)) fail("UNSAFE_ARCHIVE");
      return files;
    }
    const field = (start, length) => header.subarray(start, start + length).toString("utf8").replace(/\0.*$/su, "");
    const octal = (value) => { if (!/^[0-7]+$/u.test(value.trim())) fail("INVALID_ARCHIVE"); return Number.parseInt(value.trim(), 8); };
    const checksum = octal(field(148, 8));
    if (header.reduce((sum, byte, index) => sum + (index >= 148 && index < 156 ? 32 : byte), 0) !== checksum) fail("INVALID_ARCHIVE");
    const prefix = field(345, 155);
    const name = (prefix ? prefix + "/" : "") + field(0, 100);
    const type = header[156];
    const size = octal(field(124, 12));
    const normalized = name.replace(/\/$/u, "");
    if (!/^remnifold(?:\/|$)/u.test(normalized) || /[\\:<>"|?*\x00-\x1f]/u.test(normalized) || normalized.split("/").some((part) => !part || part === "." || part === ".." || /[. ]$/u.test(part)) || names.has(normalized.toLowerCase())) fail("UNSAFE_ARCHIVE");
    names.add(normalized.toLowerCase());
    if (![0, 48, 53].includes(type) || !Number.isSafeInteger(size) || offset + 512 + size > archive.length) fail("UNSAFE_ARCHIVE");
    if (type === 53) { if (size !== 0) fail("UNSAFE_ARCHIVE"); }
    else {
      const relative = normalized.slice("remnifold/".length);
      if (!relative || !isAllowedPackagePath(relative) || /[. ](?:\/|$)/u.test(relative) || /(?:^|\/)(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|\/|$)/iu.test(relative)) fail("UNSAFE_ARCHIVE");
      files.set(relative, Buffer.from(archive.subarray(offset + 512, offset + 512 + size)));
    }
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  fail("INVALID_ARCHIVE");
}

async function nativeCodex() {
  const root = path.join(process.env.LOCALAPPDATA || fail("LOCALAPPDATA_MISSING"), "OpenAI", "Codex", "bin");
  await noLinks(root);
  const candidates = [];
  async function walk(directory, depth = 0) {
    if (depth > 3) return;
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name);
      const stat = await lstat(absolute);
      if (stat.isSymbolicLink()) continue;
      if (entry.isDirectory()) await walk(absolute, depth + 1);
      else if (entry.isFile() && entry.name.toLowerCase() === "codex.exe") candidates.push({ absolute, time: stat.mtimeMs });
    }
  }
  await walk(root);
  candidates.sort((a, b) => b.time - a.time);
  return [candidates[0]?.absolute || fail("NATIVE_CODEX_NOT_FOUND")];
}

export async function run(argv = process.argv.slice(2)) {
  const args = {};
  for (let index = 0; index < argv.length; index++) {
    const flag = argv[index];
    if (["--version", "--work-root", "--config"].includes(flag)) {
      if (args[flag] || !argv[index + 1] || argv[index + 1].startsWith("--")) fail("INVALID_ARGUMENTS");
      args[flag] = argv[++index];
    } else if (["--check-published", "--apply", "--verify-only"].includes(flag) && !args[flag]) args[flag] = true;
    else fail("INVALID_ARGUMENTS");
  }
  const version = args["--version"];
  if (!/^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u.test(version || "")) fail("INVALID_VERSION");
  if (["--check-published", "--apply", "--verify-only"].filter((flag) => args[flag]).length !== 1) fail("INVALID_ARGUMENTS");
  const config = JSON.parse(await readFile(args["--config"] ? expand(args["--config"]) : path.join(REPOSITORY_ROOT, "vava.config.json"), "utf8"));
  const settings = config.localPlugin;
  if (!settings || !/^[\w.-]+\/[\w.-]+$/u.test(settings.repository) || settings.id !== "remnifold@personal" || settings.artifact !== "remnifold" || settings.workflow !== "CI") fail("INVALID_CONFIG");
  const source = expand(settings.sourcePath);
  const codexHome = expand(process.env.CODEX_HOME || "~/.codex");
  const work = expand(args["--work-root"] || `D:/Codex/remnifold/plugin-auto-update-${version}`);
  if (process.platform !== "win32" || !/^D:\\/iu.test(work) || inside(REPOSITORY_ROOT, work) || inside(codexHome, work) || !inside(path.resolve("D:/Codex/remnifold"), work)) fail("INVALID_WORK_ROOT");
  await noLinks(work, true);
  await mkdir(work, { recursive: true });
  await access(work, constants.W_OK);
  const commands = args["--config"] ? config.commands || {} : {};
  for (const command of Object.values(commands)) if (!Array.isArray(command) || !command.length || command.some((part) => typeof part !== "string") || /\.(?:cmd|bat)$/iu.test(command[0])) fail("INVALID_COMMAND");
  const gh = commands.gh || ["gh.exe"];
  const codex = commands.codex || await nativeCodex();
  async function call(command, parameters, code, json = true) {
    try {
      const { stdout } = await execute(command[0], [...command.slice(1), ...parameters], { shell: false, windowsHide: true, timeout: 120_000, maxBuffer: 16 * 1024 * 1024, env: { ...process.env, TEMP: work, TMP: work } });
      return json ? JSON.parse(stdout) : stdout;
    } catch { fail(code); }
  }
  const api = (suffix) => call(gh, ["api", `repos/${settings.repository}/${suffix}`], "GITHUB_QUERY_FAILED");
  const ref = await api(`git/ref/heads/release/${version}`);
  const sha = ref.object?.sha;
  if (ref.ref !== `refs/heads/release/${version}` || !/^[a-f0-9]{40}$/u.test(sha || "") || ref.object.type !== "commit") fail("RELEASE_REF_MISMATCH");
  const manifest = await api(`contents/.codex-plugin/plugin.json?ref=${sha}`);
  let published;
  try { published = JSON.parse(Buffer.from(manifest.content, "base64").toString("utf8")); } catch { fail("INVALID_RELEASE_MANIFEST"); }
  if (published.version !== version) fail("RELEASE_VERSION_MISMATCH");
  async function releaseFiles() {
    const tree = await api(`git/trees/${sha}?recursive=1`);
    if (tree.truncated !== false || !Array.isArray(tree.tree)) fail("INCOMPLETE_RELEASE_TREE");
    const expected = new Map();
    const names = new Set();
    for (const entry of tree.tree) {
      if (entry.type !== "blob" || typeof entry.path !== "string" || !isAllowedPackagePath(entry.path)) continue;
      if (!["100644", "100755"].includes(entry.mode) || !/^[a-f0-9]{40}$/u.test(entry.sha || "") || /[\\:<>"|?*\x00-\x1f]/u.test(entry.path) || entry.path.split("/").some((part) => !part || part === "." || part === ".." || /[. ]$/u.test(part)) || /(?:^|\/)(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|\/|$)/iu.test(entry.path) || names.has(entry.path.toLowerCase())) fail("UNSAFE_RELEASE_TREE");
      names.add(entry.path.toLowerCase());
      expected.set(entry.path, entry);
    }
    if (!expected.size || !expected.has(".codex-plugin/plugin.json")) fail("PACKAGE_FILE_SET_MISMATCH");
    return expected;
  }
  async function installedPlugin() {
    const list = await call(codex, ["plugin", "list", "--marketplace", "personal", "--json"], "PLUGIN_LIST_FAILED");
    const matches = list.installed?.filter((item) => item.pluginId === settings.id);
    if (matches?.length !== 1) fail("INSTALLED_PLUGIN_NOT_UNIQUE");
    const plugin = matches[0];
    if (!plugin.installed || !plugin.enabled || plugin.source?.source !== "local" || typeof plugin.source.path !== "string" || expand(plugin.source.path).toLowerCase() !== source.toLowerCase() || !/^[0-9A-Za-z.+-]+$/u.test(plugin.version || "")) fail("INSTALLED_PLUGIN_CONFIG_MISMATCH");
    return plugin;
  }
  const cachePath = (item) => path.join(codexHome, "plugins", "cache", "personal", "remnifold", item.version);
  const sameFiles = (actual, wanted) => actual.size === wanted.size && [...wanted].every(([name, hash]) => actual.get(name) === hash);
  if (args["--verify-only"]) {
    const expected = await releaseFiles();
    const wanted = new Map([...expected].map(([name, entry]) => [name, entry.sha]));
    const installed = await installedPlugin();
    if (installed.version !== version || !sameFiles(await filesAt(source, blobHash), wanted) || !sameFiles(await filesAt(cachePath(installed), blobHash), wanted)) fail("INSTALLED_PLUGIN_CONTENT_MISMATCH");
    return { status: "verified", version, sha, files: wanted.size, workRoot: work };
  }
  const runs = (await api(`actions/runs?head_sha=${sha}&event=push&per_page=100`)).workflow_runs;
  const run = runs?.filter((item) => item.name === settings.workflow && item.head_sha === sha && item.event === "push" && item.head_branch === `release/${version}`).sort((a, b) => b.id - a.id || b.run_attempt - a.run_attempt)[0];
  if (!run || run.status !== "completed" || run.conclusion !== "success") fail("RELEASE_CI_NOT_SUCCESSFUL");
  const artifacts = (await api(`actions/runs/${run.id}/artifacts?per_page=100`)).artifacts;
  const artifact = artifacts?.filter((item) => item.name === settings.artifact).sort((a, b) => b.id - a.id)[0];
  if (!artifact || artifact.expired || !(Date.parse(artifact.expires_at) > Date.now())) fail("RELEASE_ARTIFACT_UNAVAILABLE");
  const evidence = { version, sha, runId: run.id, artifactId: artifact.id, workRoot: work };
  if (args["--check-published"]) return { status: "published", ...evidence };

  const staging = path.join(work, `update-${version}-${randomUUID()}`);
  evidence.stagingPath = staging;
  await mkdir(staging);
  await call(gh, ["run", "download", String(run.id), "--repo", settings.repository, "--name", settings.artifact, "--dir", staging], "ARTIFACT_DOWNLOAD_FAILED", false);
  const downloaded = await readdir(staging);
  if (downloaded.length !== 1 || downloaded[0] !== "remnifold.tar.gz") fail("UNEXPECTED_ARTIFACT_CONTENTS");
  await noLinks(path.join(staging, downloaded[0]));
  const incoming = readArchive(await readFile(path.join(staging, downloaded[0])));
  const expected = await releaseFiles();
  if (incoming.size !== expected.size || !incoming.size) fail("PACKAGE_FILE_SET_MISMATCH");
  for (const [name, bytes] of incoming) {
    const entry = expected.get(name);
    if (!entry || !["100644", "100755"].includes(entry.mode) || entry.sha !== blobHash(bytes)) fail("PACKAGE_HASH_MISMATCH");
  }
  const packageRoot = path.join(staging, "validated");
  await mkdir(packageRoot);
  for (const [name, bytes] of incoming) {
    const target = path.join(packageRoot, name);
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, bytes, { flag: "wx" });
  }
  try { await validatePlugin(packageRoot); } catch { fail("PLUGIN_VALIDATION_FAILED"); }
  const wanted = new Map([...incoming].map(([name, bytes]) => [name, digest(bytes)]));
  const installed = await installedPlugin();
  const local = await filesAt(source);
  const cached = await filesAt(cachePath(installed));
  const equal = (files) => sameFiles(files, wanted);
  for (const [name, hash] of local) {
    if (!wanted.has(name)) fail("OBSOLETE_MANAGED_FILES_REQUIRE_PRUNLET");
    if (hash !== cached.get(name) && hash !== wanted.get(name)) fail("LOCAL_SOURCE_MODIFIED");
  }
  for (const name of cached.keys()) {
    if (!wanted.has(name)) fail("OBSOLETE_MANAGED_FILES_REQUIRE_PRUNLET");
    if (!local.has(name)) fail("LOCAL_SOURCE_MODIFIED");
  }
  if (installed.version === version && equal(local) && equal(cached)) return { status: "already-current", ...evidence, files: wanted.size };
  for (const [name] of incoming) {
    if (local.get(name) === wanted.get(name)) continue;
    const target = path.join(source, name);
    await noLinks(target, true);
    await mkdir(path.dirname(target), { recursive: true });
    await copyFile(path.join(packageRoot, name), target);
  }
  if (!equal(await filesAt(source))) fail("SOURCE_COPY_VERIFICATION_FAILED");
  const added = await call(codex, ["plugin", "add", settings.id, "--json"], "PLUGIN_ADD_FAILED");
  const after = await installedPlugin();
  if (after.version !== version || !equal(await filesAt(source)) || !equal(await filesAt(cachePath(after)))) fail("INSTALLED_PLUGIN_CONTENT_MISMATCH");
  const trust = {};
  for (const key of ["pendingTrust", "hookTrustRequired", "hooksTrusted"]) {
    if (typeof added?.[key] === "boolean") trust[key] = added[key];
  }
  return { status: "updated", ...evidence, files: wanted.size, ...trust };
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try { process.stdout.write(JSON.stringify(await run()) + "\n"); }
  catch (error) {
    process.stdout.write(JSON.stringify({ status: "blocked", code: error.updateCode || "LOCAL_PLUGIN_UPDATE_FAILED" }) + "\n");
    process.exitCode = 1;
  }
}
