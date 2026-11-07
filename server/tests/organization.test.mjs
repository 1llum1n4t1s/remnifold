import test, { assert } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';
import worker from '../src/index.ts';

async function waitFor(promise) { let timer; try { return await Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Fixture barrier did not complete')),5000)})]); } finally { clearTimeout(timer); } }

async function tick(f) {
  const tasks = [];
  await worker.scheduled({cron:'*/15 * * * *'},f.env,{waitUntil:task=>tasks.push(task)});
  await Promise.all(tasks);
}
function semanticFixture(f) {
  const vectors = new Map(); const ai = []; const control = {failEmbedding:false,failDelete:false,hits:null,onDelete:null,onUpsert:null,onEmbedding:null};
  const run = f.env.AI.run;
  f.env.AI.run = async (model,input) => {
    ai.push(input);
    if(input.text && control.onEmbedding) await control.onEmbedding(input);
    if(input.text && control.failEmbedding) throw Error('fixture embedding unavailable');
    return run(model,input);
  };
  f.env.MEMORY_VECTORS.upsert = async values => { if(control.onUpsert) await control.onUpsert(values); for(const value of values)vectors.set(value.id,value);return {mutationId:'organization-vector'}; };
  f.env.MEMORY_VECTORS.getByIds = async ids => ids.flatMap(id=>vectors.has(id)?[vectors.get(id)]:[]);
  f.env.MEMORY_VECTORS.query = async (_vector, options) => ({matches:(control.hits??[...vectors.keys()]).filter(id=>vectors.has(id)).slice(0,options.topK).map(id=>({id,score:0.99,metadata:vectors.get(id).metadata})),count:vectors.size});
  f.env.MEMORY_VECTORS.deleteByIds = async ids => {
    if(control.failDelete) throw Error('fixture vector delete unavailable');
    if(control.onDelete) await control.onDelete(ids);
    for(const id of ids)vectors.delete(id);return {};
  };
  const seedEmbedding = (id,axis=0) => {
    const values=Array.from({length:1024},(_,index)=>index===axis?1:0);
    f.db.prepare("UPDATE memories SET embedding_json=?,embedding_status='done',vector_status='indexed' WHERE id=?").run(JSON.stringify(values),id);
    const revision=f.db.prepare('SELECT topic_revision FROM memories WHERE id=?').get(id).topic_revision;
    vectors.set(id,{id,values,metadata:{topic_revision:revision}});
  };
  return {vectors,ai,control,seedEmbedding};
}

function scenario(name, run) { test(`organization: ${name}`, async () => {
  const f = fixture();
  const cleanup=[];
  const save = async (content = 'Original conclusion and its reasons 😀', metadata = {}) => {
    const r = await f.api('/v3/documents', { containerTag: 'memories', content, metadata });
    assert.equal(r.status, 201); await f.drain(); return r.body.id;
  };
  const prepare = async () => { const r = await f.api('/v4/organize/prepare', {}); assert.equal(r.status, 200, JSON.stringify(r.body)); return r.body; };
  const payload = (batch, count = 1) => {
    const groups = [...new Set(batch.sources.map(source => source.partition))];
    return { batchId: batch.batchId,
      summaries: groups.flatMap((partition, group) => Array.from({length: count}, (_, n) => ({ ref: `s${group}-${n}`, title: `Conclusion ${n}`, topic: `Evidence ${n}`,
        content: `Final conclusion ${n}: selected the verified approach because the original evidence supports it. 😀`, sources: batch.sources.filter(source => source.partition === partition).map(source => source.id) }))),
      coverage: batch.sources.map(source => ({ id: source.id, revision: source.revision, status: 'complete', summaries: Array.from({length: count}, (_, n) => `s${groups.indexOf(source.partition)}-${n}`) })) };
  };
  try { await run({ ...f, save, prepare, payload,cleanup:callback=>cleanup.push(callback) }); } finally { for(const callback of cleanup)callback();await f.drain(); f.db.close(); }
}); }

scenario('atomic publication hides ordinary search while recent awareness, direct access and lists preserve originals', async f => {
  const id = await f.save(undefined, { type: 'conversation', sessionId: 'old-task' });
  const batch = await f.prepare(); const body = f.payload(batch);
  const pending = JSON.parse(f.db.prepare('SELECT snapshot_json FROM memory_organization_batches WHERE id=?').get(batch.batchId).snapshot_json);
  assert.equal(pending.sources[0].content, undefined); assert.equal(pending.sources[0].metadata, undefined); assert.equal(pending.sources[0].memo, undefined);
  const first = await f.api('/v4/organize/commit', body); assert.equal(first.status, 200, JSON.stringify(first.body));
  const receipt = f.db.prepare('SELECT request_json,snapshot_json FROM memory_organization_batches WHERE id=?').get(batch.batchId);
  assert.match(receipt.request_json, /^[0-9a-f]{64}$/u); assert.deepEqual(JSON.parse(receipt.snapshot_json), { sources: [], summaries: [] });
  assert.deepEqual((await f.api('/v4/organize/commit', body)).body, first.body);
  assert.equal((await f.api('/v4/organize/commit', { ...body, summaries: [{ ...body.summaries[0], content: 'different' }] })).status, 409);
  assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'GET')).status, 200);
  assert.ok((await f.api('/v3/documents/list', {})).body.documents.some(d => d.id === id));
  for (const indexOnly of [false, true]) {
    const result = await f.api('/v4/search', { q: 'Original conclusion', allContainers: true, indexOnly });
    assert.equal(result.status, 200); assert.ok(!result.body.results.some(d => d.id === id));
  }
  const recent = await f.api('/v3/documents/list', { projection: 'index', recentTasks: true });
  assert.ok(recent.body.documents.some(d => d.id === id));
  const summary = first.body.summaries[0];
  assert.equal((await f.api(`/v3/documents/${summary.memoryId}`, { content: 'edit' }, 'PATCH')).status, 409);
  assert.equal((await f.api('/v3/documents', { containerTag: 'memories', customId: `organization:${summary.id}:${summary.revision}`, content: 'edit' })).status, 409);
  assert.equal(f.calls.length, 0);
});

scenario('all required summaries must remain valid; forgetting one restores original visibility', async f => {
  const id = await f.save(); const batch = await f.prepare(); const result = await f.api('/v4/organize/commit', f.payload(batch, 2));
  assert.equal(result.status, 200); assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 1);
  await f.api('/v4/memories', { containerTag: 'memories', documentId: result.body.summaries[1].memoryId }, 'DELETE');
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'GET')).status, 200);
});

for (const mutation of ['update', 'forget', 'delete', 'reenrich']) scenario(`${mutation} invalidates derived memories without reviving forgotten originals`, async f => {
  const id = await f.save(); const result = await f.api('/v4/organize/commit', f.payload(await f.prepare()));
  assert.equal(result.status, 200); const summary = result.body.summaries[0];
  if (mutation === 'update') assert.equal((await f.api(`/v3/documents/${id}`, { content: 'Changed reason' }, 'PATCH')).status, 200);
  if (mutation === 'forget') assert.equal((await f.api('/v4/memories', { containerTag: 'memories', documentId: id }, 'DELETE')).status, 200);
  if (mutation === 'delete') assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'DELETE')).status, 204);
  if (mutation === 'reenrich') { f.env.AI_ENRICHMENT_MODE = 'on'; assert.equal((await f.api('/v4/enrich', { id })).status, 202); }
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  assert.equal((await f.api(`/v3/documents/${summary.memoryId}`, undefined, 'GET')).status, 409);
  if (mutation === 'forget' || mutation === 'delete') assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'GET')).status, 404);
});

scenario('late mutation, expired lease and injected failure publish nothing', async f => {
  const id = await f.save(); const batch = await f.prepare(); const body = f.payload(batch);
  await f.api(`/v3/documents/${id}`, { content: 'New revision' }, 'PATCH');
  assert.equal((await f.api('/v4/organize/commit', body)).status, 409);
  f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00Z'").run();
  assert.equal((await f.api('/v4/organize/commit', body)).status, 409);
  const next = await f.prepare();
  f.DB.hooks.execute = sql => { if (sql.includes('INSERT INTO memory_organization_coverage(')) throw Error('injected-private-failure'); };
  assert.equal((await f.api('/v4/organize/commit', f.payload(next))).status, 500);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n, 0);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  f.DB.hooks.execute = undefined;
  assert.equal((await f.api('/v4/organize/commit', f.payload(next))).status, 200);
});

scenario('chunk progress stays private, resumes exact revision and resets after edit', async f => {
  const id = await f.save('a'.repeat(7999) + '😀' + 'b'.repeat(9000));
  const first = await f.prepare(); const s = first.sources[0]; assert.equal(s.end, 7999);
  const body = { batchId: first.batchId, summaries: [], coverage: [{ id, revision: s.revision, status: 'partial', summaries: [], memo: 'Conclusion and reasons so far; further topics unresolved.' }] };
  assert.equal((await f.api('/v4/organize/commit', body)).status, 200);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  const next = await f.prepare(); assert.equal(next.sources[0].offset, 7999); assert.ok(next.sources[0].content.startsWith('😀')); assert.ok(next.sources[0].memo);
  await f.api(`/v3/documents/${id}`, { content: 'Changed complete source' }, 'PATCH');
  f.db.prepare("UPDATE memory_organization_batches SET expires_at='2000-01-01T00:00:00Z'").run();
  const reset = await f.prepare(); assert.equal(reset.sources[0].offset, 0); assert.equal(reset.sources[0].memo, '');
});

scenario('incomplete multipart and opt-outs remain visible and unmodified', async f => {
  const optedOut=await f.save('Opted out', { memoryIndex: { recallable: false } });
  const id = await f.save('Only part one', { captureVersion: 2, sessionId: 's', turn: 1, part: 1, parts: 2, captureKey: 's:1:hash:1:part-hash' });
  let batch=await f.prepare();
  if(!batch.group){assert.equal((await f.api('/v4/organize/commit',{batchId:batch.batchId,summaries:[],coverage:batch.sources.map(source=>({id:source.id,revision:source.revision,status:'deferred',summaries:[]}))})).status,200);batch=await f.prepare()}
  assert.ok(batch.group);assert.equal(batch.sources.length,0);
  assert.equal((await f.api('/v4/organize/commit', f.payload(batch))).status, 400);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'GET')).status, 200);
  assert.equal(JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(optedOut).metadata_json).memoryIndex.recallable,false);
});

scenario('vector bookkeeping does not invalidate coverage, direct SQL content changes do', async f => {
  const id = await f.save(); assert.equal((await f.api('/v4/organize/commit', f.payload(await f.prepare()))).status, 200);
  f.db.prepare("UPDATE memories SET vector_status='pending',vector_attempted_at='later' WHERE id=?").run(id);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 1);
  f.db.prepare("UPDATE memories SET content='Direct SQL change' WHERE id=?").run(id);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
});

scenario('summary replacement preserves prior exact coverage and rejects stale writers', async f => {
  const firstId = await f.save('First evidence');
  const first = await f.api('/v4/organize/commit', f.payload(await f.prepare())); assert.equal(first.status, 200);
  const secondId = await f.save('Second evidence'); let next;
  // 生成summaryだけのID windowでは、対象sourceがあっても空の結果を返せる。
  // この3レコードでは空windowの次にcursorが折り返すため、prepareは最大2回で足りる。
  for (let attempt = 0; attempt < 2; attempt++) {
    next = await f.prepare();
    if (next.sources.some(source => source.id === secondId)) break;
    assert.equal(next.sources.length, 0, 'Unexpected sources before the replacement evidence');
    assert.equal(next.group, undefined, 'Unexpected capture progress in the replacement fixture');
  }
  assert.deepEqual(next.sources.map(source => source.id), [secondId], 'Replacement evidence must be prepared within two bounded windows');
  const update = f.payload(next);
  Object.assign(update.summaries[0], { id: first.body.summaries[0].id, revision: first.body.summaries[0].revision,
    content: 'Final conclusion retaining both first and second evidence and all reasons.' });
  const replaced = await f.api('/v4/organize/commit', update); assert.equal(replaced.status, 200, JSON.stringify(replaced.body));
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 2);
  assert.equal((await f.api(`/v3/documents/${first.body.summaries[0].memoryId}`, undefined, 'GET')).status, 409);
  assert.equal((await f.api('/v3/documents', { containerTag: 'memories', customId: `organization:${first.body.summaries[0].id}:${first.body.summaries[0].revision}`, content: 'restore obsolete generated summary' })).status, 409);
  await f.api(`/v3/documents/${firstId}`, { content: 'Mutated first evidence' }, 'PATCH');
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  assert.equal((await f.api(`/v3/documents/${replaced.body.summaries[0].memoryId}`, undefined, 'GET')).status, 409);
});

scenario('concurrent publication is retry-safe and incomplete batches never hide sources', async f => {
  await f.save(); const batch = await f.prepare(); const body = f.payload(batch);
  assert.equal((await f.prepare()).sources.length, 0);
  const results = await Promise.all([f.api('/v4/organize/commit', body), f.api('/v4/organize/commit', body)]);
  assert.deepEqual(results[0], results[1]); assert.equal(results[0].status, 200);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_summaries').get().n, 1);
});

for (const recursive of [false, true]) scenario(`checkpoint provenance invalidation with recursive triggers ${recursive}`, async f => {
  f.db.exec(`PRAGMA recursive_triggers=${recursive ? 'ON' : 'OFF'}`);
  const ids = await f.seed(2); const old = (await f.consolidate()).body; await f.drain();
  assert.equal(old.status, 'consolidated');
  const batch = await f.prepare(); const checkpoint = batch.sources.find(s => s.id === old.memoryId); assert.ok(checkpoint);
  const body = f.payload(batch); assert.equal((await f.api('/v4/organize/commit', body)).status, 200);
  await f.drain();
  // This fixture represents confirmed vector visibility; dedicated semantic
  // scenarios below exercise the actual reconciliation path.
  f.db.prepare("UPDATE memories SET vector_status='indexed' WHERE embedding_status='done' AND embedding_json IS NOT NULL").run();
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 3);
  await f.api(`/v3/documents/${ids[0]}`, undefined, 'DELETE');
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  assert.equal(f.db.prepare("SELECT COUNT(*) n FROM memory_organization_summaries WHERE status='active'").get().n, 0);
  assert.equal((await f.api(`/v3/documents/${ids[1]}`, undefined, 'GET')).status, 200);
});

scenario('incomplete or mixed capture identities cannot be hidden', async f => {
  await f.save('New revision part one', { sessionId: 's', turn: 1, captureVersion: 2, part: 1, parts: 2, captureKey: 's:1:new-body:1:a' });
  await f.save('Old revision part two', { sessionId: 's', turn: 1, captureVersion: 2, part: 2, parts: 2, captureKey: 's:1:old-body:2:b' });
  const batch = await f.prepare(); assert.ok(batch.group);assert.equal(batch.sources.length,0);
  assert.equal((await f.api('/v4/organize/commit', f.payload(batch))).status, 400);
});

scenario('large checkpoint membership stays normalized and can be deferred independently', async f => {
  await f.save('Create the default container');
  f.db.exec(`WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<12005)
    INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,status,is_forgotten,topic_revision,created_at,updated_at)
    SELECT 'organization-bulk-'||x,'organization-bulk-'||x,'memories','Evidence '||x,'{}','done',0,'bulk-revision','2020-01-01','2020-01-01' FROM n;
    INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,status,is_forgotten,topic_revision,created_at,updated_at)
      VALUES('000-organization-checkpoint','000-organization-checkpoint','memories','Historical checkpoint with exact membership','{"sm_consolidation":true}','done',0,'checkpoint-revision','1900-01-01','1900-01-01');
    INSERT INTO memory_consolidation_projects(project_key,source_container_tag,revision,created_at,updated_at)
      VALUES('organization-bulk-project','memories',1,'2020-01-01','2020-01-01');
    INSERT INTO memory_consolidations(id,project_key,memory_id,revision,status,created_at,updated_at)
      VALUES('organization-bulk-checkpoint','organization-bulk-project','000-organization-checkpoint',1,'active','2020-01-01','2020-01-01');
    INSERT INTO memory_consolidation_sources(consolidation_id,memory_id,source_revision)
      SELECT 'organization-bulk-checkpoint',id,topic_revision FROM memories WHERE id LIKE 'organization-bulk-%';`);
  const batch = await f.prepare(); assert.equal(batch.sources[0].id, '000-organization-checkpoint');
  const snapshot = f.db.prepare('SELECT snapshot_json FROM memory_organization_batches WHERE id=?').get(batch.batchId).snapshot_json;
  assert.ok(snapshot.length < 12000); assert.ok(JSON.stringify(batch).length < 48000);
  f.DB.hooks.execute = (sql, values) => {
    assert.ok(values.length <= 100);
    if (sql.startsWith('UPDATE memory_organization_batches SET request_json=') && values.some(value => typeof value === 'string' && value.includes('"checkpoint":'))) {
      throw Error('injected-checkpoint-query-limit');
    }
  };
  assert.equal((await f.api('/v4/organize/commit', f.payload(batch))).status, 500);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
  const deferred = f.payload(batch);
  deferred.summaries[0].sources = deferred.summaries[0].sources.filter(id => id !== '000-organization-checkpoint');
  Object.assign(deferred.coverage[0], { status: 'deferred', summaries: [] });
  assert.equal((await f.api('/v4/organize/commit', deferred)).status, 200);
  f.DB.hooks.execute = undefined;
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get('000-organization-checkpoint').n, 0);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_dependencies').get().n, 19);
  // Make the deferred checkpoint eligible again without changing user memory.
  f.db.prepare("UPDATE memory_organization_progress SET next_attempt='2000-01-01' WHERE memory_id='000-organization-checkpoint'").run();
  const next = await f.prepare(); const body = f.payload(next);
  assert.ok(next.sources.some(s => s.id === '000-organization-checkpoint'));
  const first = await f.api('/v4/organize/commit', body); assert.equal(first.status, 200, JSON.stringify(first.body));
  const summary = first.body.summaries[0];
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_checkpoints WHERE summary_id=?').get(summary.id).n, 1);
  assert.equal(f.db.prepare('SELECT COUNT(DISTINCT memory_id) n FROM memory_organization_dependencies_all WHERE summary_id=?').get(summary.id).n, 12006);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_dependencies WHERE summary_id=?').get(summary.id).n, next.sources.length);
  assert.deepEqual(f.db.prepare('SELECT memory_id FROM memory_organization_dependencies WHERE summary_id=? ORDER BY memory_id').all(summary.id).map(row=>row.memory_id),next.sources.map(source=>source.id).sort());
  const more = await f.prepare(); const replacement = f.payload(more);
  Object.assign(replacement.summaries[0], { id: summary.id, revision: summary.revision });
  const operations = []; f.DB.hooks.execute = sql => operations.push(sql);
  const updated = await f.api('/v4/organize/commit', replacement); assert.equal(updated.status, 200, JSON.stringify(updated.body));
  f.DB.hooks.execute = undefined;
  assert.ok(!operations.some(sql => sql.includes('DELETE FROM memory_organization_dependencies')));
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_checkpoints WHERE summary_id=?').get(summary.id).n, 1);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_dependencies WHERE summary_id=?').get(summary.id).n, next.sources.length+more.sources.length);
  assert.deepEqual(f.db.prepare('SELECT memory_id FROM memory_organization_dependencies WHERE summary_id=? ORDER BY memory_id').all(summary.id).map(row=>row.memory_id),[...next.sources,...more.sources].map(source=>source.id).sort());
  assert.equal(f.db.prepare('SELECT COUNT(DISTINCT memory_id) n FROM memory_organization_dependencies_all WHERE summary_id=?').get(summary.id).n, 12006);
  f.db.prepare("UPDATE memories SET content='Changed original outside the bounded direct membership' WHERE id='organization-bulk-12005'").run();
  assert.equal(f.db.prepare('SELECT status FROM memory_organization_summaries WHERE id=?').get(summary.id).status, 'invalid');
});

scenario('physical space, scope, project ID and historical display names stay partitioned', async f => {
  const partitions = [
    { containerTag: 'legacy-space', metadata: { sm_scope: 'private', sm_project_id: 'repo', project: '旧プロジェクト' } },
    { containerTag: 'legacy-space', metadata: { sm_scope: 'project', sm_project_id: 'repo', project: '旧プロジェクト' } },
    { containerTag: 'legacy-space', metadata: { sm_project_id: 'repo', project: '旧プロジェクト' } },
    { containerTag: 'other-space', metadata: { sm_scope: 'private', sm_project_id: 'repo', project: '旧プロジェクト' } },
    { containerTag: 'legacy-space', metadata: { sm_scope: 'private', sm_project_id: 'repo', project: '新プロジェクト' } },
  ];
  const originals = new Map();
  for (const partition of partitions) {
    const saved = await f.api('/v3/documents', { ...partition, content: 'PartitionNeedle 結論と理由 😀', metadata: { title: 'PartitionNeedle', ...partition.metadata } });
    assert.equal(saved.status, 201); originals.set(saved.body.id, partition);
  }
  const batch = await f.prepare(); assert.equal(new Set(batch.sources.map(source => source.partition)).size, 5);
  const mixed = f.payload(batch);
  mixed.summaries = [{ ...mixed.summaries[0], sources: batch.sources.map(source => source.id) }];
  mixed.coverage.forEach(source => { source.summaries = [mixed.summaries[0].ref]; });
  assert.equal((await f.api('/v4/organize/commit', mixed)).status, 409);
  const body = f.payload(batch); body.summaries.forEach(summary => { summary.content = '# PartitionNeedle 結論\n## 理由: 証拠を保存 😀'; });
  const committed = await f.api('/v4/organize/commit', body); assert.equal(committed.status, 200, JSON.stringify(committed.body));
  for (const summary of committed.body.summaries) {
    const sourceId = body.summaries.find(candidate => candidate.ref === summary.ref).sources[0];
    const original = originals.get(sourceId); const stored = f.db.prepare('SELECT container_tag,metadata_json FROM memories WHERE id=?').get(summary.memoryId);
    assert.equal(stored.container_tag, original.containerTag);
    const metadata = JSON.parse(stored.metadata_json);
    for (const key of ['sm_scope', 'sm_project_id', 'project']) assert.deepEqual(metadata[key], original.metadata[key]);
    assert.equal(metadata.memoryIndex.recallable, true); assert.ok(metadata.memoryIndex.description.includes('PartitionNeedle'));
    for (const version of ['v3', 'v4']) {
      const response = await f.api(`/${version}/search`, { q: 'PartitionNeedle', containerTag: original.containerTag,
        ...(original.metadata.sm_scope ? { filters: { AND: [{ key: 'sm_scope', filterType: 'metadata', value: original.metadata.sm_scope }] } } : {}) });
      assert.equal(response.status, 200); assert.ok(response.body.results.some(result => result.id === summary.memoryId));
      assert.ok(!response.body.results.some(result => result.id === sourceId));
    }
  }
  const foreign = await f.api('/v4/search', { q: 'PartitionNeedle', containerTag: 'legacy-space', filters: { AND: [{ key: 'sm_scope', filterType: 'metadata', value: 'foreign' }] } });
  assert.deepEqual(foreign.body.results, []);
});

scenario('prepare projects large escaped Unicode metadata without changing originals', async f => {
  const padding = '資料日本語😀'.repeat(7500) + '\u0001'.repeat(500);
  for (let n = 0; n < 20; n++) await f.save(`# ByteNeedle ${n}\n結論と理由 😀`, { title: 'ByteNeedle', padding, project: '旧名', sm_scope: 'private' });
  const batch = await f.prepare(); assert.equal(batch.sources.length, 20);
  assert.ok(new TextEncoder().encode(JSON.stringify(batch)).byteLength < 1024 * 1024);
  assert.ok(batch.sources.every(source => source.metadata.padding === undefined));
  const response = await f.api('/v4/organize/commit', f.payload(batch)); assert.equal(response.status, 200, JSON.stringify(response.body));
  for (const source of batch.sources) assert.equal(JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(source.id).metadata_json).padding, padding);
});

for (const content of ['# 結論: 検証済み方式を採用 😀\n## 理由: 出典を保持', '結論: 検証済み方式を採用\n- 理由: 根拠を保持 😀']) scenario('organization index retains meaningful authored headings and bullets: ' + content.slice(0, 5), async f => {
  await f.save('# Verified source\nThe selected approach retains provenance.', { title: 'Verified source' });
  const batch = await f.prepare(); const body = f.payload(batch); body.summaries[0].content = content;
  const result = await f.api('/v4/organize/commit', body); assert.equal(result.status, 200);
  const metadata = JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(result.body.summaries[0].memoryId).metadata_json);
  assert.equal(metadata.memoryIndex.recallable, true); assert.ok(metadata.memoryIndex.description); assert.ok(metadata.memoryIndex.sections.length);
});
for (const content of ['###\n---\n***', '   ', '```\n___\n```']) scenario('empty display content rejects publication: ' + JSON.stringify(content), async f => {
  await f.save(); const body = f.payload(await f.prepare()); body.summaries[0].content = content;
  assert.equal((await f.api('/v4/organize/commit', body)).status, 400);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n, 0);
});

scenario('partial completion and deferred/new progress survive continuing arrivals across queue turns', async f => {
  const partial = await f.save('P'.repeat(32000));
  let batch = await f.prepare(); let source = batch.sources[0];
  assert.equal((await f.api('/v4/organize/commit', { batchId: batch.batchId, summaries: [], coverage: [{id: partial, revision: source.revision, status: 'partial', summaries: [], memo: 'Conclusions and reasons from first chunk; remaining topics pending.'}] })).status, 200);
  const deferred = await f.save('A difficult ambiguous record');
  batch = await f.prepare();
  assert.equal((await f.api('/v4/organize/commit', {batchId: batch.batchId, summaries: [], coverage: batch.sources.map(item => item.id === partial
    ? {id: item.id, revision: item.revision, status: 'partial', summaries: [], memo: 'Conclusions and reasons from first two chunks.'}
    : {id: item.id, revision: item.revision, status: 'deferred', summaries: []})})).status, 200);
  let partialProgress=0,deferredProgress=0,newProgress=0;
  for (let iteration=0; iteration<12; iteration++) {
    for (let n=0; n<4; n++) await f.save(`New independent evidence ${iteration}-${n}`);
    f.db.prepare("UPDATE memory_organization_progress SET next_attempt='2000-01-01' WHERE memory_id=?").run(deferred);
    batch = await f.prepare(); source = batch.sources.find(item => item.id === partial);
    if(source)partialProgress++;
    if(batch.sources.some(item=>item.id===deferred))deferredProgress++;
    if(batch.sources.some(item=>item.id!==partial&&item.id!==deferred))newProgress++;
    const body = f.payload(batch); const incomplete = new Set([deferred, ...(source && source.end < source.total ? [partial] : [])]);
    body.summaries.forEach(summary => { summary.sources = summary.sources.filter(id => !incomplete.has(id)); });
    body.summaries = body.summaries.filter(summary => summary.sources.length);
    body.coverage.forEach(item => { if(item.id === deferred) Object.assign(item,{status:'deferred',summaries:[]});
      else if(item.id === partial && source && source.end < source.total) Object.assign(item,{status:'partial',summaries:[],memo:'Conclusions and reasons carried forward; final chunk pending.'}); });
    assert.equal((await f.api('/v4/organize/commit', body)).status, 200);
  }
  assert.ok(partialProgress>=2);assert.ok(deferredProgress>=2);assert.ok(newProgress>=2);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get(partial).n, 1);
});

scenario('AI-off organization preserves over 200 original vectors for immediate semantic reactivation',async f=>{
  const vector=semanticFixture(f);const ids=[];
  for(let n=0;n<201;n++){
    const id=await f.save(`# Retained evidence ${n}\nEstablished decision and reasons`,{title:`Retained evidence ${n}`});
    ids.push(id);vector.seedEmbedding(id,n===200?0:1);
  }
  for(let n=0;n<40;n++){
    const batch=await f.prepare();if(!batch.sources.length)break;
    assert.equal((await f.api('/v4/organize/commit',f.payload(batch))).status,200);
  }
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_complete').get().n,201);
  for(let n=0;n<3;n++)await tick(f);
  assert.equal(vector.vectors.size,201);assert.equal(vector.ai.length,0);
  f.env.AI_ENRICHMENT_MODE='on';vector.control.hits=[ids[200]];
  const result=await f.api('/v4/search',{q:'SemanticallyDistinctQuery',allContainers:true});
  assert.equal(result.status,200);assert.ok(result.body.results.some(item=>item.id===ids[200]),'Retained original vector must support immediate AI-on semantic search');
  // Once replacements are semantically ready, remove all covered originals in bounded cron pages.
  for(let n=0;n<6;n++)await tick(f);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n,201);
  for(let n=0;n<3;n++)await tick(f);
  assert.ok(ids.every(id=>!vector.vectors.has(id)),'Every semantically covered original vector must eventually be removed');
  f.env.AI_ENRICHMENT_MODE='off';
  const summaries=f.db.prepare("SELECT memory_id FROM memory_organization_summaries WHERE status='active'").all();
  for(const row of summaries)assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:row.memory_id},'DELETE')).status,200);
  await f.drain();
  assert.equal((await f.api('/v3/documents/'+ids[200],undefined,'GET')).status,200);
  const lexical=await f.api('/v4/search',{q:'Retained evidence',allContainers:true});
  assert.ok(lexical.body.results.some(item=>ids.includes(item.id)),'Invalidation must restore lexical retrieval before vector repair');
  f.env.AI_ENRICHMENT_MODE='on';
  const aiBefore=vector.ai.length;let failures=0;
  vector.control.onUpsert=async()=>{failures++;throw Error('temporary reappearance upsert failure')};
  await tick(f);assert.ok(failures>0,'Reappearance reconciliation must attempt vector registration');
  vector.control.onUpsert=null;
  // Move the failed attempts past the existing retry delay without sleeping.
  f.db.prepare("UPDATE memories SET vector_attempted_at='2000-01-01' WHERE vector_status='failed' AND is_forgotten=0").run();
  for(let n=0;n<6;n++)await tick(f);
  assert.ok(ids.every(id=>vector.vectors.has(id)),'Bounded reconciliation retries must restore every original vector');assert.equal(vector.ai.length,aiBefore);
  f.env.AI_ENRICHMENT_MODE='on';vector.control.hits=[ids[200]];
  assert.ok((await f.api('/v4/search',{q:'SemanticallyDistinctQuery',allContainers:true})).body.results.some(item=>item.id===ids[200]),'Restored original vector must support semantic search without re-embedding');
});

scenario('organization preserves recent session exclusion, opt-outs, chronology and group pagination',async f=>{
  const metadata=(sessionId,date,turn=1)=>({type:'conversation',sessionId,turn,sourceTimestamp:date,
    memoryIndex:{version:1,title:sessionId,description:'Completed task and its reasons',recallable:true}});
  await f.save('# Old turn\nEvidence',metadata('other-session','2026-01-01T00:00:00Z',1));
  await f.save('# Latest turn\nReasons',metadata('other-session','2026-01-02T00:00:00Z',2));
  await f.save('# Separate task\nReasons',metadata('second-session','2026-01-03T00:00:00Z'));
  await f.save('# Current task\nReasons',metadata('current-session','2026-01-04T00:00:00Z'));
  await f.save('# Excluded task\nReasons',{...metadata('optout-session','2026-01-05T00:00:00Z'),memoryIndex:{recallable:false}});
  const forgotten=await f.save('# Forgotten task\nReasons',metadata('forgotten-session','2026-01-06T00:00:00Z'));
  assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:forgotten},'DELETE')).status,200);
  const request={projection:'index',recentTasks:true,excludeSessionId:'current-session',limit:1};
  const before=[];for(let page=1;page<=2;page++)before.push((await f.api('/v3/documents/list',{...request,page})).body);
  assert.equal(before[0].pagination.totalItems,2);assert.equal(before[0].pagination.hasMore,true);
  for(let attempt=0;attempt<8;attempt++){const batch=await f.prepare();if(!batch.sources.length)break;assert.equal((await f.api('/v4/organize/commit',f.payload(batch))).status,200);}
  const after=[];for(let page=1;page<=2;page++)after.push((await f.api('/v3/documents/list',{...request,page})).body);
  assert.deepEqual(after,before);
  const search=await f.api('/v4/search',{q:'',allContainers:true});
  assert.ok(!search.body.results.some(item=>before.some(page=>page.documents.some(document=>document.id===item.id))));
});

scenario('summary embeddings preserve semantic-only search and hidden-vector crowding fallback', async f => {
  const original=await f.save('# SemanticOriginal\nVerified source evidence',{title:'SemanticOriginal'});
  const other=await f.save('# UnrelatedIndexed\nAnother indexed fact',{title:'UnrelatedIndexed'});
  const vector=semanticFixture(f);vector.seedEmbedding(original);vector.seedEmbedding(other);vector.control.hits=[original];
  f.env.AI_ENRICHMENT_MODE='on';
  const batch=await f.prepare();const body=f.payload(batch);
  // Keep the unrelated record independent from the organized source.
  body.summaries[0].sources=[original];body.coverage.find(item=>item.id===other).status='deferred';body.coverage.find(item=>item.id===other).summaries=[];
  const result=await f.api('/v4/organize/commit',body);assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;
  await f.drain();assert.ok(vector.ai.length);assert.ok(vector.ai.every(input=>input.text));
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM facts WHERE source_memory_id=?').get(summary).n,0);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get(original).n,0);
  await tick(f);assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get(original).n,1);
  const found=await f.api('/v4/search',{q:'DifferentLexicalPhrase',allContainers:true});
  assert.ok(found.body.results.some(item=>item.id===summary));assert.ok(found.body.results.some(item=>item.id===other));assert.ok(!found.body.results.some(item=>item.id===original));
  vector.control.failDelete=true;await tick(f);assert.ok(vector.vectors.has(original));
  const duringFailure=await f.api('/v4/search',{q:'AnotherSemanticDialect',allContainers:true});assert.ok(duringFailure.body.results.some(item=>item.id===other));
  vector.control.failDelete=false;await tick(f);assert.ok(!vector.vectors.has(original));
  await f.api('/v4/memories',{containerTag:'memories',documentId:summary},'DELETE');await f.drain();
  assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(original).is_forgotten,0);
  f.db.prepare("UPDATE memories SET vector_attempted_at='2000-01-01',vector_status='pending' WHERE id=?").run(original);
  await tick(f);assert.ok(vector.vectors.has(original));
});

scenario('AI disabled or failed summary embedding keeps semantic originals visible until recovery', async f => {
  const original=await f.save('# RecoveryOriginal\nOriginal semantic evidence',{title:'RecoveryOriginal'});const vector=semanticFixture(f);vector.seedEmbedding(original);
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;
  const off=await f.api('/v4/search',{q:'',allContainers:true});assert.ok(!off.body.results.some(item=>item.id===original));assert.equal(vector.ai.length,0);
  assert.equal((await f.prepare()).sources.length,0);
  f.env.AI_ENRICHMENT_MODE='on';vector.control.failEmbedding=true;
  const on=await f.api('/v4/search',{q:'',allContainers:true});assert.ok(on.body.results.some(item=>item.id===original));assert.equal((await f.prepare()).sources.length,0);
  // Existing enrichment records failed stages without rejecting the cron.
  await tick(f);
  assert.equal(f.db.prepare('SELECT embedding_status FROM memories WHERE id=?').get(summary).embedding_status,'failed');
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n,0);
  assert.equal((await f.prepare()).sources.length,0);
  vector.control.failEmbedding=false;await tick(f);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n,1);
  assert.ok(vector.ai.every(input=>input.text));
});

scenario('concurrent cron, commit replay and manual retry share one live embedding claim', async f => {
  await f.save('# ClaimOriginal\nReasons',{title:'ClaimOriginal'});const vector=semanticFixture(f);f.env.AI_ENRICHMENT_MODE='on';
  let release,entered;const blocked=new Promise(resolve=>{release=resolve});f.cleanup(()=>release());const started=new Promise(resolve=>{entered=resolve});
  vector.control.onEmbedding=async()=>{entered();await blocked};
  const body=f.payload(await f.prepare());const result=await f.api('/v4/organize/commit',body);assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;await waitFor(started);
  const claim=f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token;
  await waitFor(Promise.all([tick(f),f.api('/v4/organize/commit',body),f.api('/v4/enrich',{id:summary})]));
  assert.equal(vector.ai.length,1);assert.equal(f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token,claim);
  release();await f.drain();assert.equal(f.db.prepare('SELECT embedding_status FROM memories WHERE id=?').get(summary).embedding_status,'done');
});

for(const failOld of [false,true])scenario(`expired embedding attempt cannot overwrite its successor (${failOld?'old failure':'old success'})`,async f=>{
  await f.save('# StaleClaim\nReasons',{title:'StaleClaim'});const vector=semanticFixture(f);f.env.AI_ENRICHMENT_MODE='on';
  let release,entered;const blocked=new Promise(resolve=>{release=resolve});f.cleanup(()=>release());const started=new Promise(resolve=>{entered=resolve});let count=0;
  vector.control.onEmbedding=async()=>{if(++count===1){entered();await blocked;if(failOld)throw Error('old attempt failure')}};
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;await waitFor(started);
  const old=f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token;
  f.db.prepare("UPDATE memory_organization_embedding_attempts SET expires_at='2000-01-01' WHERE memory_id=?").run(summary);
  f.db.prepare("UPDATE memories SET vector_attempted_at='2000-01-01' WHERE id=?").run(summary);
  await tick(f);const current=f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token;assert.notEqual(old,current);assert.equal(vector.ai.length,2);
  release();await f.drain();const row=f.db.prepare('SELECT embedding_status,vector_status,enrichment_error FROM memories WHERE id=?').get(summary);
  assert.equal(row.embedding_status,'done');assert.equal(row.vector_status,'indexed');assert.equal(row.enrichment_error,null);assert.equal(vector.vectors.get(summary).metadata.organization_attempt,current);
});

scenario('a stale upsert repairs the authoritative successor embedding and attempt token',async f=>{
  await f.save('# LateClaimVector\nReasons',{title:'LateClaimVector'});const vector=semanticFixture(f);f.env.AI_ENRICHMENT_MODE='on';
  let release,entered,next;const blocked=new Promise(resolve=>{release=resolve});f.cleanup(()=>release());const started=new Promise(resolve=>{entered=resolve});const successor=new Promise(resolve=>{next=resolve});let writes=0;
  vector.control.onUpsert=async()=>{if(++writes===1){entered();await blocked}else next()};
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;await waitFor(started);
  const old=f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token;
  f.db.prepare("UPDATE memory_organization_embedding_attempts SET expires_at='2000-01-01' WHERE memory_id=?").run(summary);
  assert.equal((await f.api('/v4/enrich',{id:summary})).status,202);await waitFor(successor);await tick(f);
  const current=f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id=?').get(summary).token;assert.notEqual(old,current);
  release();await f.drain();assert.equal(vector.vectors.get(summary).metadata.organization_attempt,current);assert.equal(f.db.prepare('SELECT embedding_status FROM memories WHERE id=?').get(summary).embedding_status,'done');
});

scenario('forgetting a summary fences its in-flight embedding result and vector write',async f=>{
  const original=await f.save('# ForgottenClaim\nReasons',{title:'ForgottenClaim'});const vector=semanticFixture(f);f.env.AI_ENRICHMENT_MODE='on';
  let release,entered;const blocked=new Promise(resolve=>{release=resolve});f.cleanup(()=>release());const started=new Promise(resolve=>{entered=resolve});vector.control.onEmbedding=async()=>{entered();await blocked};
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;await waitFor(started);
  assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:summary},'DELETE')).status,200);release();await f.drain();
  assert.ok(!vector.vectors.has(summary));assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(summary).is_forgotten,1);assert.equal((await f.api(`/v3/documents/${original}`,undefined,'GET')).status,200);
});

scenario('late current-revision source upsert after coverage is removed again', async f => {
  const original=await f.save('# LateOriginal\nEvidence and reasons',{title:'LateOriginal',topics:['Evidence']});const vector=semanticFixture(f);vector.seedEmbedding(original);
  let release;const blocked=new Promise(resolve=>{release=resolve});f.cleanup(()=>release());let entered;const started=new Promise(resolve=>{entered=resolve});
  vector.control.onEmbedding=async input=>{if(input.text[0].startsWith('# LateOriginal')){entered();await blocked}};
  f.env.AI_ENRICHMENT_MODE='on';assert.equal((await f.api('/v4/enrich',{id:original})).status,202);await waitFor(started);
  let summaryReady;const ready=new Promise(resolve=>{summaryReady=resolve});vector.control.onUpsert=async values=>{if(values.some(value=>value.id!==original))summaryReady()};
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);await waitFor(ready);await tick(f);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get(original).n,1);
  release();await f.drain();assert.ok(!vector.vectors.has(original));
  assert.equal(f.db.prepare('SELECT is_forgotten FROM memories WHERE id=?').get(original).is_forgotten,0);
});

scenario('deletion racing with reappearance repairs the current vector even with AI disabled', async f => {
  const original=await f.save('# RaceOriginal\nEvidence',{title:'RaceOriginal'});const vector=semanticFixture(f);vector.seedEmbedding(original);f.env.AI_ENRICHMENT_MODE='on';
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(result.status,200);const summary=result.body.summaries[0].memoryId;await f.drain();await tick(f);
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n,1);
  vector.control.onDelete=async ids=>{if(ids.includes(original)){vector.control.onDelete=null;await f.api('/v4/memories',{containerTag:'memories',documentId:summary},'DELETE')}};
  f.env.AI_ENRICHMENT_MODE='off';await tick(f);await f.drain();
  assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden').get().n,0);assert.ok(vector.vectors.has(original));
});

scenario('explicit exclusion, legacy manual-only and automatic summaries preserve retrieval eligibility', async f => {
  await f.save('ClassNeedle explicit excluded',{memoryIndex:{version:1,title:'ClassNeedle',description:'ClassNeedle excluded',recallable:false}});
  await f.save('ClassNeedle legacy text');
  await f.save('# ClassNeedle automatic\nEvidence and reasons',{title:'ClassNeedle automatic'});
  const vector=semanticFixture(f);f.env.AI_ENRICHMENT_MODE='on';const batch=await f.prepare();
  assert.deepEqual(new Set(batch.sources.map(source=>source.recallClass)),new Set(['excluded','manual','automatic']));
  const body=f.payload(batch);body.summaries.forEach(summary=>{summary.content='# ClassNeedle conclusion\n## Evidence and reasons 😀'});
  const mixed={...body,summaries:[{...body.summaries[0],sources:batch.sources.map(source=>source.id)}],coverage:body.coverage.map(source=>({...source,summaries:[body.summaries[0].ref]}))};
  assert.equal((await f.api('/v4/organize/commit',mixed)).status,409);
  const result=await f.api('/v4/organize/commit',body);assert.equal(result.status,200);await f.drain();
  const byClass=new Map(result.body.summaries.map(summary=>{const sourceId=body.summaries.find(item=>item.ref===summary.ref).sources[0];return [batch.sources.find(source=>source.id===sourceId).recallClass,summary.memoryId]}));
  const excluded=f.db.prepare('SELECT embedding_status,fact_status,metadata_json FROM memories WHERE id=?').get(byClass.get('excluded'));
  assert.equal(excluded.embedding_status,'disabled');assert.equal(excluded.fact_status,'disabled');assert.equal(JSON.parse(excluded.metadata_json).memoryIndex.recallable,false);
  const manual=JSON.parse(f.db.prepare('SELECT metadata_json FROM memories WHERE id=?').get(byClass.get('manual')).metadata_json);
  assert.equal(manual.sm_organization_manual_only,true);assert.equal(manual.memoryIndex.recallable,true);
  const automatic=await f.api('/v4/search',{q:'ClassNeedle',allContainers:true,indexOnly:true});
  assert.ok(automatic.body.results.some(item=>item.id===byClass.get('automatic')));assert.ok(!automatic.body.results.some(item=>item.id===byClass.get('manual')||item.id===byClass.get('excluded')));
  const manualSearch=await f.api('/v4/search',{q:'ClassNeedle',allContainers:true});
  for(const id of byClass.values())assert.ok(manualSearch.body.results.some(item=>item.id===id));
  const rpc=await worker.fetch(new Request('https://local.example/mcp',{method:'POST',headers:{Authorization:'Bearer local-fixture','Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'search_memory',arguments:{query:'ClassNeedle'}}})}),f.env,f.ctx);
  const remote=(await rpc.json()).result.structuredContent;
  assert.ok(remote.results.some(item=>item.id===byClass.get('manual')));assert.ok(remote.results.some(item=>item.id===byClass.get('automatic')));assert.ok(!remote.results.some(item=>item.id===byClass.get('excluded')));
  assert.ok(vector.ai.every(input=>input.text));
});

scenario('registered summary getDocument races return conflict, forged metadata remains ordinary', async f => {
  const source=await f.save();const first=await f.api('/v4/organize/commit',f.payload(await f.prepare()));assert.equal(first.status,200);const summary=first.body.summaries[0];
  // Keep this race fixture inside the next bounded keyset window.
  f.db.prepare(`INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,topic_revision,created_at,updated_at)
    VALUES('zz-get-document-race','zz-get-document-race','memories','Additional evidence','{}','get-document-race-revision',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))`).run();
  const next=await f.prepare();const update=f.payload(next);assert.ok(update.summaries.length);Object.assign(update.summaries[0],{id:summary.id,revision:summary.revision});
  f.DB.hooks.first=async(sql)=>{if(sql.includes('FROM memory_organization_summaries s JOIN memories m ON m.id=s.memory_id WHERE s.memory_id=?')){f.DB.hooks.first=undefined;assert.equal((await f.api('/v4/organize/commit',update)).status,200)}};
  assert.equal((await f.api(`/v3/documents/${summary.memoryId}`,undefined,'GET')).status,409);
  f.DB.hooks.first=undefined;
  const forged=await f.save('Ordinary user text',{sm_organization:true});
  const ordinary=await f.api(`/v3/documents/${forged}`,undefined,'GET');assert.equal(ordinary.status,200);assert.equal(ordinary.body.organizationSources,undefined);
  assert.equal((await f.api(`/v3/documents/${source}`,undefined,'GET')).status,200);
});

scenario('summary forgotten during getDocument returns conflict instead of stale content', async f => {
  await f.save();const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));const summary=result.body.summaries[0];
  f.DB.hooks.first=async sql=>{if(sql.includes('FROM memory_organization_summaries s JOIN memories m ON m.id=s.memory_id WHERE s.memory_id=?')){f.DB.hooks.first=undefined;assert.equal((await f.api('/v4/memories',{containerTag:'memories',documentId:summary.memoryId},'DELETE')).status,200)}};
  assert.equal((await f.api(`/v3/documents/${summary.memoryId}`,undefined,'GET')).status,409);f.DB.hooks.first=undefined;
});

scenario('source cursor pages omit the summary body and avoid reading it again', async f => {
  for(let n=0;n<8;n++)await f.save(`Original evidence ${n}`);
  const result=await f.api('/v4/organize/commit',f.payload(await f.prepare()));const summary=result.body.summaries[0].memoryId;
  const first=await f.api(`/v3/documents/${summary}`,undefined,'GET');assert.equal(first.status,200);assert.ok(first.body.content);assert.equal(first.body.organizationSources.sources.length,5);assert.equal(first.body.organizationSources.hasMore,true);
  const queries=[];f.DB.hooks.first=sql=>queries.push(sql);f.DB.hooks.all=sql=>queries.push(sql);
  const more=await f.api(`/v3/documents/${summary}?sourceCursor=${encodeURIComponent(first.body.organizationSources.nextCursor)}`,undefined,'GET');
  f.DB.hooks.first=undefined;f.DB.hooks.all=undefined;
  assert.equal(more.status,200);assert.deepEqual(Object.keys(more.body),['organizationSources']);assert.equal(more.body.organizationSources.sources.length,3);assert.equal(more.body.organizationSources.hasMore,false);
  assert.ok(queries.every(sql=>!(/\bcontent\b/u.test(sql))));
});

scenario('ordinary metadata markers do not impersonate generated organization state',async f=>{
  const original=await f.save('# MarkerNeedle\nOrdinary evidence',{sm_organization:true,sm_organization_manual_only:true,sm_project_id:'marker-project',memoryIndex:{version:1,title:'MarkerNeedle',description:'Ordinary evidence',recallable:true}});
  const found=await f.api('/v4/search',{q:'MarkerNeedle',allContainers:true,indexOnly:true});assert.ok(found.body.results.some(row=>row.id===original));
  assert.equal((await f.api(`/v3/documents/${original}`,{content:'# MarkerNeedle\nUpdated ordinary evidence'},'PATCH')).status,200);
  f.env.AI_ENRICHMENT_MODE='on';assert.equal((await f.api('/v4/enrich',{id:original})).status,202);await f.drain();
  assert.ok(f.db.prepare('SELECT COUNT(*) n FROM facts WHERE source_memory_id=?').get(original).n>0);
  const checkpoint=await f.api('/v4/consolidate',{projectId:'marker-project',force:true});assert.equal(checkpoint.status,200);assert.equal(checkpoint.body.status,'consolidated');await f.drain();
  assert.ok(f.db.prepare('SELECT COUNT(*) n FROM memory_consolidation_sources WHERE memory_id=?').get(original).n>0);
  f.env.AI_ENRICHMENT_MODE='off';const batch=await f.prepare();assert.ok(batch.sources.some(source=>source.id===original));
  const committed=await f.api('/v4/organize/commit',f.payload(batch));assert.equal(committed.status,200);
  const previews=[];for(const summary of committed.body.summaries){const document=await f.api(`/v3/documents/${summary.memoryId}`,undefined,'GET');assert.equal(document.status,200);previews.push(...document.body.organizationSources.sources)}
  assert.ok(previews.some(source=>source.id===original));
});
