import * as cleanupFs from "node:fs";
import { assertCleanupPath } from "./lib/cleanup.mjs";
import { constants as fsConstants } from "node:fs";
import { access, copyFile, lstat, mkdir, readdir, rename } from "node:fs/promises";
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

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    const result = await buildPackage();
    process.stdout.write(`Built ${result.files.length} files in ${result.output}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
