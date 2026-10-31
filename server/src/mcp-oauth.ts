// OAuth credentials are separate from the memory database's document identities.
const ACCESS_SECONDS = 3600;
const REFRESH_SECONDS = 30 * 86400;
const CODE_SECONDS = 300;
const PENDING_CLIENT_SECONDS = 86400;
const REGISTRATION_WINDOW_SECONDS = 3600;
const REGISTRATIONS_PER_WINDOW = 10;
const MAX_BODY = 16384;
const encoder = new TextEncoder();

type Client = { id: string; name: string; redirects: string };
type Grant = { client_id: string; resource: string; scope: string; key_fingerprint: string; family: string; expires_at: number; consumed: string | null; revoked: number };
type Code = Omit<Grant, "family" | "revoked"> & { redirect_uri: string; challenge: string };

function base64(bytes: Uint8Array): string {
  let value = "";
  for (const byte of bytes) value += String.fromCharCode(byte);
  return btoa(value).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function random(): string { return base64(crypto.getRandomValues(new Uint8Array(32))); }
async function hash(value: string): Promise<string> { return base64(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(value)))); }
async function fingerprint(env: Env): Promise<string> { return hash(`remnifold-oauth-key\0${env.MEMORY_API_KEY}`); }
async function signature(key: string, value: string): Promise<string> {
  const imported = await crypto.subtle.importKey("raw", encoder.encode(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return base64(new Uint8Array(await crypto.subtle.sign("HMAC", imported, encoder.encode(value))));
}
function equal(a: string, b: string): boolean {
  const x = encoder.encode(a), y = encoder.encode(b);
  let diff = x.length ^ y.length;
  for (let i = 0; i < Math.max(x.length, y.length); i++) diff |= (x[i] ?? 0) ^ (y[i] ?? 0);
  return diff === 0;
}
function now(): number { return Math.floor(Date.now() / 1000); }
function headers(): Record<string, string> { return { "cache-control": "no-store", "pragma": "no-cache", "x-content-type-options": "nosniff", "referrer-policy": "no-referrer" }; }
function json(value: unknown, status = 200): Response { return new Response(JSON.stringify(value), { status, headers: { ...headers(), "content-type": "application/json" } }); }
function error(code: string, status = 400): Response { return json({ error: code }, status); }
function resource(url: URL): string { return `${url.origin}/mcp`; }
function escapeHtml(value: string): string { return value.replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!); }
function html(body: string, status = 200, cookie?: string, redirect?: URL): Response {
  // フォーム POST の Origin を維持し、外部へのリファラー送信は抑止する。
  // Chromium はフォームの 303 遷移にも form-action を適用するため、検証済みの戻り先だけを加える。
  const formOrigins = redirect ? ["'self'", redirect.origin] : ["'self'"];
  // Grok の登録済み Cursor コールバックは www から正規ホストへ 308 転送される。
  if (redirect?.origin === "https://www.cursor.com" && redirect.pathname === "/agents/mcp/oauth/callback") formOrigins.push("https://cursor.com");
  const formTargets = formOrigins.join(" ");
  const h = new Headers({ ...headers(), "referrer-policy": "same-origin", "content-type": "text/html; charset=utf-8", "content-security-policy": `default-src 'none'; style-src 'unsafe-inline'; form-action ${formTargets}; base-uri 'none'; frame-ancestors 'none'` });
  if (cookie) h.set("set-cookie", cookie);
  return new Response(`<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Remnifold authorization</title><style>body{font:16px system-ui;max-width:38rem;margin:3rem auto;padding:0 1rem;line-height:1.5}label,input{display:block}input{box-sizing:border-box;width:100%;padding:.65rem;margin:.5rem 0 1rem}button{padding:.6rem 1rem;margin-right:.5rem}</style><main>${body}</main></html>`, { status, headers: h });
}
async function body(request: Request): Promise<string | null> {
  if (Number(request.headers.get("content-length") ?? 0) > MAX_BODY || !request.body) return null;
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = []; let size = 0;
  while (true) { const part = await reader.read(); if (part.done) break; size += part.value.length; if (size > MAX_BODY) { await reader.cancel(); return null; } chunks.push(part.value); }
  const bytes = new Uint8Array(size); let at = 0; for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length; }
  return new TextDecoder().decode(bytes);
}
async function form(request: Request): Promise<URLSearchParams | null> {
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/x-www-form-urlencoded")) return null;
  const data = await body(request); if (data === null) return null;
  const params = new URLSearchParams(data);
  for (const [key, value] of params) if (key.length > 100 || value.length > 4096 || params.getAll(key).length !== 1) return null;
  return params;
}
function validRedirect(input: string): boolean {
  if (input.length > 2048 || !input || input.includes("*")) return false;
  try {
    const u = new URL(input);
    if (u.username || u.password || u.hash || u.href !== input) return false;
    const host = u.hostname.toLowerCase();
    if (!/^(?:[a-z0-9.-]+|\[[a-f0-9:]+\])$/.test(host)) return false;
    const loopback = host === "localhost" || host === "127.0.0.1" || host === "[::1]";
    if (u.protocol === "http:") return loopback;
    if (u.protocol !== "https:") return false;
    // Refuse IP literals and local hostnames other than the explicit loopbacks.
    return loopback || (!host.includes(":") && !/^\d+(?:\.\d+)*$/.test(host) && host.includes(".") && !host.endsWith(".local") && !host.endsWith(".localhost") && !host.endsWith(".internal"));
  } catch { return false; }
}
async function client(env: Env, id: string): Promise<Client | null> {
  if (!id || id.length > 128) return null;
  return env.DB.prepare("SELECT id, name, redirects FROM mcp_oauth_clients WHERE id = ? AND (authorized_at IS NOT NULL OR created_at > ?)").bind(id, now() - PENDING_CLIENT_SECONDS).first<Client>();
}
function redirectAllowed(c: Client, uri: string): boolean { return (JSON.parse(c.redirects) as string[]).includes(uri); }
function bearer(request: Request): string { return /^Bearer ([A-Za-z0-9_-]{43})$/i.exec(request.headers.get("authorization") ?? "")?.[1] ?? ""; }
export async function authorizeMcp(request: Request, env: Env): Promise<boolean> {
  const token = bearer(request); if (!token || !env.MEMORY_API_KEY?.trim() || new URL(request.url).pathname !== "/mcp") return false;
  try {
    const row = await env.DB.prepare("SELECT * FROM mcp_oauth_tokens WHERE hash = ? AND kind = 'access'").bind(await hash(token)).first<Grant>();
    return !!row && !row.revoked && row.expires_at > now() && row.resource === resource(new URL(request.url)) && equal(row.key_fingerprint, await fingerprint(env));
  } catch { console.error(JSON.stringify({ event: "mcp_oauth_failed", operation: "authorize_mcp" })); return false; }
}
export function oauthChallenge(request: Request): Response {
  const uri = `${new URL(request.url).origin}/.well-known/oauth-protected-resource/mcp`;
  return new Response("Unauthorized", { status: 401, headers: { ...headers(), "www-authenticate": `Bearer resource_metadata="${uri}"` } });
}
async function cleanup(env: Env): Promise<void> {
  const cutoff = now() - REFRESH_SECONDS;
  await env.DB.batch([
    env.DB.prepare("DELETE FROM mcp_oauth_codes WHERE hash IN (SELECT hash FROM mcp_oauth_codes WHERE expires_at < ? LIMIT 100)").bind(now()),
    env.DB.prepare("DELETE FROM mcp_oauth_tokens WHERE hash IN (SELECT hash FROM mcp_oauth_tokens WHERE expires_at < ? LIMIT 100)").bind(cutoff),
    env.DB.prepare("DELETE FROM mcp_oauth_clients WHERE id IN (SELECT id FROM mcp_oauth_clients WHERE authorized_at IS NULL AND created_at <= ? LIMIT 100)").bind(now() - PENDING_CLIENT_SECONDS),
    env.DB.prepare("DELETE FROM mcp_oauth_registration_limits WHERE rowid IN (SELECT rowid FROM mcp_oauth_registration_limits WHERE window_start < ? LIMIT 100)").bind(now() - 2 * REGISTRATION_WINDOW_SECONDS),
  ]);
}
async function register(request: Request, env: Env): Promise<Response> {
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) return error("invalid_client_metadata");
  const text = await body(request); if (text === null) return error("invalid_client_metadata");
  let value: Record<string, unknown>; try { value = JSON.parse(text) as Record<string, unknown>; if (!value || Array.isArray(value) || typeof value !== "object") throw Error(); } catch { return error("invalid_client_metadata"); }
  const uris = value.redirect_uris;
  if (!Array.isArray(uris) || uris.length < 1 || uris.length > 10 || !uris.every(x => typeof x === "string" && validRedirect(x)) || new Set(uris).size !== uris.length || (value.token_endpoint_auth_method !== undefined && value.token_endpoint_auth_method !== "none") || (value.client_name !== undefined && (typeof value.client_name !== "string" || value.client_name.length > 200))) return error("invalid_client_metadata");
  if (value.grant_types !== undefined && (!Array.isArray(value.grant_types) || !value.grant_types.every(x => x === "authorization_code" || x === "refresh_token"))) return error("invalid_client_metadata");
  if (value.response_types !== undefined && (!Array.isArray(value.response_types) || !value.response_types.every(x => x === "code"))) return error("invalid_client_metadata");
  // Cloudflare supplies this header; never persist the address itself. Missing or
  // malformed addresses share one fallback bucket rather than bypassing limits.
  const address = request.headers.get("cf-connecting-ip") ?? "";
  const identity = address && address.length <= 64 && /^[0-9a-fA-F:.]+$/.test(address) ? address.toLowerCase() : "unknown";
  const bucket = await hash(`${new URL(request.url).origin}\0${identity}`);
  const window = Math.floor(now() / REGISTRATION_WINDOW_SECONDS) * REGISTRATION_WINDOW_SECONDS;
  const limit = await env.DB.prepare("INSERT INTO mcp_oauth_registration_limits (bucket, window_start, registrations) VALUES (?, ?, 1) ON CONFLICT(bucket, window_start) DO UPDATE SET registrations = registrations + 1 WHERE registrations < ?").bind(bucket, window, REGISTRATIONS_PER_WINDOW).run();
  if (!limit.meta.changes) return new Response(JSON.stringify({ error: "temporarily_unavailable" }), { status: 429, headers: { ...headers(), "content-type": "application/json", "retry-after": String(window + REGISTRATION_WINDOW_SECONDS - now()) } });
  const id = random(), name = typeof value.client_name === "string" && value.client_name.trim() ? value.client_name.trim() : "OAuth client";
  const result = await env.DB.prepare("INSERT INTO mcp_oauth_clients (id, name, redirects, created_at) SELECT ?, ?, ?, ? WHERE (SELECT count(*) FROM mcp_oauth_clients WHERE authorized_at IS NULL) < 1000").bind(id, name, JSON.stringify(uris), now()).run();
  if (!result.meta.changes) return error("temporarily_unavailable", 503);
  return json({ client_id: id, client_id_issued_at: now(), client_name: name, redirect_uris: uris, token_endpoint_auth_method: "none", grant_types: ["authorization_code", "refresh_token"], response_types: ["code"] }, 201);
}
async function authorizeGet(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url), p = url.searchParams;
  for (const [k, v] of p) if (k.length > 100 || v.length > 4096 || p.getAll(k).length !== 1) return error("invalid_request");
  const c = await client(env, p.get("client_id") ?? ""), redirect = p.get("redirect_uri") ?? "", challenge = p.get("code_challenge") ?? "";
  if (!c || !redirectAllowed(c, redirect) || p.get("response_type") !== "code" || p.get("code_challenge_method") !== "S256" || !/^[A-Za-z0-9_-]{43}$/.test(challenge) || (p.has("resource") && p.get("resource") !== resource(url)) || (p.has("scope") && p.get("scope") !== "memory")) return error("invalid_request");
  const nonce = random();
  const payload = base64(encoder.encode(JSON.stringify({ client: c.id, redirect, challenge, state: p.get("state"), nonce, exp: now() + CODE_SECONDS, resource: resource(url) })));
  const session = `${payload}.${await signature(env.MEMORY_API_KEY, payload)}`;
  const content = `<h1>Authorize Remnifold access</h1><p><strong>${escapeHtml(c.name)}</strong> requests read and write access to your memories.</p><p>Return address: ${escapeHtml(new URL(redirect).host)}</p><form method="post" action="/oauth/authorize"><input type="hidden" name="session" value="${session}"><label for="key">Memory API key</label><input id="key" name="key" type="password" autocomplete="off" required><button name="decision" value="allow">Allow</button><button name="decision" value="deny" formnovalidate>Deny</button></form>`;
  return html(content, 200, `mcp_oauth_csrf=${nonce}; Path=/oauth/authorize; Max-Age=${CODE_SECONDS}; HttpOnly; Secure; SameSite=Lax`, new URL(redirect));
}
async function authorizePost(request: Request, env: Env): Promise<Response> {
  const origin = new URL(request.url).origin;
  if (request.headers.get("origin") !== origin) return error("invalid_request");
  const p = await form(request), session = p?.get("session") ?? "", parts = session.split(".");
  if (!p || parts.length !== 2 || !parts[0] || !parts[1] || !equal(parts[1], await signature(env.MEMORY_API_KEY, parts[0]))) return error("invalid_request");
  let data: { client: string; redirect: string; challenge: string; state: string | null; nonce: string; exp: number; resource: string };
  try { data = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(parts[0].replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0)))); } catch { return error("invalid_request"); }
  const cookie = /(?:^|;\s*)mcp_oauth_csrf=([^;]*)/.exec(request.headers.get("cookie") ?? "")?.[1] ?? "";
  const c = await client(env, data.client);
  if (!c || !redirectAllowed(c, data.redirect) || data.exp < now() || data.resource !== `${origin}/mcp` || !cookie || !equal(cookie, data.nonce)) return error("invalid_request");
  const result = new URL(data.redirect);
  if (data.state !== null) result.searchParams.set("state", data.state);
  if (p.get("decision") === "deny") result.searchParams.set("error", "access_denied");
  else if (p.get("decision") === "allow" && equal(await hash(p.get("key") ?? ""), await hash(env.MEMORY_API_KEY))) {
    const code = random();
    const time = now();
    const results = await env.DB.batch([
      env.DB.prepare("UPDATE mcp_oauth_clients SET authorized_at = COALESCE(authorized_at, ?) WHERE id = ? AND (authorized_at IS NOT NULL OR created_at > ?)").bind(time, c.id, time - PENDING_CLIENT_SECONDS),
      env.DB.prepare("INSERT INTO mcp_oauth_codes (hash, client_id, redirect_uri, challenge, resource, scope, key_fingerprint, expires_at) SELECT ?, ?, ?, ?, ?, 'memory', ?, ? WHERE EXISTS (SELECT 1 FROM mcp_oauth_clients WHERE id = ? AND authorized_at IS NOT NULL)").bind(await hash(code), c.id, data.redirect, data.challenge, data.resource, await fingerprint(env), time + CODE_SECONDS, c.id),
    ]);
    if (!results[0]?.meta.changes || !results[1]?.meta.changes) return error("invalid_request");
    result.searchParams.set("code", code);
  } else return html("<h1>Authorization failed</h1><p>Unable to authorize this request. Start again from your application.</p>", 403);
  return new Response(null, { status: 303, headers: { ...headers(), location: result.href, "set-cookie": "mcp_oauth_csrf=; Path=/oauth/authorize; Max-Age=0; HttpOnly; Secure; SameSite=Lax" } });
}
async function issue(env: Env, grant: Grant | Code, family: string, marker: string, source: "code" | "refresh", sourceHash: string): Promise<Response> {
  const access = random(), refresh = random(), timestamp = now();
  const table = source === "code" ? "mcp_oauth_codes" : "mcp_oauth_tokens";
  const update = env.DB.prepare(`UPDATE ${table} SET consumed = ? WHERE hash = ? AND consumed IS NULL AND expires_at > ?${source === "refresh" ? " AND revoked = 0 AND kind = 'refresh'" : ""}`).bind(marker, sourceHash, timestamp);
  const insert = (tokenHash: string, kind: string, expiry: number) => env.DB.prepare(`INSERT INTO mcp_oauth_tokens (hash, family, kind, client_id, resource, scope, key_fingerprint, expires_at) SELECT ?, ?, ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM ${table} WHERE hash = ? AND consumed = ?)`).bind(tokenHash, family, kind, grant.client_id, grant.resource, grant.scope, grant.key_fingerprint, expiry, sourceHash, marker);
  const results = await env.DB.batch([update, insert(await hash(access), "access", timestamp + ACCESS_SECONDS), insert(await hash(refresh), "refresh", timestamp + REFRESH_SECONDS)]);
  if (!results[0]?.meta.changes) {
    if (source === "refresh") await env.DB.prepare("UPDATE mcp_oauth_tokens SET revoked = 1 WHERE family = ?").bind(family).run();
    return error("invalid_grant");
  }
  return json({ access_token: access, token_type: "Bearer", expires_in: ACCESS_SECONDS, refresh_token: refresh, scope: grant.scope });
}
async function token(request: Request, env: Env): Promise<Response> {
  const p = await form(request); if (!p) return error("invalid_request");
  const c = await client(env, p.get("client_id") ?? ""); if (!c) return error("invalid_client", 401);
  const expected = resource(new URL(request.url));
  if (p.has("resource") && p.get("resource") !== expected) return error("invalid_target");
  const key = await fingerprint(env);
  if (p.get("grant_type") === "authorization_code") {
    const code = p.get("code") ?? "", verifier = p.get("code_verifier") ?? "", redirect = p.get("redirect_uri") ?? "";
    if (!/^[A-Za-z0-9_-]{43}$/.test(code) || !/^[A-Za-z0-9._~-]{43,128}$/.test(verifier)) return error("invalid_grant");
    const digest = await hash(code), row = await env.DB.prepare("SELECT * FROM mcp_oauth_codes WHERE hash = ?").bind(digest).first<Code>();
    if (!row || row.client_id !== c.id || row.redirect_uri !== redirect || row.resource !== expected || row.expires_at <= now() || !equal(row.key_fingerprint, key) || !equal(row.challenge, await hash(verifier))) return error("invalid_grant");
    return issue(env, row, random(), random(), "code", digest);
  }
  if (p.get("grant_type") === "refresh_token") {
    const value = p.get("refresh_token") ?? ""; if (!/^[A-Za-z0-9_-]{43}$/.test(value)) return error("invalid_grant");
    const digest = await hash(value), row = await env.DB.prepare("SELECT * FROM mcp_oauth_tokens WHERE hash = ? AND kind = 'refresh'").bind(digest).first<Grant>();
    if (!row || row.client_id !== c.id || row.resource !== expected || row.expires_at <= now() || !equal(row.key_fingerprint, key)) return error("invalid_grant");
    if (row.consumed || row.revoked) { await env.DB.prepare("UPDATE mcp_oauth_tokens SET revoked = 1 WHERE family = ?").bind(row.family).run(); return error("invalid_grant"); }
    return issue(env, row, row.family, random(), "refresh", digest);
  }
  return error("unsupported_grant_type");
}
async function revoke(request: Request, env: Env): Promise<Response> {
  const p = await form(request); if (!p) return error("invalid_request");
  const c = await client(env, p.get("client_id") ?? ""); if (!c) return error("invalid_client", 401);
  const value = p.get("token") ?? "";
  if (/^[A-Za-z0-9_-]{43}$/.test(value)) {
    const row = await env.DB.prepare("SELECT family FROM mcp_oauth_tokens WHERE hash = ? AND client_id = ?").bind(await hash(value), c.id).first<{ family: string }>();
    if (row) await env.DB.prepare("UPDATE mcp_oauth_tokens SET revoked = 1 WHERE family = ?").bind(row.family).run();
  }
  return new Response(null, { status: 200, headers: headers() });
}
async function routeOAuthInner(request: Request, env: Env): Promise<Response | null> {
  const url = new URL(request.url), path = url.pathname, origin = url.origin;
  if (path === "/.well-known/oauth-protected-resource" || path === "/.well-known/oauth-protected-resource/mcp") return json({ resource: resource(url), authorization_servers: [origin], bearer_methods_supported: ["header"], scopes_supported: ["memory"] });
  if (path === "/.well-known/oauth-authorization-server") return json({ issuer: origin, authorization_endpoint: `${origin}/oauth/authorize`, token_endpoint: `${origin}/oauth/token`, registration_endpoint: `${origin}/oauth/register`, revocation_endpoint: `${origin}/oauth/revoke`, response_types_supported: ["code"], grant_types_supported: ["authorization_code", "refresh_token"], token_endpoint_auth_methods_supported: ["none"], revocation_endpoint_auth_methods_supported: ["none"], code_challenge_methods_supported: ["S256"], scopes_supported: ["memory"] });
  if (!["/oauth/register", "/oauth/authorize", "/oauth/token", "/oauth/revoke"].includes(path)) return null;
  if (!env.MEMORY_API_KEY?.trim() || !env.DB) return error("temporarily_unavailable", 503);
  if (url.protocol !== "https:") return error("invalid_request");
  try {
    if (path === "/oauth/register" && request.method === "POST") { await cleanup(env); return await register(request, env); }
    if (path === "/oauth/authorize" && request.method === "GET") return await authorizeGet(request, env);
    if (path === "/oauth/authorize" && request.method === "POST") { await cleanup(env); return await authorizePost(request, env); }
    if (path === "/oauth/token" && request.method === "POST") return await token(request, env);
    if (path === "/oauth/revoke" && request.method === "POST") return await revoke(request, env);
    return error("invalid_request", 405);
  } catch { console.error(JSON.stringify({ event: "mcp_oauth_failed", endpoint: path })); return error("temporarily_unavailable", 503); }
}
export async function routeOAuth(request: Request, env: Env): Promise<Response | null> {
  const path = new URL(request.url).pathname;
  const metadata = path === "/.well-known/oauth-protected-resource" || path === "/.well-known/oauth-protected-resource/mcp" || path === "/.well-known/oauth-authorization-server";
  const api = path === "/oauth/register" || path === "/oauth/token" || path === "/oauth/revoke";
  if ((metadata || api) && request.method === "OPTIONS") return new Response(null, { status: 204, headers: { ...headers(), "access-control-allow-origin": "*", "access-control-allow-methods": metadata ? "GET, OPTIONS" : "POST, OPTIONS", "access-control-allow-headers": "content-type, authorization", "access-control-max-age": "600" } });
  const response = await routeOAuthInner(request, env);
  if (response && (metadata || api)) response.headers.set("access-control-allow-origin", "*");
  return response;
}
