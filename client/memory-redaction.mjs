const REDACTED = "[REDACTED]";

// 秘密の代入だけを消費し、長い接頭辞の途中から繰り返し探索しない。
const secretName = /(?:[a-z0-9]+[_-])*(?:api[_-]?key|api[_-]?token|access[_-]?key|access[_-]?token|refresh[_-]?token|authorization|auth[_-]?token|client[_-]?secret|secret|password|private[_-]?key|token)/.source;
const quotedValue = /(?:"((?:\\[\s\S]|[^"\\])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))/.source;
const shellValue = /(?:"((?:\\[\s\S]|\x60[\s\S]|[^"\\\x60])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))/.source;
// backtick escapeは明示されたPowerShell環境変数だけ。JSONや通常記述では通常文字。
const assignmentPatterns = [
  [String.raw`(?<![\w-])(["'])(${secretName})\1`, quotedValue],
  [String.raw`(?<![\w"'-])(?<!\$env:)()(${secretName})`, quotedValue],
  [String.raw`(?<=\$env:)()(${secretName})`, shellValue],
].map(([prefix, valuePattern]) => new RegExp(`${prefix}(\\s*[:=]\\s*)${valuePattern}`, "gi"));

function replaceWithCount(text, pattern, replacement, counter) {
  return text.replace(pattern, (...args) => {
    counter.count += 1;
    return typeof replacement === "function" ? replacement(...args) : replacement;
  });
}

export function sanitizeText(value, knownSecrets, counter) {
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
  for (const pattern of assignmentPatterns) text = text.replace(
    pattern,
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
