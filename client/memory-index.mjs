// 保存時と読出時で同じ索引を作る。要約を新しい事実として生成せず、原文への入口だけを残す。
import { truncateUtf16 } from "./unicode-text.mjs";
import { projectRequestText } from "./request-projection.mjs";

const INDEX_VERSION = 1;
const compact = (value) => typeof value === "string" ? value.replace(/\s+/g, " ").trim() : "";

// 保存本文は保ち、索引に使う利用者の依頼だけを表示用に投影する。
export function indexRequestText(value) {
  return projectRequestText(value)
    .replace(/<in-app-browser-context\b(?=[^>]*\bsource\s*=\s*["']ambient-ui-state["'])[^>]*>[\s\S]*?<\/in-app-browser-context\s*>/gi, "\n")
    .replace(/<send_user_message_question_reply\s*>([\s\S]*?)<\/send_user_message_question_reply\s*>/gi, (wrapper, body) => {
      let replies;
      try { replies = JSON.parse(body); } catch { return wrapper; }
      if (!Array.isArray(replies) || !replies.length || !replies.every((reply) => reply &&
        typeof reply.question === "string" && reply.question.trim() && typeof reply.answer === "string")) return wrapper;
      // 長い質問でも、利用者が実際に選んだ回答を索引の先頭に残す。
      return replies.map((reply) => `回答: ${compact(reply.answer)}（質問: ${compact(reply.question)}）`).join(" / ");
    });
}

function degradedIndexRequest(value) {
  const text = compact(value);
  return /^<(?:skill|name|path|external_codex_apps_open_page)\b/i.test(text) ||
    /^This block is automatically supplied ambient UI state\b/i.test(text) ||
    /<in-app-browser-context\b[^>]*\bsource\s*=\s*["']ambient-ui-state["']/i.test(text) ||
    /^<send_user_message_question_reply\b/i.test(text) ||
    (/^\[\s*\{/.test(text) && /"questionItemId"\s*:/.test(text) && /request_user_input_async/.test(text));
}

export function matchingTopicTerms(text, terms) {
  const normalize = (value) => value.normalize("NFKC").toLowerCase().replace(/[._-]+/gu, " ");
  const topic = normalize(text);
  return terms.filter((value) => {
    const term = normalize(value);
    if (!/^[a-z\d ]+$/u.test(term)) return topic.includes(term);
    // 語の断片は除外し、バージョン未指定の .NET と .NET8 は対応させる。
    const after = /\d$/u.test(term) ? /[a-z\d]/u : /[a-z]/u;
    for (let position = topic.indexOf(term); position !== -1; position = topic.indexOf(term, position + 1)) {
      if (!/[a-z\d]/u.test(topic[position - 1] || "") && !after.test(topic[position + term.length] || "")) return true;
    }
    return false;
  });
}

export function shortText(value, limit = 160) {
  const text = compact(value).replace(/<[^>]*>/g, "");
  return text.length <= limit ? text : `${truncateUtf16(text, limit - 1).trimEnd()}…`;
}

const promptText = (value, limit = 160) => shortText(shortText(value, limit)
  .replace(/&/g, "&amp;")
  .replace(/</g, "&lt;")
  .replace(/>/g, "&gt;"), limit);

function plainLine(line) {
  return line.replace(/^\s*(?:#{1,6}\s+|[-*+]\s+|\d+[.)]\s+|>\s*)/, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/[*`]/g, "")
    .replace(/^(?:desc|description|task|Rollout context):\s*/i, "").trim();
}

function meaningfulLines(value) {
  let fenced = false;
  return String(value || "").split(/\r?\n/).filter((line) => {
    if (/^\s*```/.test(line)) { fenced = !fenced; return false; }
    return !fenced && !/^\s*#{1,6}\s/.test(line) &&
      !/^\s*\[[^\]\n]*\]\((?:thread|codex):\/\/[^)]+\)\s*$/iu.test(line);
  }).map(plainLine).filter((line) => line &&
    !/^(?:Session|Turn|Part|Source(?: SHA-256)?|Imported snapshot|updated_at|rollout_path|cwd|thread_id|git_branch|task_group|task_outcome|keywords)\s*:/i.test(line) &&
    !/^rollout_summaries[\\/]/i.test(line) &&
    !/^(?:User request|User comment:|Referenced assistant response:|Final assistant response|Codex saved memory import|Historical memory evidence|v\d+$|<|:codex-|::)/i.test(line));
}

function firstText(value) {
  return meaningfulLines(value).find((line) => !/^(?:ゆーくん[、,。!！\s]*|ありがとう[。!！\s]*|了解(?:です)?[。!！\s]*|承知しました[。!！\s]*)$/.test(line)) || "";
}

export function validDate(value) {
  return typeof value === "string" && /^\d{4}-\d{2}-\d{2}T/.test(value) && Number.isFinite(Date.parse(value)) ? value : undefined;
}

export function buildMemoryIndex({ title, request, response, sourceUpdatedAt, sourceKind = "conversation" } = {}) {
  const taskTitle = shortText(projectRequestText(title), 100);
  const requestLine = firstText(indexRequestText(request).split("### Current user request\n").at(-1));
  const sections = [...new Set(String(response || "").split(/\r?\n/)
    .filter((line) => /^#{1,4}\s+/.test(line)).map(plainLine).filter(Boolean))].slice(0, 4).map((line) => shortText(line, 60));
  const briefFollowup = /^(?:はい|いいえ|了解(?:です|しました)?|承知しました|ありがとう(?:ございます)?|お疲れ(?:様|さま)(?:です|でした)?|[Oo][Kk]|続けて|お願いします|よろしく(?:お願いします)?)[。！!\s]*$/.test(requestLine);
  const detailedResponse = sections.length > 0 || meaningfulLines(response).join(" ").length >= 160;
  const invocationOnly = /^[$/][a-z][\w:-]*$/i.test(projectRequestText(request, { keepInvocation: true }).trim());
  return {
    version: INDEX_VERSION,
    title: taskTitle && !/^(?:s|Codex task|Memory [a-f0-9]+)$/i.test(taskTitle) ? taskTitle : shortText(requestLine || firstText(response) || "保存された記録", 100),
    description: shortText(briefFollowup && detailedResponse ? `${taskTitle || "作業"}の継続結果・詳細` : requestLine || firstText(response), 180),
    sections,
    recallable: Boolean(requestLine || invocationOnly && detailedResponse) && (!briefFollowup || detailedResponse),
    sourceKind,
    ...(validDate(sourceUpdatedAt) ? { sourceUpdatedAt } : {}),
  };
}

function matchingSection(content, terms) {
  if (!terms.length) return content;
  const headings = [...content.matchAll(/^(#{1,4})\s+(.+)$/gm)];
  let selected;
  let best = 0;
  for (const heading of headings) {
    if (/^Codex saved memory import:/i.test(heading[2])) continue;
    const matches = matchingTopicTerms(heading[2], terms).length;
    if (matches > best) { best = matches; selected = heading; }
  }
  if (!selected) return content;
  const next = headings.find((h) => h.index > selected.index && h[1].length <= selected[1].length);
  return content.slice(selected.index, next?.index ?? content.length);
}

export function documentIndex(document, fallbackContainer, terms = []) {
  const metadata = document.metadata && typeof document.metadata === "object" ? document.metadata : {};
  const originalContent = String(document.content || document.memory || document.summary || "");
  const content = matchingSection(originalContent, terms);
  const sourceDates = [...new Set([...originalContent.matchAll(/^updated_at:\s*(\S+)\s*$/gm)].map((match) => match[1]).filter(validDate))];
  const originalDate = sourceDates.length === 1 ? sourceDates[0] : undefined;
  const request = content.match(/### User request\s*\n([\s\S]*?)(?=\n### Final assistant response|$)/)?.[1];
  const response = content.match(/### Final assistant response\s*\n([\s\S]*)/)?.[1];
  const headings = [...content.matchAll(/^#{1,4}\s+(.+)$/gm)].map((match) => match[1]);
  const heading = headings.find((h) => !/^Codex saved memory import:/i.test(h)) || headings[0];
  const importedTitle = heading?.replace(/^Codex saved memory import:\s*/i, "");
  const saved = metadata.memoryIndex?.version === INDEX_VERSION ? metadata.memoryIndex : null;
  const index = saved ? buildMemoryIndex({
    title: saved.title,
    request: degradedIndexRequest(saved.description) ? indexRequestText(request).trim() || saved.title : saved.description,
    response: "",
    sourceUpdatedAt: saved.sourceUpdatedAt,
    sourceKind: saved.sourceKind,
  }) : buildMemoryIndex({
    title: metadata.title || (document.title && !/^Memory [a-f0-9]+$/i.test(document.title) ? document.title : importedTitle),
    request: request || content.match(/^description:\s*(.+)$/m)?.[1] || firstText(content),
    response,
    sourceUpdatedAt: metadata.sourceTimestamp || metadata.sourceUpdatedAt || metadata.completedAt || originalDate,
    sourceKind: metadata.type === "conversation" ? "conversation" : "historical-document",
  });
  if (saved && Array.isArray(saved.sections)) index.sections = saved.sections.filter((s) => typeof s === "string").slice(0, 4).map((s) => shortText(s, 60));
  if (saved?.recallable === false) index.recallable = false;
  else if (saved?.recallable === true && !index.description) index.recallable = true;
  if (!saved && !request && !metadata.title && !heading) index.recallable = false;
  const containerTag = document.containerTag || document.containerTags?.[0] || fallbackContainer;
  const topicValues = Array.isArray(document.topics)
    ? document.topics
    : typeof document.topic === "string" ? [document.topic] : Array.isArray(metadata.topics) ? metadata.topics : [];
  const topics = [...new Set(topicValues.filter((value) => typeof value === "string" && value.trim()).map((value) => value.trim()))];
  const providedProvenance = document.provenance && typeof document.provenance === "object" ? document.provenance : {};
  const project = typeof metadata.project === "string" && metadata.project.trim() ? metadata.project.trim() : undefined;
  const projectId = typeof providedProvenance.projectId === "string" && providedProvenance.projectId.trim()
    ? providedProvenance.projectId.trim()
    : typeof metadata.sm_project_id === "string" && metadata.sm_project_id.trim() ? metadata.sm_project_id.trim() : undefined;
  const provenanceContainer = typeof providedProvenance.containerTag === "string" && providedProvenance.containerTag.trim()
    ? providedProvenance.containerTag.trim()
    : containerTag;
  const filepath = typeof providedProvenance.filepath === "string" && providedProvenance.filepath.trim()
    ? providedProvenance.filepath.trim()
    : typeof metadata.filepath === "string" && metadata.filepath.trim() ? metadata.filepath.trim() : undefined;
  const consolidation = metadata.sm_consolidation === true;
  const sourceMemoryIds = consolidation && Array.isArray(metadata.sourceMemoryIds)
    ? [...new Set(metadata.sourceMemoryIds.filter((value) => typeof value === "string" && value.trim()).map((value) => value.trim()))]
    : [];
  return {
    id: String(document.id || ""),
    containerTag,
    topics,
    provenance: project || projectId || provenanceContainer || filepath
      ? { containerTag: provenanceContainer, projectId, project, filepath }
      : undefined,
    ...index,
    createdAt: validDate(document.createdAt),
    updatedAt: validDate(document.updatedAt),
    sourceUpdatedAt: index.sourceUpdatedAt || validDate(metadata.sourceTimestamp) || validDate(metadata.sourceUpdatedAt) || validDate(metadata.completedAt),
    sessionId: typeof metadata.sessionId === "string" ? metadata.sessionId : undefined,
    turn: Number.isInteger(metadata.turn) ? metadata.turn : undefined,
    part: Number.isInteger(metadata.part) ? metadata.part : undefined,
    parts: Number.isInteger(metadata.parts) ? metadata.parts : undefined,
    consolidation,
    ...(typeof document.searchExcerpt === "string" && document.searchExcerpt.trim() ? { searchExcerpt: shortText(document.searchExcerpt, 240) } : {}),
    sourceMemoryIds,
    consolidationCreatedAt: validDate(metadata.consolidationCreatedAt),
    evidence: "historical record; inspect the document before relying on it",
  };
}

export function publicIndex(item) {
  return {
    id: item.id,
    title: item.title,
    description: item.description,
    containerTag: item.containerTag,
    topics: item.topics,
    sections: item.sections,
    ...(item.sourceUpdatedAt ? { sourceUpdatedAt: item.sourceUpdatedAt } : {}),
    ...(item.updatedAt ? { updatedAt: item.updatedAt } : {}),
    ...(item.createdAt ? { createdAt: item.createdAt } : {}),
    recallable: item.recallable,
    ...(Number.isInteger(item.part) && Number.isInteger(item.parts) && item.parts > 1
      ? { part: item.part, parts: item.parts }
      : {}),
  };
}

export function formatIndexItem(item) {
  const timestamp = item.sourceUpdatedAt || item.updatedAt || item.createdAt || "日時不明";
  const when = item.description && item.description !== item.title ? ` — 参照する場面: ${promptText(item.description, 180)}` : "";
  const sections = item.sections?.length ? `（収録: ${item.sections.slice(0, 2).map((value) => promptText(value, 60)).join("、")}）` : "";
  const parts = item.parts > 1 ? ` | part=${item.part}/${item.parts}` : "";
  const kind = item.consolidation ? " | kind=consolidated-checkpoint" : "";
  const excerpt = item.searchExcerpt ? `\n  原文の該当箇所: ${promptText(item.searchExcerpt, 240)}` : "";
  const topics = item.topics?.length ? ` | topics=${item.topics.slice(0, 3).map((value) => promptText(value, 60)).join(", ")}` : "";
  const provenance = item.provenance?.project || item.provenance?.projectId || item.provenance?.containerTag
    ? ` | provenance=${promptText(item.provenance.project || item.provenance.projectId || item.provenance.containerTag, 160)}${item.provenance.project && item.provenance.projectId ? ` (${promptText(item.provenance.projectId, 160)})` : ""}`
    : "";
  return `- ◪ ${promptText(item.title, 100)}${when}${sections}\n  id=${promptText(item.id, 80)}${topics} | container=${promptText(item.containerTag || "unknown", 160)}${provenance} | ${item.sourceUpdatedAt ? "sourceDate" : "storedDate"}=${promptText(timestamp, 40)}${parts}${kind}${excerpt}`;
}
