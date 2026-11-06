import test,{assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';

for(const recursive of [0,1])test(`organization core recovery: shared capture graph rebuilds with recursive_triggers=${recursive}`,async()=>{
  const f=fixture();const originals={};
  let scenarioError;
  const save=async(content,metadata={})=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
  try {
    f.db.exec(`PRAGMA recursive_triggers=${recursive}`);
    for(const name of ['A','B','C','D']) {
      originals[name]=[];
      for(let part=1;part<=2;part++)originals[name].push(await save(`Part ${part}: verified complete conclusion and reasons.`,{sessionId:name,turn:1,part,parts:2,captureKey:`${name}:1:body:${part}:hash`}));
    }
    const summaries={};
    for(const id of ['s1','s2','s3']) {
      summaries[id]=await save(`Shared summary ${id} and its supporting reasons.`);
      f.db.prepare("INSERT INTO memory_organization_summaries(id,memory_id,revision,status,updated_at) VALUES (?,?,'r1','active','2026-01-01')").run(id,summaries[id]);
      f.db.prepare('INSERT INTO memory_organization_versions(memory_id,summary_id) VALUES (?,?)').run(summaries[id],id);
    }
    for(const name of ['A','B','C','D']) {
      f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,status,anchor_memory_id) VALUES(?,?,?,1,2,'active',?)")
        .run(`group-${name}`,`${name}:1:body`,name,originals[name][0]);
      for(let part=1;part<=2;part++)f.db.prepare(`INSERT INTO memory_organization_group_members(group_id,part,memory_id,source_revision,read_offset,total)
        SELECT ?,?,id,COALESCE(topic_revision,updated_at),length(content),length(content) FROM memories WHERE id=?`).run(`group-${name}`,part,originals[name][part-1]);
    }
    for(const [group,summary]of[['A','s1'],['B','s1'],['B','s2'],['C','s2'],['C','s3'],['D','s3']])f.db.prepare('INSERT INTO memory_organization_group_summaries(group_id,summary_id) VALUES(?,?)').run(`group-${group}`,summary);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,8);
    if(recursive===0)assert.equal((await f.api(`/v3/documents/${originals.A[0]}`,undefined,'DELETE')).status,204);
    else {
      let mutationSql='';f.DB.hooks.execute=sql=>{mutationSql=sql;};
      let updated;
      try{updated=await f.api(`/v3/documents/${originals.A[1]}`,{content:'Updated complete conclusion and its new reasons.'},'PATCH');}
      finally{f.DB.hooks.execute=undefined;}
      assert.equal(updated.status,200,`Source PATCH failed in SQL: ${mutationSql}`);
    }
    if(recursive===0) {
      // B first reopens because S1 failed. Its reset invalidates S2 before C
      // becomes eligible; active, fully covered C is correctly skipped earlier.
      f.db.prepare("UPDATE memory_organization_lane_cursors SET last_id='group-A' WHERE kind='group'").run();
      f.db.prepare('UPDATE memory_organization_cursor SET next_lane=0').run();
      const reopenedB=await f.api('/v4/organize/prepare',{});assert.equal(reopenedB.status,200,JSON.stringify(reopenedB.body));
      assert.equal(reopenedB.body.group.id,'group-B');
      const target=originals.C[0];const predecessor=target.slice(0,-1)+String.fromCharCode(target.charCodeAt(target.length-1)-1);
      f.db.prepare('UPDATE memory_organization_cursor SET next_lane=3,last_id=?').run(predecessor);
      const prepared=await f.api('/v4/organize/prepare',{});assert.equal(prepared.status,200,JSON.stringify(prepared.body));
      assert.equal(prepared.body.group.id,'group-C');assert.equal(prepared.body.group.phase,'reading');
      assert.equal(f.db.prepare("SELECT status FROM memory_organization_summaries WHERE id='s3'").get().status,'invalid');
      assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(summaries.s3).is_forgotten,1);
      assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete WHERE memory_id IN(?,?)').get(...originals.D).n,0);
      assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:originals.C[0]},'DELETE')).status,200);
      assert.equal(f.db.prepare("SELECT status FROM memory_organization_summaries WHERE id='s3'").get().status,'invalid');
      assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(summaries.s3).is_forgotten,1);
      for(const id of originals.D)assert.equal((await f.api(`/v3/documents/${id}`,undefined,'GET')).status,200);
      return;
    }
    // OFF may leave C active with failed S2 and live S3. Re-discovery must
    // invalidate all its conclusions before removing any of its dependencies.
    let rebuilt=false;
    for(let run=0;run<24;run++) {
      const prepared=await f.api('/v4/organize/prepare',{});assert.equal(prepared.status,200,JSON.stringify(prepared.body));const b=prepared.body;
      let payload;
      if(b.group && b.group.phase!=='ready') {
        assert.notEqual(b.group.phase,'active');
        payload={batchId:b.batchId,summaries:[],coverage:[],groupProgress:{status:b.group.phase==='deferred'?'deferred':'partial',memo:'Preserve all available conclusions and reasons; missing evidence and original decision dates remain unknown.'}};
      } else {
        payload={batchId:b.batchId,summaries:b.sources.length?[{ref:'final',title:'Recovered conclusion',topic:'Recovery',content:'Recovered conclusion with every important supporting reason.',sources:b.sources.map(source=>source.id)}]:[],
          coverage:b.sources.map(source=>({id:source.id,revision:source.revision,status:'complete',summaries:['final']}))};
      }
      const committed=await f.api('/v4/organize/commit',payload);assert.equal(committed.status,200,JSON.stringify(committed.body));
      if(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_groups WHERE status='active'").get().n===(recursive===0?3:4)
        && f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n===(recursive===0?6:8)){rebuilt=true;break;}
    }
    assert.ok(rebuilt);
    assert.equal(f.db.prepare("SELECT status FROM memory_organization_summaries WHERE id='s3'").get().status,'invalid');
    assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(summaries.s3).is_forgotten,1);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete WHERE memory_id IN(?,?)').get(...originals.D).n,2);
    const dependent=f.db.prepare("SELECT summary_id FROM memory_organization_group_summaries WHERE group_id='group-C'").get().summary_id;
    if(recursive===0)assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:originals.C[1]},'DELETE')).status,200);
    else assert.equal((await f.api(`/v3/documents/${originals.C[1]}`,undefined,'DELETE')).status,204);
    assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(dependent).status,'invalid');
    assert.notEqual(f.db.prepare("SELECT status FROM memory_organization_groups WHERE id='group-C'").get().status,'active');
    assert.equal((await f.api(`/v3/documents/${originals.C[0]}`,undefined,'GET')).status,200);
  }catch(error){scenarioError=error;throw error;}
  finally{
    await f.drain();
    try{
      // Product operations and drained background work ran with the requested
      // setting. DROP-all fixture teardown is outside the product contract.
      f.db.exec('PRAGMA recursive_triggers=OFF');f.db.close();
    }catch(error){throw Error(scenarioError?`Scenario failed: ${scenarioError.stack??scenarioError}; cleanup also failed: ${error.stack??error}`:`Scenario completed; cleanup failed: ${error.stack??error}`);}
  }
});
