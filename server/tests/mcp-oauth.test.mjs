import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import { authorizeMcp, oauthChallenge, routeOAuth } from "../src/mcp-oauth.ts";
import worker from "../src/index.ts";

function fixture() {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  const migrations = new URL("../migrations/", import.meta.url);
  for (const name of readdirSync(migrations).filter(name => name.endsWith(".sql")).sort()) db.exec(readFileSync(new URL(name, migrations), "utf8"));
  function prepare(sql, values = []) {
    return {
      bind: (...args) => prepare(sql, args),
      first: async () => db.prepare(sql).get(...values) ?? null,
      all: async () => ({ results: db.prepare(sql).all(...values) }),
      run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
      execute: () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
    };
  }
  const env = { MEMORY_API_KEY: "owner-secret", AI_ENRICHMENT_MODE: "off", DB: { prepare, batch: async (ops) => {
    db.exec("BEGIN"); try { const results = ops.map(op => op.execute()); db.exec("COMMIT"); return results; } catch (error) { db.exec("ROLLBACK"); throw error; }
  } } };
  const request = (path, options) => worker.fetch(new Request(`https://memory.example${path}`, options), env, { waitUntil() {} });
  const post = (path, values, extra = {}) => request(path, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded", ...extra }, body: new URLSearchParams(values) });
  return { db, env, request, post };
}
async function register(f, redirects = ["https://client.example/callback"]) {
  const response = await f.request("/oauth/register", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ client_name: "<client>", redirect_uris: redirects, token_endpoint_auth_method: "none" }) });
  assert.equal(response.status, 201); return response.json();
}
async function authorization(f, client, options = {}) {
  const verifier = "v".repeat(43);
  const challenge = Buffer.from(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))).toString("base64url");
  const params = new URLSearchParams({ client_id: client.client_id, redirect_uri: client.redirect_uris[0], response_type: "code", code_challenge_method: "S256", code_challenge: challenge, resource: "https://memory.example/mcp", state: "test-state" });
  const response = await f.request(`/oauth/authorize?${params}`);
  assert.equal(response.status, 200);
  const html = await response.text();
  assert.match(html, /&lt;client&gt;/); assert.doesNotMatch(html, /owner-secret/);
  const session = /name="session" value="([^"]+)"/.exec(html)[1];
  const cookie = response.headers.get("set-cookie").split(";")[0];
  const consent = await f.post("/oauth/authorize", { session, key: options.key ?? "owner-secret", decision: options.decision ?? "allow" }, { origin: options.origin ?? "https://memory.example", cookie });
  return { consent, verifier, cookie, session };
}
function exchange(f, client, code, verifier, extra = {}) {
  return f.post("/oauth/token", { grant_type: "authorization_code", client_id: client.client_id, redirect_uri: client.redirect_uris[0], code, code_verifier: verifier, ...extra });
}

test("database failures emit fixed diagnostics without exposing credentials", async () => {
  const f = fixture(), client = await register(f);
  const logs = [], original = console.error;
  console.error = (entry) => logs.push(JSON.parse(entry));
  try {
    const invalid = await exchange(f, client, "missing-code", "v".repeat(43));
    assert.equal(invalid.status, 400); assert.equal((await invalid.json()).error, "invalid_grant"); assert.deepEqual(logs, []);
    f.env.DB.prepare = () => { throw new Error("private-db-exception owner-secret query-secret body-secret token-secret"); };
    const token = "a".repeat(43);
    const denied = await f.request("/mcp?query-secret", { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: "body-secret" });
    assert.equal(denied.status, 401);
    assert.deepEqual(logs.splice(0), [{ event: "mcp_oauth_failed", operation: "authorize_mcp" }]);
    const unavailable = await f.request("/oauth/register?query-secret", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ client_name: "body-secret", redirect_uris: ["https://client.example/callback"] }) });
    assert.equal(unavailable.status, 503); assert.equal((await unavailable.json()).error, "temporarily_unavailable");
    assert.deepEqual(logs.splice(0), [{ event: "mcp_oauth_failed", endpoint: "/oauth/register" }]);
  } finally { console.error = original; f.db.close(); }
});

test("discovery, consent, PKCE, rotation, replay, revocation, and owner key rotation", async () => {
  const f = fixture(), client = await register(f);
  assert.equal((await (await f.request("/.well-known/oauth-protected-resource/mcp")).json()).resource, "https://memory.example/mcp");
  assert.match(oauthChallenge(new Request("https://memory.example/mcp")).headers.get("www-authenticate"), /oauth-protected-resource\/mcp/);
  const { consent, verifier } = await authorization(f, client);
  assert.equal(consent.status, 303);
  const location = new URL(consent.headers.get("location"));
  assert.equal(location.searchParams.get("state"), "test-state");
  const code = location.searchParams.get("code");
  assert.equal((await exchange(f, client, code, "w".repeat(43))).status, 400);
  assert.equal((await exchange(f, client, code, verifier, { resource: "https://other.example/mcp" })).status, 400);
  const issued = await (await exchange(f, client, code, verifier)).json();
  assert.equal((await exchange(f, client, code, verifier)).status, 400);
  const auth = token => authorizeMcp(new Request("https://memory.example/mcp", { headers: { authorization: `Bearer ${token}` } }), f.env);
  assert.equal(await auth(issued.access_token), true);
  const call = async (name, args) => {
    const response = await f.request("/mcp", { method: "POST", headers: { Authorization: `Bearer ${issued.access_token}`, "Content-Type": "application/json", Accept: "application/json, text/event-stream" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }) });
    assert.equal(response.status, 200);
    const { result } = await response.json();
    assert.equal(result.isError, undefined, JSON.stringify(result));
    return result.structuredContent;
  };
  const saved = await call("add_memory", { content: `Durable OAuth decision ${issued.access_token}`, projectId: "cloud-project", idempotencyKey: "oauth-save" });
  const stored = await call("getDocument", { documentId: saved.id });
  assert.equal(stored.content, "Durable OAuth decision [REDACTED]");
  assert.equal(stored.metadata.sm_project_id, "cloud-project");
  assert.equal((await f.request("/v3/session", { headers: { Authorization: `Bearer ${issued.access_token}` } })).status, 401);
  assert.equal(await authorizeMcp(new Request("https://memory.example/v3/session", { headers: { authorization: `Bearer ${issued.access_token}` } }), f.env), false);
  assert.equal(await authorizeMcp(new Request("https://other.example/mcp", { headers: { authorization: `Bearer ${issued.access_token}` } }), f.env), false);
  const refresh = value => f.post("/oauth/token", { grant_type: "refresh_token", client_id: client.client_id, refresh_token: value });
  const rotated = await (await refresh(issued.refresh_token)).json();
  assert.equal(await auth(rotated.access_token), true);
  assert.equal((await refresh(issued.refresh_token)).status, 400);
  assert.equal(await auth(rotated.access_token), false);
  const second = await authorization(f, client);
  const code2 = new URL(second.consent.headers.get("location")).searchParams.get("code");
  const token2 = await (await exchange(f, client, code2, second.verifier)).json();
  f.env.MEMORY_API_KEY = "rotated-key";
  assert.equal(await auth(token2.access_token), false);
  assert.equal((await refresh(token2.refresh_token)).status, 400);
  assert.equal(f.db.prepare("SELECT count(*) AS n FROM mcp_oauth_tokens WHERE hash = ?").get(token2.access_token).n, 0);
  f.db.close();
});

test("concurrent exchanges are one-time, revocation is client-bound, and missing migration fails closed", async () => {
  const f = fixture(), client = await register(f), other = await register(f);
  const { consent, verifier } = await authorization(f, client);
  const code = new URL(consent.headers.get("location")).searchParams.get("code");
  const responses = await Promise.all([exchange(f, client, code, verifier), exchange(f, client, code, verifier)]);
  assert.deepEqual(responses.map(r => r.status).sort(), [200, 400]);
  const tokens = await responses.find(r => r.status === 200).json();
  const auth = () => authorizeMcp(new Request("https://memory.example/mcp", { headers: { authorization: `Bearer ${tokens.access_token}` } }), f.env);
  assert.equal((await f.post("/oauth/revoke", { client_id: other.client_id, token: tokens.refresh_token })).status, 200);
  assert.equal(await auth(), true);
  assert.equal((await f.post("/oauth/revoke", { client_id: client.client_id, token: tokens.refresh_token })).status, 200);
  assert.equal(await auth(), false);
  f.db.exec("DROP TABLE mcp_oauth_clients");
  assert.equal((await f.request("/oauth/register", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ redirect_uris: ["https://client.example/callback"] }) })).status, 503);
  f.db.close();
});

test("concurrent refresh reuse revokes the whole token family", async () => {
  const f = fixture(), client = await register(f);
  const { consent, verifier } = await authorization(f, client);
  const code = new URL(consent.headers.get("location")).searchParams.get("code");
  const tokens = await (await exchange(f, client, code, verifier)).json();
  const refresh = () => f.post("/oauth/token", { grant_type: "refresh_token", client_id: client.client_id, refresh_token: tokens.refresh_token });
  const results = await Promise.all([refresh(), refresh()]);
  assert.deepEqual(results.map(r => r.status).sort(), [200, 400]);
  const rotated = await results.find(r => r.status === 200).json();
  assert.equal(await authorizeMcp(new Request("https://memory.example/mcp", { headers: { authorization: `Bearer ${rotated.access_token}` } }), f.env), false);
  f.db.close();
});

test("registration rate is atomic and pending exhaustion recovers while approved clients persist", async () => {
  const f = fixture();
  const registerFrom = ip => f.request("/oauth/register", { method: "POST", headers: { "content-type": "application/json", "cf-connecting-ip": ip }, body: JSON.stringify({ redirect_uris: ["https://client.example/callback"] }) });
  const burst = await Promise.all(Array.from({ length: 20 }, () => registerFrom("192.0.2.1")));
  assert.equal(burst.filter(r => r.status === 201).length, 10);
  assert.equal(burst.filter(r => r.status === 429).length, 10);
  const client = await register(f);
  assert.equal((await authorization(f, client)).consent.status, 303);
  f.db.prepare("UPDATE mcp_oauth_clients SET created_at = 1 WHERE id = ?").run(client.client_id);
  // Fill only the unapproved registration pool, simulating distributed senders.
  f.db.exec(`WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 990)
    INSERT INTO mcp_oauth_clients (id, name, redirects, created_at)
    SELECT 'attacker-' || x, 'attacker', '["https://client.example/callback"]', unixepoch() FROM n`);
  assert.equal((await registerFrom("192.0.2.2")).status, 503);
  f.db.exec("UPDATE mcp_oauth_clients SET created_at = 1 WHERE authorized_at IS NULL");
  assert.equal((await registerFrom("192.0.2.2")).status, 201);
  assert.ok(f.db.prepare("SELECT authorized_at FROM mcp_oauth_clients WHERE id = ?").get(client.client_id).authorized_at);
  assert.equal((await authorization(f, client)).consent.status, 303);
  assert.equal(f.db.prepare("SELECT count(*) AS n FROM mcp_oauth_registration_limits WHERE bucket = ?").get("192.0.2.1").n, 0);
  f.db.close();
});

test("browser OAuth endpoints expose CORS without weakening consent origin checks", async () => {
  const f = fixture();
  for (const path of ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-authorization-server", "/oauth/register", "/oauth/token", "/oauth/revoke"]) {
    const preflight = await f.request(path, { method: "OPTIONS", headers: { origin: "https://client.example", "access-control-request-method": path.startsWith("/.well-known/") ? "GET" : "POST", "access-control-request-headers": "content-type" } });
    assert.equal(preflight.status, 204, path);
    assert.equal(preflight.headers.get("access-control-allow-origin"), "*");
  }
  assert.equal((await f.request("/.well-known/oauth-authorization-server")).headers.get("access-control-allow-origin"), "*");
  assert.equal((await f.post("/oauth/token", {})).headers.get("access-control-allow-origin"), "*");
  const client = await register(f);
  const forged = await authorization(f, client, { origin: "https://evil.example" });
  assert.equal(forged.consent.status, 400);
  assert.equal(forged.consent.headers.get("access-control-allow-origin"), null);
  f.db.close();
});

test("registration and consent reject untrusted redirects, invalid origins, and invalid credentials", async () => {
  const f = fixture();
  for (const redirect of ["http://example.com/cb", "https://192.168.1.2/cb", "https://client.example/cb#x", "https://x:pw@client.example/cb", "http://127.0.0.2/cb", "https://*.example/cb"]) {
    const response = await f.request("/oauth/register", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ redirect_uris: [redirect] }) });
    assert.equal(response.status, 400, redirect);
  }
  await register(f, ["http://127.0.0.1:1234/callback", "http://localhost:1234/callback", "http://[::1]:1234/callback"]);
  const client = await register(f);
  assert.equal((await authorization(f, client, { origin: "https://evil.example" })).consent.status, 400);
  assert.equal((await authorization(f, client, { key: "wrong" })).consent.status, 403);
  const denied = await authorization(f, client, { decision: "deny" });
  assert.equal(new URL(denied.consent.headers.get("location")).searchParams.get("error"), "access_denied");
  assert.equal(f.db.prepare("SELECT count(*) AS n FROM mcp_oauth_codes").get().n, 0);
  f.db.close();
});
