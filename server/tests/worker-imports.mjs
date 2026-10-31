import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

// 計測用data URLでも、Workerの実モジュール依存を通常と同じ場所から読み込む。
export function resolveWorkerImports(source, root) {
  return source.replace(/^(import[^\n]*?\sfrom\s+)(["'])(\.[^"']+)\2/gmu,
    (_match, prefix, _quote, specifier) => `${prefix}${JSON.stringify(pathToFileURL(resolve(root, "server/src", specifier)).href)}`);
}
