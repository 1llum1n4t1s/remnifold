#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  createReadStream,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { isIP } from "node:net";
import { basename, dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { buildMemoryIndex } from "./memory-index.mjs";
import { truncateUtf16 } from "./unicode-text.mjs";
import { projectRequestText, requestProjectionEdits, projectTextSlice } from "./request-projection.mjs";

const IMPORT_VERSION = 2;
const DEFAULT_MAX_DOCUMENT_CHARS = 120_000;
const MAX_DOCUMENT_CHARS = 180_000;
const DEFAULT_LIST_LIMIT = 50;
const RECENT_TRANSCRIPT_WINDOW_MS = 10 * 60 * 1000;
const REDACTED = "[REDACTED]";
const RENAMED_REPOSITORY_IDENTITIES = new Map([
  ["github.com/1llum1n4t1s/remnifold", {
    identity: "github.com/1llum1n4t1s/cloudflare-supermemory",
    projectName: "cloudflare_supermemory",
    displayProjectName: "kagayoi_memory", // 既存Kagayoi名で保存済みのproject provenanceを維持する。
  }],
  ["github.com/1llum1n4t1s/kagayoi-memory", {
    identity: "github.com/1llum1n4t1s/cloudflare-supermemory",
    projectName: "cloudflare_supermemory",
  }],
]);
export const DEFAULT_MEMORY_CONTAINER = "memories";
export const CONFIG_FILE_NAME = "remnifold.json";
export const LEGACY_CONFIG_FILE_NAMES = ["kagayoi-memory.json", "supermemory.json"];
export const LEGACY_CONFIG_FILE_NAME = "supermemory.json";

function fail(message) {
  throw new Error(message);
}

function parseArguments(argv) {
  const options = {
    apply: false,
    codexHome: join(homedir(), ".codex"),
    containerTag: "",
    rootOnly: false,
    includeRecent: false,
    limit: 0,
    maxDocumentChars: DEFAULT_MAX_DOCUMENT_CHARS,
    statePath: "",
    sessionId: "",
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--apply") {
      options.apply = true;
    } else if (argument === "--root-only") {
      options.rootOnly = true;
    } else if (argument === "--include-recent") {
      options.includeRecent = true;
    } else if (["--codex-home", "--container-tag", "--limit", "--max-document-chars", "--state-path", "--session-id"].includes(argument)) {
      const value = argv[index + 1];
      if (value === undefined || value.startsWith("--")) fail(`${argument} requires a value.`);
      index += 1;
      if (argument === "--codex-home") options.codexHome = value;
      if (argument === "--container-tag") options.containerTag = value;
      if (argument === "--limit") options.limit = Number(value);
      if (argument === "--max-document-chars") options.maxDocumentChars = Number(value);
      if (argument === "--state-path") options.statePath = value;
      if (argument === "--session-id") options.sessionId = value;
    } else {
      fail(`Unknown argument: ${argument}`);
    }
  }

  if (!Number.isInteger(options.limit) || options.limit < 0) fail("--limit must be a non-negative integer.");
  if (!Number.isInteger(options.maxDocumentChars) || options.maxDocumentChars < 10_000 || options.maxDocumentChars > MAX_DOCUMENT_CHARS) {
    fail(`--max-document-chars must be an integer from 10000 through ${MAX_DOCUMENT_CHARS}.`);
  }

  options.codexHome = resolve(options.codexHome);
  if (options.statePath) {
    options.statePath = resolve(options.statePath);
  } else {
    options.statePath = join(homedir(), ".remnifold", `history-import-v${IMPORT_VERSION}.json`);
    // 旧ファイルは読取専用の互換入口とし、新規保存はRemnifoldの状態へ集約する。
    options.legacyStatePaths = [".kagayoi-memory", ".codex-supermemory"]
      .map((name) => join(homedir(), name, `history-import-v${IMPORT_VERSION}.json`));
  }
  return options;
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function git(args, cwd) {
  try {
    return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return "";
  }
}

function normalizeRemote(remote) {
  const raw = remote.trim();
  if (!raw) return "";

  let normalized = raw;
  if (/^[a-z][a-z\d+.-]*:\/\//i.test(raw)) {
    try {
      const parsed = new URL(raw);
      normalized = `${parsed.hostname.toLowerCase()}${parsed.port ? `:${parsed.port}` : ""}/${parsed.pathname.replace(/^\/+/, "")}`;
    } catch {
      normalized = raw;
    }
  } else {
    const scp = raw.match(/^(?:[^@/]+@)?([^:]+):(.+)$/);
    normalized = scp ? `${scp[1].toLowerCase()}/${scp[2]}` : `file:${resolve(raw)}`;
  }

  return normalized
    .replace(/[?#].*$/, "")
    .replace(/\/+$/, "")
    .replace(/\.git$/i, "")
    .replace(/\/{2,}/g, "/")
    .toLowerCase();
}

function getProjectContext(cwd = process.cwd()) {
  const root = git(["rev-parse", "--show-toplevel"], cwd) || resolve(cwd);
  const remote = normalizeRemote(git(["remote", "get-url", "origin"], root));
  const repoName = basename(remote || root).replace(/\.git$/i, "") || basename(root) || "unknown";
  const projectName = repoName.toLowerCase().replace(/[^a-z0-9]/g, "_").replace(/_+/g, "_").replace(/^_|_$/g, "").slice(0, 72) || "unknown";
  // 製品リポジトリの改名では、既存文書・fact関係のprovenance IDだけを1.xと同一に保つ。
  const legacy = RENAMED_REPOSITORY_IDENTITIES.get(remote);
  const identity = legacy?.identity || remote || `path:${root}`;
  const provenanceProjectName = legacy?.projectName || projectName;
  return {
    projectName: legacy?.displayProjectName || projectName,
    containerTag: `repo_${provenanceProjectName}__${sha256(identity).slice(0, 16)}`,
  };
}

function loadConfig(codexHome) {
  const environmentBaseUrl = [process.env.REMNIFOLD_API_URL, process.env.KAGAYOI_MEMORY_API_URL, process.env.SUPERMEMORY_API_URL]
    .find((value) => typeof value === "string" && value.trim())?.trim() || "";
  const environmentApiKey = [process.env.REMNIFOLD_API_KEY, process.env.KAGAYOI_MEMORY_API_KEY, process.env.SUPERMEMORY_CODEX_API_KEY, process.env.CLOUDFLARE_MEMORY_API_KEY]
    .find((value) => typeof value === "string" && value.trim())?.trim() || "";
  const currentConfigPath = join(codexHome, CONFIG_FILE_NAME);
  const configPath = [currentConfigPath, ...LEGACY_CONFIG_FILE_NAMES.map((name) => join(codexHome, name))].find(existsSync);
  let config = {};
  if (configPath) {
    try {
      config = JSON.parse(readFileSync(configPath, "utf8"));
      if (!config || typeof config !== "object" || Array.isArray(config)) throw new Error("invalid configuration");
    } catch {
      if (!environmentBaseUrl || !environmentApiKey) fail(`${configPath} must contain valid JSON.`);
      config = {};
    }
  }

  // 環境変数は項目ごとにファイル値を上書きし、検索設定を保ったまま接続だけを差し替える。
  const baseUrl = environmentBaseUrl || (typeof config.baseUrl === "string" ? config.baseUrl.trim() : "");
  const apiKey = environmentApiKey || (typeof config.apiKey === "string" ? config.apiKey.trim() : "");
  if (!baseUrl || !apiKey) {
    fail("Remnifold configuration requires baseUrl and apiKey (file values may be overridden by REMNIFOLD_API_URL and REMNIFOLD_API_KEY). Values are never printed.");
  }

  const normalizedBaseUrl = normalizeEndpoint(baseUrl);
  return { ...config, baseUrl: normalizedBaseUrl, apiKey };
}

function normalizeEndpoint(baseUrl) {
  let parsed;
  try {
    parsed = new URL(baseUrl);
  } catch {
    fail("Remnifold endpoint is invalid.");
  }
  const hostname = parsed.hostname.toLowerCase();
  // setupのIPAddress.IsLoopbackと揃え、URL正規化後のIPだけを判定する。
  const ip = hostname.replace(/^\[|\]$/g, "");
  const local = hostname === "localhost" || (isIP(ip) === 4 && ip.startsWith("127.")) ||
    ip === "::1" || ip === "::ffff:7f00:1";
  const officialSupermemory = hostname === "supermemory.ai" || hostname.endsWith(".supermemory.ai");
  const acceptedProtocol = parsed.protocol === "https:" || (local && parsed.protocol === "http:");
  if (!acceptedProtocol || parsed.username || parsed.password || parsed.search || parsed.hash || officialSupermemory) {
    fail("Refusing an unsupported Remnifold endpoint.");
  }
  return parsed.toString().replace(/\/+$/, "");
}

async function api(config, path, { method = "GET", body } = {}) {
  const response = await fetch(`${config.baseUrl}${path}`, {
    method,
    redirect: "error",
    headers: {
      Authorization: `Bearer ${config.apiKey}`,
      "Content-Type": "application/json",
      "x-sm-source": "codex-history-backfill",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) fail(`Remnifold API request failed (HTTP ${response.status}).`);
  return response.status === 204 ? null : response.json();
}

function listJsonlFiles(directory) {
  if (!existsSync(directory)) return [];
  const files = [];
  const pending = [directory];
  while (pending.length > 0) {
    const current = pending.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const entryPath = join(current, entry.name);
      if (entry.isDirectory()) pending.push(entryPath);
      if (entry.isFile() && entryPath.endsWith(".jsonl")) files.push(entryPath);
    }
  }
  return files;
}

async function readSessionMeta(filePath) {
  const input = createReadStream(filePath, { encoding: "utf8" });
  const lines = createInterface({ input, crlfDelay: Infinity });
  try {
    for await (const line of lines) {
      if (!line.includes("session_meta")) continue;
      try {
        const record = JSON.parse(line);
        if (record.type !== "session_meta" || !record.payload || typeof record.payload !== "object") continue;
        const payload = record.payload;
        const id = typeof payload.id === "string" ? payload.id.trim() : "";
        const rootSessionId = typeof payload.session_id === "string" ? payload.session_id.trim() : "";
        if (!id) return null;
        const parentThreadId = typeof payload.parent_thread_id === "string" ? payload.parent_thread_id.trim() : "";
        const agentPath = typeof payload.agent_path === "string" ? payload.agent_path.trim() : "";
        const agentRole = typeof payload.agent_role === "string" ? payload.agent_role.trim() : "";
        const timestamp = typeof payload.timestamp === "string" ? payload.timestamp : "";
        const isSubagent = Boolean(parentThreadId) || (agentPath && agentPath !== "/root") || Boolean(rootSessionId && id !== rootSessionId);
        return { id, rootSessionId: rootSessionId || id, parentThreadId, agentRole, timestamp, isSubagent, cwd: payload.cwd || "" };
      } catch {
        // Skip malformed JSONL lines and continue looking for session metadata.
      }
    }
  } finally {
    lines.close();
    const closed = input.closed ? null : new Promise((done) => input.once("close", done));
    input.destroy();
    if (closed) await closed;
  }
  return null;
}

function replaceWithCount(text, pattern, replacement, counter) {
  return text.replace(pattern, (...args) => {
    counter.count += 1;
    return typeof replacement === "function" ? replacement(...args) : replacement;
  });
}

function sanitizeText(value, knownSecrets, counter) {
  if (typeof value !== "string") return "";
  let text = value
    .replace(/<private>[\s\S]*?<\/private>/gi, REDACTED)
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/gi, "")
    .replace(/<(?:remnifold|kagayoi-memory|supermemory)-context>[\s\S]*?<\/(?:remnifold|kagayoi-memory|supermemory)-context>/gi, "")
    .replace(/\r\n/g, "\n")
    .trim();

  for (const secret of knownSecrets) {
    if (secret.length > 0 && text.includes(secret)) {
      text = text.split(secret).join(REDACTED);
      counter.count += 1;
    }
  }

  text = replaceWithCount(
    text,
    /-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----[\s\S]*?-----END(?: [A-Z0-9]+)? PRIVATE KEY-----/g,
    REDACTED,
    counter,
  );
  text = replaceWithCount(text, /\bBearer\s+[A-Za-z0-9._~+/=-]{12,}\b/gi, "Bearer [REDACTED]", counter);
  // 秘密の代入だけを消費し、長い接頭辞の途中から繰り返し探索しない。
  const secretName = /(?:[a-z0-9]+[_-])*(?:api[_-]?key|api[_-]?token|access[_-]?key|access[_-]?token|refresh[_-]?token|authorization|auth[_-]?token|client[_-]?secret|secret|password|private[_-]?key|token)/.source;
  const quotedValue = /(?:"((?:\\[\s\S]|[^"\\])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))/.source;
  const shellValue = /(?:"((?:\\[\s\S]|\x60[\s\S]|[^"\\\x60])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))/.source;
  // backtick escapeは明示されたPowerShell環境変数だけ。JSONや通常記述では通常文字。
  const assignments = [
    [String.raw`(?<![\w-])(["'])(${secretName})\1`, quotedValue],
    [String.raw`(?<![\w"'-])(?<!\$env:)()(${secretName})`, quotedValue],
    [String.raw`(?<=\$env:)()(${secretName})`, shellValue],
  ];
  for (const [prefix, valuePattern] of assignments) text = text.replace(
    new RegExp(`${prefix}(\\s*[:=]\\s*)${valuePattern}`, "gi"),
    (match, nameQuote, name, separator, doubleQuoted, singleQuoted, unquoted) => {
      const valueQuote = doubleQuoted !== undefined ? '"' : singleQuoted !== undefined ? "'" : "";
      const value = doubleQuoted ?? singleQuoted ?? unquoted;
      if (value === REDACTED) return match;
      counter.count += 1;
      return `${nameQuote}${name}${nameQuote}${separator}${valueQuote}${REDACTED}${valueQuote}`;
    },
  );
  text = replaceWithCount(
    text,
    /([?&](?:api[_-]?key|api[_-]?token|access[_-]?token|refresh[_-]?token|authorization|auth[_-]?token|client[_-]?secret|secret|password|token)=)[^&#\s]+/gi,
    (_match, prefix) => `${prefix}${REDACTED}`,
    counter,
  );
  text = replaceWithCount(
    text,
    /\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{16,}|sm_[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_-]{16,}|github_pat_[A-Za-z0-9_]{16,}|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[A-Za-z0-9_-]{20,})\b/g,
    REDACTED,
    counter,
  );
  return text.trim();
}

function extractTextBlocks(content, allowedTypes) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((block) => block && allowedTypes.includes(block.type) && typeof block.text === "string")
    .map((block) => block.text)
    .join("\n");
}

async function parseTaskTranscript(filePath, knownSecrets) {
  const input = createReadStream(filePath, { encoding: "utf8" });
  const lines = createInterface({ input, crlfDelay: Infinity });
  const entries = [];
  const counter = { count: 0 };
  let lineIndex = 0;

  function push(role, rawContent, sourceTimestamp) {
    const content = sanitizeText(role === "user" ? cleanUserRequest(rawContent) : rawContent, knownSecrets, counter);
    if (!content) return;
    const previous = entries.at(-1);
    if (previous?.role === role && previous.content === content && lineIndex - previous.lineIndex <= 5) return;
    entries.push({ role, content, lineIndex, sourceTimestamp: typeof sourceTimestamp === "string" ? sourceTimestamp : "" });
  }

  try {
    for await (const line of lines) {
      lineIndex += 1;
      if (!line.trim()) continue;
      try {
        const record = JSON.parse(line);
        const payload = record.payload;
        if (!payload || typeof payload !== "object") continue;
        if (record.type === "event_msg" && payload.type === "user_message" && typeof payload.message === "string") {
          push("user", payload.message, record.timestamp);
        } else if (record.type === "event_msg" && payload.type === "assistant_output_text" && typeof payload.text === "string" && isFinalOutput(payload)) {
          push("assistant", payload.text, record.timestamp);
        } else if (record.type === "response_item" && payload.role === "user") {
          push("user", extractTextBlocks(payload.content, ["input_text"]), record.timestamp);
        } else if (record.type === "response_item" && payload.role === "assistant" && isFinalOutput(payload)) {
          push("assistant", extractTextBlocks(payload.content, ["output_text"]), record.timestamp);
        }
      } catch {
        // A malformed event must not stop collection from the rest of the local transcript.
      }
    }
  } finally {
    lines.close();
    input.destroy();
  }

  const turns = [];
  let currentTurn = null;
  for (const entry of entries) {
    if (entry.role === "user") {
      if (currentTurn && !currentTurn.assistant) {
        // 作業中に届いた補足・訂正も、完了した要求と一緒に保持する。
        currentTurn.user += `\n\n${entry.content}`;
      } else {
        currentTurn = { user: entry.content, assistant: "" };
        if (entry.sourceTimestamp) currentTurn.sourceTimestamp = entry.sourceTimestamp;
        turns.push(currentTurn);
      }
    } else if (currentTurn) {
      // The last response after a user request is the task result; intermediate commentary stays local.
      currentTurn.assistant = entry.content;
      if (!currentTurn.sourceTimestamp && entry.sourceTimestamp) currentTurn.sourceTimestamp = entry.sourceTimestamp;
    }
  }
  return { turns: turns.filter((turn) => turn.assistant), redactions: counter.count };
}

// 自動注入されたユーザーロール文を、実際の依頼と分離する。
function isFinalOutput(payload) {
  if (payload.phase) return payload.phase === "final_answer";
  return !payload.channel || payload.channel === "final";
}

function cleanUserRequest(value) {
  if (typeof value !== "string") return "";
  let text = value.replace(/\r\n/g, "\n").trim();
  if (/^# AGENTS\.md instructions(?: for [^\n]*)?\s*\n/i.test(text)) return "";
  for (const tag of ["recommended_plugins", "environment_context", "codex_internal_context", "remnifold-context", "remnifold-recall", "remnifold-index", "kagayoi-memory-context", "kagayoi-memory-recall", "kagayoi-memory-index", "supermemory-context", "supermemory-recall", "supermemory-index", "system-reminder"]) {
    text = text.replace(new RegExp(`<${tag}\\b[^>]*>[\\s\\S]*?<\\/${tag}>`, "gi"), "");
  }
  const annotationBlocks = [];
  let hadAnnotationWrapper = false;
  text = text.replace(/<response-annotations\b[^>]*>([\s\S]*?)<\/response-annotations>/gi, (_match, body) => {
    hadAnnotationWrapper = true;
    try {
      const annotations = JSON.parse(body);
      if (!Array.isArray(annotations)) return "";
      for (const item of annotations) {
        if (!item || typeof item !== "object") continue;
        const referencedText = typeof item.text === "string" ? item.text.trim() : "";
        const userComment = typeof item.annotation === "string" ? item.annotation.trim() : "";
        if (!referencedText && !userComment) continue;
        const parts = [`### Feedback on earlier response ${annotationBlocks.length + 1}`];
        if (referencedText) parts.push(`Referenced assistant response:\n${referencedText}`);
        if (userComment) parts.push(`User comment:\n${userComment}`);
        annotationBlocks.push(parts.join("\n\n"));
      }
    } catch {
      // 不正なラッパーから内部メタデータを保存しない。本文の依頼はこの後で抽出する。
    }
    return "";
  });
  text = text.replace(/\[SUPERMEMORY CONTAINERS\][\s\S]*?\[END SUPERMEMORY CONTAINERS\]/g, "")
    .replace(/<supermemory-containers>[\s\S]*?<\/supermemory-containers>/gi, "").trim();
  if (/^# AGENTS\.md instructions(?: for [^\n]*)?\s*\n/i.test(text)) return "";
  if (text.includes("## My request:") && (hadAnnotationWrapper || /^(## Referenced chats with Codex:|# Files mentioned by the user:)/.test(text))) {
    text = text.slice(text.indexOf("## My request:") + "## My request:".length).trim();
  }
  text = text.replace(/<image\b[^>]*>[\s\S]*?<\/image>/gi, "").trim();
  return [...annotationBlocks, annotationBlocks.length && text ? `### Current user request\n${text}` : text].filter(Boolean).join("\n\n").trim();
}

async function readTaskTitles(codexHome, requiredIds) {
  const result = new Map();
  const path = join(codexHome, "session_index.jsonl");
  if (!existsSync(path)) return result;
  const input = createReadStream(path, { encoding: "utf8" });
  const lines = createInterface({ input, crlfDelay: Infinity });
  try {
    for await (const line of lines) {
      try {
        const row = JSON.parse(line);
        if (typeof row.id === "string" && typeof row.thread_name === "string" && (!requiredIds || requiredIds.has(row.id))) result.set(row.id, row.thread_name);
      } catch { /* 末尾の書きかけ行は次回読む。 */ }
    }
  } finally { lines.close(); input.destroy(); }
  return result;
}

function buildTurnDocuments(candidate, transcript, project = {}, title, maxChars = DEFAULT_MAX_DOCUMENT_CHARS, containerTag) {
  const titleText = sanitizeText(title || transcript.turns[0]?.user.split("\n")[0] || "Codex task", [], { count: 0 });
  const legacyTitle = titleText.slice(0, 200);
  const safeTitle = truncateUtf16(projectRequestText(titleText).trim() || "Codex task", 200);
  const hasProjectProvenance = typeof project.containerTag === "string" && Boolean(project.containerTag.trim());
  const reuseExistingCapture = containerTag === undefined && hasProjectProvenance;
  containerTag ||= DEFAULT_MEMORY_CONTAINER;
  return transcript.turns.flatMap((turn, index) => {
    const body = `### User request\n${turn.user}\n\n### Final assistant response\n${turn.assistant}`;
    const requestStart = "### User request\n".length;
    const projectionEdits = requestProjectionEdits(turn.user, { keepInvocation: true })
      .map((edit) => ({ ...edit, start: edit.start + requestStart, end: edit.end + requestStart }));
    // 本文ハッシュをIDに含め、再送は同じID、別内容は別IDにする。
    const identity = `${candidate.meta.id}:${index + 1}:${sha256(body)}`;
    const legacyHeader = `# ${legacyTitle}\n\nSession: ${candidate.meta.id}\nTurn: ${index + 1}\n\n`;
    const size = maxChars - legacyHeader.length - 80;
    if (size < 100) fail("Document size limit is too small.");
    const parts = [];
    let start = 0;
    for (let offset = 0; offset < body.length; offset += size) {
      // 旧固定幅のhash/part数はreceiptとcanonical rowの互換に維持する。
      // 出力境界だけ最大1 code unit戻し、補助文字を次のpartへ全体で移す。
      const legacyPart = body.slice(offset, offset + size);
      const end = truncateUtf16(body, Math.min(offset + size, body.length)).length;
      parts.push({ content: projectTextSlice(body, start, end, projectionEdits), hash: sha256(legacyPart).slice(0, 16) });
      start = end;
    }
    const memoryIndex = buildMemoryIndex({
      title: safeTitle,
      request: turn.user,
      response: turn.assistant,
      sourceUpdatedAt: turn.sourceTimestamp,
      sourceKind: "conversation",
    });
    return parts.map((part, partIndex) => ({
      customId: `codex-turn-v2:${identity}:${partIndex + 1}:${part.hash}`,
      content: `# ${safeTitle}\n\n${part.content}`,
      containerTag,
      ...(reuseExistingCapture ? { reuseExistingCapture: true } : {}),
      metadata: {
        type: "conversation", title: safeTitle,
        ...(project.projectName ? { project: project.projectName } : {}),
        ...(hasProjectProvenance ? { sm_project_id: project.containerTag } : {}),
        sm_scope: hasProjectProvenance ? "project" : "shared", sm_source: "codex",
        // 既存文書のprovenanceと重複判定を保つため、永続化済み識別子を維持する。
        sm_client: "codex-kagayoi-memory", sessionId: candidate.meta.id,
        rootSessionId: candidate.meta.rootSessionId, sessionKind: candidate.meta.isSubagent ? "subagent" : "root",
        turn: index + 1, part: partIndex + 1, parts: parts.length,
        captureVersion: 2, captureKey: `${identity}:${partIndex + 1}:${part.hash}`,
        memoryIndex: { ...memoryIndex, sections: [...memoryIndex.sections], ...(!part.content.replace(/^### (?:User request|Final assistant response)\s*$/gm, "").trim()
          ? { recallable: false, sections: [] } : {}) },
        // このturnを開始した最初の利用者イベントの記録日時。保存処理の実行日時ではない。
        ...(turn.sourceTimestamp ? { sourceTimestamp: turn.sourceTimestamp } : {}),
      },
    }));
  });
}

async function chooseCandidates(files, rootOnly) {
  const grouped = new Map();
  let eligibleFiles = 0;
  // 固定workerだけがstreamを生成する。--limit適用前も全候補を走査する。
  let next = 0;
  async function worker() {
    while (next < files.length) {
      const filePath = files[next++];
      const meta = await readSessionMeta(filePath);
      if (!meta || (rootOnly && meta.isSubagent)) continue;
      const stats = statSync(filePath);
      eligibleFiles += 1;
      const candidate = {
        filePath,
        meta,
        size: stats.size,
        modifiedMs: stats.mtimeMs,
        recent: Date.now() - stats.mtimeMs < RECENT_TRANSCRIPT_WINDOW_MS,
      };
      const existing = grouped.get(meta.id);
      if (!existing) {
        grouped.set(meta.id, candidate);
      } else {
        const replacement = candidate.size > existing.size || (candidate.size === existing.size && candidate.modifiedMs > existing.modifiedMs)
          ? candidate
          : existing;
        replacement.recent = existing.recent || candidate.recent;
        grouped.set(meta.id, replacement);
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(8, files.length) }, worker));
  const candidates = [...grouped.values()].sort((left, right) => {
    const leftKey = left.meta.timestamp || "";
    const rightKey = right.meta.timestamp || "";
    return leftKey.localeCompare(rightKey) || left.meta.id.localeCompare(right.meta.id);
  });
  return {
    candidates,
    duplicateFiles: eligibleFiles - candidates.length,
    recentlyActiveSessions: candidates.filter((candidate) => candidate.recent).length,
  };
}

async function listCaptureHistory(config, containerTag) {
  const captureKeys = new Set();
  let count = 0;
  let page = 1;
  while (true) {
    const result = await api(config, "/v3/documents/list", {
      method: "POST",
      body: { ...(containerTag ? { containerTag } : {}), page, limit: DEFAULT_LIST_LIMIT, projection: "capture" },
    });
    const batch = Array.isArray(result?.documents) ? result.documents : [];
    count += batch.length;
    for (const document of batch) {
      if (document.metadata?.captureVersion === 2 && document.metadata?.captureKey) captureKeys.add(document.metadata.captureKey);
    }
    const totalPages = Number(result?.pagination?.totalPages);
    if ((Number.isFinite(totalPages) && page >= totalPages) || batch.length < DEFAULT_LIST_LIMIT) break;
    page += 1;
    if (page > 10_000) fail("Too many document-list pages.");
  }
  return { count, captureKeys };
}

function loadState(statePath) {
  if (!existsSync(statePath)) return { version: IMPORT_VERSION, documents: {} };
  try {
    const state = JSON.parse(readFileSync(statePath, "utf8"));
    if (state.version !== IMPORT_VERSION || !state.documents || typeof state.documents !== "object") {
      fail("History import state has an unsupported format.");
    }
    return state;
  } catch (error) {
    if (error instanceof Error && error.message === "History import state has an unsupported format.") throw error;
    fail("History import state is not valid JSON.");
  }
}

function saveState(statePath, state) {
  mkdirSync(dirname(statePath), { recursive: true });
  const temporaryPath = `${statePath}.${process.pid}.tmp`;
  writeFileSync(temporaryPath, `${JSON.stringify(state, null, 2)}\n`, "utf8");
  renameSync(temporaryPath, statePath);
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (!existsSync(options.codexHome)) fail("Codex home directory was not found.");

  const config = loadConfig(options.codexHome);
  const sessionRoots = [join(options.codexHome, "sessions"), join(options.codexHome, "archived_sessions")];
  const files = sessionRoots.flatMap(listJsonlFiles).filter((path) => !options.sessionId || path.includes(options.sessionId));
  const discovery = await chooseCandidates(files, options.rootOnly);
  const idleCandidates = discovery.candidates.filter((candidate) =>
    (!options.sessionId || candidate.meta.id === options.sessionId) && (options.includeRecent || !candidate.recent));
  const selectedCandidates = options.limit > 0 ? idleCandidates.slice(0, options.limit) : idleCandidates;
  const titles = await readTaskTitles(options.codexHome, new Set(selectedCandidates.map((candidate) => candidate.meta.id)));
  const planned = [];
  let turnCount = 0;
  let redactions = 0;
  const projects = new Map();
  for (const candidate of selectedCandidates) {
    const projectPath = candidate.meta.cwd;
    if (projectPath && !projects.has(projectPath)) projects.set(projectPath, getProjectContext(projectPath));
    const project = projectPath ? { ...projects.get(projectPath) } : {};
    const transcript = await parseTaskTranscript(candidate.filePath, [config.apiKey]);
    turnCount += transcript.turns.length;
    redactions += transcript.redactions;
    planned.push(...buildTurnDocuments(candidate, transcript, project,
      sanitizeText(titles.get(candidate.meta.id) || "", [config.apiKey], { count: 0 }), options.maxDocumentChars,
      options.containerTag || undefined));
  }
  const { count: existingCount, captureKeys: existingHistory } = planned.length
    ? await listCaptureHistory(config) : { count: 0, captureKeys: new Set() };
  let created = 0;
  let unchanged = 0;
  const state = options.apply ? loadState(options.statePath) : null;
  if (state) {
    for (const legacyPath of options.legacyStatePaths || []) {
      const legacyState = loadState(legacyPath);
      state.documents = { ...legacyState.documents, ...state.documents };
    }
  }
  if (options.apply) {
    for (const document of planned) {
      const key = `${document.metadata.sm_project_id || document.containerTag}:${document.metadata.captureKey}`;
      if (existingHistory.has(document.metadata.captureKey)) { unchanged += 1; continue; }
      const result = await api(config, "/v3/documents", { method: "POST", body: document });
      if (!result || typeof result.id !== "string" || !result.id) fail("Memory API did not acknowledge the document.");
      created += 1;
      state.documents[key] = { completedAt: new Date().toISOString() };
      saveState(options.statePath, state);
    }
  } else {
    unchanged = planned.filter((document) => existingHistory.has(document.metadata.captureKey)).length;
  }

  const rootSessions = selectedCandidates.filter((candidate) => !candidate.meta.isSubagent).length;
  const subagentSessions = selectedCandidates.length - rootSessions;
  const plannedCharacters = planned.reduce((total, document) => total + document.content.length, 0);
  process.stdout.write(`${JSON.stringify({
    mode: options.apply ? "applied" : "plan",
    containerTags: [...new Set(planned.map((document) => document.containerTag))],
    sourceFiles: files.length,
    duplicateTranscriptFiles: discovery.duplicateFiles,
    discoveredSessions: discovery.candidates.length,
    recentlyActiveSessions: discovery.recentlyActiveSessions,
    selectedSessions: selectedCandidates.length,
    rootSessions,
    subagentSessions,
    taskTurns: turnCount,
    plannedDocuments: planned.length,
    plannedCharacters,
    secretRedactions: redactions,
    existingDocuments: existingCount,
    existingHistoricalDocuments: existingHistory.size,
    created,
    updated: 0,
    unchanged,
    stateFile: options.apply ? options.statePath : "not created in plan mode",
    nextStep: options.apply ? "Run the same command again to verify idempotency." : "Re-run with --apply to upload the sanitized task records.",
  }, null, 2)}\n`);
}

export { api, loadConfig, normalizeEndpoint, getProjectContext, listJsonlFiles, readSessionMeta, chooseCandidates, parseTaskTranscript, cleanUserRequest, readTaskTitles, buildTurnDocuments, sanitizeText };

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : "History import failed."}\n`);
  process.exitCode = 1;
});
