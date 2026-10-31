interface Env {
  DB: D1Database;
  MEMORY_VECTORS: VectorizeIndex;
  AI: Ai;
  AI_ENRICHMENT_MODE: "on" | "off";
  MEMORY_API_KEY: string;
  MCP_ALLOWED_ORIGINS?: string;
}
