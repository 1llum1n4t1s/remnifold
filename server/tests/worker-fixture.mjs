// 認証済みWorkerと移行済みSQLiteを使う検証の共通fixture。シナリオは呼び出し元に置く。
import test, { assert } from "./support/test.mjs";
import { migrations, applyMigrations } from "./support/migrations.mjs";

import { DatabaseSync } from "./support/sqlite.mjs";
import worker from "../src/index.ts";
import { adapter } from "./d1-adapter.mjs";

export { migrations };
export const checkpoint = { title: "Checkpoint", overview: "Verified restart state.", verified: ["Sources stored."], unverified: [], unresolved: [], nextActions: [] };
export const output = (value, finish_reason = "stop") => ({ choices: [{ finish_reason, message: { content: typeof value === "string" ? value : JSON.stringify(value) } }] });
export const embedding = Array.from({ length: 1024 }, (_, index) => index === 0 ? 1 : 0);

export function scenarioWithLogs(name, run) { test(name, async () => {
 const f=fixture(); const logs=[]; const original={log:console.log,error:console.error,warn:console.warn};
 const capture=(...values)=>{for(const value of values){try{logs.push(JSON.parse(value))}catch{}}};Object.keys(original).forEach(k=>console[k]=capture);
 try { await run(f, logs); } catch(error) { original.error(JSON.stringify({test:name,events:logs})); throw error; } finally { await f.drain(); Object.assign(console,original); f.db.close(); }
}); }

export function fixture({ beforeMigration } = {}) {
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  if (beforeMigration) {
    for (const migration of migrations) if (migration.name < beforeMigration) db.exec(migration.sql);
  } else applyMigrations(db);
  const DB = adapter(db);
  const waits = [];
  const ctx = { waitUntil(promise) { waits.push(Promise.resolve(promise).then(() => ({ status: "fulfilled" }), (error) => ({ status: "rejected", error: String(error) }))); } };
  const calls = [];
  let respond = async () => output(checkpoint);
  const env = { DB, MEMORY_API_KEY: "local-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      if (input.text) return { data: [embedding] };
      if (input.response_format?.json_schema?.name === "memory_consolidation") { calls.push(input); return respond(input, calls.length); }
      return output({ facts: [{ subject: "Source", predicate: "has", object: "evidence", confidence: 1, exclusive: false }], topics: ["Validation"] });
    } },
    MEMORY_VECTORS: { upsert: async () => ({ mutationId: "local-vector" }), deleteByIds: async () => ({}), getByIds: async () => [] },
  };
  const api = async (path, body, method = "POST") => {
    const response = await worker.fetch(new Request(`https://local.example${path}`, { method,
      headers: { Authorization: "Bearer local-fixture", "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) }), env, ctx);
    return { status: response.status, body: response.status === 204 ? null : await response.json() };
  };
  return { db, DB, env, ctx, calls, api, metrics: {}, respond(callback) { respond = callback; },
    drain: async () => { const results = []; while (waits.length) results.push(...await Promise.all(waits.splice(0))); return results; },
    seed: async (count = 2) => { const ids = []; env.AI_ENRICHMENT_MODE = "off";
      for (let i = 0; i < count; i++) { const saved = await api("/v3/documents", { containerTag: "memories", customId: `source-${i}`, content: `Verified source ${i}`, metadata: { sm_project_id: "log-project" } }); assert.equal(saved.status, 201, JSON.stringify(saved.body)); ids.push(saved.body.id); }
      env.AI_ENRICHMENT_MODE = "on"; return ids; },
    consolidate: () => api("/v4/consolidate", { projectId: "log-project", force: true }),
    active: () => db.prepare("SELECT * FROM memory_consolidations WHERE status = 'active'").all(),
    lease: () => db.prepare("SELECT lease_token FROM memory_consolidation_projects").get()?.lease_token,
  };
}
