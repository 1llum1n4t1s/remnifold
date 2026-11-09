import test,{assert} from './support/test.mjs';
import {fixture,migrations} from './worker-fixture.mjs';

test('organization core queues: continuations and due work progress before a 50000-row fresh sweep wraps',{timeout:90000},async()=>{
  const f=fixture();
  const save=async(content,metadata={})=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
  try {
    const partial=await save('Partial decision and reasons. '.repeat(3000));
    const deferred=await save('Deferred decision and reasons.');
    const parts=[];
    for(let part=1;part<=2;part++)parts.push(await save('Capture conclusion and reasons. '.repeat(600),{sessionId:'fair',turn:1,part,parts:2,captureKey:`fair:1:body:${part}:hash`}));
    for(const [id,offset]of[[partial,8000],[deferred,0]]) f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),?,'Existing private conclusion and reasons.','2000-01-01' FROM memories WHERE id=?`).run(offset,id);
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES('fair-group','fair:1:body','fair',1,2,'reading',?)").run(parts[0]);
    for(let part=1;part<=2;part++)f.db.prepare(`INSERT INTO memory_organization_group_members(group_id,part,memory_id,source_revision)
      SELECT 'fair-group',?,id,COALESCE(topic_revision,updated_at) FROM memories WHERE id=?`).run(part,parts[part-1]);
    f.db.exec(`WITH RECURSIVE n(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<50000)
      INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,is_forgotten,created_at,updated_at)
      SELECT printf('z-%05d',i),printf('z-%05d',i),'memories','Unrelated archive row','{}',1,'2020-01-01','2020-01-01' FROM n;
      UPDATE memories SET is_forgotten=0 WHERE id='z-00010';
      UPDATE memory_organization_cursor SET last_id='z-00000',next_lane=0;`);
    const plan=f.db.prepare('EXPLAIN QUERY PLAN SELECT memory_id FROM memory_organization_progress WHERE next_offset>0 AND memory_id>? ORDER BY memory_id LIMIT 64').all('');
    assert.ok(plan.some(row=>String(row.detail).includes('idx_organization_partial_ids')));
    const scans={group:0,progress:0,fresh:0};const seen=new Set();
    f.DB.hooks.all=sql=>{if(sql.includes('ORDER BY id LIMIT 64'))scans.group++;if(sql.includes('ORDER BY memory_id LIMIT 64'))scans.progress++;if(sql==='SELECT id FROM memories WHERE id>? ORDER BY id LIMIT 512')scans.fresh++;};
    for(let run=0;run<12;run++) {
      const prepared=await f.api('/v4/organize/prepare',{});assert.equal(prepared.status,200,JSON.stringify(prepared.body));const b=prepared.body;
      for(const source of b.sources)seen.add(source.id);
      let payload;
      if(b.group && b.group.phase!=='ready')payload={batchId:b.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Preserve all source IDs, conclusions, reasons and unknown decision dates across every chunk.'}};
      else {
        const complete=b.sources.filter(source=>source.end===source.total && source.completeCapture);
        payload={batchId:b.batchId,summaries:complete.length?[{ref:'s',title:'Conclusion',topic:'Evidence',content:'The verified conclusion follows from all represented reasons.',sources:complete.map(source=>source.id)}]:[],
          coverage:b.sources.map(source=>({id:source.id,revision:source.revision,status:complete.includes(source)?'complete':'partial',summaries:complete.includes(source)?['s']:[],...(!complete.includes(source)?{memo:'Existing conclusions and reasons; later chunks remain unread.'}:{})}))};
      }
      const committed=await f.api('/v4/organize/commit',payload);assert.equal(committed.status,200,JSON.stringify(committed.body));
    }
    assert.ok(seen.has(partial));assert.ok(seen.has(deferred));assert.ok(seen.has('z-00010'));assert.ok(parts.every(id=>seen.has(id)));
    assert.ok(f.db.prepare('SELECT next_offset FROM memory_organization_progress WHERE memory_id=?').get(partial).next_offset>=24000);
    assert.ok(f.db.prepare("SELECT SUM(read_offset) n FROM memory_organization_group_members WHERE group_id='fair-group'").get().n>=32000);
    assert.ok(f.db.prepare('SELECT last_id FROM memory_organization_cursor').get().last_id<'z-50000');
    assert.ok(scans.group<=24 && scans.progress<=48 && scans.fresh<=12);
  }finally{f.DB.hooks.all=undefined;await f.drain();f.db.close();}
});

const prepare=async f=>{const r=await f.api('/v4/organize/prepare',{});assert.equal(r.status,200,JSON.stringify(r.body));return r.body;};
const defer=async(f,b)=>{const r=await f.api('/v4/organize/commit',{batchId:b.batchId,summaries:[],coverage:b.sources.map(s=>({id:s.id,revision:s.revision,status:'deferred',summaries:[],memo:'Unresolved evidence requires another visit.'}))});assert.equal(r.status,200,JSON.stringify(r.body));};
function seedDue(f,count,size=40){
  f.db.exec("INSERT OR IGNORE INTO container_tags(tag,name,created_at,updated_at) VALUES('memories','Memories','2020-01-01','2020-01-01')");
  for(let i=0;i<count;i++){
    const id=`due-${String(i).padStart(3,'0')}`;
    f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES(?,?,'memories',?,'{}','2020-01-01','2020-01-01')").run(id,id,'Evidence and supporting reasons. '.repeat(Math.ceil(size/32)).slice(0,size));
    f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) VALUES(?,'2020-01-01',0,'Private pending conclusion',?)").run(id,`2000-01-01T00:00:${String(i%60).padStart(2,'0')}.000Z`);
  }
}
function dueLane(f){f.db.exec("UPDATE memory_organization_cursor SET next_lane=2; UPDATE memory_organization_lane_cursors SET last_id='' WHERE kind='deferred';");}

test('organization due FIFO survives repeated deferrals and a daily run before yesterday commit time',async()=>{
  const f=fixture();try{
    seedDue(f,61);dueLane(f);
    const first=await prepare(f);assert.equal(first.sources.length,20);await defer(f,first);
    const delay=f.db.prepare("SELECT (julianday(next_attempt)-julianday('now'))*24 hours FROM memory_organization_progress WHERE memory_id=?").get(first.sources[0].id).hours;
    assert.ok(delay>0.99&&delay<=1.001);
    // Simulate tomorrow's run starting one minute before yesterday's commit.
    // Preserve relative retry ordering instead of assigning one deadline to all rows.
    f.db.exec("UPDATE memory_organization_progress SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-1439 minutes');");
    dueLane(f);const second=await prepare(f);assert.equal(second.sources.length,20);
    assert.ok(second.sources.every(s=>!first.sources.some(old=>old.id===s.id)));await defer(f,second);
    dueLane(f);const third=await prepare(f);assert.equal(third.sources.length,20);
    assert.ok(third.sources.every(s=>![...first.sources,...second.sources].some(old=>old.id===s.id)));await defer(f,third);
  }finally{await f.drain();f.db.close();}
});

test('organization text budget leaves omitted due records ahead of retried records',async()=>{
  const f=fixture();try{
    seedDue(f,20,8000);dueLane(f);const first=await prepare(f);
    assert.equal(first.sources.length,6);assert.equal(first.sources.reduce((n,s)=>n+s.content.length,0),48000);await defer(f,first);
    f.db.exec("UPDATE memory_organization_progress SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-2 hours');");
    dueLane(f);const second=await prepare(f);assert.equal(second.sources.length,6);
    assert.ok(second.sources.every(s=>!first.sources.some(old=>old.id===s.id)));await defer(f,second);
  }finally{await f.drain();f.db.close();}
});

test('organization due raw windows advance beyond 64 without unbounded eligibility scans',async()=>{
  const f=fixture();try{
    seedDue(f,70);f.db.exec("UPDATE memories SET is_forgotten=1 WHERE id<'due-064';");dueLane(f);
    const first=await prepare(f);await defer(f,first);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=2');const next=await prepare(f);
    const seen=[...first.sources,...next.sources];assert.equal(seen.length,6);assert.ok(seen.every(s=>s.id>='due-064'));await defer(f,next);
  }finally{await f.drain();f.db.close();}
});

test('organization multipart partial and deferred groups yield to older queued groups despite anchor progress',async()=>{
  const f=fixture();try{
    for(let i=0;i<3;i++){
      const session=`group-${i}`;let anchor;
      for(let part=1;part<=2;part++){
        const r=await f.api('/v3/documents',{containerTag:'memories',content:'Capture conclusions and reasons. '.repeat(400),metadata:{sessionId:session,turn:1,part,parts:2,captureKey:`${session}:1:body:${part}:hash`}});
        assert.equal(r.status,201);await f.drain();anchor??=r.body.id;
      }
      f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id,next_attempt) VALUES(?,?,?,1,2,'discovering',?,?)").run(session,`${session}:1:body`,session,anchor,`2000-01-01T00:00:0${i}.000Z`);
      // This old per-document deadline must not steal a fresh/progress lane forever.
      f.db.prepare("INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt) SELECT id,COALESCE(topic_revision,updated_at),0,'Old anchor memo','' FROM memories WHERE id=?").run(anchor);
    }
    const visit=async lane=>{f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);f.db.exec("UPDATE memory_organization_lane_cursors SET last_id='' WHERE kind='group'");return prepare(f);};
    const first=await visit(0);assert.equal(first.group.id,'group-0');
    const partial=await f.api('/v4/organize/commit',{batchId:first.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Private combined conclusions and remaining evidence.'}});assert.equal(partial.status,200,JSON.stringify(partial.body));
    assert.ok(f.db.prepare("SELECT next_attempt FROM memory_organization_groups WHERE id='group-0'").get().next_attempt);
    const second=await visit(2);assert.equal(second.group.id,'group-1');
    const deferred=await f.api('/v4/organize/commit',{batchId:second.batchId,summaries:[],coverage:[],groupProgress:{status:'deferred',memo:'Unresolved evidence.'}});assert.equal(deferred.status,200);
    const hours=f.db.prepare("SELECT (julianday(next_attempt)-julianday('now'))*24 hours FROM memory_organization_groups WHERE id='group-1'").get().hours;assert.ok(hours>0.99&&hours<=1.001);
    f.db.exec("UPDATE memory_organization_groups SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-2 hours') WHERE id='group-1'");
    const third=await visit(3);assert.equal(third.group.id,'group-2');
    const finish=await f.api('/v4/organize/commit',{batchId:third.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Preserve all conclusions and remaining evidence.'}});assert.equal(finish.status,200);
  }finally{await f.drain();f.db.close();}
});

test('organization response byte trimming preserves omitted due work for the next visit',async()=>{
  const f=fixture();try{
    seedDue(f,20,2400);
    const metadata=Object.fromEntries(Object.entries({title:200,filepath:2048,sessionId:160,rootSessionId:160,sm_scope:160,sm_project_id:160,project:160,sourceTimestamp:64,completedAt:64}).map(([key,size])=>[key,'\u0001'.repeat(size)]));
    metadata.topics=Array(8).fill('\u0001'.repeat(80));
    f.db.prepare('UPDATE memories SET content=?,metadata_json=?').run('\u0001'.repeat(2400),JSON.stringify(metadata));
    f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),0,?,'2000-01-01' FROM memories WHERE 1
      ON CONFLICT(memory_id) DO UPDATE SET source_revision=excluded.source_revision,memo=excluded.memo,next_attempt=excluded.next_attempt`).run('\u0001'.repeat(4000));
    dueLane(f);const first=await prepare(f);
    assert.equal(first.summaries.length,0);assert.ok(first.sources.length>0&&first.sources.length<20);
    assert.ok(first.sources.reduce((n,s)=>n+s.content.length,0)<48000);
    assert.ok(new TextEncoder().encode(JSON.stringify(first)).byteLength<1024*1024);
    await defer(f,first);
    f.db.exec("UPDATE memory_organization_progress SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-2 hours')");
    dueLane(f);const second=await prepare(f);
    assert.ok(second.sources.some(s=>!first.sources.some(old=>old.id===s.id)));
    assert.ok(new TextEncoder().encode(JSON.stringify(second)).byteLength<1024*1024);await defer(f,second);
  }finally{await f.drain();f.db.close();}
});

test('organization group raw windows advance past 64 leased groups and preserve partial continuation',async()=>{
  const f=fixture();try{
    // These raw queue entries cannot yield a source, but must advance the cursor.
    for(let i=0;i<64;i++)f.db.prepare(`INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,lease_until)
      VALUES(?,?,?,1,2,'reading','2099-01-01')`).run(`a-${String(i).padStart(3,'0')}`,`blocked-${i}`,`blocked-${i}`);
    const ids=[];
    for(let part=1;part<=2;part++){
      const r=await f.api('/v3/documents',{containerTag:'memories',content:'Multipart evidence and reasons. '.repeat(600),metadata:{sessionId:'beyond-window',turn:1,part,parts:2,captureKey:`beyond-window:1:body:${part}:hash`}});
      assert.equal(r.status,201);await f.drain();ids.push(r.body.id);
    }
    f.db.prepare(`INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id,next_attempt)
      VALUES('z-live','beyond-window:1:body','beyond-window',1,2,'discovering',?,'2000-01-01')`).run(ids[0]);
    // Keep fresh discovery out of this cursor-specific journey without changing
    // group eligibility: the group lane owns its own retry clock.
    for(const id of ids)f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),0,'','2099-01-01' FROM memories WHERE id=?`).run(id);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');
    const first=await prepare(f);assert.equal(first.group,undefined);assert.equal(first.sources.length,0);await defer(f,first);
    assert.equal(f.db.prepare("SELECT last_id FROM memory_organization_lane_cursors WHERE kind='group'").get().last_id,'a-063');
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');
    const second=await prepare(f);assert.equal(second.group.id,'z-live');assert.ok(second.sources.length>0);
    const commit=await f.api('/v4/organize/commit',{batchId:second.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Combined evidence retained across the bounded queue window.'}});assert.equal(commit.status,200);
    const offsets=f.db.prepare("SELECT SUM(read_offset) n FROM memory_organization_group_members WHERE group_id='z-live'").get().n;assert.ok(offsets>0);
    // One wrap visits the leased prefix; the following window reaches the tail again.
    for(let run=0;run<2;run++){
      f.db.exec('UPDATE memory_organization_cursor SET next_lane=0');const next=await prepare(f);
      if(next.group){assert.equal(next.group.id,'z-live');assert.ok(next.sources.every(s=>s.offset>0||!second.sources.some(old=>old.id===s.id)));const r=await f.api('/v4/organize/commit',{batchId:next.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Continue preserving all combined evidence.'}});assert.equal(r.status,200);}
      else await defer(f,next);
    }
    assert.ok(f.db.prepare("SELECT SUM(read_offset) n FROM memory_organization_group_members WHERE group_id='z-live'").get().n>offsets);
  }finally{await f.drain();f.db.close();}
});

for(const lane of [2,3])test(`organization invalid future group recovers through ${lane===2?'progress':'fresh'} after a sibling edit`,async()=>{
  const f=fixture();try{
    const session='invalid-復旧😀';const ids=[];
    for(let part=1;part<=2;part++){
      const r=await f.api('/v3/documents',{containerTag:'memories',content:'Original multipart conclusions and supporting reasons.',metadata:{sessionId:session,turn:1,part,parts:2,captureKey:`${session}:1:body:${part}:hash`}});
      assert.equal(r.status,201);await f.drain();ids.push(r.body.id);
    }
    f.db.prepare(`INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id,next_attempt)
      VALUES('invalid-future',?,?,1,2,'deferred',?,'2099-01-01')`).run(`${session}:1:body`,session,ids[0]);
    f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),0,'Anchor private evidence','2000-01-01' FROM memories WHERE id=?`).run(ids[0]);
    const anchorProgress=f.db.prepare('SELECT * FROM memory_organization_progress WHERE memory_id=?').get(ids[0]);
    const edited=await f.api(`/v3/documents/${ids[1]}`,{content:'Edited sibling conclusions and verified supporting reasons.'},'PATCH');assert.equal(edited.status,200);
    const invalid=f.db.prepare("SELECT generation,status,next_attempt FROM memory_organization_groups WHERE id='invalid-future'").get();
    assert.equal(invalid.status,'invalid');assert.equal(invalid.next_attempt,'2099-01-01');
    assert.deepEqual(f.db.prepare('SELECT * FROM memory_organization_progress WHERE memory_id=?').get(ids[0]),anchorProgress);
    // Isolate the anchor's current progress: the edited sibling cannot provide
    // a separate fresh first-explanation route around the regression.
    f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),0,'','2099-01-01' FROM memories WHERE id=?`).run(ids[1]);
    f.db.prepare('UPDATE memory_organization_cursor SET next_lane=?').run(lane);
    const recovered=await prepare(f);assert.equal(recovered.group.id,'invalid-future');assert.equal(recovered.group.generation,invalid.generation+1);assert.equal(recovered.group.phase,'reading');assert.equal(recovered.sources.length,2);
    const committed=await f.api('/v4/organize/commit',{batchId:recovered.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Both updated parts and their conclusions and reasons are retained.'}});assert.equal(committed.status,200);
  }finally{await f.drain();f.db.close();}
});

test('organization fresh due group yields to earlier saved untouched work after partial commit',async()=>{
  const f=fixture();try{
    const ordinary=await f.api('/v3/documents',{containerTag:'memories',content:'Untouched ordinary conclusion and supporting reasons.',metadata:{}});assert.equal(ordinary.status,201);await f.drain();
    f.db.prepare("UPDATE memories SET created_at='2001-01-01T00:00:00.000Z' WHERE id=?").run(ordinary.body.id);
    const session='fresh\u0000日本😀';const ids=[];
    for(let part=1;part<=2;part++){
      const r=await f.api('/v3/documents',{containerTag:'memories',content:'Multipart conclusions and their reasons.',metadata:{sessionId:session,turn:1,part,parts:2,captureKey:`${session}:1:body:${part}:hash`}});
      assert.equal(r.status,201);await f.drain();ids.push(r.body.id);
    }
    f.db.prepare(`INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id,next_attempt)
      VALUES('fresh-due',?,?,1,2,'discovering',?,'2000-01-01')`).run(`${session}:1:body`,session,ids[0]);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');const first=await prepare(f);assert.equal(first.group.id,'fresh-due');
    const committed=await f.api('/v4/organize/commit',{batchId:first.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Both multipart conclusions and supporting reasons were preserved.'}});assert.equal(committed.status,200);
    const priority=f.db.prepare("SELECT next_attempt FROM memory_organization_groups WHERE id='fresh-due'").get().next_attempt;
    const arrival=f.db.prepare('SELECT created_at FROM memories WHERE id=?').get(ordinary.body.id).created_at;assert.ok(priority>arrival);
    f.db.exec('UPDATE memory_organization_cursor SET next_lane=3');const next=await prepare(f);
    assert.equal(next.group,undefined);assert.equal(next.sources[0].id,ordinary.body.id);await defer(f,next);
  }finally{await f.drain();f.db.close();}
});

for(const future of [false,true])test(`organization older ordinary due work precedes a ${future?'future':'later due'} multipart group with stale anchor progress`,async()=>{
  const f=fixture();try{
    seedDue(f,19);const ids=[];const session='mixed\u0000日本😀';
    for(let part=1;part<=2;part++){
      const r=await f.api('/v3/documents',{containerTag:'memories',content:'Multipart conclusions and reasons.',metadata:{sessionId:session,turn:1,part,parts:2,captureKey:`${session}:1:body:${part}:hash`}});
      assert.equal(r.status,201);await f.drain();ids.push(r.body.id);
    }
    f.db.prepare(`INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id,next_attempt)
      VALUES('mixed',?,?,1,2,'deferred',?,?)`).run(`${session}:1:body`,session,ids[0],future?'2099-01-01':'2001-01-01');
    for(let part=0;part<2;part++)f.db.prepare(`INSERT INTO memory_organization_progress(memory_id,source_revision,next_offset,memo,next_attempt)
      SELECT id,COALESCE(topic_revision,updated_at),0,'Old private anchor progress',? FROM memories WHERE id=?`).run(part===0?'1990-01-01':'2099-01-01',ids[part]);
    dueLane(f);const first=await prepare(f);
    assert.equal(first.group,undefined);assert.equal(first.sources.length,19);assert.ok(first.sources.every(s=>s.id.startsWith('due-')));await defer(f,first);
    const original=f.db.prepare("SELECT next_attempt FROM memory_organization_groups WHERE id='mixed'").get().next_attempt;
    dueLane(f);const next=await prepare(f);
    if(future){assert.equal(next.group,undefined);assert.equal(next.sources.length,0);await defer(f,next);assert.equal(f.db.prepare("SELECT next_attempt FROM memory_organization_groups WHERE id='mixed'").get().next_attempt,original);}
    else {assert.equal(next.group.id,'mixed');const r=await f.api('/v4/organize/commit',{batchId:next.batchId,summaries:[],coverage:[],groupProgress:{status:'partial',memo:'Combined source conclusions and reasons.'}});assert.equal(r.status,200);}
  }finally{await f.drain();f.db.close();}
});

test('organization retry migration preserves progress and group state while rebasing only future deferrals',async()=>{
  const f=fixture({beforeMigration:'0012_organization_retry_fairness.sql'});try{
    seedDue(f,4);
    f.db.exec(`UPDATE memory_organization_progress SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ','now','+24 hours') WHERE memory_id='due-000';
      UPDATE memory_organization_progress SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ','now','+2 hours') WHERE memory_id='due-001';
      UPDATE memory_organization_progress SET next_offset=8000,next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ','now','+24 hours') WHERE memory_id='due-002';
      INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,memo,reason,next_attempt,batch_id,lease_until)
        VALUES('deferred','prefix','session',1,2,'deferred','Private memo','Unresolved',strftime('%Y-%m-%dT%H:%M:%fZ','now','+24 hours'),'lease','2099-01-01'),
        ('reading','other','session',2,2,'reading','Reading memo',NULL,strftime('%Y-%m-%dT%H:%M:%fZ','now','+24 hours'),NULL,NULL);`);
    const before=f.db.prepare('SELECT * FROM memory_organization_progress ORDER BY memory_id').all();
    const groups=f.db.prepare('SELECT * FROM memory_organization_groups ORDER BY id').all();
    f.db.exec(migrations.find(m=>m.name==='0012_organization_retry_fairness.sql').sql);
    const after=f.db.prepare('SELECT * FROM memory_organization_progress ORDER BY memory_id').all();
    for(let i=0;i<4;i++){
      assert.deepEqual({...after[i],next_attempt:before[i].next_attempt},before[i]);
      assert.equal(Date.parse(after[i].next_attempt),Date.parse(before[i].next_attempt)-(i<2?23*3600000:0));
    }
    const changed=f.db.prepare('SELECT * FROM memory_organization_groups ORDER BY id').all();
    for(let i=0;i<2;i++){
      assert.deepEqual({...changed[i],next_attempt:groups[i].next_attempt},groups[i]);
      assert.equal(Date.parse(changed[i].next_attempt),Date.parse(groups[i].next_attempt)-(i===0?23*3600000:0));
    }
  }finally{await f.drain();f.db.close();}
});
