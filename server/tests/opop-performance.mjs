const utf8Bytes=value=>new TextEncoder().encode(value).length;
// 実行: cargo xtask server-test
// 失敗条件: 表示列/状態/出典の欠落、内部embeddingのNULL化、同点順序/checkpoint優先の変化、
// 段階間の古いキー再利用、同ID別rowの混同、scope/revision/limit/membershipの変化、
// metadata再解析の残存、UTF-16補正後の同一抜粋windowの再採点。
// 実Workerの応答全体をfixture由来の固定期待と照合し、順位・UTF-16境界は独立した参照計算で検証する。
import test, { assert, recordMetrics } from "./support/test.mjs";
import { DatabaseSync } from "./support/sqlite.mjs";
import after from "./support/observed.mjs";
import searchGoldens from "./support/search-goldens.json" with {type:"json"};
import {documentExpected,listExpected} from "./support/expected.mjs";
import { applyMigrations } from "./support/migrations.mjs";
import { buildMemoryIndex } from "../src/shared/memory-index.ts";

const timestamp = "2026-03-04T12:00:00.000Z";
const embedding = Array.from({ length: 1024 }, (_, i) => Math.sin(i + 1) / 32);
const embeddingJson = JSON.stringify(embedding);
// D1の返却直前に、実SELECTのJSONサイズとembedding取得数を測る。WireサイズやD1課金の推定ではない。
function adapter(db) {
  const reads = [];
  let beforeRead;
  function prepare(sql) {
    const bind = (values = []) => {
      const select = () => {
        beforeRead?.(sql, values);
        const rows = db.prepare(sql).all(...values);
        reads.push({ sql, rows: rows.length, rowBytes: rows.reduce((sum, row) => sum + utf8Bytes(JSON.stringify(row)), 0),
          embeddings: rows.filter((row) => typeof row.embedding_json === "string").length });
        return rows;
      };
      return { sql, values, bind: (...next) => bind(next),
        first: async () => select()[0], all: async () => ({ results: select() }),
        run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
        execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
          ? { results: select(), meta: { changes: 0 } }
          : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
      };
    };
    return bind();
  }
  return { prepare, reads, setBeforeRead(callback) { beforeRead = callback; }, batch: async (operations) => {
    return db.transaction(() => operations.map(operation=>operation.execute()));
  } };
}

test("display, ranking, checkpoint and Unicode contracts", async () => {
const db = new DatabaseSync(":memory:");
const fixtureRecords=new Map();

let artifact;
try {
  db.exec("PRAGMA foreign_keys = ON");
  applyMigrations(db);
  const DB = adapter(db);
  const pending = [];
  const ctx = { waitUntil(promise) { pending.push(Promise.resolve(promise)); } };
  const env = { DB, MEMORY_API_KEY: "opop-local-fixture", AI_ENRICHMENT_MODE: "off",
    AI: { run: async (_model, input) => {
      assert.ok(input.text, "this fixture does not perform generated enrichment");
      return { data: [embedding] };
    } },
    MEMORY_VECTORS: { query: async () => ({ matches: [], count: 0 }) },
  };
  async function request(module, path, body, method = "POST", authenticated = true) {
    DB.reads.length = 0; module.resetCounts();
    const response = await module.default.fetch(new Request(`https://local.example${path}`, { method,
      headers: { ...(authenticated ? { Authorization: `Bearer ${env.MEMORY_API_KEY}` } : {}), "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), env, ctx);
    const result = { status: response.status, body: await response.json(), reads: [...DB.reads], counts: module.counts() };
    while (pending.length) await Promise.all(pending.splice(0));
    return result;
  }
  async function compare(name, path, body, method = "POST", expectedStatus = 200, beforeRead) {
    DB.setBeforeRead(beforeRead?.());
    const current=await request(after,path,body,method);DB.setBeforeRead(undefined);
    recordMetrics(name,{operations:current.counts,sqlReads:current.reads,resultRows:current.body.results?.length??current.body.documents?.length??null});
    assert.equal(current.status,expectedStatus,name);
    if(method==='GET'){assert.deepEqual(current.body,expectedStatus===404?{error:{message:'Document not found'}}:documentExpected({...fixtureRecords.get(path.split('/').at(-1)),enrichment:{...fixtureRecords.get(path.split('/').at(-1)).enrichment,embeddedAt:timestamp,factsExtractedAt:timestamp,vectorAttemptedAt:timestamp}},'full',true),name)}
    if(path==='/v3/documents/list'){
      const selected=[...fixtureRecords.values()].filter(record=>record.containerTag===body.containerTag&&(!body.topic||record.topics.includes(body.topic)));
      const page=body.page??1,limit=body.limit??10;
      assert.deepEqual(current.body,listExpected(selected.slice((page-1)*limit,page*limit),{projection:body.projection??'full',page,limit,total:selected.length}),name);
    }
    if(path==='/v4/search'){
      const normalized=structuredClone(current.body);delete normalized.timing;
      const expected=searchGoldens[name];assert.ok(expected,`${name}: fixed independent public expectation`);
      for(const row of normalized.results){
        const record=fixtureRecords.get(row.id);assert.ok(record,`${name}: unknown result id`);row.id=record.customId;
        if(Object.hasOwn(row.metadata??{},'padding')){assert.equal(row.metadata.padding,record.metadata.padding);row.metadata.padding='$fixture-padding';}
        if(row.metadata?.sourceMemoryIds)row.metadata.sourceMemoryIds=row.metadata.sourceMemoryIds.map(id=>{assert.ok(fixtureRecords.has(id));return fixtureRecords.get(id).customId});
      }
      assert.deepEqual(normalized.results.map(row=>row.id),expected.results.map(row=>row.id),`${name}: exact eligibility, order and limit`);
      for(let i=0;i<normalized.results.length;i++)for(const key of ['similarity','score','lexicalSimilarity','semanticSimilarity']){
        const actual=normalized.results[i][key],value=expected.results[i][key];
        if(typeof value==='number'){assert.ok(typeof actual==='number'&&Math.abs(actual-value)<=1e-12,`${name}: ${key}`);normalized.results[i][key]=value;}
      }
      assert.deepEqual(normalized,expected,`${name}: complete public JSON`);
    }    return {current};
  }  async function seed(customId, containerTag, content, metadata = {}, topics = []) {
    metadata={...metadata,...(topics.length?{topics:[...topics]}:{})};
    const saved = await request(after, "/v3/documents", { customId, containerTag, content, metadata });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    db.prepare(`UPDATE memories SET embedding_json = ?, embedding_status = 'done', embedding_model = 'fixture-embedding',
      fact_status = 'done', fact_model = 'fixture-facts', topic_status = 'done', topic_model = 'fixture-topics',
      embedded_at = ?, facts_extracted_at = ?, topics_extracted_at = ?, vector_attempted_at = ?,
      vector_status = 'pending', vector_mutation_id = 'fixture-mutation', enrichment_error = 'fixture-diagnostic',
      created_at = ?, updated_at = ? WHERE id = ?`).run(embeddingJson, ...Array(6).fill(timestamp), saved.body.id);
    fixtureRecords.set(saved.body.id,{id:saved.body.id,customId,containerTag,content,metadata:structuredClone(metadata),topics:[...topics].sort(),createdAt:timestamp,enrichment:{embeddingModel:'fixture-embedding',factModel:'fixture-facts',topicModel:'fixture-topics',topicsExtractedAt:timestamp,error:'fixture-diagnostic'}});
    return saved.body.id;
  }
  const index = (title, description = title) => buildMemoryIndex({ title, request: description, sourceUpdatedAt: timestamp });
  const displayIds = [];
  for (let i = 0; i < 50; i++) displayIds.push(await seed(`display-${i}`, "display", `Stored display evidence ${i}`,
    { title: `Evidence ${i}`, filepath: `fixture/${i}.md`, sm_project_id: "display-project", sm_scope: "private", memoryIndex: index(`Evidence ${i}`) }, ["Evidence"]));
  assert.equal((await request(after, `/v3/documents/${displayIds[0]}`, undefined, "GET", false)).status, 401);

  const readOne = await compare("GET retains all public fields", `/v3/documents/${displayIds[0]}`, undefined, "GET");
  const listFull = await compare("full list retains all public fields", "/v3/documents/list", { containerTag: "display", projection: "full", limit: 50 });
  for (const [name, result, match] of [["GET", readOne, (read) => read.sql.includes("WHERE") && read.sql.includes("embedding") || read.sql.startsWith("SELECT * FROM memories")],
    ["list-full", listFull, (read) => read.sql.includes("ORDER BY m.updated_at DESC LIMIT")]]) {
    const current=result.current.reads.filter(match);assert.equal(current.length,1);
    assert.equal(current[0].rows,name==='GET'?1:50);assert.equal(current[0].embeddings,0);
    assert.ok(current[0].rowBytes < (name==='GET'?1:50)*new TextEncoder().encode(embeddingJson).length/8);
  }  for (const projection of ["index", "ids", "capture"]) await compare(`list ${projection} unchanged`, "/v3/documents/list", { containerTag: "display", projection, limit: 50 });
  await compare("full list topic filter and pagination", "/v3/documents/list", { containerTag: "display", topic: "Evidence", page: 2, limit: 7 });
  await compare("missing document still returns 404", "/v3/documents/missing", undefined, "GET", 404);

  const searchIds = [];
  for (let i = 0; i < 75; i++) {
    const title = i % 3 === 0 ? "Orion transport" : i % 3 === 1 ? "Orion" : "Transport";
    searchIds.push(await seed(`search-${i}`, i % 2 ? "search-b" : "search-a", `# ${title}\nVerified source ${i}${i % 4 ? "" : " AbortSignal.timeout transport"}`,
      { title, memoryIndex: i % 5 ? index(title) : { version: "legacy" }, padding: "x".repeat(10_000),
        sm_project_id: "search-project", sm_scope: i % 2 ? "public" : "private" }, i % 7 ? [title] : []));
  }
  const checkpointIds = [];
  for (let i = 0; i < 6; i++) {
    const id = await seed(`checkpoint-${i}`, "search-a", "# Orion transport\nCheckpoint evidence",
      { title: "Orion transport", memoryIndex: index("Orion transport"), sm_consolidation: true, sourceCount: 2,
        sourceMemoryIds: [searchIds[i], searchIds[i + 6]], sm_project_id: `project-${i}`, sm_scope: "private" }, ["Orion"]);
    const projectKey = `fixture-project-${i}`;
    db.prepare(`INSERT INTO memory_consolidation_projects (project_key, source_container_tag, revision, created_at, updated_at)
      VALUES (?, 'search-a', 1, ?, ?)`).run(projectKey, timestamp, timestamp);
    db.prepare(`INSERT INTO memory_consolidations (id, project_key, memory_id, revision, status, created_at, updated_at)
      VALUES (?, ?, ?, 1, 'active', ?, ?)`).run(`consolidation-${i}`, projectKey, id, timestamp, timestamp);
    for (const sourceId of [searchIds[i], searchIds[i + 6]]) db.prepare(`INSERT INTO memory_consolidation_sources
      (consolidation_id, memory_id, source_revision) SELECT ?, id, topic_revision FROM memories WHERE id = ?`).run(`consolidation-${i}`, sourceId);
    checkpointIds.push(id);
  }
  const scopes = [undefined, "private", "public"];
  for (const indexOnly of [true, false]) for (const allContainers of [true, false]) for (const scope of scopes) {
    const body = { q: "Orion transport AbortSignal.timeout", indexOnly, limit: 12,
      ...(allContainers ? { allContainers: true } : { containerTag: "search-a" }),
      ...(scope ? { filters: { AND: [{ key: "sm_scope", filterType: "metadata", value: scope }] } } : {}),
    };
    const name = `search index=${indexOnly} global=${allContainers} scope=${scope ?? "all"}`;
    const result = await compare(name, "/v4/search", body);
    assert.equal(result.current.body.results.length,!allContainers&&scope==="public"?0:12);
    for (const row of result.current.body.results) {
      const stored = JSON.parse(db.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(row.id).metadata_json);
      if (scope) assert.equal(stored.sm_scope, scope);
      if (!allContainers) assert.equal(row.containerTag, "search-a");
    }

  }
  const checkpointResult = await compare("checkpoint preference and source membership", "/v4/search", { q: "Orion transport", containerTag: "search-a", indexOnly: true, limit: 50 });
  assert.ok(checkpointResult.current.body.results.some((row) => checkpointIds.includes(row.id)), "checkpoint stage not exercised");
  for (const row of checkpointResult.current.body.results.filter((row) => checkpointIds.includes(row.id))) assert.equal(row.metadata.sourceMemoryIds.length, 2);
  await compare("empty query recent sorting", "/v4/search", { q: "", allContainers: true, indexOnly: true, limit: 7 });
  await compare("excerpt-specific query with legacy index hydration", "/v4/search", { q: "AbortSignal.timeout", allContainers: true, indexOnly: true, limit: 50 });
  await compare("no matching term", "/v4/search", { q: "missing-term-xyz", allContainers: true, indexOnly: true, limit: 5 });

  // 原文再取得の直前に一度だけ改訂をずらす。比較Workerごとに同じ変更を行う。
  const staleId = searchIds[0];
  const revision = db.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(staleId).topic_revision;
  const revisionRace = () => {
    db.prepare("UPDATE memories SET topic_revision = ? WHERE id = ?").run(revision, staleId);
    let changed = false;
    return (sql, values) => {
      if (!changed && sql.includes("SELECT id, container_tag, content, metadata_json, created_at, updated_at, topic_revision") && values.includes(staleId)) {
        changed = true;
        db.prepare("UPDATE memories SET topic_revision = 'fixture-late-revision' WHERE id = ?").run(staleId);
      }
    };
  };
  const stale = await compare("source revision recheck after candidate selection", "/v4/search", { q: "Orion", containerTag: "search-a", indexOnly: true, limit: 50 }, "POST", 200, revisionRace);
  assert.ok(!stale.current.body.results.some((row) => row.id === staleId));
  db.prepare("UPDATE memories SET topic_revision = ? WHERE id = ?").run(revision, staleId);

  const semanticId = await seed("semantic-only", "semantic", "Different evidence without the lookup phrase", { title: "Distinct", memoryIndex: index("Distinct") });
  env.AI_ENRICHMENT_MODE = "on";
  const semantic = await compare("semantic fallback keeps stored embeddings", "/v4/search", { q: "neutrino", containerTag: "semantic", limit: 5 });
  assert.ok(semantic.current.body.results.some((row) => row.id === semanticId && row.semanticSimilarity > 0.99));
  assert.ok(semantic.current.reads.some((read) => read.embeddings > 0));
  env.AI_ENRICHMENT_MODE = "off";

  const excerptId = await seed("shared-excerpt-window", "excerpt-window", "# Evidence\n\nalpha beta gamma delta\n" + "原文😀".repeat(100),
    { title: "Evidence", memoryIndex: index("Evidence") });
  const sharedExcerpt = await compare("shared excerpt window through authenticated search", "/v4/search",
    { q: "alpha beta gamma delta", containerTag: "excerpt-window", indexOnly: true, limit: 5 });
  assert.ok(sharedExcerpt.current.body.results.some((row) => row.id === excerptId && row.searchExcerpt));
  assert.ok(sharedExcerpt.current.counts.excerptWindows > 0);

  const unicodeContent = "# Evidence\n\n" + "İ".repeat(100) + " targetneedle answer " + "原文😀".repeat(100);
  const unicodeId = await seed("unicode-excerpt-offset", "unicode-excerpt", unicodeContent,
    { title: "Evidence", memoryIndex: index("Evidence") });
  const unicodeQuery = { q: "targetneedle", containerTag: "unicode-excerpt", indexOnly: true, limit: 5 };
  const unicodeResult = await request(after, "/v4/search", unicodeQuery);
  assert.equal(unicodeResult.status, 200);
  const unicodeExcerpt = unicodeResult.body.results.find((row) => row.id === unicodeId)?.searchExcerpt;
  assert.match(unicodeExcerpt, /targetneedle/u);
  assert.ok(unicodeContent.includes(unicodeExcerpt.replace(/^…|…$/gu, "")));


  const longUnicodeContent = "İ".repeat(4_000) + " targetneedle answer";
  const fullUnicodeId = await seed("unicode-full-excerpt-offset", "unicode-full-excerpt", longUnicodeContent,
    { title: "Evidence", memoryIndex: index("Evidence") });
  const fullUnicodeQuery = { q: "targetneedle", containerTag: "unicode-full-excerpt", indexOnly: false, limit: 5 };
  const fullUnicodeResult = await request(after, "/v4/search", fullUnicodeQuery);
  assert.equal(fullUnicodeResult.status, 200);
  const fullUnicodeRow = fullUnicodeResult.body.results.find((row) => row.id === fullUnicodeId);
  assert.match(fullUnicodeRow?.content, /targetneedle/u);
  assert.equal(fullUnicodeRow.memory, fullUnicodeRow.content);
  assert.ok(longUnicodeContent.includes(fullUnicodeRow.content.replace(/^…|…$/gu, "")));


  // Static reference comparator checks stable ties without running a historical Worker or timings.
  const candidates=Array.from({length:200},(_,i)=>({id:`sort-${i%197}`,content:'',container_tag:'synthetic',created_at:timestamp,updated_at:timestamp,metadata_json:JSON.stringify({memoryIndex:index(i%6?'Orion':'Orion transport'),sm_consolidation:i%11===0}),...(i%4?{}:{searchExcerpt:'transport AbortSignal.timeout'})}));
  const topics=new Map(candidates.map(row=>[row.id,Number(row.id.slice(5))%7?[]:['Transport']]));const terms=['orion','transport','abortsignal.timeout'];
  const referenceCoverage=(row,withExcerpt=true)=>{
   const data=JSON.parse(row.metadata_json);const index=data.memoryIndex?.version===1?data.memoryIndex:{};
   const fields=[index.title,index.description,...(index.sections??[]),...(topics.get(row.id)??[]),...(withExcerpt?[row.searchExcerpt]:[])];
   const haystack=fields.map(value=>value??'').join(' ').toLowerCase();
   return terms.filter(term=>haystack.includes(term)).map(term=>Math.min(12,term.length)).reduce((a,b)=>a+b,0);
  };
  for(const [secondary,checkpoint] of [[false,false],[false,true],[true,true],[true,false]]){
   const original=[...candidates];after.resetCounts();const ordered=after.sortCoverage([...candidates],topics,terms,secondary,checkpoint);
   assert.equal(ordered.length,original.length);assert.equal(after.counts().coverage,candidates.length*(secondary?2:1));assert.equal(after.counts().metadataParses,candidates.length);recordMetrics("ranking stage",{secondary,checkpoint,rows:candidates.length,operations:after.counts()});
   const reference=[...original].sort((a,b)=>referenceCoverage(b)-referenceCoverage(a)||(secondary?referenceCoverage(b,false)-referenceCoverage(a,false):0)||(checkpoint?Number(JSON.parse(b.metadata_json).sm_consolidation)-Number(JSON.parse(a.metadata_json).sm_consolidation):0));
   assert.deepEqual(ordered,reference);ordered.forEach((row,i)=>assert.equal(row,reference[i]));
   const changed=candidates.map((row,i)=>i%3?row:{...row,metadata_json:JSON.stringify({memoryIndex:index('transport')}),searchExcerpt:'orion'});
   const fresh=after.sortCoverage([...changed],topics,terms,secondary,checkpoint);assert.deepEqual(fresh,[...changed].sort((a,b)=>referenceCoverage(b)-referenceCoverage(a)||(secondary?referenceCoverage(b,false)-referenceCoverage(a,false):0)||(checkpoint?Number(JSON.parse(b.metadata_json).sm_consolidation===true)-Number(JSON.parse(a.metadata_json).sm_consolidation===true):0)));
  }
  // Unoptimized scalar-window oracle retains exact output checks without loading another Worker.
  const expectedExcerpt=(content,terms)=>{
   const windows=terms.map(term=>{const hit=content.toLowerCase().indexOf(term);if(hit<0)return null;let begin=Math.max(0,hit-60);if(begin>0&&/[\uD800-\uDBFF]/u.test(content[begin-1])&&/[\uDC00-\uDFFF]/u.test(content[begin]))begin++;
    let text='';for(const scalar of content.slice(begin)){if(text.length+scalar.length>236)break;text+=scalar}
    return {score:terms.reduce((score,t)=>score+(text.toLowerCase().includes(t)?Math.min(t.length,12):0),0),text:(begin?'…':'')+text+(begin+text.length<content.length?'…':'')};
   }).filter(Boolean).sort((a,b)=>b.score-a.score);return windows[0]?.score?windows[0].text:'';
  };
  for(let i=0;i<1000;i++){const content='前😀'.repeat(i%31)+'alpha beta gamma alpha\n'+'後𠮷😀'.repeat(i%97);const terms=i%4===0?[]:i%4===1?['missing']:i%4===2?['alpha','beta','gamma','alpha']:['😀alpha','beta','後','𠮷'];const snippet=after.searchExcerpt(content,terms);assert.equal(snippet,expectedExcerpt(content,terms));}
  const sharedWindowContent='abcdefghijklmnopqrstuvwxyz0123456789'+'😀原文'.repeat(100);
  const sharedWindowTerms=Array.from({length:30},(_,i)=>sharedWindowContent.slice(i,i+2));
  after.resetCounts();assert.equal(after.searchExcerpt(sharedWindowContent,sharedWindowTerms),expectedExcerpt(sharedWindowContent,sharedWindowTerms));assert.equal(after.counts().excerptWindows,1);recordMetrics("shared excerpt window",{operations:after.counts()});
  const collision='x'.repeat(61)+'😀'+'x'.repeat(59)+'abc'+'後'.repeat(300);
  after.resetCounts();assert.equal(after.searchExcerpt(collision,['ab','bc']),expectedExcerpt(collision,['ab','bc']));assert.equal(after.counts().excerptWindows,1);recordMetrics("shared excerpt window",{operations:after.counts()});  for (const content of ["İ".repeat(100), "ΟΣ.İ😀".repeat(50), "\uD800İ\uDC00".repeat(80)]) {
    const text = content + " targetneedle answer " + "後😀".repeat(100);
    const excerpt = after.searchExcerpt(text, ["targetneedle"]);
    assert.match(excerpt, /targetneedle/u);
    assert.ok(text.includes(excerpt.replace(/^…|…$/gu, "")));
  }
  assert.match(after.searchExcerpt("İ".repeat(100) + " ΟΣ.", ["ος"]), /ΟΣ/u);
  assert.match(after.searchExcerpt("x".repeat(100) + "İ", ["i\u0307"]), /İ/u);
  assert.match(after.searchExcerpt("İ".repeat(100) + "😀", ["😀"]), /😀/u);
  const originalLocaleLower = String.prototype.toLocaleLowerCase;
  try {
    for (const locale of ["en", "tr", "lt"]) {
      String.prototype.toLocaleLowerCase = function () { return Reflect.apply(originalLocaleLower, this, [locale]); };
      for (const prefix of ["İ".repeat(4_000), "I\u0307".repeat(3_000), "I\u0323\u0307".repeat(2_000), "I\u0301😀".repeat(1_500)]) {
        const content = prefix + " targetneedle answer";
        const excerpt = after.sourceExcerpt(content, "targetneedle", 4_000);
        assert.match(excerpt, /targetneedle/u);
        assert.ok(content.includes(excerpt.replace(/^…|…$/gu, "")));
      }
    }
  } finally { String.prototype.toLocaleLowerCase = originalLocaleLower; }
  assert.equal(String.prototype.toLocaleLowerCase, originalLocaleLower);

} finally { db.close(); }
});
