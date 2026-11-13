// 直近タスク一覧の D1 負荷と過負荷時の応答を確認する。
// - 通常のページでは全件走査の文を1回だけ実行し、総数は同じ走査の窓関数から得る。
// - 範囲外のページだけ従来の件数文で総数を補い、総数・並びは従来の定義と一致させる。
// - D1 の過負荷エラーは再試行可能な 503 + Retry-After にし、それ以外は従来どおり 500 にする。
// ローカルの node:sqlite では D1 の過負荷そのものは再現できないため、公式の過負荷文言を投げる DB で代替する。
import test, {assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
import worker from '../src/index.ts';

const capture = (sessionId, date, extra = {}) => ({ type: 'conversation', sessionId, captureVersion: 2, turn: 1, part: 1, parts: 1,
  sm_project_id: 'load-project', sourceTimestamp: date, ...extra });

// 変更前と同じ定義で、除外セッション以外のタスク群の数を直接数える。
const legacyTotal = (db, exclude) => db.prepare(`SELECT COUNT(*) AS total FROM (SELECT 1 FROM memories AS m
  WHERE m.is_forgotten = 0 AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0
    AND json_extract(m.metadata_json, '$.sm_consolidation') IS NOT 1 AND json_extract(m.metadata_json, '$.type') = 'conversation'
    AND json_type(m.metadata_json, '$.sessionId') = 'text' AND length(trim(json_extract(m.metadata_json, '$.sessionId'))) > 0
    AND (? IS NULL OR json_extract(m.metadata_json, '$.sessionId') <> ?)
  GROUP BY COALESCE(CASE WHEN json_type(m.metadata_json, '$.sm_project_id') = 'text'
    THEN NULLIF(trim(json_extract(m.metadata_json, '$.sm_project_id')), '') END, m.container_tag), json_extract(m.metadata_json, '$.sessionId'))`)
  .get(exclude ?? null, exclude ?? null).total;

test('recent tasks use one grouped scan per page and keep totals for every page', async () => {
  const f = fixture();
  try {
    const save = async (tag, content, metadata) => { const r = await f.api('/v3/documents', { containerTag: tag, content, metadata }); assert.equal(r.status, 201); await f.drain(); return r.body.id; };
    for (let session = 0; session < 5; session++) {
      for (let part = 1; part <= 3; part++) {
        await save(session % 2 ? 'space-a' : 'space-b', `# 作業 ${session}-${part}\n完了`,
          capture(`load-${session}`, `2026-10-2${session}T10:00:00Z`, { turn: part, part, parts: 3 }));
      }
    }
    await save('space-a', '# 現在\n完了', capture('current', '2099-01-01T00:00:00Z'));
    const statements = [];
    f.DB.hooks.all = (sql) => { statements.push(sql); };
    f.DB.hooks.first = (sql) => { statements.push(sql); };
    const scans = () => statements.filter((sql) => sql.includes('task_rank') || /GROUP BY/.test(sql) && sql.includes('COUNT(*) AS total'));
    for (const [body, expectedDocuments] of [
      [{ page: 1, limit: 2 }, 2],
      [{ page: 3, limit: 2 }, 2],
      [{ page: 2, limit: 2, excludeSessionId: 'current' }, 2],
      [{ page: 50, limit: 2 }, 0],
    ]) {
      statements.length = 0;
      const response = await f.api('/v3/documents/list', { projection: 'index', recentTasks: true, ...body });
      assert.equal(response.status, 200, JSON.stringify(response.body));
      assert.equal(response.body.documents.length, expectedDocuments);
      assert.equal(response.body.pagination.totalItems, legacyTotal(f.db, body.excludeSessionId));
      // 行がある頁は走査1回、範囲外の頁だけ件数文を追加で1回実行する。
      assert.equal(scans().length, expectedDocuments ? 1 : 2, scans().join('\n---\n'));
      for (const document of response.body.documents) assert.equal(document.recent_total, undefined);
    }
    // 頁をまたいだ並びが重複・欠落なく、新しいタスク順であることを確認する。
    const seen = [];
    for (let page = 1; page <= 3; page++) {
      const response = await f.api('/v3/documents/list', { projection: 'index', recentTasks: true, page, limit: 2, excludeSessionId: 'current' });
      seen.push(...response.body.documents.map((document) => document.metadata.sessionId));
    }
    assert.deepEqual(seen, ['load-4', 'load-3', 'load-2', 'load-1', 'load-0']);
  } finally { f.DB.hooks.all = undefined; f.DB.hooks.first = undefined; await f.drain(); }
});

test('D1 overload errors become a retryable 503 while other failures stay redacted 500s', async () => {
  const f = fixture();
  try {
    const call = async () => {
      const response = await worker.fetch(new Request('https://local.example/v3/documents/list', { method: 'POST',
        headers: { Authorization: 'Bearer local-fixture', 'Content-Type': 'application/json' },
        body: JSON.stringify({ projection: 'index', recentTasks: true, page: 1, limit: 2 }) }), f.env, f.ctx);
      return { status: response.status, retryAfter: response.headers.get('Retry-After'), text: await response.text() };
    };
    const errors = console.error;
    console.error = () => {};
    try {
      for (const message of [
        'D1_ERROR: D1 DB is overloaded. Too many resources consumed within a 10 second window',
        'D1_ERROR: D1 DB is overloaded. Requests queued for too long.',
      ]) {
        f.DB.hooks.all = (sql) => { if (sql.includes('task_rank')) throw new Error(message); };
        const overloaded = await call();
        assert.equal(overloaded.status, 503);
        assert.equal(overloaded.retryAfter, '10');
        assert.ok(!overloaded.text.includes('task_rank') && !overloaded.text.includes('D1_ERROR'), overloaded.text);
      }
      // cause に包まれた過負荷も 503 にする。
      f.DB.hooks.all = (sql) => { if (sql.includes('task_rank')) throw new Error('wrapped', { cause: new Error('D1 DB is overloaded. Too many requests queued.') }); };
      assert.equal((await call()).status, 503);
      f.DB.hooks.all = (sql) => { if (sql.includes('task_rank')) throw new Error('D1_ERROR: no such column: secret_bind_value'); };
      const failed = await call();
      assert.equal(failed.status, 500);
      assert.equal(failed.retryAfter, null);
      assert.ok(!failed.text.includes('secret_bind_value'), failed.text);
    } finally { console.error = errors; }
  } finally { f.DB.hooks.all = undefined; await f.drain(); }
});
