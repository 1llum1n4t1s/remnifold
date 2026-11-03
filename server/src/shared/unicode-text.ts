// 制限はUTF-16 code unitのまま、補助文字の途中で切断しない。
export function truncateUtf16(text: string, limit: number) {
  let end = Math.max(0, Math.trunc(limit));
  if (end < text.length && /[\uD800-\uDBFF]/u.test(text[end - 1] || "") && /[\uDC00-\uDFFF]/u.test(text[end] || "")) end -= 1;
  return text.slice(0, end);
}
