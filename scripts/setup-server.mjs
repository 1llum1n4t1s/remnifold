#!/usr/bin/env node

import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { truncateUtf16 } from "../client/unicode-text.mjs";

const EXPECTED_VECTOR_DIMENSIONS = 1_024;
const EXPECTED_VECTOR_METRIC = "cosine";
const DEFAULT_NAME = "remnifold";
const LEGACY_RESOURCE_NAMES = ["kagayoi-memory", "cloudflare-supermemory"];
const LOCATIONS = new Set(["weur", "eeur", "apac", "oc", "wnam", "enam"]);
const JURISDICTIONS = new Set(["eu", "fedramp", "us"]);

class SetupError extends Error {}

function usage() {
  return `Remnifold server setup

Usage:
  node scripts/setup-server.mjs [options]

The default mode performs authenticated, read-only discovery and prints a plan.
Pass --apply to create or reuse resources, write server/wrangler.jsonc, apply
migrations, set the Worker secret when supplied, and deploy.
Without explicit names, reuse priority is remnifold, kagayoi-memory, then
cloudflare-supermemory. Only create remnifold when no existing resource matches.

Options:
  --apply                       Perform remote and local changes
  --worker-name <name>          Worker name (default: ${DEFAULT_NAME})
  --database-name <name>        D1 database name (default: ${DEFAULT_NAME})
  --vector-index-name <name>    Vectorize index name (default: ${DEFAULT_NAME})
  --location <hint>             D1 location: weur, eeur, apac, oc, wnam, enam
  --jurisdiction <value>        D1 jurisdiction: eu, fedramp, us
  --api-key-env <name>          Environment variable containing the API key
                                (default: REMNIFOLD_API_KEY)
  --base-url <https-url>        Endpoint to verify after deployment
  --profile <name>              Wrangler authentication profile
  --help                        Show this help

Examples:
  node scripts/setup-server.mjs
  $env:REMNIFOLD_API_KEY = '<secret>'
  node scripts/setup-server.mjs --apply --location apac
`;
}

function valueAfter(args, index, option) {
  const value = args[index + 1];
  if (!value || value.startsWith("--")) throw new SetupError(`${option} requires a value.`);
  return value;
}

export function parseArgs(args) {
  const options = {
    apply: false,
    workerName: DEFAULT_NAME,
    databaseName: DEFAULT_NAME,
    vectorIndexName: DEFAULT_NAME,
    workerNameExplicit: false,
    databaseNameExplicit: false,
    vectorIndexNameExplicit: false,
    apiKeyEnv: "REMNIFOLD_API_KEY",
  };

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--apply") options.apply = true;
    else if (argument === "--help" || argument === "-h") options.help = true;
    else if (argument === "--worker-name") {
      options.workerName = valueAfter(args, index++, argument);
      options.workerNameExplicit = true;
    } else if (argument === "--database-name") {
      options.databaseName = valueAfter(args, index++, argument);
      options.databaseNameExplicit = true;
    } else if (argument === "--vector-index-name") {
      options.vectorIndexName = valueAfter(args, index++, argument);
      options.vectorIndexNameExplicit = true;
    }
    else if (argument === "--location") options.location = valueAfter(args, index++, argument);
    else if (argument === "--jurisdiction") options.jurisdiction = valueAfter(args, index++, argument);
    else if (argument === "--api-key-env") options.apiKeyEnv = valueAfter(args, index++, argument);
    else if (argument === "--base-url") options.baseUrl = valueAfter(args, index++, argument);
    else if (argument === "--profile") options.profile = valueAfter(args, index++, argument);
    else throw new SetupError(`Unknown option: ${argument}`);
  }

  for (const [option, name] of [
    ["--worker-name", options.workerName],
    ["--database-name", options.databaseName],
    ["--vector-index-name", options.vectorIndexName],
  ]) {
    if (!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/u.test(name)) {
      throw new SetupError(`${option} must contain 1-63 lowercase letters, digits, or hyphens.`);
    }
  }
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/u.test(options.apiKeyEnv)) {
    throw new SetupError("--api-key-env must be a valid environment variable name.");
  }
  if (options.location && !LOCATIONS.has(options.location)) {
    throw new SetupError(`Unsupported D1 location: ${options.location}`);
  }
  if (options.jurisdiction && !JURISDICTIONS.has(options.jurisdiction)) {
    throw new SetupError(`Unsupported D1 jurisdiction: ${options.jurisdiction}`);
  }
  if (options.location && options.jurisdiction) {
    throw new SetupError("Use either --location or --jurisdiction, not both.");
  }
  if (options.baseUrl) {
    let url;
    try {
      url = new URL(options.baseUrl);
    } catch {
      throw new SetupError("--base-url must be an absolute URL.");
    }
    const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
    if (url.username || url.password || (url.protocol !== "https:" && !(local && url.protocol === "http:"))) {
      throw new SetupError("--base-url must use HTTPS (HTTP is accepted only for localhost).");
    }
    options.baseUrl = url.href.replace(/\/$/u, "");
  }
  return options;
}

function stripJsonComments(text) {
  let result = "";
  let state = "normal";
  for (let index = 0; index < text.length; index += 1) {
    const current = text[index];
    const next = text[index + 1];
    if (state === "string") {
      result += current;
      if (current === "\\") {
        result += next ?? "";
        index += 1;
      } else if (current === '"') state = "normal";
    } else if (state === "line") {
      if (current === "\n" || current === "\r") {
        result += current;
        state = "normal";
      } else result += " ";
    } else if (state === "block") {
      if (current === "*" && next === "/") {
        result += "  ";
        index += 1;
        state = "normal";
      } else result += current === "\n" || current === "\r" ? current : " ";
    } else if (current === '"') {
      result += current;
      state = "string";
    } else if (current === "/" && next === "/") {
      result += "  ";
      index += 1;
      state = "line";
    } else if (current === "/" && next === "*") {
      result += "  ";
      index += 1;
      state = "block";
    } else result += current;
  }
  return result.replace(/,(\s*[}\]])/gu, "$1");
}

function parseJsonc(text, source) {
  try {
    return JSON.parse(stripJsonComments(text.replace(/^\uFEFF/u, "")));
  } catch (error) {
    throw new SetupError(`Could not parse ${source}: ${error.message}`);
  }
}

function parseCommandJson(output, label) {
  const clean = output.replace(/\u001B\[[0-?]*[ -/]*[@-~]/gu, "").trim();
  const starts = [0];
  for (let index = 0; index < clean.length; index += 1) {
    if (clean[index] === "\n") starts.push(index + 1);
  }
  for (const start of starts) {
    const candidate = clean.slice(start).trim();
    if (!candidate.startsWith("[") && !candidate.startsWith("{")) continue;
    try {
      const value = JSON.parse(candidate);
      return value && typeof value === "object" && "result" in value ? value.result : value;
    } catch {
      // Wrangler can print a warning before JSON; try the next line.
    }
  }
  throw new SetupError(`Wrangler returned invalid JSON while ${label}.`);
}

function redact(text, secret) {
  if (!text) return "";
  return secret ? text.replaceAll(secret, "[redacted]") : text;
}

async function defaultRunner({ wranglerBin, serverDir, args, input }) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [wranglerBin, ...args], {
      cwd: serverDir,
      env: { ...process.env, CI: "true", NO_COLOR: "1" },
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", reject);
    child.on("close", (code) => resolve({ code: code ?? 1, stdout, stderr }));
    if (input !== undefined) child.stdin.end(input);
    else child.stdin.end();
  });
}

async function callWrangler(context, args, label, { input, allowFailure = false } = {}) {
  const withProfile = context.options.profile ? [...args, "--profile", context.options.profile] : args;
  const result = await context.runner({
    wranglerBin: context.wranglerBin,
    serverDir: context.serverDir,
    args: withProfile,
    input,
    label,
  });
  if (result.code !== 0 && !allowFailure) {
    const details = label === "setting MEMORY_API_KEY"
      ? ""
      : truncateUtf16(redact((result.stderr || result.stdout).trim(), context.apiKey), 2_000);
    throw new SetupError(`Wrangler failed while ${label}.${details ? `\n${details}` : ""}`);
  }
  return result;
}

function findD1(databases, name) {
  if (!Array.isArray(databases)) throw new SetupError("Wrangler returned an invalid D1 database list.");
  const match = databases.find((database) => database?.name === name);
  if (!match) return undefined;
  const id = match.uuid ?? match.id ?? match.database_id;
  if (typeof id !== "string" || id.length === 0) {
    throw new SetupError(`D1 database ${name} did not include an ID.`);
  }
  return { name, id };
}

function findVector(indexes, name) {
  if (!Array.isArray(indexes)) throw new SetupError("Wrangler returned an invalid Vectorize index list.");
  return indexes.find((index) => index?.name === name);
}

function vectorSettings(index) {
  const config = index?.config && typeof index.config === "object" ? index.config : index;
  return { dimensions: Number(config?.dimensions), metric: String(config?.metric ?? "").toLowerCase() };
}

async function listD1(context) {
  const result = await callWrangler(context, ["d1", "list", "--json"], "listing D1 databases");
  return parseCommandJson(result.stdout, "listing D1 databases");
}

async function listVectors(context) {
  const result = await callWrangler(context, ["vectorize", "list", "--json"], "listing Vectorize indexes");
  return parseCommandJson(result.stdout, "listing Vectorize indexes");
}

async function inspectVector(context, name) {
  const result = await callWrangler(context, ["vectorize", "get", name, "--json"], `inspecting Vectorize index ${name}`);
  const value = parseCommandJson(result.stdout, `inspecting Vectorize index ${name}`);
  const settings = vectorSettings(value);
  if (settings.dimensions !== EXPECTED_VECTOR_DIMENSIONS || settings.metric !== EXPECTED_VECTOR_METRIC) {
    throw new SetupError(
      `Vectorize index ${name} uses ${settings.dimensions || "unknown"} dimensions and ` +
      `${settings.metric || "an unknown metric"}; expected ${EXPECTED_VECTOR_DIMENSIONS} and ${EXPECTED_VECTOR_METRIC}.`,
    );
  }
}

async function inspectSecret(context) {
  const result = await callWrangler(
    context,
    ["secret", "list", "--name", context.options.workerName, "--format", "json"],
    "checking Worker secrets",
    { allowFailure: true },
  );
  if (result.code !== 0) return { verifiable: false, present: false };
  const secrets = parseCommandJson(result.stdout, "checking Worker secrets");
  if (!Array.isArray(secrets)) throw new SetupError("Wrangler returned an invalid Worker secret list.");
  return { verifiable: true, present: secrets.some((secret) => secret?.name === "MEMORY_API_KEY") };
}

async function retryDiscovery(discover, description, sleep) {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const value = await discover();
    if (value) return value;
    if (attempt < 3) await sleep(250 * (attempt + 1));
  }
  throw new SetupError(`${description} was not visible after creation. Rerun the same command to resume.`);
}

async function buildConfig(serverDir, options, d1) {
  const templatePath = path.join(serverDir, "wrangler.example.jsonc");
  const configPath = path.join(serverDir, "wrangler.jsonc");
  const template = parseJsonc(await readFile(templatePath, "utf8"), templatePath);
  let config = template;
  try {
    config = parseJsonc(await readFile(configPath, "utf8"), configPath);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }

  config.$schema ??= template.$schema ?? "./node_modules/wrangler/config-schema.json";
  config.name = options.workerName;
  config.main = "src/index.ts";
  config.compatibility_date = template.compatibility_date;
  config.workers_dev ??= template.workers_dev ?? true;
  config.preview_urls ??= template.preview_urls ?? false;
  const requiredCrons = Array.isArray(template.triggers?.crons) ? template.triggers.crons : [];
  const existingCrons = Array.isArray(config.triggers?.crons) ? config.triggers.crons : [];
  config.triggers = {
    ...(config.triggers ?? {}),
    crons: [...new Set([...existingCrons, ...requiredCrons])],
  };
  config.ai = { ...(config.ai ?? {}), binding: "AI" };
  config.vars = { ...(config.vars ?? {}), AI_ENRICHMENT_MODE: config.vars?.AI_ENRICHMENT_MODE ?? "on" };
  config.vectorize = [
    ...(Array.isArray(config.vectorize)
      ? config.vectorize.filter((binding) => binding?.binding !== "MEMORY_VECTORS")
      : []),
    { binding: "MEMORY_VECTORS", index_name: options.vectorIndexName },
  ];
  config.d1_databases = [
    ...(Array.isArray(config.d1_databases)
      ? config.d1_databases.filter((binding) => binding?.binding !== "DB")
      : []),
    {
      binding: "DB",
      database_name: options.databaseName,
      database_id: d1.id,
      migrations_dir: "migrations",
    },
  ];
  delete config.secrets;
  return { configPath, content: `${JSON.stringify(config, null, 2)}\n` };
}

function deployUrl(output) {
  const urls = output.match(/https:\/\/[^\s)\]]+/gu) ?? [];
  return urls.find((url) => url.includes(".workers.dev"))?.replace(/[.,;]+$/u, "");
}

async function verifyEndpoint(context, baseUrl) {
  const request = async (route, headers = {}) => {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 15_000);
    try {
      return await context.fetchImpl(`${baseUrl}${route}`, { headers, signal: controller.signal });
    } finally {
      clearTimeout(timeout);
    }
  };
  let health;
  try {
    health = await request("/health");
  } catch (error) {
    throw new SetupError(`Deployment completed, but ${baseUrl}/health could not be reached: ${error.message}`);
  }
  if (!health.ok) throw new SetupError(`Deployment health check returned HTTP ${health.status}.`);
  if (context.apiKey) {
    const session = await request("/v3/session", { Authorization: `Bearer ${context.apiKey}` });
    if (!session.ok) throw new SetupError(`Authenticated session check returned HTTP ${session.status}.`);
  }
}

export async function runSetup(args, dependencies = {}) {
  const options = parseArgs(args);
  const output = dependencies.output ?? ((line) => process.stdout.write(`${line}\n`));
  if (options.help) {
    output(usage());
    return { mode: "help" };
  }

  const currentFile = fileURLToPath(import.meta.url);
  const serverDir = dependencies.serverDir ?? path.resolve(path.dirname(currentFile), "..", "server");
  const wranglerBin = dependencies.wranglerBin ?? path.join(serverDir, "node_modules", "wrangler", "bin", "wrangler.js");
  const nodeMajor = Number((dependencies.nodeVersion ?? process.versions.node).split(".")[0]);
  if (!Number.isInteger(nodeMajor) || nodeMajor < 24) {
    throw new SetupError("Node.js 24 or newer is required.");
  }
  for (const required of [
    path.join(serverDir, "package.json"),
    path.join(serverDir, "wrangler.example.jsonc"),
    path.join(serverDir, "migrations"),
    wranglerBin,
  ]) {
    try {
      await access(required, fsConstants.R_OK);
    } catch {
      const dependencyHint = required === wranglerBin
        ? " Run `corepack pnpm -C server install --frozen-lockfile` first."
        : "";
      throw new SetupError(`Required setup input is missing: ${required}.${dependencyHint}`);
    }
  }

  const environment = dependencies.env ?? process.env;
  const apiKey = [
    environment[options.apiKeyEnv],
    ...(options.apiKeyEnv === "REMNIFOLD_API_KEY" ? [environment.KAGAYOI_MEMORY_API_KEY,
      environment.SUPERMEMORY_CODEX_API_KEY, environment.CLOUDFLARE_MEMORY_API_KEY] : []),
  ].find((value) => typeof value === "string" && value.trim())?.trim();
  const context = {
    options,
    serverDir,
    wranglerBin,
    apiKey,
    runner: dependencies.runner ?? defaultRunner,
    fetchImpl: dependencies.fetchImpl ?? globalThis.fetch,
  };
  const sleep = dependencies.sleep ?? ((milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)));

  output(options.apply ? "Mode: apply" : "Mode: dry run (read-only; pass --apply to make changes)");
  await callWrangler(context, ["whoami", "--json"], "checking Cloudflare authentication");
  output("Cloudflare authentication: ready");

  const databases = await listD1(context);
  const vectors = await listVectors(context);
  let d1 = findD1(databases, options.databaseName);
  let vector = findVector(vectors, options.vectorIndexName);
  // 1.x利用者のD1とVectorizeは名称変更後も再利用し、保存済み記憶を分断しない。
  if (!d1 && !options.databaseNameExplicit && options.databaseName === DEFAULT_NAME) {
    for (const name of LEGACY_RESOURCE_NAMES) {
      d1 = findD1(databases, name);
      if (d1) { options.databaseName = name; break; }
    }
  }
  if (!vector && !options.vectorIndexNameExplicit && options.vectorIndexName === DEFAULT_NAME) {
    for (const name of LEGACY_RESOURCE_NAMES) {
      vector = findVector(vectors, name);
      if (vector) { options.vectorIndexName = name; break; }
    }
  }
  if (vector) await inspectVector(context, options.vectorIndexName);
  let secret = await inspectSecret(context);
  if (!secret.verifiable && !options.workerNameExplicit && options.workerName === DEFAULT_NAME) {
    const currentWorkerName = options.workerName;
    for (const name of LEGACY_RESOURCE_NAMES) {
      options.workerName = name;
      const legacySecret = await inspectSecret(context);
      if (legacySecret.verifiable) { secret = legacySecret; break; }
    }
    if (!secret.verifiable) options.workerName = currentWorkerName;
  }

  output(`D1 database: ${d1 ? "reuse" : "create"} ${options.databaseName}`);
  output(`Vectorize index: ${vector ? "reuse" : "create"} ${options.vectorIndexName} (${EXPECTED_VECTOR_DIMENSIONS}, ${EXPECTED_VECTOR_METRIC})`);
  if (apiKey) output("Worker secret: set from the configured environment variable");
  else if (secret.present) output("Worker secret: preserve existing MEMORY_API_KEY");
  else output(`Worker secret: ${secret.verifiable ? "missing" : "not verifiable"}; set ${options.apiKeyEnv} before --apply`);
  output("Then: write ignored Wrangler config, apply remote migrations, deploy Worker, verify the endpoint when discoverable.");

  if (!options.apply) return { mode: "dry-run", d1: Boolean(d1), vector: Boolean(vector), secret };
  if (!apiKey && !secret.present) {
    throw new SetupError(
      `MEMORY_API_KEY is not available on the Worker. Set ${options.apiKeyEnv} in this process and rerun with --apply.`,
    );
  }

  if (!d1) {
    const createArgs = ["d1", "create", options.databaseName];
    if (options.location) createArgs.push("--location", options.location);
    if (options.jurisdiction) createArgs.push("--jurisdiction", options.jurisdiction);
    const created = await callWrangler(context, createArgs, `creating D1 database ${options.databaseName}`, { allowFailure: true });
    d1 = await retryDiscovery(
      async () => findD1(await listD1(context), options.databaseName),
      `D1 database ${options.databaseName}`,
      sleep,
    );
    if (created.code !== 0) output("D1 create reported a conflict or interruption; the discovered database will be reused.");
  }

  if (!vector) {
    const created = await callWrangler(
      context,
      [
        "vectorize", "create", options.vectorIndexName,
        "--dimensions", String(EXPECTED_VECTOR_DIMENSIONS),
        "--metric", EXPECTED_VECTOR_METRIC,
        "--json",
      ],
      `creating Vectorize index ${options.vectorIndexName}`,
      { allowFailure: true },
    );
    vector = await retryDiscovery(
      async () => findVector(await listVectors(context), options.vectorIndexName),
      `Vectorize index ${options.vectorIndexName}`,
      sleep,
    );
    if (created.code !== 0) output("Vectorize create reported a conflict or interruption; the discovered index will be reused.");
    await inspectVector(context, options.vectorIndexName);
  }

  const generated = await buildConfig(serverDir, options, d1);
  await writeFile(generated.configPath, generated.content, { encoding: "utf8", mode: 0o600 });
  output(`Wrangler config: wrote ${generated.configPath}`);

  const configArgument = path.basename(generated.configPath);
  if (apiKey) {
    await callWrangler(
      context,
      ["secret", "put", "MEMORY_API_KEY", "--name", options.workerName, "--config", configArgument],
      "setting MEMORY_API_KEY",
      { input: `${apiKey}\n` },
    );
    output("Worker secret: set (value not displayed)");
  }

  await callWrangler(
    context,
    ["d1", "migrations", "apply", "DB", "--remote", "--config", configArgument],
    "applying D1 migrations",
  );
  output("D1 migrations: applied");

  const deployed = await callWrangler(
    context,
    ["deploy", "--minify", "--config", configArgument],
    "deploying the Worker",
  );
  output("Worker deployment: complete");

  const endpoint = options.baseUrl ?? deployUrl(`${deployed.stdout}\n${deployed.stderr}`);
  if (endpoint) {
    await verifyEndpoint(context, endpoint);
    output(`Endpoint verification: healthy at ${endpoint}${apiKey ? " (authenticated session verified)" : ""}`);
  } else {
    output("Endpoint verification: skipped because Wrangler did not report a workers.dev URL; rerun with --base-url to verify a custom domain.");
  }
  return { mode: "apply", endpoint, configPath: generated.configPath };
}

async function main() {
  try {
    await runSetup(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`Error: ${error.message}\n`);
    process.exitCode = 1;
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) await main();
