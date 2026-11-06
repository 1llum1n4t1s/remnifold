import test,{assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
const save=async(f,metadata,content='Complete decision and every supporting reason.')=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
const prepare=async f=>{const r=await f.api('/v4/organize/prepare',{});assert.equal(r.status,200,JSON.stringify(r.body));return r.body;};
const payload=b=>({batchId:b.batchId,summaries:[{ref:'s',title:'Decision',topic:'Evidence',content:'The complete verified decision follows from all important reasons.',sources:b.sources.map(s=>s.id)}],coverage:b.sources.map(s=>({id:s.id,revision:s.revision,status:'complete',summaries:['s']}))});
const identity={sessionId:'履歴😀',turn:1,part:1,parts:1,sm_project_id:'project'};

for(const legacy of [false,true])test(`organization capture conflict: ${legacy?'legacy unknown':'different key'} sibling restores originals and defers ambiguity`,async()=>{
  const f=fixture();
  try{
    const metadata={...identity,sm_project_id:'\u3000project\u3000',...(!legacy?{captureKey:'old-key'}:{})};
    const first=await save(f,metadata,'Decision A and its original supporting reason.');
    const published=await f.api('/v4/organize/commit',payload(await prepare(f)));assert.equal(published.status,200);
    const second=await save(f,{...identity,...(!legacy?{captureKey:'new-key'}:{})},'Decision B and its conflicting later reason.');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,2);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(published.body.summaries[0].id).status,'invalid');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
    const plan=f.db.prepare('EXPLAIN QUERY PLAN SELECT 1 FROM memory_organization_capture_conflicts WHERE id=?').all(first).map(r=>String(r.detail)).join('\n');
    assert.match(plan,/SEARCH m USING INDEX idx_organization_capture_logical .*<expr>=\? AND <expr>=\? AND <expr>=\? AND <expr>=\?/);
    assert.doesNotMatch(plan,/SCAN m(?:\s|$)/);
    let b;for(let attempt=0;attempt<4;attempt++){b=await prepare(f);if(b.sources.length)break;}
    assert.ok(b.sources.length);assert.ok(b.sources.every(s=>s.deferReason==='Duplicate or conflicting capture identity'));
    assert.equal((await f.api('/v4/organize/commit',{batchId:b.batchId,summaries:[],coverage:b.sources.map(s=>({id:s.id,revision:s.revision,status:'deferred',summaries:[]}))})).status,200);
    for(const id of[first,second])assert.equal((await f.api(`/v3/documents/${id}`,undefined,'GET')).status,200);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: deterministic ID retry is unchanged and a separate identical copy is deferred',async()=>{
  const f=fixture();try{
    const metadata={...identity,captureKey:'retry-key',captureVersion:2};
    const request={containerTag:'memories',customId:'codex-turn-v2:retry-key',content:'Complete decision and every supporting reason.',metadata,reuseExistingCapture:true};
    const first=await f.api('/v3/documents',request);assert.equal(first.status,201);await f.drain();
    const result=await f.api('/v4/organize/commit',payload(await prepare(f)));assert.equal(result.status,200);
    const retry=await f.api('/v3/documents',request);assert.equal(retry.status,201);assert.equal(retry.body.id,first.body.id);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,0);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(result.body.summaries[0].id).status,'active');
    await save(f,metadata);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,2);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(result.body.summaries[0].id).status,'invalid');
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: late insertion is rechecked by the atomic publication guard',async()=>{
  const f=fixture();try{
    await save(f,{...identity,captureKey:'before'});const body=payload(await prepare(f));
    f.DB.hooks.batch=async()=>{f.DB.hooks.batch=undefined;await save(f,{...identity,captureKey:'after'},'The conflicting decision arrived after preparation.');};
    assert.equal((await f.api('/v4/organize/commit',body)).status,409);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
  }finally{f.DB.hooks.batch=undefined;await f.drain();f.db.close();}
});

test('organization capture conflict: metadata identity changes invalidate sibling summaries',async()=>{
  const f=fixture();try{
    await save(f,{...identity,captureKey:'first'});const first=await f.api('/v4/organize/commit',payload(await prepare(f)));assert.equal(first.status,200);
    const other=await save(f,{...identity,turn:2,captureKey:'second'},'Other turn evidence.');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,0);
    assert.equal((await f.api(`/v3/documents/${other}`,{metadata:{...identity,captureKey:'second'}},'PATCH')).status,200);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(first.body.summaries[0].id).status,'invalid');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,2);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: distinct logical keys and malformed native identities remain independent',async()=>{
  const f=fixture();try{
    await save(f,{...identity,captureKey:'base'});
    for(const changes of[{sm_project_id:'other'},{sessionId:'別session😀'},{turn:2},{part:2},{sessionId:123},{turn:'1'},{part:1.5},{sm_consolidation:true}])await save(f,{...identity,...changes,captureKey:'different'});
    const malformed=await save(f,{sessionId:123,turn:'bad',part:1,parts:2});
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,0);
    const batch=await prepare(f);const source=batch.sources.find(s=>s.id===malformed);assert.ok(source);
    assert.equal(source.deferReason,undefined);assert.equal(source.completeCapture,true);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: checkpoint membership cannot republish an ambiguous original',async()=>{
  const f=fixture();try{
    const original=await save(f,{...identity,captureKey:'old'},'Original decision A.');
    const checkpoint=await save(f,{sm_consolidation:true},'Historical checkpoint representing decision A.');
    f.db.prepare("INSERT INTO memory_consolidation_projects(project_key,source_container_tag,created_at,updated_at) VALUES('conflict-project','memories','now','now')").run();
    f.db.prepare("INSERT INTO memory_consolidations(id,project_key,memory_id,revision,status,created_at,updated_at) VALUES('conflict-cp','conflict-project',?,1,'active','now','now')").run(checkpoint);
    f.db.prepare("INSERT INTO memory_consolidation_sources(consolidation_id,memory_id,source_revision) SELECT 'conflict-cp',id,COALESCE(topic_revision,updated_at) FROM memories WHERE id=?").run(original);
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','3000-01-01' FROM memories WHERE id=?").run(original);
    const before=await prepare(f);assert.equal(before.sources.length,1);assert.equal(before.sources[0].id,checkpoint);
    await save(f,{...identity,captureKey:'new'},'Conflicting decision B already exists.');
    assert.equal((await f.api('/v4/organize/commit',payload(before))).status,409);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
    f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00.000Z' WHERE id=?").run(before.batchId);
    let source;
    for(let attempt=0;attempt<5;attempt++){const b=await prepare(f);source=b.sources.find(s=>s.id===checkpoint);if(source)break;}
    assert.ok(source);assert.equal(source.deferReason,'Duplicate or conflicting capture identity');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: numeric spellings normalize safely without rewriting legacy metadata',async()=>{
  const f=fixture();try{
    const seed=await save(f,{});f.db.prepare('UPDATE memories SET is_forgotten=1 WHERE id=?').run(seed);
    const raws=['{"sessionId":"numeric","turn":1,"part":1}', '{"sessionId":"numeric","turn":1.0,"part":1e0}', '{"sessionId":"numeric","turn":1e0,"part":1.0}'];
    for(let i=0;i<raws.length;i++)f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES(?,?,'memories','Numeric legacy evidence',?,'now','now')").run(`numeric-${i}`,`numeric-${i}`,raws[i]);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,3);
    for(let i=0;i<raws.length;i++)assert.equal(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(`numeric-${i}`).metadata_json,raws[i]);
    for(const [id,metadata]of[['safe-low','{"sessionId":"low","turn":-9007199254740991,"part":1}'],['safe-high','{"sessionId":"high","turn":9007199254740991,"part":1}'],['unsafe','{"sessionId":"unsafe","turn":9007199254740993,"part":1}'],['fraction','{"sessionId":"fraction","turn":1.5,"part":1,"parts":2}']])
      f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES(?,?,'memories','Boundary evidence',?,'now','now')").run(id,id,metadata);
    const b=await prepare(f);
    for(const id of ['safe-low','safe-high','fraction']){const s=b.sources.find(s=>s.id===id);assert.ok(s);assert.equal(s.deferReason,undefined);assert.equal(s.completeCapture,true);}
    assert.equal(b.sources.find(s=>s.id==='unsafe').deferReason,'Capture identity exceeds the safe numeric range');
    assert.equal((await f.api('/v4/organize/commit',payload(b))).status,400);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: raw real multipart remains private until every part is acknowledged',async()=>{
  const f=fixture();try{
    const seed=await save(f,{});f.db.prepare('UPDATE memories SET is_forgotten=1 WHERE id=?').run(seed);
    const raws=['{"sessionId":"real-group","turn":1e0,"part":1.0,"parts":2.0,"captureKey":"real-group:1:body:1:hash"}',
      '{"sessionId":"real-group","turn":1.0,"part":2e0,"parts":2.0,"captureKey":"real-group:1:body:2:hash"}'];
    for(let i=0;i<2;i++)f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES(?,?,'memories','Multipart conclusion and reasons',?,'now','now')").run(`real-${i}`,`real-${i}`,raws[i]);
    const b=await prepare(f);assert.equal(b.group.phase,'reading');assert.equal(b.sources.length,2);assert.ok(b.sources.every(s=>!s.completeCapture));
    assert.equal((await f.api('/v4/organize/commit',payload(b))).status,400);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
    for(let i=0;i<2;i++)assert.equal(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(`real-${i}`).metadata_json,raws[i]);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: unsafe legacy numeric membership fences checkpoint publication',async()=>{
  const f=fixture();try{
    const original=await save(f,{sessionId:'unsafe-cp',turn:1,part:1},'Original evidence.');
    const checkpoint=await save(f,{sm_consolidation:true},'Historical checkpoint evidence.');
    f.db.prepare("INSERT INTO memory_consolidation_projects(project_key,source_container_tag,created_at,updated_at) VALUES('unsafe-project','memories','now','now')").run();
    f.db.prepare("INSERT INTO memory_consolidations(id,project_key,memory_id,revision,status,created_at,updated_at) VALUES('unsafe-cp','unsafe-project',?,1,'active','now','now')").run(checkpoint);
    f.db.prepare("INSERT INTO memory_consolidation_sources(consolidation_id,memory_id,source_revision) SELECT 'unsafe-cp',id,COALESCE(topic_revision,updated_at) FROM memories WHERE id=?").run(original);
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','3000-01-01' FROM memories WHERE id=?").run(original);
    const before=await prepare(f);assert.equal(before.sources.length,1);assert.equal(before.sources[0].id,checkpoint);
    const raw='{"sessionId":"unsafe-cp","turn":9007199254740993,"part":1}';
    // Retain the source revision deliberately, so the numeric membership guard,
    // rather than the ordinary revision guard, has to stop publication.
    f.db.prepare('UPDATE memories SET metadata_json=? WHERE id=?').run(raw,original);
    assert.equal((await f.api('/v4/organize/commit',payload(before))).status,409);
    assert.equal(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(original).metadata_json,raw);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
  }finally{await f.drain();f.db.close();}
});
