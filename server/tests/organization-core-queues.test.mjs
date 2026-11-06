import test,{assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';

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
