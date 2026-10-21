// 伝送用の説明と利用者の依頼を分ける。元の位置を保持し、capture IDの計算には使わない。
export function requestProjectionEdits(value, { keepInvocation = false } = {}) {
  const text = String(value || "");
  const edits = [];
  const add = (pattern, replacement = "") => {
    for (const match of text.matchAll(pattern)) {
      let start = match.index;
      let end = start + match[0].length;
      const projected = typeof replacement === "function" ? replacement(match) : replacement;
      const overlapping = edits.filter((edit) => start < edit.end && end > edit.start);
      if (overlapping.length) {
        if (projected || overlapping.some((edit) => edit.text)) continue;
        start = Math.min(start, ...overlapping.map((edit) => edit.start));
        end = Math.max(end, ...overlapping.map((edit) => edit.end));
        for (const edit of overlapping) edits.splice(edits.indexOf(edit), 1);
      }
      edits.push({ start, end, text: projected });
    }
  };
  add(/<skill\b[^>]*>\s*<name>[^<]+<\/name>\s*<path>[^<]+<\/path>[\s\S]*?<\/skill\s*>/gi);
  add(/<in-app-browser-context\b(?=[^>]*\bsource\s*=\s*["']ambient-ui-state["'])[^>]*>[\s\S]*?<\/in-app-browser-context\s*>/gi);
  add(/<external_codex_apps_open_page\b[^>]*>[\s\S]*?<\/external_codex_apps_open_page\s*>/gi);
  add(/\[\$([\w:-]+)\]\([^\r\n)]*\bSKILL\.md(?:[?#][^\r\n)]*)?\)/gi,
    (match) => keepInvocation ? `$${match[1]}` : "");
  if (!keepInvocation) {
    // 裸の変数名・絶対パスは呼出しと区別できない。添付定義で名前を確認できた場合だけ除く。
    const names = [...text.matchAll(/<skill\b[^>]*>\s*<name>([\w:-]+)<\/name>\s*<path>/gi)].map((match) => match[1]);
    for (const name of names) {
      const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      add(new RegExp(`^[ \\t]*[$/]${escaped}(?=\\s|$)`, "gim"));
    }
  }
  return edits.sort((a, b) => a.start - b.start);
}

export function projectTextSlice(text, start, end, edits) {
  let result = "";
  let cursor = start;
  for (const edit of edits) {
    if (edit.end <= start || edit.start >= end) continue;
    result += text.slice(cursor, Math.max(cursor, edit.start));
    if (edit.start >= start) result += edit.text;
    cursor = Math.max(cursor, Math.min(end, edit.end));
  }
  return result + text.slice(cursor, end);
}

export function projectRequestText(value, options) {
  const text = String(value || "");
  return projectTextSlice(text, 0, text.length, requestProjectionEdits(text, options));
}

export function projectDocumentContent(value) {
  const content = String(value || "");
  const marker = /^### User request\s*\n/m.exec(content);
  if (!marker) return content;
  const start = marker.index + marker[0].length;
  const response = /^### Final assistant response\s*$/m.exec(content.slice(start));
  const end = response ? start + response.index : content.length;
  return content.slice(0, start) + projectRequestText(content.slice(start, end), { keepInvocation: true }) + content.slice(end);
}
