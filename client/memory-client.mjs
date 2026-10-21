import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir, hostname } from "node:os";
import { join, resolve } from "node:path";
import { CONFIG_FILE_NAME, LEGACY_CONFIG_FILE_NAMES, getProjectContext, loadConfig } from "./Import-RemnifoldHistory.mjs";
import { documentIndex, formatIndexItem, matchingTopicTerms, publicIndex } from "./memory-index.mjs";
import { truncateUtf16 } from "./unicode-text.mjs";
import { projectDocumentContent } from "./request-projection.mjs";

const hash = (text) => createHash("sha256").update(text).digest("hex").slice(0, 16);
const unique = (values) => [...new Set(values.filter((v) => typeof v === "string" && v.trim()).map((v) => v.trim()))];
function git(args, cwd) {
  try { return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim(); }
  catch { return ""; }
}

export function readSettings(codexHome = process.env.CODEX_HOME || join(homedir(), ".codex")) {
  let config = {};
  for (const name of [CONFIG_FILE_NAME, ...LEGACY_CONFIG_FILE_NAMES]) {
    try {
      const candidate = JSON.parse(readFileSync(join(codexHome, name), "utf8"));
      if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) continue;
      config = candidate;
      break;
    } catch { /* 認証のエラーはAPI接続時に通知する。 */ }
  }
  return {
    codexHome,
    recallMode: config.recallMode || "direct",
    maxMemories: Math.max(1, Math.min(5, Number(config.maxMemories) || 3)),
    minimumSimilarity: Math.max(0.65, Math.min(0.95, Number(config.similarityThreshold) || 0.70)),
    projectContainerTag: config.projectContainerTag,
    userContainerTag: config.userContainerTag,
    readContainerTags: Array.isArray(config.readContainerTags) ? config.readContainerTags : [],
    sharedContainerTags: Array.isArray(config.sharedContainerTags) ? config.sharedContainerTags : [],
  };
}

// project由来tagは出典と旧形式空間の検索用別名として扱い、新規保存は共通containerへ集約する。
// Codex公式のmemoriesディレクトリや生成物を開く処理は持たない。
export function getReadContext(cwd = process.cwd(), settings = readSettings()) {
  const project = getProjectContext(cwd);
  const root = git(["rev-parse", "--show-toplevel"], cwd) || resolve(cwd);
  const pathHash = hash(root);
  const identity = git(["config", "user.email"], root) || process.env.USER || process.env.USERNAME || hostname();
  const userHash = hash(identity);
  const projectTags = unique([
    project.containerTag, settings.projectContainerTag,
    `user_project_${pathHash}`, `claudecode_project_${pathHash}`, `repo_${project.projectName}`,
    `codex_project_${pathHash}`, `opencode_project_${pathHash}`, `cursor_project_${pathHash}`,
    ...(settings.readContainerTags || []),
  ]);
  const sharedTags = unique([
    settings.userContainerTag, `codex_user_${userHash}`, `opencode_user_${userHash}`, `cursor_user_${userHash}`,
    ...(settings.sharedContainerTags || []),
  ]).filter((tag) => !projectTags.includes(tag));
  return { ...project, projectTags, sharedTags, readTags: unique([...projectTags, ...sharedTags]) };
}

export async function api(path, { body, method = body ? "POST" : "GET", timeoutMs = 8_000, fetchImpl = fetch, config, codexHome } = {}) {
  config ||= loadConfig(codexHome || process.env.CODEX_HOME || join(homedir(), ".codex"));
  const response = await fetchImpl(`${config.baseUrl}${path}`, {
    method,
    redirect: "error",
    // 既存サーバーのprovenance識別子との互換性を保つ。
    headers: { Authorization: `Bearer ${config.apiKey}`, "Content-Type": "application/json", "x-sm-source": "codex-kagayoi-memory" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!response.ok) {
    const error = new Error(`Remnifold API request failed (HTTP ${response.status})`);
    error.status = response.status;
    if (response.status === 400) {
      const detail = await response.json().catch(() => null);
      error.unsupportedGlobalSearch = /container.?tag.*required|all.?containers.*(?:unsupported|invalid|unknown)|unsupported.*all.?containers/i.test(String(detail?.error?.message || detail?.error || detail?.message || ""));
    }
    throw error;
  }
  return response.status === 204 ? null : response.json();
}

const STOP_WORDS = new Set(["memory", "memories", "codex", "情報", "こと", "もの", "これ", "それ", "この", "その", "あの", "どの", "ここ", "そこ", "this", "that", "ため", "よう", "です", "ます", "ください", "する", "した", "して", "れる", "ある", "いる", "確認", "対応", "作業", "実装", "調査"]);
const AUTOMATIC_STOP_WORDS = new Set(["修正", "改善", "最適化", "機能", "部分", "発見", "でき", "出来", "てい", "いた", "たら", "そう", "お願い", "お願いします", "please", "fix", "improve", "optimize", "optimization", "implement", "implementation", "investigate"]);
const AUTOMATIC_ACTION_WORDS = new Set(["ので", "なので", "から", "ない", "時に", "とき", "って", "うん", "けど", "だけ", "まだ", "もう", "また", "同じ", "今回", "前回", "こちら", "それで", "続き", "続け", "再度", "問題", "チェック", "試行", "再試行", "発生", "検索", "調べ", "大丈夫", "再開", "実行", "結果", "原因", "みたい", "おかしい", "思う"]);
const AUTOMATIC_CONTEXT_WORDS = new Set(["タイムアウト", "timeout", "入力", "エラー", "error", "動作", "現象", "状態", "必要", "とき", "場合", "発生", "問題", "チェック", "再試行", "過去", "最新", "履歴", "記憶", "全部", "全体", "内容", "情報", "日間", "ヶ月", "か月", "以前", "以降"]);
const querySegmenter = new Intl.Segmenter("ja", { granularity: "word" });
const SEARCH_CONCURRENCY = 8;
function automaticSearchText(query) {
  const pathTopic = (path) => {
    const normalized = path.replace(/\\/gu, "/").replace(/\/+$/u, "");
    let parts = normalized.replace(/^[a-z]:\//iu, "").split("/").filter(Boolean);
    if (normalized.startsWith("//")) parts = parts.slice(2);
    else if (/^(?:users|home)$/iu.test(parts[0] || "")) parts = parts.slice(2);
    else if (parts[0] === "~") parts = parts.slice(1);
    const workspace = parts.findIndex((part) => /^(?:dev|projects?|workspaces?|repos?|work)$/iu.test(part));
    if (workspace !== -1) return parts[workspace + 1] || "";
    return parts.at(-1) || "";
  };
  // 端末・アカウント名を除き、ファイルパスでも所属プロジェクトを残す。
  return query.normalize("NFKC")
    .replace(/(["'])([a-z]:[\\/][^\r\n]*?|\\\\[^"'\r\n]*?|\/[^\r\n]*?)\1/giu, (_match, _quote, path) => pathTopic(path))
    .replace(/\b[a-z]:[\\/][^\s"'<>|]+|\\\\[^\s"'<>|]+|(?<![:\w/])(?:~)?\/[^\s"'<>|]+/giu, (path) => pathTopic(path));
}
export function queryTerms(query, { automatic = false } = {}) {
  if (automatic) query = automaticSearchText(query);
  const identifiers = automatic ? [...query.matchAll(/\b[a-z\d]+(?:[._-][a-z\d]+)+\b/giu)] : [];
  const words = [...querySegmenter.segment(query)].filter((segment) => segment.isWordLike &&
    !identifiers.some((identifier) => segment.index >= identifier.index && segment.index < identifier.index + identifier[0].length))
    .map((segment) => ({ value: segment.segment.toLowerCase(), index: segment.index, end: segment.index + segment.segment.length }));
  const stopped = (value) => STOP_WORDS.has(value) || automatic && (AUTOMATIC_STOP_WORDS.has(value) || AUTOMATIC_ACTION_WORDS.has(value));
  const useful = words.filter(({ value }) => value.length >= 2 && !stopped(value));
  const japanese = /^[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}ー]+$/u;
  const compounds = [];
  const compounded = new Set();
  for (let index = 0; index < useful.length - 1; index++) {
    const current = useful[index];
    const next = useful[index + 1];
    if (current.end !== next.index || !japanese.test(current.value) || !japanese.test(next.value)) continue;
    compounds.push(`${current.value}${next.value}`);
    compounded.add(current.value);
    compounded.add(next.value);
  }
  return unique([...identifiers.map((identifier) => identifier[0].toLowerCase()),
    ...useful.filter(({ value }) => !compounded.has(value)).map(({ value }) => value), ...compounds])
    .filter((value) => !stopped(value)).slice(0, 30);
}

// 継続指示や一般的な症状だけでは、別案件の履歴を選ぶ根拠にしない。
export function recallTopicTerms(query) {
  // 単語そのものを指定した検索と、状況報告だけの継続発話を区別する。
  if (/^(?:timeout|タイムアウト)$/iu.test(query.trim())) return queryTerms(query, { automatic: true });
  return queryTerms(query, { automatic: true }).filter((term) => !AUTOMATIC_CONTEXT_WORDS.has(term) && [...querySegmenter.segment(term)].some(({ segment, isWordLike }) =>
    isWordLike && segment.length >= 2 && !/^\d+$/u.test(segment) && !AUTOMATIC_CONTEXT_WORDS.has(segment) &&
    !AUTOMATIC_ACTION_WORDS.has(segment) && !AUTOMATIC_STOP_WORDS.has(segment) && !STOP_WORDS.has(segment)));
}

function matchingTerms(index, terms, withExcerpt = true) {
  return matchingTopicTerms(`${index.title} ${index.description} ${(index.sections || []).join(" ")} ${(index.topics || []).join(" ")} ${withExcerpt ? index.searchExcerpt || "" : ""}`, terms);
}

function scoreSearchCandidates(candidates, terms, withIndexRelevance = false) {
  const totalWeight = terms.reduce((total, term) => total + Math.min(12, term.length), 0);
  const relevance = (matches) => matches.length
    ? matches.reduce((total, term) => total + Math.min(12, term.length), 0) / totalWeight : 0;
  return candidates.map((candidate) => {
    const matches = terms.length ? matchingTerms(candidate.index, terms) : [];
    return { ...candidate, matches, relevance: relevance(matches),
      ...(withIndexRelevance ? { indexRelevance: relevance(terms.length ? matchingTerms(candidate.index, terms, false) : []) } : {}) };
  });
}

function semanticEligible(row, minimumSimilarity) {
  return typeof row.semanticSimilarity === "number" &&
    Number.isFinite(row.semanticSimilarity) && row.semanticSimilarity >= minimumSimilarity;
}

function lexicalEligible(row) {
  return typeof row.lexicalSimilarity === "number" && Number.isFinite(row.lexicalSimilarity);
}

function latestCaptureRevisions(candidates) {
  const latest = new Map();
  const revisionKeys = new Map();
  for (const candidate of candidates) {
    const index = candidate.index;
    if (index.consolidation || !index.sessionId || !Number.isInteger(index.turn) || !Number.isInteger(index.part)) continue;
    const key = JSON.stringify([index.provenance?.projectId || index.containerTag, index.sessionId, index.turn, index.part]);
    revisionKeys.set(candidate, key);
    const previous = latest.get(key);
    const revision = (item) => [item.index.sourceUpdatedAt || "", item.index.createdAt || "", item.index.updatedAt || ""].join("\n");
    if (!previous || revision(candidate) > revision(previous)) latest.set(key, candidate);
  }
  return candidates.filter((candidate) => !revisionKeys.has(candidate) || latest.get(revisionKeys.get(candidate)) === candidate);
}

function rankForHydration(candidates, terms, settings, limit, automatic) {
  let ranked = scoreSearchCandidates(candidates.filter(({ index }) => index.id && index.containerTag), terms)
    .filter(({ row, relevance }) => row.metadata?.memoryIndex?.version !== 1 ||
      semanticEligible(row, settings.minimumSimilarity) || lexicalEligible(row) || relevance > 0);
  if (automatic) ranked = latestCaptureRevisions(ranked);
  ranked.sort((a, b) => b.relevance - a.relevance ||
    Number(b.row.semanticSimilarity ?? -1) - Number(a.row.semanticSimilarity ?? -1) || a.rank - b.rank);
  const hydrateLimit = Math.min(80, Math.max(20, Number(limit) * 8));
  return [...new Map(ranked.map((candidate) => [`${candidate.index.containerTag}:${candidate.index.id}`, candidate])).values()].slice(0, hydrateLimit);
}

function selectSearchDocuments(hydrated, terms, settings, limit, automatic) {
  if (automatic) hydrated = latestCaptureRevisions(hydrated);
  // 詳細取得で索引が変わるため、取得前の採点をここでは再利用しない。
  const candidates = scoreSearchCandidates(hydrated, terms, true)
    .filter(({ row, index, relevance }) =>
      (semanticEligible(row, settings.minimumSimilarity) || lexicalEligible(row) || relevance > 0) &&
      (!automatic || index.recallable && relevance > 0));
  candidates.sort((a, b) => {
    const consolidationDifference = automatic ? Number(b.index.consolidation) - Number(a.index.consolidation) : 0;
    return b.relevance - a.relevance || b.indexRelevance - a.indexRelevance || consolidationDifference ||
      Number(b.row.semanticSimilarity ?? -1) - Number(a.row.semanticSimilarity ?? -1) || a.rank - b.rank ||
      String(b.index.consolidationCreatedAt || "").localeCompare(String(a.index.consolidationCreatedAt || "")) ||
      String(b.index.sourceUpdatedAt || b.index.updatedAt || "").localeCompare(String(a.index.sourceUpdatedAt || a.index.updatedAt || ""));
  });
  const selected = [];
  const seen = new Set();
  for (const candidate of candidates) {
    const { index, matches } = candidate;
    const key = `${index.containerTag}:${index.id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    // 最終選択済みの清書が検索語も覆うときだけ、同じ原文の重複を抑える。
    if (automatic && !index.consolidation && selected.some((checkpoint) => checkpoint.index.consolidation &&
      checkpoint.index.sourceMemoryIds.includes(index.id) && matches.every((term) => checkpoint.matches.includes(term)))) continue;
    selected.push(candidate);
    if (selected.length >= Math.min(20, Math.max(1, limit))) break;
  }
  return selected.map(({ index }) => index);
}

export async function discoverSearchContainers(request = api) {
  let response;
  try {
    response = await request("/v3/container-tags");
  } catch (error) {
    throw new Error("Remnifold space discovery failed; global topic search was not performed", { cause: error });
  }
  if (!Array.isArray(response?.spaces)) throw new Error("Remnifold space discovery returned an invalid response; global topic search was not performed");
  const tags = unique(response.spaces.filter((space) => {
    if (!space || typeof space !== "object" || typeof space.containerTag !== "string") return false;
    return space.memoryCount === undefined || Number(space.memoryCount) > 0;
  }).map((space) => space.containerTag));
  return { tags, complete: response.spaces.length < 100, returnedCount: response.spaces.length };
}

async function mapConcurrent(values, concurrency, task) {
  const results = new Array(values.length);
  let next = 0;
  async function worker() {
    while (next < values.length) {
      const index = next++;
      try { results[index] = { status: "fulfilled", value: await task(values[index], index) }; }
      catch (reason) { results[index] = { status: "rejected", reason }; }
    }
  }
  await Promise.all(Array.from({ length: Math.min(concurrency, values.length) }, worker));
  return results;
}

export async function searchIndex({ query = "", containerTag, cwd, settings = readSettings(), context, limit = settings.maxMemories, request = api, automatic = false } = {}) {
  query = typeof query === "string" ? truncateUtf16(query.trim(), 1_000) : "";
  if (automatic) query = automaticSearchText(query);
  const terms = automatic ? recallTopicTerms(query) : queryTerms(query);
  if (automatic && !terms.length) query = "";
  if (!query) return { query, containerTag: containerTag || null, searchScope: "none", searchedContainers: [], failedContainers: [], failedDocuments: [], spaceDiscoveryComplete: true, results: [], total: 0 };
  let globalResponse;
  if (!containerTag) {
    try {
      const response = await request("/v4/search", {
        body: { allContainers: true, q: query, limit: 40, ...(automatic ? { indexOnly: true } : {}) },
      });
      if (response?.searchScope === "all-containers") {
        if (!Array.isArray(response.results) || !Array.isArray(response.searchedContainers) || response.spaceDiscoveryComplete !== true) {
          throw new Error("Invalid global memory search response");
        }
        globalResponse = response;
      }
    } catch (error) {
      const unsupported = [404, 501].includes(error?.status) || error?.status === 400 &&
        (error.unsupportedGlobalSearch || /container.?tag.*required|all.?containers.*unsupported/i.test(error.message));
      if (!unsupported) throw error;
    }
  }
  const discovery = globalResponse
    ? { tags: unique(globalResponse.searchedContainers), complete: true, returnedCount: globalResponse.searchedContainers.length }
    : containerTag ? { tags: [containerTag], complete: true, returnedCount: 1 } : await discoverSearchContainers(request);
  const tags = discovery.tags;
  const results = globalResponse ? [{ status: "fulfilled", value: globalResponse.results.map((row, rank) => ({ row, rank, index: documentIndex(row) })) }] : await mapConcurrent(tags, SEARCH_CONCURRENCY, async (tag) => {
    const response = await request("/v4/search", {
      body: { containerTag: tag, q: query, limit: 20, ...(automatic ? { indexOnly: true } : {}) },
    });
    if (!Array.isArray(response?.results)) throw new Error("Invalid memory search response");
    return response.results.map((row, rank) => ({ row, rank, index: documentIndex(row, tag) }));
  });
  const failedTags = globalResponse ? [] : tags.filter((_, i) => results[i].status === "rejected");
  if (tags.length && failedTags.length === tags.length) throw new Error("Remnifold search unavailable for all requested spaces");
  // indexOnly応答の旧文書は、関連度がなくても詳細取得の候補に残す。
  const distinct = rankForHydration(results.flatMap((r) => r.status === "fulfilled" ? r.value : []), terms, settings, limit, automatic);
  const failedDocuments = [];
  // 旧文書の検索excerptには見出しの直前に別話題が混ざる。索引metadataが無い場合は原文を確認してから入口を作る。
  const hydratedResults = await mapConcurrent(distinct, SEARCH_CONCURRENCY, async (candidate) => {
    if (candidate.row.metadata?.memoryIndex?.version === 1) return candidate;
    try {
      const document = await request(`/v3/documents/${encodeURIComponent(candidate.index.id)}`);
      if (document?.id !== candidate.index.id || typeof document.content !== "string") throw new Error("Invalid memory document response");
      return { ...candidate, index: documentIndex(document, candidate.index.containerTag, terms) };
    } catch {
      failedDocuments.push(candidate.index.id);
      return { ...candidate, index: { ...candidate.index, title: candidate.row.metadata?.title || "保存された記録", description: "", recallable: false } };
    }
  });
  const hydrated = hydratedResults.filter((result) => result.status === "fulfilled").map((result) => result.value);
  const documents = selectSearchDocuments(hydrated, terms, settings, limit, automatic);
  return {
    query,
    containerTag: containerTag || null,
    searchScope: containerTag ? "explicit-container" : globalResponse ? "all-containers" : "all-discovered-containers",
    searchedContainers: tags,
    failedContainers: failedTags,
    failedDocuments,
    spaceDiscoveryComplete: discovery.complete,
    discoveredContainerCount: discovery.returnedCount,
    results: documents,
    total: documents.length,
  };
}

export function formatSearchResult(result) {
  const text = result.results.length ? result.results.map(formatIndexItem).join("\n") : "No matching memory index entries found.";
  const failure = result.failedContainers.length || result.failedDocuments?.length ? `\nIncomplete search: ${result.failedContainers.length} spaces and ${result.failedDocuments?.length || 0} document indexes were unavailable.` : "";
  const discovery = result.spaceDiscoveryComplete === false ? "\nIncomplete space discovery: the API returned its 100-space limit, so additional spaces may exist." : "";
  return `${text}${failure}${discovery}\nWhen an entry appears applicable, read the original with getDocument(documentId), validate it against the current implementation, and reuse the parts that still fit.`;
}

export async function listIndex({ containerTag, topic, page = 1, limit = 10, request = api } = {}) {
  const body = { page, limit, projection: "index", ...(containerTag ? { containerTag } : {}), ...(topic ? { topic } : {}) };
  const result = await request("/v3/documents/list", { body });
  if (!Array.isArray(result?.documents)) throw new Error("Invalid memory document-list response");
  return {
    documents: result.documents.map((document) => publicIndex(documentIndex(document, containerTag))),
    pagination: result.pagination,
    containerTag: containerTag || null,
    topic: topic || null,
    listScope: containerTag ? "explicit-container" : "all-containers",
  };
}

function sanitizedDocumentContent(document) {
  const content = projectDocumentContent(String(document.content || document.summary || ""));
  const metadata = document.metadata && typeof document.metadata === "object" ? document.metadata : {};
  if (metadata.captureVersion !== 2 || typeof metadata.title !== "string" ||
      typeof metadata.sessionId !== "string" || !Number.isInteger(metadata.turn)) return content;
  const title = `# ${metadata.title}`;
  const prefix = `${title}\n\nSession: ${metadata.sessionId}\nTurn: ${metadata.turn}\n\n`;
  if (!content.startsWith(prefix)) return content;
  const hasPartMetadata = metadata.part !== undefined || metadata.parts !== undefined;
  if (hasPartMetadata) {
    if (!Number.isInteger(metadata.part) || !Number.isInteger(metadata.parts)) return content;
    const fullPrefix = `${prefix}Part: ${metadata.part}/${metadata.parts}\n\n`;
    if (!content.startsWith(fullPrefix)) return content;
    return `${title}\n\n${content.slice(fullPrefix.length)}`;
  }
  return `${title}\n\n${content.slice(prefix.length)}`;
}

function publicEnrichment(enrichment) {
  if (!enrichment || typeof enrichment !== "object") return undefined;
  const keys = [
    "embeddingStatus", "factStatus", "vectorStatus", "topicStatus", "error",
    "embeddedAt", "factsExtractedAt", "topicsExtractedAt", "vectorAttemptedAt", "updatedAt",
  ];
  return Object.fromEntries(keys.flatMap((key) => enrichment[key] === undefined ? [] : [[key, enrichment[key]]]));
}

export async function listTopics(request = api, options = {}) {
  const page = options.page ?? 1;
  const limit = options.limit ?? 100;
  const query = options.page === undefined && options.limit === undefined ? "" : `?page=${page}&limit=${limit}`;
  const result = await request(`/v4/topics${query}`);
  if (!Array.isArray(result?.topics) || !Number.isInteger(result.unclassifiedCount) || result.unclassifiedCount < 0) {
    throw new Error("Invalid memory topic-list response");
  }
  const topics = result.topics.map((entry) => {
    if (!entry || typeof entry.topic !== "string" || !entry.topic || !Number.isInteger(entry.documentCount) || entry.documentCount < 0) {
      throw new Error("Invalid memory topic-list response");
    }
    return { topic: entry.topic, documentCount: entry.documentCount };
  });
  return { topics, unclassifiedCount: result.unclassifiedCount, ...(result.pagination ? { pagination: result.pagination } : {}) };
}

export async function readDocument(documentId, request = api) {
  const document = await request(`/v3/documents/${encodeURIComponent(documentId)}`);
  const index = documentIndex(document);
  const content = sanitizedDocumentContent(document);
  const projectedIndex = publicIndex(index);
  const enrichment = publicEnrichment(document.enrichment);
  const text = `${formatIndexItem(index)}\n\nこれは過去の記録です。現在の状態と照合して利用してください。\n\n${content}`;
  return {
    text,
    document: {
      id: String(document.id || ""),
      content,
      topics: index.topics,
      ...(index.provenance ? { provenance: index.provenance } : {}),
      index: projectedIndex,
      ...(enrichment ? { enrichment } : {}),
    },
  };
}
