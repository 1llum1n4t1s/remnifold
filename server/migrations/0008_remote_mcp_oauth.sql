CREATE TABLE IF NOT EXISTS mcp_oauth_clients (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, redirects TEXT NOT NULL, created_at INTEGER NOT NULL,
  authorized_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_mcp_oauth_clients_pending ON mcp_oauth_clients(authorized_at, created_at);
CREATE TABLE IF NOT EXISTS mcp_oauth_registration_limits (
  bucket TEXT NOT NULL, window_start INTEGER NOT NULL, registrations INTEGER NOT NULL,
  PRIMARY KEY (bucket, window_start)
);
CREATE INDEX IF NOT EXISTS idx_mcp_oauth_registration_limits_window ON mcp_oauth_registration_limits(window_start);
CREATE TABLE IF NOT EXISTS mcp_oauth_codes (
  hash TEXT PRIMARY KEY, client_id TEXT NOT NULL, redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL, resource TEXT NOT NULL, scope TEXT NOT NULL,
  key_fingerprint TEXT NOT NULL, expires_at INTEGER NOT NULL, consumed TEXT
);
CREATE TABLE IF NOT EXISTS mcp_oauth_tokens (
  hash TEXT PRIMARY KEY, family TEXT NOT NULL, kind TEXT NOT NULL,
  client_id TEXT NOT NULL, resource TEXT NOT NULL, scope TEXT NOT NULL,
  key_fingerprint TEXT NOT NULL, expires_at INTEGER NOT NULL, consumed TEXT,
  revoked INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_mcp_oauth_tokens_family ON mcp_oauth_tokens(family);
CREATE INDEX IF NOT EXISTS idx_mcp_oauth_codes_expiry ON mcp_oauth_codes(expires_at);
CREATE INDEX IF NOT EXISTS idx_mcp_oauth_tokens_expiry ON mcp_oauth_tokens(expires_at);
