// 手動整理の「直近N日分」指定(prepare days)の契約を、移行済みSQLiteと実Workerで確認する。
// 範囲外の除外、全体カーソル/公平性レーンの不変、延期済みの再出現防止、要約の再選択防止、
// 分割キャプチャの範囲跨ぎ、入力検証、従来の {} prepare の互換性を対象にする。
import test,{assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';

const cursorState=f=>({
  global:f.db.prepare('SELECT last_id,next_lane FROM memory_organization_cursor WHERE singleton=1').get(),
  lanes:f.db.prepare('SELECT kind,last_id FROM memory_organization_lane_cursors ORDER BY kind').all(),
});

test('organization days: selects only recent eligible records without moving global discovery state',async()=>{
  const f=fixture();
  const save=async(content,metadata={})=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
  try {
    const recent=await save('Recent decision and its reasons.');
    const deferred=await save('Recent ambiguous decision.');
    const old=await save('Old decision and its reasons.');
    f.db.prepare("UPDATE memories SET created_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days') WHERE id=?").run(old);
    const plan=f.db.prepare("EXPLAIN QUERY PLAN SELECT id FROM memories WHERE created_at>=strftime('%Y-%m-%dT%H:%M:%fZ','now',?)").all('-7 days');
    assert.ok(plan.some(row=>String(row.detail).includes('idx_memories_created')),JSON.stringify(plan));
    const before=cursorState(f);

    const first=await f.api('/v4/organize/prepare',{days:7});
    assert.equal(first.status,200,JSON.stringify(first.body));
    const ids=first.body.sources.map(source=>source.id).sort();
    assert.deepEqual(ids,[recent,deferred].sort());
    assert.deepEqual(cursorState(f),before);

    const source=first.body.sources.find(item=>item.id===recent);
    const later=first.body.sources.find(item=>item.id===deferred);
    const committed=await f.api('/v4/organize/commit',{batchId:first.body.batchId,
      summaries:[{ref:'s',title:'Recent conclusion',topic:'Evidence',content:'The recent decision follows from its stated reasons.',sources:[recent]}],
      coverage:[{id:source.id,revision:source.revision,status:'complete',summaries:['s']},
        {id:later.id,revision:later.revision,status:'deferred',summaries:[],memo:'Evidence is ambiguous; retry later.'}]});
    assert.equal(committed.status,200,JSON.stringify(committed.body));

    // 完了済み原本・新しい要約・延期中の記録は、同じ実行の次の prepare に出ない。
    const second=await f.api('/v4/organize/prepare',{days:7});
    assert.equal(second.status,200,JSON.stringify(second.body));
    assert.deepEqual(second.body.sources,[]);
    assert.equal(second.body.group,undefined);
    assert.deepEqual(second.body.range,{cursor:null,exhausted:true});
    assert.deepEqual(cursorState(f),before);

    // 範囲を広げると古い記録だけが対象になる。
    const wide=await f.api('/v4/organize/prepare',{days:60});
    assert.equal(wide.status,200,JSON.stringify(wide.body));
    assert.deepEqual(wide.body.sources.map(item=>item.id),[old]);
    assert.deepEqual(cursorState(f),before);

    // 従来の {} prepare は全体カーソルを従来どおり進める。
    f.db.prepare('DELETE FROM memory_organization_leases').run();
    const plain=await f.api('/v4/organize/prepare',{});
    assert.equal(plain.status,200,JSON.stringify(plain.body));
    assert.ok(JSON.stringify(cursorState(f))!==JSON.stringify(before));
  } finally {await f.drain();}
});

test('organization days: bounded range windows reach work behind thousands of ineligible rows',{timeout:90000},async()=>{
  const f=fixture();
  try {
    // 範囲内に適格でない行を大量に置き、その後ろにある1件へ有界な走査で到達できることを確認する。
    // 先に通常保存で 'memories' コンテナを作り、一括挿入の外部キーを満たす。
    const anchor=await f.api('/v3/documents',{containerTag:'memories',content:'Container anchor.',metadata:{}});
    assert.equal(anchor.status,201);await f.drain();
    f.db.prepare("UPDATE memories SET is_forgotten=1 WHERE id=?").run(anchor.body.id);
    f.db.exec(`WITH RECURSIVE n(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<6000)
      INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,is_forgotten,created_at,updated_at)
      SELECT printf('r-%05d',i),printf('r-%05d',i),'memories','Forgotten row','{}',1,
        strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 days'),strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 days') FROM n;`);
    const saved=await f.api('/v3/documents',{containerTag:'memories',content:'Newest decision and its reasons.',metadata:{}});
    assert.equal(saved.status,201);await f.drain();
    const before=cursorState(f);
    let windows=0;f.DB.hooks.all=sql=>{if(sql.includes('ORDER BY created_at,id LIMIT 512'))windows++;};
    let cursor;let found;const calls=[];
    for(let call=0;call<5 && !found;call++) {
      windows=0;
      const prepared=await f.api('/v4/organize/prepare',{days:7,...(cursor?{rangeCursor:cursor}:{})});
      assert.equal(prepared.status,200,JSON.stringify(prepared.body));
      calls.push(windows);
      assert.ok(windows<=8,`window scans per prepare: ${windows}`);
      if(prepared.body.sources.length)found=prepared.body;
      else {assert.equal(prepared.body.range.exhausted,false);cursor=prepared.body.range.cursor;assert.match(cursor,/^[A-Za-z0-9_-]+$/);}
    }
    assert.ok(found,JSON.stringify(calls));
    assert.deepEqual(found.sources.map(source=>source.id),[saved.body.id]);
    assert.ok(calls.length>=2,'the first call stops at its window bound');
    assert.deepEqual(cursorState(f),before);

    // 完了後、同じカーソルから範囲の終端まで有界に進んで exhausted を返す。
    const source=found.sources[0];
    const committed=await f.api('/v4/organize/commit',{batchId:found.batchId,
      summaries:[{ref:'s',title:'Newest conclusion',topic:'Evidence',content:'The newest decision follows from its reasons.',sources:[source.id]}],
      coverage:[{id:source.id,revision:source.revision,status:'complete',summaries:['s']}]});
    assert.equal(committed.status,200,JSON.stringify(committed.body));
    const done=await f.api('/v4/organize/prepare',{days:7,rangeCursor:found.range.cursor??undefined});
    assert.equal(done.status,200,JSON.stringify(done.body));
    assert.deepEqual(done.body.sources,[]);
    assert.deepEqual(done.body.range,{cursor:null,exhausted:true});

    for(const body of [{rangeCursor:cursor},{days:7,rangeCursor:'not valid!'},{days:7,rangeCursor:'e30'},{days:7,rangeCursor:''}]) {
      const rejected=await f.api('/v4/organize/prepare',body);
      assert.equal(rejected.status,400,JSON.stringify(body));
    }
  } finally {f.DB.hooks.all=undefined;await f.drain();}
});

test('organization days: a multipart capture with any in-range part is prepared as a group',async()=>{
  const f=fixture();
  const save=async(content,metadata={})=>{const r=await f.api('/v3/documents',{containerTag:'memories',content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id;};
  try {
    const parts=[];
    for(let part=1;part<=2;part++)parts.push(await save(`Capture conclusion part ${part}.`,{sessionId:'days',turn:1,part,parts:2,captureKey:`days:1:body:${part}:hash`}));
    f.db.prepare("UPDATE memories SET created_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days') WHERE id=?").run(parts[0]);
    const prepared=await f.api('/v4/organize/prepare',{days:7});
    assert.equal(prepared.status,200,JSON.stringify(prepared.body));
    assert.ok(prepared.body.group,JSON.stringify(prepared.body));
    assert.ok(prepared.body.sources.length>=1);
    assert.ok(prepared.body.sources.every(source=>parts.includes(source.id)));
  } finally {await f.drain();}
});

test('organization days: rejects invalid ranges and targeted combinations',async()=>{
  const f=fixture();
  try {
    const saved=await f.api('/v3/documents',{containerTag:'memories',content:'Recent decision.',metadata:{}});await f.drain();
    for(const body of [{days:0},{days:1.5},{days:'7'},{days:3651},{days:-1},{days:null},{days:7,sourceIds:[saved.body.id]}]) {
      const response=await f.api('/v4/organize/prepare',body);
      assert.equal(response.status,400,JSON.stringify(body));
    }
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_leases').get().n,0);
  } finally {await f.drain();}
});
