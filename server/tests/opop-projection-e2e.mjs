const utf8Bytes=value=>new TextEncoder().encode(value).length;
// 失敗条件: SQL/JSの文字数差・NULで本文/ellipsis/AI入力が変わる、scope/同時刻順序/40件境界が変わる、
// metadata/embeddingが欠落する、PATCHの生成物/競合ガードが弱まる、取得量が減らない。
// 再強化のembedding/facts/topics入力が変わる、非同期中の版更新/forget/除外で古い結果を保存する、保存全文を切り詰める。
// 既存の長いcheckpoint引継ぎでAIのpreviousCheckpoint/累積membership/置換動作が変わる。
// 実行: cargo xtask server-test
import test, { assert, recordMetrics } from "./support/test.mjs";
import { DatabaseSync } from "./support/sqlite.mjs";
import observed from "./support/observed.mjs";
import projectionGoldens from "./support/projection-goldens.json" with {type:"json"};
import {normalizeFixtureResult} from "./support/golden.mjs";
import {documentExpected} from "./support/expected.mjs";
import { migrations, applyMigrations } from "./support/migrations.mjs";

const timestamp = "2026-10-21T01:00:00.000Z";
const embedding = JSON.stringify(Array.from({ length: 1024 }, (_, index) => Math.sin(index + 1)));
const privateFilters = { AND: [{ key: "sm_scope", filterType: "metadata", value: "private" }] };

async function bounded(promise, description) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${description} exceeded 10 seconds`)), 10_000); })]);
  } finally { clearTimeout(timer); }
}

function adapter(db) {
  const reads = [];
  let beforeBatch;
  function prepare(sql) {
    const bind = (values = []) => {
      const select = () => {
        const rows = db.prepare(sql).all(...values);
        reads.push({ sql, rows: rows.length, rowBytes: rows.reduce((sum, row) => sum + utf8Bytes(JSON.stringify(row)), 0),
          records:rows, embeddings: rows.filter((row) => typeof row.embedding_json === "string").length });
        return rows;
      };
      return { bind: (...next) => bind(next), first: async () => select()[0], all: async () => ({ results: select() }),
        run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
        execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
          ? { results: select(), meta: { changes: 0 } }
          : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
      };
    };
    return bind();
  }
  return { reads, prepare, beforeBatch(callback) { beforeBatch = callback; }, batch: async (operations) => {
    const callback = beforeBatch; beforeBatch = undefined; callback?.();
    return db.transaction(() => operations.map(operation=>operation.execute()));
  } };
}

function fixture(module) {
  module.resetFixtureIds();
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  applyMigrations(db);
  const DB = adapter(db);
  const seeds=new Map();
  const calls = [];
  const aiCalls = [];
  const vectorCalls = [];
  const waits = [];
  const checkpoint = { title: "Checkpoint", overview: "Preserved evidence.", verified: ["Source is recorded."], unverified: [], unresolved: [], nextActions: [] };
  const env = { DB, MEMORY_API_KEY: "projection-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      aiCalls.push({ model: _model, input });
      if (input.text) return { data: [JSON.parse(embedding)] };
      if (input.response_format?.json_schema?.name === "memory_consolidation") calls.push(input);
      return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify(input.response_format?.json_schema?.name === "memory_consolidation" ? checkpoint : { facts: [], topics: ["Evidence"] }) } }] };
    } },
    MEMORY_VECTORS: { upsert: async (vectors) => { vectorCalls.push({ operation: "upsert", vectors }); return { mutationId: "fixture-vector" }; },
      deleteByIds: async (ids) => { vectorCalls.push({ operation: "delete", ids }); return {}; }, getByIds: async () => [] },
  };
  const ctx = { waitUntil(promise) { waits.push(promise); } };
  const api = async (path, body, method = "POST", authenticated = true) => {
    const start = DB.reads.length;
    const response = await bounded(module.default.fetch(new Request(`https://fixture.example${path}`, { method,
      headers: { ...(authenticated ? { Authorization: "Bearer projection-fixture" } : {}), "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), env, ctx), `${method} ${path}`);
    const result = { status: response.status, body: await response.json() };
    while (waits.length) await bounded(Promise.all(waits.splice(0)), `waitUntil ${path}`);
    return { ...result, reads: DB.reads.slice(start) };
  };
  function seed(id, content, { container = "memories", metadata = {}, forgotten = 0, revision = `revision-${id}` } = {}) {
    seeds.set(id,{id,content,container,metadata:structuredClone(metadata),revision});
    db.prepare("INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)").run(container, container, timestamp, timestamp);
    db.prepare(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, created_at, updated_at,
      topic_revision, embedding_json, embedding_status, fact_status, topic_status, vector_status, entity_context, is_forgotten)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'done', 'done', 'done', 'pending', ?, ?)`).run(
      id, id, container, content, JSON.stringify(metadata), timestamp, timestamp, revision, embedding, "large context ".repeat(1000), forgotten);
  }
  return { db, DB, env, calls, aiCalls, vectorCalls, api, seed, seeds };
}

const sqlPrefix=(text,limit)=>text.includes('\0')?text:[...text].slice(0,limit).join('');
function verifyReadContract(name,result,f){
 if(!result?.reads)return;
 assert.equal(result.reads.length,1,`${name}: exactly one projected read`);
 const read=result.reads[0];let expected;
 if(name.startsWith('profile UTF'))expected=[{content:sqlPrefix(f.seeds.get('profile').content,1201)}];
 else if(name.startsWith('profile scope'))expected=[1,2,3,4,5].map(i=>({content:sqlPrefix(f.seeds.get(`tie-${i}`).content,1201)}));
 else if(name.startsWith('consolidation ')){
  const count=Number(name.split(' ')[1]);expected=Array.from({length:Math.min(count,40)},(_,i)=>f.seeds.get(`source-${String(i).padStart(2,'0')}`)).map(row=>({id:row.id,containerTag:row.container,content:sqlPrefix(row.content,5000),metadataJson:JSON.stringify(row.metadata),createdAt:timestamp,sourceRevision:row.revision}));
 }else if(name.startsWith('prior checkpoint')){const row=f.seeds.get('old-checkpoint');expected=[{id:'prior-consolidation',memoryId:row.id,content:sqlPrefix(row.content,20000),metadataJson:JSON.stringify(row.metadata),revision:1}];}
 else if(name.startsWith('PATCH ')){const row=f.seeds.get('patch');expected=[{container_tag:row.container,content:row.content,metadata_json:JSON.stringify(row.metadata),updated_at:timestamp,topic_revision:row.revision}];}
 else if(name.startsWith('enrichment AI')){const row=f.seeds.get('enrich');expected=[{id:row.id,container_tag:row.container,content:sqlPrefix(row.content,60000),metadata_json:JSON.stringify(row.metadata),updated_at:timestamp,topic_revision:row.revision}];}
 else throw Error(`Missing projection expectation: ${name}`);
 assert.deepEqual(read.records,expected,`${name}: selected columns and exact projected rows`);
 assert.equal(read.rows,expected.length,`${name}: row count`);assert.equal(read.embeddings,0);
 assert.equal(read.rowBytes,expected.reduce((sum,row)=>sum+utf8Bytes(JSON.stringify(row)),0),`${name}: exact transferred row bytes`);
 assert.ok(!/SELECT\s+(?:\*|[a-z]+\.\*)/iu.test(read.sql),`${name}: full-row projection is forbidden`);
}function compare(name,run){test(name,async()=>{const f=fixture(observed);try{const result=await run(f);recordMetrics(name,{operations:observed.counts(),sqlReads:(result.reads??[]).map(({sql,rows,rowBytes,embeddings})=>({sql,rows,rowBytes,embeddings}))});verifyReadContract(name,result,f);assert.deepEqual(normalizeFixtureResult(result.public,f.seeds,embedding),projectionGoldens[name],`${name}: complete public response, AI/vector effects and persisted state`)}finally{f.db.close()}})}const contents = ["short", "a".repeat(1200), "界".repeat(1201), "a".repeat(1199) + "😀tail", "😀".repeat(600),
  "😀".repeat(601), "界".repeat(200_000), "a".repeat(4999) + "😀" + "z".repeat(20_000), "before\0" + "界".repeat(10_000),
  "a".repeat(5500) + "\0tail", "q".repeat(200_000)];

const profileExpected=['short','a'.repeat(1200),'界'.repeat(1200)+'…','a'.repeat(1199)+'…','😀'.repeat(600),'😀'.repeat(600)+'…','界'.repeat(1200)+'…','a'.repeat(1200)+'…','before\0'+'界'.repeat(1193)+'…','a'.repeat(1200)+'…','q'.repeat(1200)+'…'];
// Scalar iteration gives an independent UTF-16 boundary oracle for these transport fixtures.
function prefixWithin(text,limit){let result='';for(const scalar of text){if(result.length+scalar.length>limit)break;result+=scalar}return result}for (const [index, content] of contents.entries()) compare(`profile UTF-16/NUL boundary ${index}`, async (f) => {
  f.seed("profile", content, { metadata: { padding: "m".repeat(20_000), sm_scope: "private" } });
  const result = await f.api("/v4/profile", { containerTag: "memories", q: "  ", filters: privateFilters });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.deepEqual(result.body, {profile:{static:[],dynamic:[profileExpected[index]]}});
  return { public: result.body, reads: result.reads.filter((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)) };
});

compare("profile scope, forgotten filtering and tied order", async (f) => {
  for (let i = 0; i < 9; i++) f.seed(`tie-${i}`, `${i}:` + "evidence ".repeat(1000),
    { container: i === 8 ? "other" : "memories", forgotten: i === 7 ? 1 : 0,
      metadata: { sm_scope: i === 0 ? "public" : "private", padding: "m".repeat(5000) } });
  const result = await f.api("/v4/profile", { containerTag: "memories", filters: privateFilters });
  assert.equal(result.body.profile.dynamic.length, 5);
  assert.ok(result.body.profile.dynamic.every((value) => /^[1-5]:/u.test(value)));
  return { public: result.body, reads: result.reads.filter((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)) };
});

compare("profile facts and query branches remain unchanged", async (f) => {
  f.seed("fact-source", "Searchable technical evidence.", { metadata: { sm_scope: "private" } });
  f.db.prepare(`INSERT INTO facts(id, container_tag, source_memory_id, subject, predicate, object, subject_key, predicate_key,
    object_key, is_exclusive, confidence, status, created_at, updated_at) VALUES ('fact', 'memories', 'fact-source', 'Subject', 'has', 'evidence', 'subject', 'has', 'evidence', 0, 1, 'active', ?, ?)`).run(timestamp, timestamp);
  const facts = await f.api("/v4/profile", { containerTag: "memories" });
  assert.deepEqual(facts.body.profile.dynamic, ["Subject has evidence"]);
  assert.ok(!facts.reads.some((read) => /ORDER BY updated_at DESC LIMIT/u.test(read.sql)));
  const search = await f.api("/v4/profile", { containerTag: "memories", q: "technical" });
  const { timing: _timing, ...searchResults } = search.body.searchResults;
  return { public: { facts: facts.body, search: { ...search.body, searchResults } } };
});

for (const count of [4, 41]) compare(`consolidation ${count} source AI input, revisions and membership`, async (f) => {
  for (let i = 0; i < count; i++) f.seed(`source-${String(i).padStart(2, "0")}`, contents[(i + 6) % contents.length],
    { container: i % 2 ? "legacy" : "memories", metadata: { sm_project_id: "projection-project", title: `Source ${i}` } });
  f.seed("unrelated", "Do not include", { metadata: { sm_project_id: "other-project" } });
  f.seed("forgotten", "Do not include", { forgotten: 1, metadata: { sm_project_id: "projection-project" } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "projection-project", force: true });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(f.calls.length, 1);
  const records = JSON.parse(f.calls[0].messages[1].content).newMemoryRecords;
  assert.equal(records.length, Math.min(40, count));
  for(const record of records){const i=Number(record.id.slice(7));assert.equal(record.content,prefixWithin(contents[(i+6)%contents.length],count===4?5000:2000));assert.equal(record.containerTag,i%2?"legacy":"memories");}
  const membership = f.db.prepare("SELECT memory_id, source_revision FROM memory_consolidation_sources ORDER BY memory_id").all();
  assert.deepEqual(membership.map((row) => row.memory_id), records.map((record) => record.id));
  const persisted = f.db.prepare("SELECT content, embedding_json FROM memories WHERE id LIKE 'source-%' ORDER BY id").all();
  assert.ok(persisted.every((row) => row.embedding_json === embedding));
  return { public: { response: result.body, calls: f.calls, membership, persisted },
    reads: result.reads.filter((read) => read.sql.includes("AS sourceRevision") && read.sql.includes("LIMIT ?")) };
});

compare("consolidation rejects a source revised during AI generation", async (f) => {
  f.seed("source", "界".repeat(20_000), { metadata: { sm_project_id: "racing-project" } });
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    if (input.response_format?.json_schema?.name === "memory_consolidation") {
      f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'source'").run();
    }
    return run(model, input);
  };
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "racing-project", force: true });
  assert.equal(result.status, 409, JSON.stringify(result.body));
  assert.equal(result.body.error.message, "Source memories changed during consolidation");
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources").get().count, 0);
  return { public: { response: result.body, calls: f.calls } };
});

const priorContents = ["Previous evidence.", "x".repeat(20_000), "界".repeat(20_001), "x".repeat(19_999) + "😀tail",
  "😀".repeat(10_001), "界".repeat(120_000), "before\0" + "x".repeat(100_000), "x".repeat(25_000) + "\0tail"];
for (const [index, content] of priorContents.entries()) compare(`prior checkpoint AI input and cumulative membership boundary ${index}`, async (f) => {
  const metadata = { sm_project_id: "prior-project", sm_scope: "private" };
  f.seed("old-source", "Previously covered evidence.", { metadata });
  f.seed("old-checkpoint", content, { metadata: { ...metadata, sm_consolidation: true } });
  f.db.prepare(`INSERT INTO memory_consolidation_projects(project_key, project_id, source_container_tag, revision,
    active_consolidation_id, last_success_at, created_at, updated_at)
    VALUES ('project:prior-project', 'prior-project', 'memories', 1, 'prior-consolidation', ?, ?, ?)`).run(timestamp, timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
    VALUES ('prior-consolidation', 'project:prior-project', 'old-checkpoint', 1, 'active', ?, ?)`).run(timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision)
    VALUES ('prior-consolidation', 'old-source', 'revision-old-source')`).run();
  f.seed("new-source", "Next evidence.", { metadata });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/consolidate", { projectId: "prior-project", force: true });
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(f.calls.length, 1);
  const input = JSON.parse(f.calls[0].messages[1].content);
  assert.ok(input.previousCheckpoint.length <= 20_000);
  assert.equal(input.previousCheckpoint,prefixWithin(content,20_000));
  assert.deepEqual(input.newMemoryRecords.map((record) => record.id), ["new-source"]);
  const membership = f.db.prepare(`SELECT source.memory_id, source.source_revision FROM memory_consolidation_sources AS source
    JOIN memory_consolidations AS consolidation ON consolidation.id = source.consolidation_id WHERE consolidation.status = 'active'
    ORDER BY source.memory_id`).all();
  assert.deepEqual(membership.map((row) => row.memory_id), ["new-source", "old-source"]);
  const previous = f.db.prepare("SELECT content, is_forgotten, status FROM memories WHERE id = 'old-checkpoint'").get();
  assert.equal(previous.content, content);
  assert.equal(previous.is_forgotten, 1);
  return { public: { response: result.body, calls: f.calls, membership, previous },
    ...(!content.includes("\0") && [...content].length > 20_000
      ? { reads: result.reads.filter((read) => /^SELECT consolidation.id, consolidation.memory_id AS memoryId,/u.test(read.sql)) } : {}) };
});

for (const [name, body] of [["metadata-only", { metadata: { title: "Updated", sm_project_id: "project", padding: "p".repeat(30_000) } }],
  ["content-only", { content: "Updated 😀 evidence." }]]) compare(`PATCH ${name} preserves response and stored state`, async (f) => {
  f.seed("patch", "Original 😀 content.", { metadata: { title: "Original", sm_project_id: "project", topics: ["Original"], padding: "m".repeat(30_000) } });
  const before=f.db.prepare("SELECT * FROM memories WHERE id = ?").get("patch");
  const result = await f.api("/v3/documents/patch", body, "PATCH");
  assert.equal(result.status, 200, JSON.stringify(result.body));
  assert.equal(result.body.content, body.content ?? "Original 😀 content.");
  const stored = f.db.prepare("SELECT * FROM memories WHERE id = 'patch'").get();
  assert.equal(stored.embedding_json, null);
  const nextMetadata=body.metadata??{title:'Original',sm_project_id:'project',padding:'m'.repeat(30000)};
  const expectedState={...before,content:body.content??before.content,metadata_json:JSON.stringify(nextMetadata),embedding_json:null,embedding_status:'disabled',fact_status:'disabled',vector_status:'disabled',embedding_model:null,fact_model:null,embedded_at:null,facts_extracted_at:null,vector_mutation_id:null,vector_attempted_at:null,topic_status:'disabled',topic_model:null,topic_revision:'00000000-0000-4000-8000-000000000001',topics_extracted_at:null,enrichment_error:null};
  assert.deepEqual(stored,expectedState,'PATCH complete persisted state');
  assert.deepEqual(result.body,documentExpected({id:'patch',content:expectedState.content,metadata:nextMetadata,containerTag:'memories',createdAt:timestamp,enrichment:{embeddingStatus:'disabled',factStatus:'disabled',vectorStatus:'disabled',topicStatus:'disabled'}},'full',true),'PATCH complete response');
  return { public: { response: result.body, stored }, reads: result.reads.filter((read) => /^SELECT (?:\*|container_tag, content)/u.test(read.sql)) };
});

compare("PATCH generated checkpoint, stale revision, missing and unauthenticated guards", async (f) => {
  f.seed("patch", "Original guarded content.");
  const unauthorized = await f.api("/v3/documents/patch", { content: "Denied" }, "PATCH", false);
  const missing = await f.api("/v3/documents/missing", { content: "Missing" }, "PATCH");
  f.DB.beforeBatch(() => f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'patch'").run());
  const stale = await f.api("/v3/documents/patch", { content: "Stale replacement" }, "PATCH");
  assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = 'patch'").get().content, "Original guarded content.");
  f.seed("checkpoint", "Generated content.");
  f.db.prepare(`INSERT INTO memory_consolidation_projects(project_key, source_container_tag, created_at, updated_at)
    VALUES ('container:memories', 'memories', ?, ?)`).run(timestamp, timestamp);
  f.db.prepare(`INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
    VALUES ('generated', 'container:memories', 'checkpoint', 1, 'active', ?, ?)`).run(timestamp, timestamp);
  const generated = await f.api("/v3/documents/checkpoint", { metadata: {} }, "PATCH");
  assert.deepEqual([unauthorized.status, missing.status, stale.status, generated.status], [401, 404, 409, 409]);
  return { public: [unauthorized, missing, stale, generated].map(({ status, body }) => ({ status, body })) };
});

const enrichmentContents = ["short", "x".repeat(60_000), "x".repeat(60_001), "x".repeat(59_999) + "😀tail",
  "x".repeat(23_999) + "😀" + "tail".repeat(10_000), "😀".repeat(30_001), "😀".repeat(70_000),
  "界".repeat(200_000), "x".repeat(200_000), "before\0" + "x".repeat(100_000), "x".repeat(70_000) + "\0tail"];
for (const [index, content] of enrichmentContents.entries()) compare(`enrichment AI input and stored content boundary ${index}`, async (f) => {
  f.seed("enrich", content, { metadata: { sm_project_id: "enrichment-project", padding: "m".repeat(5000) } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    const result = await run(model, input);
    if (input.response_format?.json_schema?.name === "durable_memory_enrichment") {
      return { choices: [{ finish_reason: "stop", message: { content: JSON.stringify({
        facts: [{ subject: "Project", predicate: "has", object: "evidence", exclusive: false, confidence: 1 }], topics: ["Evidence"],
      }) } }] };
    }
    return result;
  };
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  assert.equal(f.aiCalls.length, 2);
  assert.deepEqual(f.aiCalls[0].input.text,[prefixWithin(content,60_000)]);
  assert.equal(f.aiCalls[1].input.messages[1].content,prefixWithin(content,24_000));
  assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = 'enrich'").get().content, content);
  const facts = f.db.prepare("SELECT * FROM facts ORDER BY id").all();
  const topics = f.db.prepare("SELECT * FROM memory_topics ORDER BY topic_key").all();
  assert.equal(facts.length, 1);
  assert.equal(topics.length, 1);
  assert.equal(f.vectorCalls.filter((call) => call.operation === "upsert").length, 1);
  const document = await f.api("/v3/documents/enrich", undefined, "GET");
  return { public: { response: result.body, document: document.body, aiCalls: f.aiCalls, vectorCalls: f.vectorCalls, facts, topics },
    ...(!content.includes("\0") && [...content].length > 60_000
      ? { reads: result.reads.filter((read) => /^SELECT id, container_tag,/u.test(read.sql)) } : {}) };
});

for (const mode of ["explicit topics", "checkpoint"]) compare(`enrichment ${mode} branch`, async (f) => {
  f.seed("enrich", "界".repeat(80_000), { metadata: mode === "checkpoint" ? { sm_consolidation: true } : { topics: ["Explicit"] } });
  f.env.AI_ENRICHMENT_MODE = "on";
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  const topics = f.db.prepare("SELECT * FROM memory_topics ORDER BY topic_key").all();
  assert.equal(topics[0].topic, mode === "checkpoint" ? "Evidence" : "Explicit");
  const document = await f.api("/v3/documents/enrich", undefined, "GET");
  return { public: { response: result.body, document: document.body, aiCalls: f.aiCalls, vectorCalls: f.vectorCalls, topics } };
});

for (const mutation of ["revision", "forgotten", "excluded"]) compare(`enrichment discards results after concurrent ${mutation}`, async (f) => {
  f.seed("enrich", "x".repeat(100_000));
  f.env.AI_ENRICHMENT_MODE = "on";
  const run = f.env.AI.run;
  f.env.AI.run = async (model, input) => {
    if (input.text) {
      if (mutation === "revision") f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'enrich'").run();
      if (mutation === "forgotten") f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = 'enrich'").run();
      if (mutation === "excluded") f.db.prepare("UPDATE memories SET metadata_json = ? WHERE id = 'enrich'").run(JSON.stringify({ memoryIndex: { recallable: false } }));
    }
    return run(model, input);
  };
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 202, JSON.stringify(result.body));
  assert.deepEqual(f.vectorCalls, []);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM memory_topics").get().count, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS count FROM facts").get().count, 0);
  const stored = f.db.prepare("SELECT * FROM memories WHERE id = 'enrich'").get();
  assert.equal(stored.embedding_json, embedding);
  return { public: { response: result.body, stored, aiCalls: f.aiCalls } };
});

compare("enrichment rejects a revision changed before its guarded update", async (f) => {
  f.seed("enrich", "x".repeat(100_000));
  f.env.AI_ENRICHMENT_MODE = "on";
  f.DB.beforeBatch(() => f.db.prepare("UPDATE memories SET topic_revision = 'concurrent-revision' WHERE id = 'enrich'").run());
  const result = await f.api("/v4/enrich", { id: "enrich" });
  assert.equal(result.status, 409, JSON.stringify(result.body));
  assert.deepEqual(f.aiCalls, []);
  return { public: { response: result.body, stored: f.db.prepare("SELECT * FROM memories WHERE id = 'enrich'").get() } };
});
