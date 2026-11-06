import test, { assert } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';

const prepare = async f => {
  const result = await f.api('/v4/organize/prepare', {});
  assert.equal(result.status, 200, JSON.stringify(result.body));
  return result.body;
};
const publish = batch => ({ batchId: batch.batchId,
  summaries: [{ref:'final', title:'Decision', topic:'Decision', content:'Use the verified solution because all evidence supports this conclusion.', sources:batch.sources.map(s=>s.id)}],
  coverage:batch.sources.map(s=>({id:s.id,revision:s.revision,status:'complete',summaries:['final']})) });
const save = async (f, metadata = {}, content = 'Verified conclusion and its full reasons.') => {
  const result = await f.api('/v3/documents', {containerTag:'memories',content,metadata});
  assert.equal(result.status,201); await f.drain(); return result.body.id;
};

test('organization core: fixed raw ID windows advance across 12000 rows and wrap', async () => {
  const f = fixture(); await save(f);
  f.db.exec(`WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<12000)
    INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,is_forgotten,created_at,updated_at)
    SELECT printf('window-%05d',i),printf('window-%05d',i),'memories','ineligible body','{}',1,'2026-01-01','2026-01-01' FROM n`);
  f.db.prepare("UPDATE memory_organization_cursor SET last_id='window-00000'").run();
  const plan = f.db.prepare('EXPLAIN QUERY PLAN SELECT id FROM memories WHERE id>? ORDER BY id LIMIT 512').all('window-00000');
  assert.ok(plan.some(row=>String(row.detail).includes('SEARCH') && String(row.detail).includes('id>')));
  const first = await prepare(f); assert.equal(first.sources.length,0);
  assert.equal(f.db.prepare('SELECT last_id FROM memory_organization_cursor').get().last_id,'window-00512');
  for(let i=0;i<23;i++) await prepare(f);
  assert.equal(f.db.prepare('SELECT last_id FROM memory_organization_cursor').get().last_id,'window-12000');
  const wrapped = await prepare(f); assert.equal(wrapped.sources.length,1);
});

test('organization core: database-time publication fence rejects an expired suspended commit and preserves receipts', async () => {
  const f=fixture(); await save(f); const old=await prepare(f); const oldBody=publish(old);
  let intercepted=false;
  f.DB.hooks.batch=async () => {
    if(intercepted) return; intercepted=true;
    f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00.000Z' WHERE id=?").run(old.batchId);
  };
  assert.equal((await f.api('/v4/organize/commit',oldBody)).status,409);
  f.DB.hooks.batch=undefined;
  const fresh=await prepare(f); const body=publish(fresh);
  const committed=await f.api('/v4/organize/commit',body); assert.equal(committed.status,200);
  f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00.000Z'").run();
  assert.deepEqual((await f.api('/v4/organize/commit',body)).body,committed.body);
  assert.equal((await f.api('/v4/organize/commit',oldBody)).status,409);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,1);
});

test('organization core: ordinary marker metadata stays eligible and unanimous filepath is inherited', async () => {
  const f=fixture(); const id=await save(f,{sm_organization:true,sm_organization_manual_only:true,filepath:'D:/project/決定.md',memoryIndex:{version:1,title:'Decision',description:'Verified decision',sections:['Decision'],recallable:true}});
  const batch=await prepare(f); assert.equal(batch.sources[0].id,id);
  assert.equal(batch.sources[0].metadata.filepath,'D:/project/決定.md');
  const result=await f.api('/v4/organize/commit',publish(batch)); assert.equal(result.status,200);
  const metadata=JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(result.body.summaries[0].memoryId).metadata_json);
  assert.equal(metadata.filepath,'D:/project/決定.md');
  assert.equal(metadata.sm_organization_manual_only,undefined);
  assert.equal((await prepare(f)).sources.length,0);
});

for(const count of [7,21]) test(`organization core: ${count}-part capture stays private until every chunk is read`,async()=>{
  const f=fixture();const ids=[];const session='session-😀-組織';
  for(let part=1;part<=count;part++) ids.push(await save(f,{sessionId:session,turn:3,part,parts:count,captureVersion:2,
    captureKey:`${session}:3:wholehash:${part}:parthash`,filepath:'D:/project/source.jsonl'},
    (part===1?'Decision A was provisional. ':part===count?'Final decision B because the final evidence rejects A. ':'Intermediate reasons. ')+(count===7?'x'.repeat(8100):'')));
  let ready;let reads=0;
  for(let iteration=0;iteration<40;iteration++) {
    const batch=await prepare(f);assert.ok(batch.group);
    assert.ok(batch.sources.length<=20);assert.ok(batch.sources.reduce((n,s)=>n+s.content.length,0)<=48000);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
    if(batch.group.phase==='ready'){ready=batch;break;}
    reads+=batch.sources.length;
    assert.equal((await f.api('/v4/organize/commit',publish(batch))).status,400);
    const payload={batchId:batch.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'A was provisional. Preserve each source ID and its unknown decision time; retain B and its reasons when the final part arrives.'}};
    const result=await f.api('/v4/organize/commit',payload);assert.equal(result.status,200,JSON.stringify(result.body));
    assert.deepEqual((await f.api('/v4/organize/commit',payload)).body,result.body);
  }
  assert.ok(ready);assert.ok(reads>=count);
  const body=publish(ready);body.summaries[0].content='Final decision B replaces provisional A because the last captured evidence rejects A.';
  const committed=await f.api('/v4/organize/commit',body);assert.equal(committed.status,200,JSON.stringify(committed.body));
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,count);
  assert.equal(f.db.prepare('SELECT COUNT(DISTINCT memory_id) n FROM memory_organization_dependencies_all').get().n,count);
  const summary=committed.body.summaries[0];
  assert.equal(JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(summary.memoryId).metadata_json).filepath,'D:/project/source.jsonl');
  await f.api(`/v3/documents/${ids[count-1]}`,{content:'The final reason changed.'},'PATCH');
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
  assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(summary.id).status,'invalid');
});

test('organization core: source dates distinguish invalid evidence dates from import and summary dates',async()=>{
  const f=fixture();await save(f,{sessionId:'dates',turn:2,sourceTimestamp:'invalid',completedAt:'2020-01-02T12:00:00+09:00',sourceUpdatedAt:'2019-01-02T00:00:00Z',memoryIndex:{recallable:true,sourceUpdatedAt:'2018-01-02T00:00:00Z'}});
  const batch=await prepare(f);const source=batch.sources[0];
  assert.equal(source.dates.sourceTimestamp,null);assert.equal(source.dates.completedAt,'2020-01-02T03:00:00.000Z');
  assert.equal(source.dates.sourceUpdatedAt,'2019-01-02T00:00:00.000Z');assert.equal(source.dates.memoryIndexSourceUpdatedAt,'2018-01-02T00:00:00.000Z');
  assert.ok(source.dates.createdAt);assert.ok(source.dates.updatedAt);
});

test('organization core: summary generation dates do not impersonate the original decision date',async()=>{
  const f=fixture();
  try {
    const originalDate='2020-01-02T03:00:00.000Z';
    await save(f,{sourceTimestamp:originalDate,sourceUpdatedAt:originalDate},'The decision made in 2020 and its reasons.');
    const batch=await prepare(f);assert.equal(batch.sources[0].dates.sourceTimestamp,originalDate);
    const result=await f.api('/v4/organize/commit',publish(batch));assert.equal(result.status,200);
    const summary=f.db.prepare('SELECT metadata_json,created_at,updated_at FROM memories WHERE id=?').get(result.body.summaries[0].memoryId);
    const metadata=JSON.parse(summary.metadata_json);
    assert.equal(metadata.sourceTimestamp,undefined);assert.equal(metadata.sourceUpdatedAt,undefined);
    assert.equal(metadata.memoryIndex.sourceUpdatedAt,undefined);
    assert.ok(summary.created_at);assert.ok(summary.updated_at);assert.notEqual(summary.created_at,originalDate);
    const lookup=await f.api('/v4/organize/lookup',{query:'Decision'});assert.equal(lookup.status,200);
    assert.equal(lookup.body.summaries[0].generatedAt,summary.updated_at);
  } finally {await f.drain();f.db.close();}
});

test('organization core: missing parts defer visibly while unrelated work proceeds',async()=>{
  const f=fixture();await save(f,{sessionId:'missing',turn:1,part:1,parts:2,captureKey:'missing:1:body:1:part'});
  const normal=await save(f,{},'Unrelated complete conclusion and reasons.');
  const batch=await prepare(f);assert.equal(batch.group.phase,'deferred');assert.match(batch.group.reason,/missing/);
  assert.equal((await f.api('/v4/organize/commit',{batchId:batch.batchId,summaries:[],coverage:[],groupProgress:{status:'deferred',memo:'Missing final part; preserve visible originals.'}})).status,200);
  const next=await prepare(f);assert.ok(next.sources.some(source=>source.id===normal));
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
});

test('organization core: in-flight multipart edits reject the old generation without publishing a partial conclusion',async()=>{
  const f=fixture();const ids=[];
  for(let part=1;part<=2;part++)ids.push(await save(f,{sessionId:'race',turn:1,part,parts:2,captureKey:`race:1:body:${part}:hash`},`Part ${part} conclusion ${'x'.repeat(9000)}`));
  const batch=await prepare(f);assert.equal(batch.group.phase,'reading');
  await f.api(`/v3/documents/${ids[1]}`,{content:'Changed final conclusion and reasons.'},'PATCH');
  const payload={batchId:batch.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Old provisional A, not a final conclusion.'}};
  assert.equal((await f.api('/v4/organize/commit',payload)).status,409);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
});

test('organization core: stale reset cannot erase a newer active capture or its non-anchor dependency',async()=>{
  const f=fixture();const ids=[];
  let release;
  const gate=new Promise(resolve=>{release=resolve;});
  let entered;
  const paused=new Promise(resolve=>{entered=resolve;});
  let stale;
  try {
    for(let part=1;part<=2;part++)ids.push(await save(f,{sessionId:'reset-race',turn:1,part,parts:2,captureKey:`reset-race:1:body:${part}:hash`},`Part ${part}: complete decision and evidence.`));
    const initial=await prepare(f);
    assert.equal((await f.api('/v4/organize/commit',{batchId:initial.batchId,summaries:[],coverage:[],groupProgress:{status:'deferred',memo:'Wait for a review of the evidence.'}})).status,200);
    f.db.prepare("UPDATE memory_organization_groups SET next_attempt='2000-01-01T00:00:00.000Z'").run();
    const original=f.db.prepare('SELECT generation FROM memory_organization_groups').get().generation;
    f.DB.hooks.batch=async operations=>{
      if(!operations[0].sql.includes('UPDATE memory_organization_groups SET generation=generation+1'))return;
      f.DB.hooks.batch=undefined;entered();
      let timer;
      try {await Promise.race([gate,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Reset barrier did not release')),5000);})]);}
      finally {clearTimeout(timer);}
    };
    // B has observed the old retryable generation but has not executed its reset CAS.
    stale=f.api('/v4/organize/prepare',{});
    let timer;
    try {await Promise.race([paused,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Reset barrier was not reached')),5000);})]);}
    finally {clearTimeout(timer);}
    // A wins the reset and publishes while B still holds the stale read.
    const reading=await prepare(f);assert.equal(reading.group.generation,original+1);
    assert.equal((await f.api('/v4/organize/commit',{batchId:reading.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Both complete source IDs support the final decision, with decision dates unknown.'}})).status,200);
    const ready=await prepare(f);assert.equal(ready.group.phase,'ready');
    const result=await f.api('/v4/organize/commit',publish(ready));assert.equal(result.status,200,JSON.stringify(result.body));
    const summary=result.body.summaries[0];
    release();assert.equal((await stale).status,409);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_groups').get().status,'active');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_group_members').get().n,2);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_group_summaries').get().n,1);
    assert.equal(f.db.prepare('SELECT COUNT(DISTINCT memory_id) n FROM memory_organization_dependencies_all').get().n,2);
    const nonAnchor=ids.find(id=>id!==ready.sources[0].id);
    assert.ok(nonAnchor);
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:nonAnchor},'DELETE')).status,200);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(summary.id).status,'invalid');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
  } finally {
    release();f.DB.hooks.batch=undefined;
    if(stale)await stale.catch(()=>{});
    await f.drain();f.db.close();
  }
});

test('organization core: source invalidation uses indexed pending references despite retained receipts',async()=>{
  const f=fixture();
  try {
    const id=await save(f);
    f.db.exec(`WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000)
      INSERT INTO memory_organization_batches(id,expires_at,snapshot_json,request_json,response_json)
      SELECT 'receipt-'||i,'2000-01-01','{"sources":[],"summaries":[]}','digest','{}' FROM n`);
    const batch=await prepare(f);
    const sql=`DELETE FROM memory_organization_batches WHERE request_json IS NULL AND id IN (
      SELECT batch_id FROM memory_organization_leases WHERE memory_id=?
      UNION SELECT batch_id FROM memory_organization_batch_summaries WHERE memory_id=?)`;
    const plan=f.db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(id,id).map(row=>String(row.detail)).join('\n');
    assert.match(plan,/SEARCH memory_organization_batches/);assert.match(plan,/SEARCH memory_organization_leases/);
    assert.match(plan,/SEARCH memory_organization_batch_summaries/);assert.doesNotMatch(plan,/SCAN memory_organization_batches|json_tree/);
    await f.api(`/v3/documents/${id}`,{content:'Updated evidence and conclusion.'},'PATCH');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_batches WHERE id=?').get(batch.batchId).n,0);
    assert.equal(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_batches WHERE id LIKE 'receipt-%'").get().n,5000);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_leases').get().n,0);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_batch_summaries').get().n,0);
  } finally {await f.drain();f.db.close();}
});

for(const mutation of ['forget','delete']) test(`organization core: deferred memo has one private copy and is cleared on ${mutation}`,async()=>{
  const f=fixture();
  try {
    const ids=[];
    for(let part=1;part<=2;part++)ids.push(await save(f,{sessionId:'private-reason',turn:1,part,parts:2,captureKey:`private-reason:1:body:${part}:hash`},`Original private evidence ${part}.`));
    const batch=await prepare(f);const secret='Private continuation: exact unique evidence that must be forgotten.';
    assert.equal((await f.api('/v4/organize/commit',{batchId:batch.batchId,summaries:[],coverage:[],groupProgress:{status:'deferred',memo:secret}})).status,200);
    const group=f.db.prepare('SELECT memo,reason FROM memory_organization_groups').get();
    assert.equal(group.memo,secret);assert.equal(group.reason,'Caller deferred capture group');
    assert.ok(!f.db.prepare('SELECT snapshot_json FROM memory_organization_batches WHERE id=?').get(batch.batchId).snapshot_json.includes(secret));
    if(mutation==='forget')assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:ids[1]},'DELETE')).status,200);
    else assert.equal((await f.api(`/v3/documents/${ids[1]}`,undefined,'DELETE')).status,204);
    const cleared=f.db.prepare('SELECT memo,reason FROM memory_organization_groups').get();
    assert.equal(cleared.memo,'');assert.equal(cleared.reason,null);
  } finally {await f.drain();f.db.close();}
});

test('organization core: current summary forget reopens its capture, while replacement preserves active dependencies',async()=>{
  const f=fixture();
  try {
    for(let part=1;part<=2;part++)await save(f,{sessionId:'summary-forget',turn:1,part,parts:2,captureKey:`summary-forget:1:body:${part}:hash`},`Part ${part}: the complete decision and reasons.`);
    const reading=await prepare(f);
    assert.equal((await f.api('/v4/organize/commit',{batchId:reading.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Both parts preserve the conclusion and its supporting reasons; decision time unknown.'}})).status,200);
    const first=await f.api('/v4/organize/commit',publish(await prepare(f)));assert.equal(first.status,200);
    const previous=first.body.summaries[0];
    const added=await save(f,{},'Additional evidence supports the same decision for the same reasons.');
    let next;
    for(let window=0;window<4;window++){next=await prepare(f);if(next.sources.some(source=>source.id===added))break;}
    assert.ok(next.sources.some(source=>source.id===added));
    const replacement=publish(next);Object.assign(replacement.summaries[0],{id:previous.id,revision:previous.revision});
    const updated=await f.api('/v4/organize/commit',replacement);assert.equal(updated.status,200,JSON.stringify(updated.body));
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_groups').get().status,'active');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_group_members').get().n,2);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,3);
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:updated.body.summaries[0].memoryId},'DELETE')).status,200);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_groups').get().status,'invalid');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
    const reopened=await prepare(f);assert.equal(reopened.group.phase,'reading');assert.equal(reopened.sources.length,2);
  } finally {await f.drain();f.db.close();}
});

test('organization core: bounded lookup supports Unicode content, exact short topics and recent title hints',async()=>{
  const f=fixture();
  try {
    await save(f);const body=publish(await prepare(f));
    Object.assign(body.summaries[0],{title:'非本文の題名🦊',topic:'計画',content:'日本語の結論。理由を明確に保持します。'});
    const committed=await f.api('/v4/organize/commit',body);assert.equal(committed.status,200);
    const queries=[];f.DB.hooks.all=sql=>queries.push(sql);
    for(const query of ['日本語','計画','題','非本文の題名🦊']) {
      const result=await f.api('/v4/organize/lookup',{query});assert.equal(result.status,200,JSON.stringify(result.body));
      assert.equal(result.body.summaries.length,1);assert.equal(result.body.summaries[0].id,committed.body.summaries[0].id);
    }
    assert.ok(queries.some(sql=>sql.includes('memories_fts MATCH')));
    assert.ok(queries.some(sql=>sql.includes('t.topic_key=?')));
    assert.ok(queries.every(sql=>!sql.includes('instr(') && !sql.includes('ORDER BY CASE')));
  }finally{f.DB.hooks.all=undefined;await f.drain();f.db.close();}
});

test('organization core: late capture part invalidates every direct group conclusion without recursive registry cascades',async()=>{
  const f=fixture();try{
    for(let part=1;part<=2;part++)await save(f,{sessionId:'late-part',turn:1,part,parts:2,captureKey:`late-part:1:body:${part}:hash`},`Part ${part}: conclusion and complete reasons.`);
    const reading=await prepare(f);
    assert.equal((await f.api('/v4/organize/commit',{batchId:reading.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Every prepared source supports the decision and its reasons; original date unknown.'}})).status,200);
    const result=await f.api('/v4/organize/commit',publish(await prepare(f)));assert.equal(result.status,200);
    const summary=result.body.summaries[0];
    await save(f,{sessionId:'late-part',turn:1,part:3,parts:3,captureKey:'late-part:1:body:3:hash'},'A late part adds an unresolved decision.');
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(summary.id).status,'invalid');
    const memory=f.db.prepare('SELECT is_forgotten,vector_status FROM memories WHERE id=?').get(summary.memoryId);
    assert.equal(memory.is_forgotten,1);assert.equal(memory.vector_status,'pending');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM facts WHERE source_memory_id=?').get(summary.memoryId).n,0);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_topics WHERE memory_id=?').get(summary.memoryId).n,0);
  }finally{await f.drain();f.db.close();}
});
