import { cases, startMetrics, collectedMetrics } from "./test.mjs";
import { bindStorage } from "./sqlite.mjs";
import worker from "../../src/index.ts";
import { fixture, output } from "../worker-fixture.mjs";
import "../auth.test.mjs";
import "../topics.test.mjs";
import "../races.test.mjs";
import "../mcp-oauth.test.mjs";
import "../remote-mcp.test.mjs";
import "../organization.test.mjs";
import "../organization-sources.test.mjs";
import "../organization-core.test.mjs";
import "../organization-core-queues.test.mjs";
import "../organization-core-recovery.test.mjs";
import "../organization-core-conflicts.test.mjs";
class TestDatabase {
  constructor(ctx) {
    this.ctx = ctx;
    this.fixture = null;
    this.requests = [];
    this.metrics = { embeddings: 0, vectorQueries: 0, responseBytes: 0 };
    this.vectors = /* @__PURE__ */ new Map();
    this.controls = {};
  }
  async fetch(request) {
    bindStorage(this.ctx);
    const url = new URL(request.url);
    if (url.pathname === "/__run") {
      const c = cases[Number(url.searchParams.get("case"))];
      const logs = [];
      startMetrics();
      const old = { log: console.log, error: console.error, warn: console.warn };
      for (const key of Object.keys(old)) console[key] = (...v) => logs.push({ level: key, values: v.map(String) });
      let timer;
      try {
        await Promise.race([c.run(), new Promise((_, reject) => {
          timer = setTimeout(() => reject(Error(`Test exceeded ${c.timeout}ms`)), c.timeout);
        })]);
        return Response.json({ name: c.name, status: "passed", metrics:collectedMetrics(), logs });
      } catch (error) {
        return Response.json({ name: c.name, status: "failed", error: String(error), stack: error.stack, metrics:collectedMetrics(), logs });
      } finally {
        clearTimeout(timer);
        Object.assign(console, old);
      }
    }
    if (!this.fixture) {
      this.fixture = fixture();
      this.fixture.env.MEMORY_API_KEY = "native-fixture-key";
      const ai = this.fixture.env.AI;
      this.fixture.env.AI = { run: async (model, input) => {
        if (input.text) {
          this.metrics.embeddings++;
          if (this.controls.failAI) throw Error("injected-ai-unavailable");
        }
        return ai.run(model, input);
      } };
      this.fixture.env.MEMORY_VECTORS = { upsert: async (rows) => {
        for (const row of rows) this.vectors.set(row.id, row);
        return { mutationId: "local-vector" };
      }, deleteByIds: async (ids) => {
        for (const id of ids) this.vectors.delete(id);
        return {};
      }, getByIds: async (ids) => ids.flatMap((id) => this.vectors.has(id) ? [this.vectors.get(id)] : []), query: async (_values, options) => {
        this.metrics.vectorQueries++;
        if (this.controls.failVector) throw Error("injected-vector-unavailable");
        return { matches: [...this.vectors.values()].filter((row) => !options?.namespace || row.namespace === options.namespace).map((row) => ({ id: row.id, score: row.score ?? 0.99, metadata: row.metadata })).slice(0, options?.topK ?? 20), count: this.vectors.size };
      } };
    }
    if (url.pathname === "/__configure") {
      const options = await request.json();
      if (options.AI_ENRICHMENT_MODE !== void 0) this.fixture.env.AI_ENRICHMENT_MODE = options.AI_ENRICHMENT_MODE;
      Object.assign(this.controls, options);
      if(options.checkpoint)this.fixture.respond(async()=>output(options.checkpoint));
      if (options.resetMetrics) {
        this.requests = [];
        this.metrics = { embeddings: 0, vectorQueries: 0, responseBytes: 0 };
      }
      return Response.json({ ok: true });
    }
    if(url.pathname === '/__vectors'){
      const {upsert=[],deleteIds=[]}=await request.json();
      for(const id of deleteIds)this.vectors.delete(id);
      for(const row of upsert)this.vectors.set(row.id,row);
      return Response.json({ok:true});
    }
    if (url.pathname === "/__sql") {
      const { sql, params = [] } = await request.json();
      if (/^\s*(SELECT|WITH|PRAGMA)\b/i.test(sql)) return Response.json({ results: this.fixture.db.prepare(sql).all(...params) });
      return Response.json(this.fixture.db.prepare(sql).run(...params));
    }
    if (url.pathname === "/__reset_metrics") {
      this.requests = [];
      this.metrics = { embeddings: 0, vectorQueries: 0, responseBytes: 0 };
      return Response.json({ ok: true });
    }
    if (url.pathname === "/__state") return Response.json({ requests: this.requests, metrics: this.metrics, vectors: [...this.vectors.values()] });
    if (url.pathname === "/__drain") return Response.json(await this.fixture.drain());
    let body;
    try {
      if (request.body) body = await request.clone().json();
    } catch {
    }
    const requestRecord={path:url.pathname,method:request.method,status:500,...(body===undefined?{}:{body})};
    this.requests.push(requestRecord);
    const reply=response=>{requestRecord.status=response.status;return response;};
    if (url.pathname === "/v4/search" && body?.allContainers && this.controls.transportStatus) return reply(Response.json({ error: "injected-transport-error" }, { status: this.controls.transportStatus }));
    if(url.pathname==='/v3/documents/list'&&body?.recentTasks&&this.controls.recentStatus)return reply(Response.json({error:'injected-recent-error'},{status:this.controls.recentStatus}));
    let response;
    if (url.pathname === "/v4/search" && body?.allContainers && this.controls.legacy400) {
      const old = { ...body };
      delete old.allContainers;
      response = await worker.fetch(new Request(request.url, { method: "POST", headers: request.headers, body: JSON.stringify(old) }), this.fixture.env, this.fixture.ctx);
    } else response = await worker.fetch(request, this.fixture.env, this.fixture.ctx);
    if (url.pathname === "/v4/search" && response.ok && (this.controls.markerless || this.controls.legacy400)) {
      const result = await response.json();
      if (this.controls.markerless && body?.allContainers) {
        delete result.searchScope;
        delete result.spaceDiscoveryComplete;
        delete result.searchedContainers;
      }
      if (this.controls.legacy400) for (const row of result.results) {
        const saved = JSON.parse(this.fixture.db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(row.id).metadata_json);
        if (!saved.memoryIndex) {
          row.metadata = saved;
          delete row.searchExcerpt;
        }
      }
      response = Response.json(result, { status: response.status, headers: response.headers });
    }
    if(url.pathname==='/v3/documents'&&request.method==='POST'&&this.controls.captureFault){
      if(this.controls.captureFault==='http-failure')response=Response.json({error:{message:'fixture-http-failure'}},{status:500});
      if(this.controls.captureFault==='acknowledgement')response=Response.json({message:'fixture-ack-missing'});
    }
    if(url.pathname==='/v3/documents/list'&&body?.recentTasks&&response.ok&&this.controls.recentMarkerless){const value=await response.json();delete value.recentTasks;response=Response.json(value,{status:response.status,headers:response.headers});}    this.metrics.responseBytes += new TextEncoder().encode(await response.clone().text()).length;
    return reply(response);
  }
}
let fixtureGeneration;
var entry_default = { fetch(request, env) {
  const url = new URL(request.url);
  fixtureGeneration ??= crypto.randomUUID();
  if (url.pathname === "/__reset") {
    fixtureGeneration = crypto.randomUUID();
    return new Response("ok");
  }
  if (url.pathname === "/__cases") return Response.json(cases.map((c, i) => ({ name: c.name, index: i, timeout: c.timeout })));
  if (url.pathname === "/__health") return new Response("ok");
  const key = url.pathname === "/__run" ? `case-${url.searchParams.get("case")}-${url.searchParams.get("run")}` : "fixture-" + fixtureGeneration;
  return env.DB.get(env.DB.idFromName(key)).fetch(request);
} };
import "../log-regressions.mjs";
import "../review-fixes-e2e.mjs";
import "../opop-projection-e2e.mjs";
import "../structure-performance-e2e.mjs";
import "../opop-performance.mjs";
import "../recall-api.mjs";
import "../smoke.mjs";
import "./assertions.test.mjs";
export {
  TestDatabase,
  entry_default as default
};
import './foreign-keys.test.mjs';
