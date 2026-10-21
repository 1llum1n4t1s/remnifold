import { readSettings, getReadContext, api, discoverSearchContainers } from "./memory-client.mjs";

export async function showStatus() {
  const settings = readSettings();
  const context = getReadContext(process.cwd(), settings);
  let connected = false;
  let discovery;
  try { await api("/v3/session"); connected = true; }
  catch { /* 秘密値を含む例外や設定本文は表示しない。 */ }
  if (connected) {
    try { discovery = await discoverSearchContainers(api); }
    catch { /* 一覧取得の失敗は状態行で明示する。 */ }
  }
  process.stdout.write([
    "Remnifold status",
    `API reachability: ${connected ? "connected" : "unavailable"}`,
    "Storage: configured Cloudflare D1",
    "Default storage container: memories",
    `Workspace provenance tag: ${context.containerTag}`,
    `Topic search scope: ${discovery ? `${discovery.tags.length} nonempty spaces discovered by the API${discovery.complete ? "" : " (incomplete at the 100-space API limit)"}` : "unavailable; global topic search cannot claim complete coverage"}`,
    `Auto-recall: ${settings.recallMode}; document index only`,
    "Session start: no index is injected until a topic query is available",
    "Details: getDocument(documentId)",
    "Codex built-in memory files: not read by this integration",
    "MCP reachability: check whoAmI separately",
  ].join("\n") + "\n");
}
