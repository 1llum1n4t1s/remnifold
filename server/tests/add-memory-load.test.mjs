// customId 付き保存の D1 往復を確認する。
// - 生成要約・生成チェックポイントの事前確認は1文にまとめ、要約の拒否を優先する。
// - 保存行の ID と無効化したチェックポイントは書込み batch 末尾の SELECT で読み、batch 後に別の文を出さない。
// - 既存 customId の更新では、従来どおり最初に作られた行の ID を返す。
import test, {assert} from './support/test.mjs';
import {fixture, scenarioWithLogs as scenario} from './worker-fixture.mjs';

// batch 末尾の SELECT で読んだ無効化チェックポイントに対して、派生記憶の後片付けが1回だけ走ることを確認する。
// 末尾 SELECT が空を返すと、この後片付け（facts/topics 削除と未支持 facts の復元）が黙って抜けるため直接数える。
scenario('custom-id upsert cleans up the checkpoint it invalidated from the write batch', async (f) => {
  await f.seed();
  const consolidated = await f.consolidate();
  assert.equal(consolidated.status, 200, JSON.stringify(consolidated.body));
  const checkpointId = consolidated.body.memoryId;
  await f.drain();
  const cleanups = [];
  f.DB.hooks.batch = (operations) => {
    if (operations[0]?.sql.startsWith('DELETE FROM facts WHERE source_memory_id = ? AND EXISTS')) cleanups.push(operations[0].values[0]);
  };
  const saved = await f.api('/v3/documents', { containerTag: 'memories', customId: 'source-0', content: 'Replacement', metadata: { sm_project_id: 'log-project' } });
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  f.DB.hooks.batch = undefined;
  assert.deepEqual(cleanups, [checkpointId]);
  assert.equal(f.active().length, 0);
  assert.equal((await f.api(`/v3/documents/${checkpointId}`, undefined, 'GET')).status, 404);
});

test('custom-id saves read generated guards once and resolve the stored row inside the write batch', async () => {
  const f = fixture();
  try {
    const statements = [];
    let batches = 0;
    f.DB.hooks.first = (sql) => { statements.push(sql); };
    f.DB.hooks.all = (sql) => { statements.push(sql); };
    f.DB.hooks.run = (sql) => { statements.push(sql); };
    f.DB.hooks.batch = () => { batches++; };
    const save = (content) => f.api('/v3/documents', { containerTag: 'memories', customId: 'load-custom', content, metadata: { type: 'note' } });
    const first = await save('最初の内容');
    assert.equal(first.status, 201, JSON.stringify(first.body));
    // 応答までに出た文だけを数える（補強処理は応答後の waitUntil で動く）。
    assert.equal(statements.filter((sql) => sql.includes('AS summary') && sql.includes('AS checkpoint')).length, 1, statements.join('\n---\n'));
    assert.equal(statements.filter((sql) => /memory_organization_versions|memory_consolidations/.test(sql)).length, 1, statements.join('\n---\n'));
    assert.equal(statements.some((sql) => sql.trim() === 'SELECT id FROM memories WHERE container_tag = ? AND custom_id = ?'), false);
    assert.equal(batches, 1);
    await f.drain();
    statements.length = 0;
    batches = 0;
    const second = await save('更新した内容');
    assert.equal(second.status, 201, JSON.stringify(second.body));
    assert.equal(second.body.id, first.body.id);
    assert.equal(statements.filter((sql) => /memory_organization_versions|memory_consolidations/.test(sql)).length, 1, statements.join('\n---\n'));
    assert.equal(batches, 1);
    await f.drain();
    assert.equal(f.db.prepare("SELECT content FROM memories WHERE id = ?").get(first.body.id).content, '更新した内容');
  } finally {
    f.DB.hooks.first = undefined; f.DB.hooks.all = undefined; f.DB.hooks.run = undefined; f.DB.hooks.batch = undefined;
    await f.drain();
  }
});
