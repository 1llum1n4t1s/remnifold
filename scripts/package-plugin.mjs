import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
import { constants as fsConstants } from "node:fs";
import { access, chmod, copyFile, lstat, mkdir, readFile, readdir, rename, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { isNativeSourcePath, nativeHooks, nativeJson, nativeMcp, nativeTarget, sha256, verifyNativeFiles } from "./native-distribution.mjs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_PATH = fileURLToPath(import.meta.url);
export const REPOSITORY_ROOT = path.dirname(path.dirname(SCRIPT_PATH));
export const DEFAULT_OUTPUT = path.join(REPOSITORY_ROOT, "dist", "remnifold");

const REQUIRED_FILES = new Set([
  ".codex-plugin/plugin.json",
  ".mcp.json",
  "README.md",
  "hooks/hooks.json",
  "scripts/hook-launcher.mjs",
  "scripts/mcp-launcher.mjs",
  "scripts/setup-client.ps1",
  "scripts/setup-server.mjs",
  "scripts/setup-server.ps1",
  "server/package.json",
  "server/pnpm-lock.yaml",
  "server/pnpm-workspace.yaml",
  "server/tsconfig.json",
  "server/wrangler.example.jsonc",
]);

const OPTIONAL_FILES = new Set(["CHANGELOG.md", "LICENSE", "LICENSE.md"]);
const DIRECTORY_RULES = [
  { prefix: "client/", extension: /\.(?:js|mjs)$/u },
  { prefix: "server/migrations/", extension: /\.sql$/u },
  { prefix: "server/src/", extension: /\.ts$/u },
  { prefix: "skills/", extension: /\.(?:md|ya?ml)$/u },
];

function toPackagePath(value) {
  return value.split(path.sep).join("/");
}

export function isAllowedPackagePath(relativePath) {
  const candidate = relativePath.replaceAll("\\", "/");
  if (["native-build.json", "bin/remnifold", "bin/remnifold.exe"].includes(candidate)) return true;
  if (REQUIRED_FILES.has(candidate) || OPTIONAL_FILES.has(candidate)) return true;
  if (/^scripts\/setup-[a-z0-9-]+\.(?:mjs|ps1)$/u.test(candidate)) return true;
  if (/(?:^|\/)node_modules(?:\/|$)/u.test(candidate)) return false;
  if (/(?:^|\/)(?:\.dev\.vars(?:\..*)?|\.env(?:\..*)?|wrangler\.jsonc)$/u.test(candidate)) return false;
  if (/(?:^|\/)(?:tests?|__tests__)(?:\/|$)/u.test(candidate) || /\.test\.[^.]+$/u.test(candidate)) return false;
  return DIRECTORY_RULES.some(({ prefix, extension }) => candidate.startsWith(prefix) && extension.test(candidate));
}

async function collectDirectoryFiles(root, relativeDirectory, files) {
  const absoluteDirectory = path.join(root, relativeDirectory);
  const entries = await readdir(absoluteDirectory, { withFileTypes: true });
  for (const entry of entries) {
    const relativePath = toPackagePath(path.join(relativeDirectory, entry.name));
    const absolutePath = path.join(root, relativePath);
    const stats = await lstat(absolutePath);
    if (stats.isSymbolicLink()) throw new Error(`Symbolic links cannot be packaged: ${relativePath}`);
    if (entry.isDirectory()) {
      await collectDirectoryFiles(root, relativePath, files);
    } else if (entry.isFile() && isAllowedPackagePath(relativePath)) {
      files.add(relativePath);
    }
  }
}

export async function collectPackageFiles(root = REPOSITORY_ROOT) {
  const resolvedRoot = path.resolve(root);
  const files = new Set();

  for (const relativePath of REQUIRED_FILES) {
    const absolutePath = path.join(resolvedRoot, relativePath);
    await access(absolutePath, fsConstants.R_OK).catch(() => {
      throw new Error(`Required package file is missing: ${relativePath}`);
    });
    const stats = await lstat(absolutePath);
    if (!stats.isFile() || stats.isSymbolicLink()) {
      throw new Error(`Required package path must be a regular file: ${relativePath}`);
    }
    files.add(relativePath);
  }

  for (const relativePath of OPTIONAL_FILES) {
    const absolutePath = path.join(resolvedRoot, relativePath);
    try {
      const stats = await lstat(absolutePath);
      if (!stats.isFile() || stats.isSymbolicLink()) {
        throw new Error(`Optional package path must be a regular file: ${relativePath}`);
      }
      files.add(relativePath);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }

  for (const relativeDirectory of ["client", "scripts", "server/migrations", "server/src", "skills"]) {
    await collectDirectoryFiles(resolvedRoot, relativeDirectory, files);
  }

  return [...files].sort((left, right) => left.localeCompare(right, "en"));
}

function assertSafeOutput(root, output) {
  const distRoot = path.resolve(root, "dist");
  const resolvedOutput = path.resolve(output);
  const relative = path.relative(distRoot, resolvedOutput);
  if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`Package output must be a child of ${distRoot}`);
  }
  return resolvedOutput;
}

export async function buildPackage({ root = REPOSITORY_ROOT, output = DEFAULT_OUTPUT } = {}) {
  const resolvedRoot = path.resolve(root);
  const resolvedOutput = assertSafeOutput(resolvedRoot, output);
  const staging = `${resolvedOutput}.tmp-${process.pid}`;
  const files = await collectPackageFiles(resolvedRoot);

  await mkdir(path.resolve(resolvedRoot, "dist"), { recursive: true });
  await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(staging, { allowedRoot: path.resolve(resolvedRoot, "dist"), recursive: true, force: true });
  try {
    const createdDirectories = new Set();
    for (const relativePath of files) {
      const destination = path.join(staging, relativePath);
      const directory = path.dirname(destination);
      if (!createdDirectories.has(directory)) {
        await mkdir(directory, { recursive: true });
        createdDirectories.add(directory);
      }
      await copyFile(path.join(resolvedRoot, relativePath), destination);
    }
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(resolvedOutput, { allowedRoot: path.resolve(resolvedRoot, "dist"), recursive: true, force: true });
    await rename(staging, resolvedOutput);
  } catch (error) {
    await ((target, { allowedRoot, ...options }) => cleanupFs.promises.rm(assertCleanupPath(target, allowedRoot), options))(staging, { allowedRoot: path.resolve(resolvedRoot, "dist"), recursive: true, force: true });
    throw error;
  }

  return { output: resolvedOutput, files };
}

// 配布物は実行OSごとに作る。開発用JSをインストール時の実行経路へ含めない。
export async function buildNativePackage({ root = REPOSITORY_ROOT, output, target, binary, sourceCommit } = {}) {
  const info = nativeTarget(target);
  const resolvedRoot = path.resolve(root);
  const resolvedOutput = assertSafeOutput(resolvedRoot, output ?? path.join(resolvedRoot, "dist", target, "remnifold"));
  const commit = sourceCommit ?? execFileSync("git", ["rev-parse", "HEAD"], { cwd: resolvedRoot, encoding: "utf8" }).trim();
  if (!/^[a-f0-9]{40}$/u.test(commit)) throw new Error("Native packaging requires an exact source commit");
  const binaryPath = path.resolve(binary ?? path.join(resolvedRoot, "native", "target", target, "release", path.basename(info.executable)));
  const stats = await lstat(binaryPath);
  if (!stats.isFile() || stats.isSymbolicLink() || !stats.size) throw new Error("Native binary must be a nonempty regular file");
  const files = new Map();
  for (const name of (await collectPackageFiles(resolvedRoot)).filter(isNativeSourcePath)) files.set(name, await readFile(path.join(resolvedRoot, name)));
  files.set(info.executable, await readFile(binaryPath));
  files.set(".mcp.json", Buffer.from(nativeJson(nativeMcp(target))));
  files.set("hooks/hooks.json", Buffer.from(nativeJson(nativeHooks(target, JSON.parse(await readFile(path.join(resolvedRoot, "hooks/hooks.json"), "utf8"))))));
  const version = JSON.parse(files.get(".codex-plugin/plugin.json").toString("utf8")).version;
  files.set("native-build.json", Buffer.from(nativeJson({ schema: 1, target, sourceCommit: commit, version,
    files: Object.fromEntries([...files].sort(([a], [b]) => a.localeCompare(b, "en")).map(([name, bytes]) => [name, sha256(bytes)])) })));
  verifyNativeFiles(files, { target, sourceCommit: commit, version });
  // 既存出力を削除・上書きしない。別の出力先か明示的な清掃を利用する。
  await mkdir(path.dirname(resolvedOutput), { recursive: true });
  await mkdir(resolvedOutput);
  for (const [name, bytes] of files) {
    const destination = path.join(resolvedOutput, name);
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, bytes, { flag: "wx" });
  }
  await chmod(path.join(resolvedOutput, info.executable), 0o755);
  return { output: resolvedOutput, files: [...files.keys()], target, sourceCommit: commit };
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    const result = await buildPackage();
    process.stdout.write(`Built ${result.files.length} files in ${result.output}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
