import { createHash, randomUUID } from "node:crypto";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const digest = (value) => createHash("sha256").update(value).digest("hex");

// 状態が読めないときは抑止しない。履歴の取得失敗を「既読」として保存しない。
export function recallState(payload, codexHome, event) {
  if (!codexHome || typeof payload.session_id !== "string" || !payload.session_id.trim()) return null;
  // compact後の回復経路がない入力では、必要な索引を抑止しない。
  if (typeof payload.transcript_path !== "string" || !payload.transcript_path.trim()) return null;
  const directory = join(codexHome, "remnifold", "recall-state");
  const path = join(directory, `${digest(payload.session_id)}.json`);
  const readPath = [path, ...["kagayoi-memory", "cloudflare-memory"]
    .map((name) => join(codexHome, name, "recall-state", `${digest(payload.session_id)}.json`))].find(existsSync);
  let state = { version: 1, documents: {}, offset: 0 };
  try {
    try { if (readPath) state = JSON.parse(readFileSync(readPath, "utf8")); }
    catch (error) { if (error.code !== "ENOENT") return null; }
    if (state.version !== 1 || !state.documents || typeof state.documents !== "object" || Array.isArray(state.documents)) return null;
    if ("remainder" in state) {
      if (state.remainder) { state.documents = {}; state.offset = 0; }
      delete state.remainder;
    }
    if (event === "SessionStart" && ["compact", "clear"].includes(payload.source)) state.documents = {};
    if (payload.transcript_path) {
      const transcript = payload.transcript_path;
      const size = statSync(transcript).size;
      if (state.transcript !== transcript || !Number.isSafeInteger(state.offset) || state.offset > size) {
        state.documents = {};
        state.offset = 0;
      }
      state.transcript = transcript;
      // 長い初回履歴・前回からの大量追記は末尾1MiBだけ読む。見落とした
      // compactがあり得る場合は既読を捨て、必要な履歴を抑止しない。
      if (size - state.offset > 1024 * 1024) {
        state.documents = {};
        state.offset = size - 1024 * 1024;
      }
      const file = openSync(transcript, "r");
      try {
        const buffer = Buffer.alloc(size - state.offset);
        let read = 0;
        while (read < buffer.length) {
          const count = readSync(file, buffer, read, buffer.length - read, state.offset + read);
          if (!count) break;
          read += count;
        }
        // 完全行だけ進める。未完行の本文は保存せず、次回同じbyte offsetから読む。
        const completeEnd = buffer.subarray(0, read).lastIndexOf(10) + 1;
        for (const entry of buffer.subarray(0, completeEnd).toString("utf8").split("\n")) {
          try {
            const row = JSON.parse(entry);
            if (row.type === "compacted" || row.type === "event_msg" && row.payload?.type === "context_compacted") state.documents = {};
          } catch { /* 末尾から読み始めた部分行や未知の行は無視する。 */ }
        }
        state.offset += completeEnd;
        if (completeEnd < read) state.documents = {};
      } finally { closeSync(file); }
    }
    return {
      has(item, formatted) { return state.documents[digest(`${item.containerTag}:${item.id}`)] === digest(`${formatted}\n${item.updatedAt || ""}\n${item.sourceUpdatedAt || ""}`); },
      mark(item, formatted) { state.documents[digest(`${item.containerTag}:${item.id}`)] = digest(`${formatted}\n${item.updatedAt || ""}\n${item.sourceUpdatedAt || ""}`); },
      save() {
        let temporary;
        try {
          mkdirSync(directory, { recursive: true });
          temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
          writeFileSync(temporary, JSON.stringify(state), { mode: 0o600 });
          renameSync(temporary, path);
        } catch { /* 保存失敗時は次回も索引を渡す。 */ }
        finally { if (temporary) { try { unlinkSync(temporary); } catch { /* rename済み。 */ } } }
      },
    };
  } catch { return null; }
}
