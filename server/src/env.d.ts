interface Env {
  DB: D1Database;
  MEMORY_VECTORS: VectorizeIndex;
  AI: Ai;
  AI_ENRICHMENT_MODE: "on" | "off";
  MEMORY_API_KEY: string;
  MCP_ALLOWED_ORIGINS?: string;
  /** 任意。ISO 8601 UTC。この時刻まで抽出の拾い直しを一括モードにする（最大7日先）。 */
  EXTRACTION_BACKLOG_UNTIL?: string;
}
