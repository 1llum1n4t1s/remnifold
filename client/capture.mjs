#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { captureStage, markCaptureFailure, captureFailureMessage } from "./capture-diagnostics.mjs";
import { api, loadConfig, getProjectContext, listJsonlFiles, readSessionMeta,
  chooseCandidates, parseTaskTranscript, readTaskTitles, buildTurnDocuments, sanitizeText } from "./Import-RemnifoldHistory.mjs";

export async function findTranscript(payload, codexHome) {
  if (!payload.session_id) throw new Error("Missing session ID.");
  if (payload.transcript_path && existsSync(payload.transcript_path)) {
    const meta = await readSessionMeta(payload.transcript_path);
    if (meta?.id === payload.session_id) return { filePath: payload.transcript_path, meta };
    throw new Error("Transcript session ID mismatch.");
  }
  // 古い日付の継続タスクも対象にし、ファイル名の部分一致だけでは採用しない。
  const files = ["sessions", "archived_sessions"].flatMap((name) => listJsonlFiles(join(codexHome, name)))
    .filter((path) => path.includes(payload.session_id));
  const { candidates } = await chooseCandidates(files, false);
  const candidate = candidates.find((item) => item.meta.id === payload.session_id);
  if (!candidate) throw new Error("Session transcript was not found.");
  return candidate;
}

function readReceipts(statePath) {
  if (!existsSync(statePath)) return new Set();
  let text;
  try { text = readFileSync(statePath, "utf8"); }
  catch (error) { throw markCaptureFailure(error, "receipt-read"); }
  let state;
  try { state = JSON.parse(text); }
  catch (error) { throw markCaptureFailure(error, "receipt-invalid"); }
  if (!Array.isArray(state) || state.some((receipt) => typeof receipt !== "string" || !receipt)) {
    throw markCaptureFailure(new Error("Invalid capture receipts."), "receipt-invalid");
  }
  return new Set(state);
}

function readReceiptUnion(...statePaths) {
  const receipts = new Set();
  for (const statePath of statePaths) {
    for (const receipt of readReceipts(statePath)) receipts.add(receipt);
  }
  return receipts;
}

async function recordReceipt(statePath, legacyStatePaths, customId) {
  // Node 24標準SQLiteを保存処理の短い排他だけに使い、送信済み記録の正本は既存JSONのまま保つ。
  const { DatabaseSync } = await import("node:sqlite");
  const databasePath = join(dirname(statePath), "capture-coordination.sqlite3");
  const database = new DatabaseSync(databasePath);
  let transactionOpen = false;
  try {
    database.exec("PRAGMA busy_timeout = 10000");
    database.exec("CREATE TABLE IF NOT EXISTS capture_mutex (id INTEGER PRIMARY KEY CHECK (id = 1), touched_at INTEGER NOT NULL)");
    database.exec("INSERT OR IGNORE INTO capture_mutex (id, touched_at) VALUES (1, 0)");
    database.exec("BEGIN IMMEDIATE");
    transactionOpen = true;
    database.prepare("UPDATE capture_mutex SET touched_at = ? WHERE id = 1").run(Date.now());
    // 初回の新形式書込みでも旧レシートを統合し、次回から旧turnが再送されないようにする。
    const receipts = readReceiptUnion(statePath, ...legacyStatePaths);
    receipts.add(customId);
    const temporary = `${statePath}.${process.pid}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify([...receipts].sort()), "utf8");
      renameSync(temporary, statePath);
    } catch (error) {
      try { unlinkSync(temporary); } catch (cleanupError) { if (cleanupError?.code !== "ENOENT") throw cleanupError; }
      throw error;
    }
    database.exec("COMMIT");
    transactionOpen = false;
  } finally {
    if (transactionOpen) {
      try { database.exec("ROLLBACK"); } catch { /* 元の保存エラーを優先する。 */ }
    }
    database.close();
  }
}

export async function capture(payload, { codexHome = process.env.CODEX_HOME || join(homedir(), ".codex"),
  config, send, budgetMs = 90_000 } = {}) {
  config ||= await captureStage("configuration", () => loadConfig(codexHome));
  const candidate = await captureStage("transcript", () => findTranscript(payload, codexHome));
  const cwd = candidate.meta.cwd || payload.cwd;
  if (!cwd) throw markCaptureFailure(new Error("Missing project path."), "transcript");
  const project = await captureStage("transcript", () => getProjectContext(cwd));
  const transcript = await captureStage("transcript", () => parseTaskTranscript(candidate.filePath, [config.apiKey]));
  const titles = await captureStage("transcript", () => readTaskTitles(codexHome));
  const title = sanitizeText(titles.get(candidate.meta.id) || "", [config.apiKey], { count: 0 });
  const documents = buildTurnDocuments(candidate, transcript, project, title);
  // 応答を確認した文書だけ記録する。中断後の再送も同じcustomIdなので上書き損失は起きない。
  const stateDirectory = join(codexHome, "remnifold", "capture-state");
  const stateId = createHash("sha256").update(`${config.baseUrl}:${project.containerTag}:${candidate.meta.id}`).digest("hex");
  const statePath = join(stateDirectory, `${stateId}.json`);
  const legacyStatePaths = ["kagayoi-memory", "cloudflare-memory"]
    .map((name) => join(codexHome, name, "capture-state", `${stateId}.json`));
  const receipts = readReceiptUnion(statePath, ...legacyStatePaths);
  const pending = documents.filter((document) => !receipts.has(document.customId));
  const started = Date.now();
  let saved = 0;
  for (const document of pending) {
    if (Date.now() - started >= budgetMs) break;
    const result = await captureStage("transport", () => send ? send(document) : api(config, "/v3/documents", { method: "POST", body: document }));
    if (!result || typeof result.id !== "string" || !result.id) throw markCaptureFailure(new Error("Memory API did not acknowledge the document."), "acknowledgement");
    await captureStage("receipt-write", async () => {
      mkdirSync(stateDirectory, { recursive: true });
      await recordReceipt(statePath, legacyStatePaths, document.customId);
    });
    saved += 1;
  }
  return { saved, pending: pending.length - saved, completedTurns: transcript.turns.length };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const payload = await captureStage("payload", () => JSON.parse(readFileSync(0, "utf8")));
    const result = await capture(payload);
    if (result.saved || result.pending) process.stdout.write(JSON.stringify({
      systemMessage: `Remnifold: saved ${result.saved} documents; pending ${result.pending}.`,
    }));
  } catch (error) {
    // 本文・接続設定・HTTP応答をログへ出さず、失敗は呼び出し元に通知する。
    process.stderr.write(captureFailureMessage(error));
    process.exitCode = 1;
  }
}
