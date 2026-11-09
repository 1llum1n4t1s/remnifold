import test,{assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
const save=async(f,metadata,content='Complete decision and every supporting reason.')=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
const prepare=async f=>{const r=await f.api('/v4/organize/prepare',{});assert.equal(r.status,200,JSON.stringify(r.body));return r.body;};
const payload=b=>({batchId:b.batchId,summaries:[{ref:'s',title:'Decision',topic:'Evidence',content:'The complete verified decision follows from all important reasons.',sources:b.sources.map(s=>s.id)}],coverage:b.sources.map(s=>({id:s.id,revision:s.revision,status:'complete',summaries:['s']}))});
const identity={sessionId:'履歴😀',turn:1,part:1,parts:1,sm_project_id:'project'};
const variant=async(body,legacy=false,extra={})=>{
  const hash=[...new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(body)))].map(b=>b.toString(16).padStart(2,'0')).join('');
  const metadata={...identity,sessionId:'変種:\u0000😀',title:'Decision',captureVersion:2,...extra};
  metadata.captureKey=`${metadata.sessionId}:${metadata.turn}:${hash}:1:${hash.slice(0,16)}`;
  return {metadata,content:`# ${metadata.title}\n\n${legacy?`Session: ${metadata.sessionId}\nTurn: ${metadata.turn}\n\nPart: 1/1\n\n`:''}${body}`};
};
test('organization verified variants preserve exact bodies and targeted preparation guards',async()=>{
  const f=fixture();try {
    const a=await variant('First independent conclusion.');
    const b=await variant('Second unresolved contradictory conclusion.',true);
    const ids=[await save(f,a.metadata,a.content),await save(f,b.metadata,b.content)];
    const oldCursor=f.db.prepare('SELECT * FROM memory_organization_cursor').get();
    const r=await f.api('/v4/organize/prepare',{sourceIds:ids});assert.equal(r.status,200,JSON.stringify(r.body));
    assert.deepEqual(f.db.prepare('SELECT * FROM memory_organization_cursor').get(),oldCursor);
    assert.ok(r.body.sources.every(s=>s.captureVariant?.kind==='verified-single-part-v2'&&!s.deferReason));
    assert.equal((await f.api('/v4/organize/prepare',{sourceIds:ids})).status,409);
    assert.equal((await f.api('/v4/organize/commit',payload(r.body))).status,200);
    assert.equal((await f.api('/v4/organize/prepare',{sourceIds:ids})).status,409);
    for(const sourceIds of [[],[ids[0],ids[0]],['missing'],Array(21).fill(ids[0])])
      assert.equal((await f.api('/v4/organize/prepare',{sourceIds})).status,400);
    // The old conflict view remains strict during migration/Worker rollout.
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_capture_conflicts').get().n,2);
  }finally{await f.drain();f.db.close();}
});
test('organization unverifiable hash stays blocked until edit while targeted retries remain safe',async()=>{
  const f=fixture();try {
    const a=await variant('Original captured body.');
    const id=await save(f,a.metadata,a.content+' altered');
    const peer=await variant('Independent peer evidence.');await save(f,peer.metadata,peer.content);
    const r=await f.api('/v4/organize/prepare',{sourceIds:[id]});assert.equal(r.status,200);
    assert.equal(r.body.sources[0].deferReason,'capture-hash-unverifiable');
    assert.equal((await f.api('/v4/organize/commit',payload(r.body))).status,400);
    await defer(f,r.body);due(f);
    const automatic=await prepare(f);assert.ok(automatic.sources.every(s=>s.id!==id));await defer(f,automatic);
    const legacy=await save(f,{...a.metadata,captureKey:'legacy-peer'},'Unverified conflicting legacy evidence.');
    assert.equal(f.db.prepare('SELECT defer_reason FROM memory_organization_progress WHERE memory_id=?').get(id).defer_reason,'capture-hash-unverifiable');
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:legacy},'DELETE')).status,200);
    const retry=await f.api('/v4/organize/prepare',{sourceIds:[id]});assert.equal(retry.body.sources[0].deferReason,'capture-hash-unverifiable');await defer(f,retry.body);
    assert.equal((await f.api(`/v3/documents/${id}`,{content:a.content},'PATCH')).status,200);
    const fixed=await f.api('/v4/organize/prepare',{sourceIds:[id]});assert.ok(fixed.body.sources[0].captureVariant);
  }finally{await f.drain();f.db.close();}
});
for(const phase of ['published','pending'])test(`organization lone projected capture retains ordinary coverage until a ${phase} peer conflict`,async()=>{
  const f=fixture();try {
    const a=await variant('Full capture before project projection.');
    const id=await save(f,a.metadata,a.content+' projected content');
    const first=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    assert.equal(first.sources[0].captureVariant,undefined);assert.equal(first.sources[0].deferReason,undefined);
    if(phase==='published'){
      assert.equal((await f.api('/v4/organize/commit',payload(first))).status,200);
      assert.equal(f.db.prepare('SELECT capture_verified_revision FROM memory_organization_coverage WHERE memory_id=?').get(id).capture_verified_revision,null);
    }
    const peer=await variant('Independent distinct-key evidence.');const peerId=await save(f,peer.metadata,peer.content);
    if(phase==='pending'){
      assert.equal((await f.api('/v4/organize/commit',payload(first))).status,409);
      f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01' WHERE id=?").run(first.batchId);
      f.db.prepare("UPDATE memory_organization_leases SET expires_at='2000-01-01' WHERE batch_id=?").run(first.batchId);
    }
    assert.equal(f.db.prepare('SELECT memory_id FROM memory_organization_complete WHERE memory_id=?').get(id),undefined);
    const conflict=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    assert.equal(conflict.sources[0].deferReason,'capture-hash-unverifiable');await defer(f,conflict);
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:peerId},'DELETE')).status,200);due(f);
    const resolved=await prepare(f);assert.equal(resolved.sources[0].id,id);
    assert.equal(resolved.sources[0].deferReason,undefined);
    assert.equal((await f.api('/v4/organize/commit',payload(resolved))).status,200);
  }finally{await f.drain();f.db.close();}
});
const defer=async(f,b)=>{const r=await f.api('/v4/organize/commit',{batchId:b.batchId,summaries:[],coverage:b.group?[]:b.sources.map(s=>({id:s.id,revision:s.revision,status:'deferred',summaries:[]})),...(b.group?{groupProgress:{status:'deferred',memo:'Conflicting capture evidence remains unresolved.'}}:{})});assert.equal(r.status,200,JSON.stringify(r.body));};
for(const race of ['same-key','edit'])test(`organization verified variant rejects ${race} before publication`,async()=>{
  const f=fixture();try {
    const a=await variant('A complete independent conclusion.');
    const id=await save(f,a.metadata,a.content);
    const prepared=await f.api('/v4/organize/prepare',{sourceIds:[id]});assert.equal(prepared.status,200);
    f.DB.hooks.batch=async()=>{f.DB.hooks.batch=undefined;
      if(race==='same-key')await save(f,a.metadata,a.content);
      else assert.equal((await f.api(`/v3/documents/${id}`,{content:a.content+' changed'},'PATCH')).status,200);
    };
    assert.equal((await f.api('/v4/organize/commit',payload(prepared.body))).status,409);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
  }finally{f.DB.hooks.batch=undefined;await f.drain();f.db.close();}
});
test('organization verified variants preserve partition and chunk budgets',async()=>{
  const f=fixture();try {
    const ids=[];
    for(let i=0;i<7;i++){const a=await variant(`Conclusion ${i}. `+'e'.repeat(8100),false,{sm_scope:i===0?'shared':'project'});ids.push(await save(f,a.metadata,a.content));}
    const r=await f.api('/v4/organize/prepare',{sourceIds:ids});assert.equal(r.status,200);
    assert.ok(r.body.sources.length<=6);assert.ok(r.body.sources.reduce((n,s)=>n+s.content.length,0)<=48000);
    assert.ok(r.body.sources.every(s=>s.content.length<=8000&&s.captureVariant));
    assert.equal((await f.api('/v4/organize/commit',payload(r.body))).status,409);
  }finally{await f.drain();f.db.close();}
});
test('organization targeted preparation resumes the exact source revision and private memo',async()=>{
  const f=fixture();try {
    const a=await variant('Full evidence. '+'x'.repeat(9000));
    const id=await save(f,a.metadata,a.content);
    const first=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    const source=first.sources[0];assert.equal(source.offset,0);assert.equal(source.end,8000);
    assert.equal((await f.api('/v4/organize/commit',{batchId:first.batchId,summaries:[],coverage:[{id,revision:source.revision,status:'partial',summaries:[],memo:'All prior conclusions and reasons.'}]})).status,200);
    const second=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    assert.equal(second.sources[0].offset,8000);assert.equal(second.sources[0].memo,'All prior conclusions and reasons.');
    assert.equal(second.sources[0].end,second.sources[0].total);
    assert.equal((await f.api('/v4/organize/commit',payload(second))).status,200);
  }finally{await f.drain();f.db.close();}
});
test('organization new independent capture preserves peer progress and published coverage',async()=>{
  const f=fixture();try {
    const a=await variant('First full evidence. '+'x'.repeat(9000));
    const id=await save(f,a.metadata,a.content);
    const first=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    assert.equal((await f.api('/v4/organize/commit',{batchId:first.batchId,summaries:[],coverage:[{id,revision:first.sources[0].revision,status:'partial',summaries:[],memo:'Complete first chunk evidence.'}]})).status,200);
    const b=await variant('Second independent evidence.');
    await save(f,b.metadata,b.content);
    assert.equal(f.db.prepare('SELECT next_offset FROM memory_organization_progress WHERE memory_id=?').get(id).next_offset,8000);
    const second=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    const committed=await f.api('/v4/organize/commit',payload(second));assert.equal(committed.status,200);
    const c=await variant('Third independent evidence.');
    await save(f,c.metadata,c.content);
    assert.ok(f.db.prepare('SELECT memory_id FROM memory_organization_complete WHERE memory_id=?').get(id));
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(committed.body.summaries[0].id).status,'active');
    await save(f,a.metadata,a.content);
    assert.equal(f.db.prepare('SELECT memory_id FROM memory_organization_complete WHERE memory_id=?').get(id),undefined);
  }finally{await f.drain();f.db.close();}
});
for(const proof of ['absent','stale'])test(`organization legacy ${proof} coverage proof invalidates on a new capture variant`,async()=>{
  const f=fixture();try {
    const a=await variant('Original evidence from an older Worker.');
    const id=await save(f,a.metadata,a.content);
    const first=(await f.api('/v4/organize/prepare',{sourceIds:[id]})).body;
    const committed=await f.api('/v4/organize/commit',payload(first));assert.equal(committed.status,200);
    const original=f.db.prepare('SELECT * FROM memory_organization_coverage WHERE memory_id=?').get(id);
    assert.equal(original.capture_verified_revision,first.sources[0].revision);
    if(proof==='absent')f.db.prepare('UPDATE memory_organization_coverage SET capture_verified_revision=NULL WHERE memory_id=?').run(id);
    else {
      // Historical Worker upserts do not know the new proof column.
      f.db.prepare(`INSERT INTO memory_organization_coverage(memory_id,source_revision,status) VALUES(?,?,'active')
        ON CONFLICT(memory_id) DO UPDATE SET source_revision=excluded.source_revision,status='active'`).run(id,'changed-old-worker-revision');
      assert.equal(f.db.prepare('SELECT capture_verified_revision FROM memory_organization_coverage WHERE memory_id=?').get(id).capture_verified_revision,original.source_revision);
    }
    const b=await variant('New independent capture evidence.');
    await save(f,b.metadata,b.content);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_coverage WHERE memory_id=?').get(id).status,'invalid');
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(committed.body.summaries[0].id).status,'invalid');
  }finally{await f.drain();f.db.close();}
});
const due=f=>f.db.exec("UPDATE memory_organization_progress SET next_attempt='2000-01-01'; UPDATE memory_organization_groups SET next_attempt='2000-01-01';");
const explainSibling=async(f,id)=>{const b=await prepare(f);assert.equal(b.group.phase,'deferred');assert.match(b.group.reason,/conflicting capture identity/);assert.equal(f.db.prepare('SELECT anchor_memory_id FROM memory_organization_groups WHERE id=?').get(b.group.id).anchor_memory_id,id);await defer(f,b);};

for(const resolution of ['forget','delete','identity'])test(`organization capture conflict: known revisions skip all lanes until ${resolution} resolves them`,async()=>{
  const f=fixture();try{
    const first=await save(f,{...identity,captureKey:'first'});
    const second=await save(f,{...identity,captureKey:'second'});
    const b=await prepare(f);assert.equal(b.sources.length,2);assert.ok(b.sources.every(s=>s.deferReason));await defer(f,b);due(f);
    const ordinary=await save(f,{},'Independent decision still progresses.');
    for(let lane=0;lane<4;lane++){
      f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);
      const next=await prepare(f);assert.ok(next.sources.every(s=>s.id!==first&&s.id!==second));
      if(lane===0){assert.ok(next.sources.some(s=>s.id===ordinary));await defer(f,next);}
    }
    if(resolution==='identity')assert.equal((await f.api(`/v3/documents/${second}`,{metadata:{...identity,turn:2,captureKey:'second'}},'PATCH')).status,200);
    else if(resolution==='delete')assert.equal((await f.api(`/v3/documents/${second}`,undefined,'DELETE')).status,204);
    else assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:second},'DELETE')).status,200);
    const restored=await prepare(f);assert.ok(restored.sources.some(s=>s.id===first&&!s.deferReason));
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: edited conflicting revision explains its deferral again and caller deferrals still retry',async()=>{
  const f=fixture();try{
    const first=await save(f,{...identity,captureKey:'first'});await save(f,{...identity,captureKey:'second'});
    await defer(f,await prepare(f));due(f);
    assert.equal((await prepare(f)).sources.length,0);
    assert.equal((await f.api(`/v3/documents/${first}`,{content:'Changed original evidence with the same conflicting identity.'},'PATCH')).status,200);
    const changed=await prepare(f);assert.ok(changed.sources.some(s=>s.id===first&&s.deferReason));await defer(f,changed);
    const ordinary=await save(f,{},'Caller needs additional evidence.');await defer(f,await prepare(f));due(f);
    assert.ok((await prepare(f)).sources.some(s=>s.id===ordinary&&!s.deferReason));
  }finally{await f.drain();f.db.close();}
});

for(const conflictPart of [1,2])test(`organization capture conflict: multipart ${conflictPart===1?'empty-member anchor':'nonanchor member'} blocks fresh sibling rediscovery`,async()=>{
  const f=fixture();try{
    const session='日本😀:session\u0000suffix';const prefix=`${session}:1:body`;const ids=[];
    for(let part=1;part<=2;part++)ids.push(await save(f,{sessionId:session,turn:1,part,parts:2,captureKey:`${prefix}:${part}:hash`}));
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('retry-group',?,?,1,2,'reading',?)").run(prefix,session,ids[0]);
    if(conflictPart===2)for(let part=1;part<=2;part++)f.db.prepare(`INSERT INTO memory_organization_group_members(group_id,part,memory_id,source_revision)
      SELECT 'retry-group',?,id,COALESCE(topic_revision,updated_at) FROM memories WHERE id=?`).run(part,ids[part-1]);
    const duplicate=await save(f,{sessionId:session,turn:1,part:conflictPart,parts:2,captureKey:`${session}:1:other:${conflictPart}:hash`});
    // 競合を維持したまま、異なるprefixの重複だけを初回探索から外す。
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','3000-01-01' FROM memories WHERE id=?").run(duplicate);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');
    const first=await prepare(f);assert.equal(first.group.phase,'deferred');assert.match(first.group.reason,/conflicting capture identity/);await defer(f,first);
    if(conflictPart===1)assert.equal(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_group_members WHERE group_id='retry-group'").get().n,0);
    if(conflictPart===1){
      const oldRevision=f.db.prepare('SELECT source_revision FROM memory_organization_progress WHERE memory_id=?').get(ids[0]).source_revision;
      assert.equal((await f.api(`/v3/documents/${ids[0]}`,{content:'Edited anchor evidence with its original conflicting identity.'},'PATCH')).status,200);
      assert.equal(f.db.prepare('SELECT source_revision FROM memory_organization_progress WHERE memory_id=?').get(ids[0]),undefined);
      f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');due(f);
      const edited=await prepare(f);assert.equal(edited.group.phase,'deferred');assert.match(edited.group.reason,/conflicting capture identity/);
      assert.notEqual(f.db.prepare('SELECT source_revision FROM memory_organization_progress WHERE memory_id=?').get(ids[0]).source_revision,oldRevision);
      await defer(f,edited);
      await explainSibling(f,duplicate);
    }
    due(f);
    let freshSql;
    f.DB.hooks.all=sql=>{if(sql.includes('candidates AS')&&sql.includes('blocked.prefix='))freshSql=sql;};
    for(let lane=0;lane<4;lane++){
      f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);
      const b=await prepare(f);assert.equal(b.group,undefined);assert.equal(b.sources.length,0);
    }
    f.DB.hooks.all=undefined;
    assert.ok(freshSql);
    const plan=f.db.prepare(`EXPLAIN QUERY PLAN ${freshSql}`).all(JSON.stringify(ids)).map(r=>String(r.detail)).join('\n');
    assert.match(plan,/SEARCH blocked USING INDEX .*\(prefix=\?\)/);
    assert.doesNotMatch(plan,/SCAN blocked(?:\s|$)/);
    const ordinary=[];
    ordinary.push(await save(f,{sessionId:session,turn:1,part:1.5,parts:2,captureKey:`${prefix}:1.5:hash`}));
    ordinary.push(await save(f,{sessionId:session,turn:1,part:3,captureKey:`${prefix}:3:hash`}));
    const seen=new Set();
    for(let attempt=0;attempt<8&&seen.size<ordinary.length;attempt++){
      f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');
      const independent=await prepare(f);
      for(const source of independent.sources)if(ordinary.includes(source.id)){assert.equal(source.deferReason,undefined);assert.equal(source.completeCapture,true);seen.add(source.id);}
      await defer(f,independent);
    }
    for(const id of ordinary)assert.ok(seen.has(id));
    for(let i=0;i<ordinary.length;i++)assert.equal((await f.api(`/v3/documents/${ordinary[i]}`,{metadata:{sessionId:session,turn:1,part:i===0?1.5:3,...(i===0?{parts:2}:{}),captureKey:`${session}:1:independent:${i}:hash`}},'PATCH')).status,200);
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:duplicate},'DELETE')).status,200);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');
    const restored=await prepare(f);assert.ok(restored.group);assert.notEqual(restored.group.phase,'deferred');assert.ok(restored.sources.length);
  }finally{f.DB.hooks.all=undefined;await f.drain();f.db.close();}
});

test('organization capture conflict: legacy empty-member group observed through a fresh nonanchor records the anchor revision',async()=>{
  const f=fixture();try{
    const session='legacy\u0000:日本😀';const prefix=`${session}:1:body`;
    const anchor=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${prefix}:1:hash`});
    const sibling=await save(f,{sessionId:session,turn:1,part:2,parts:2,captureKey:`${prefix}:2:hash`});
    const duplicate=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${session}:1:other:1:hash`});
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('legacy-retry',?,?,1,2,'deferred',?)").run(prefix,session,anchor);
    f.db.exec("UPDATE memory_organization_cursor SET next_lane=3;");
    // 既知markerのない旧groupを、anchor以外のfresh候補から観測する。
    f.db.exec("INSERT INTO memory_organization_batches(id,expires_at,snapshot_json) VALUES('other-lease','3000-01-01','{}')");
    f.db.prepare("INSERT INTO memory_organization_leases(memory_id,batch_id,expires_at) VALUES(?,'other-lease','3000-01-01')").run(anchor);
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','3000-01-01' FROM memories WHERE id=?").run(duplicate);
    assert.equal(f.db.prepare('SELECT 1 FROM memory_organization_progress WHERE memory_id=?').get(anchor),undefined);
    const first=await prepare(f);
    assert.equal(first.group.id,'legacy-retry');assert.equal(first.group.phase,'deferred');
    assert.ok(f.db.prepare('SELECT 1 FROM memory_organization_progress WHERE memory_id=?').get(anchor));await defer(f,first);due(f);
    assert.equal(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_group_members WHERE group_id='legacy-retry'").get().n,0);
    for(let lane=0;lane<4;lane++){f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);const b=await prepare(f);assert.equal(b.group,undefined);assert.equal(b.sources.length,0);}
  }finally{f.DB.hooks.all=undefined;await f.drain();f.db.close();}
});

test('organization capture conflict: anchor revision changing before deferred marker storage fails without a stale marker',async()=>{
  const f=fixture();try{
    const session='race\u0000session';const prefix=`${session}:1:body`;
    const anchor=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${prefix}:1:hash`});
    await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${session}:1:other:1:hash`});
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('race-retry',?,?,1,2,'reading',?)").run(prefix,session,anchor);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');
    f.DB.hooks.batch=async()=>{f.DB.hooks.batch=undefined;assert.equal((await f.api(`/v3/documents/${anchor}`,{content:'Changed evidence during deferral.'},'PATCH')).status,200);};
    const result=await f.api('/v4/organize/prepare',{});assert.equal(result.status,409);
    assert.equal(f.db.prepare('SELECT 1 FROM memory_organization_progress WHERE memory_id=?').get(anchor),undefined);
  }finally{f.DB.hooks.batch=undefined;await f.drain();f.db.close();}
});

test('organization capture conflict: fresh conflicting nonanchor becomes the private retry anchor until resolution',async()=>{
  const f=fixture();try{
    const session='clean-anchor\u0000日本';const prefix=`${session}:1:body`;
    const anchor=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${prefix}:1:hash`});
    const member=await save(f,{sessionId:session,turn:1,part:2,parts:2,captureKey:`${prefix}:2:hash`});
    const duplicate=await save(f,{sessionId:session,turn:1,part:2,parts:2,captureKey:`${session}:1:other:2:hash`});
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('clean-retry',?,?,1,2,'discovering',?)").run(prefix,session,anchor);
    f.db.exec("INSERT INTO memory_organization_batches(id,expires_at,snapshot_json) VALUES('anchor-lease','3000-01-01','{}'); UPDATE memory_organization_cursor SET next_lane=3;");
    f.db.prepare("INSERT INTO memory_organization_leases(memory_id,batch_id,expires_at) VALUES(?,'anchor-lease','3000-01-01')").run(anchor);
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','3000-01-01' FROM memories WHERE id=?").run(duplicate);
    const first=await prepare(f);assert.equal(first.group.id,'clean-retry');assert.equal(first.group.phase,'deferred');
    assert.equal(f.db.prepare("SELECT anchor_memory_id FROM memory_organization_groups WHERE id='clean-retry'").get().anchor_memory_id,member);
    assert.ok(f.db.prepare('SELECT 1 FROM memory_organization_progress WHERE memory_id=?').get(member));
    assert.equal(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_group_members WHERE group_id='clean-retry'").get().n,0);
    await defer(f,first);
    // 初回の非anchor選択を固定するためのfixture専用リースは役目を終えた。
    f.db.exec("UPDATE memory_organization_leases SET expires_at='2000-01-01' WHERE batch_id='anchor-lease'; UPDATE memory_organization_batches SET expires_at='2000-01-01' WHERE id='anchor-lease';");
    due(f);
    for(let lane=0;lane<4;lane++){f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);const b=await prepare(f);assert.equal(b.group,undefined);assert.equal(b.sources.length,0);}
    assert.equal((await f.api(`/v3/documents/${member}`,{content:'Updated second-part evidence while its duplicate remains.'},'PATCH')).status,200);
    due(f);
    const edited=await prepare(f);assert.equal(edited.group.id,'clean-retry');assert.equal(edited.group.phase,'deferred');assert.match(edited.group.reason,/conflicting capture identity/);
    await defer(f,edited);await explainSibling(f,duplicate);due(f);
    for(let lane=0;lane<4;lane++){f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);const b=await prepare(f);assert.equal(b.group,undefined);assert.equal(b.sources.length,0);}
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:duplicate},'DELETE')).status,200);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');
    const restored=await prepare(f);assert.equal(restored.group.id,'clean-retry');assert.notEqual(restored.group.phase,'deferred');assert.ok(restored.sources.length);
    assert.equal((await f.api('/v4/organize/commit',{batchId:restored.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Both original parts and their complete conclusions and reasons have been read.'}})).status,200);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');
    const ready=await prepare(f);assert.equal(ready.group.phase,'ready');assert.equal(ready.sources.length,1);assert.equal(ready.sources[0].id,anchor);
  }finally{await f.drain();f.db.close();}
});

test('organization capture conflict: moved anchor marker does not block the original empty-member group',async()=>{
  const f=fixture();try{
    const session='prefix\u0000日本';const prefix=`${session}:1:A`;const movedPrefix=`${session}:1:B`;
    const anchor=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${prefix}:1:hash`});
    const remaining=await save(f,{sessionId:session,turn:1,part:2,parts:2,captureKey:`${prefix}:2:hash`});
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('original-prefix',?,?,1,2,'deferred',?)").run(prefix,session,anchor);
    assert.equal((await f.api(`/v3/documents/${anchor}`,{metadata:{sessionId:session,turn:1,part:1,parts:2,captureKey:`${movedPrefix}:1:hash`}},'PATCH')).status,200);
    const duplicate=await save(f,{sessionId:session,turn:1,part:1,parts:2,captureKey:`${movedPrefix}:1:other`});
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('moved-prefix',?,?,1,2,'deferred',?)").run(movedPrefix,session,anchor);
    for(const id of [anchor,duplicate])f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'','2000-01-01' FROM memories WHERE id=?").run(id);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');
    let selectedByGroup=[];
    f.DB.hooks.all=(sql,values)=>{if(sql.includes('JOIN memory_organization_groups g ON g.id=w.id'))selectedByGroup=f.db.prepare(sql).all(...values);};
    const retry=await prepare(f);f.DB.hooks.all=undefined;
    assert.equal(selectedByGroup.length,0);
    assert.equal(retry.group.id,'original-prefix');assert.equal(retry.group.phase,'deferred');assert.equal(retry.group.reason,'Capture parts are missing');
    assert.equal(f.db.prepare("SELECT memory_id FROM memory_organization_group_members WHERE group_id='original-prefix'").get().memory_id,remaining);
  }finally{f.DB.hooks.all=undefined;await f.drain();f.db.close();}
});

test('organization capture conflict: unsafe known identity resumes after a safe metadata correction',async()=>{
  const f=fixture();try{
    const id=await save(f,{sessionId:'unsafe-retry',turn:1,part:1});
    f.db.prepare('UPDATE memories SET metadata_json=? WHERE id=?').run('{"sessionId":"unsafe-retry","turn":9007199254740993,"part":1}',id);
    const first=await prepare(f);assert.match(first.sources[0].deferReason,/safe numeric range/);await defer(f,first);due(f);
    for(let lane=0;lane<4;lane++){f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);assert.equal((await prepare(f)).sources.length,0);}
    assert.equal((await f.api(`/v3/documents/${id}`,{metadata:{sessionId:'unsafe-retry',turn:2,part:1}},'PATCH')).status,200);
    const restored=await prepare(f);assert.equal(restored.sources[0].id,id);assert.equal(restored.sources[0].deferReason,undefined);
  }finally{await f.drain();f.db.close();}
});

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
    const conflicting=await save(f,{...identity,captureKey:'new'},'Conflicting decision B already exists.');
    assert.equal((await f.api('/v4/organize/commit',payload(before))).status,409);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n,0);
    f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00.000Z' WHERE id=?").run(before.batchId);
    let source,b;
    for(let attempt=0;attempt<5;attempt++){b=await prepare(f);source=b.sources.find(s=>s.id===checkpoint);if(source)break;}
    assert.ok(source);assert.equal(source.deferReason,'Duplicate or conflicting capture identity');
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,0);
    await defer(f,b);due(f);
    for(let lane=0;lane<4;lane++){f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);assert.ok((await prepare(f)).sources.every(s=>s.id!==checkpoint));}
    assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:conflicting},'DELETE')).status,200);
    assert.ok((await prepare(f)).sources.some(s=>s.id===checkpoint&&!s.deferReason));
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
