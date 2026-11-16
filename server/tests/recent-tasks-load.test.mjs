// 直近タスク一覧の D1 負荷と過負荷時の応答を確認する。
// - 通常のページでは全件走査の文を1回だけ実行し、総数は同じ走査の窓関数から得る。
// - 範囲外のページだけ従来の件数文で総数を補い、総数・並びは従来の定義と一致させる。
// - D1 の過負荷エラーは再試行可能な 503 + Retry-After にし、それ以外は従来どおり 500 にする。
// ローカルの node:sqlite では D1 の過負荷そのものは再現できないため、公式の過負荷文言を投げる DB で代替する。
import test, {assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
import worker from '../src/index.ts';
import {beforeRecentStatement, insertBoundary} from './search-projection.test.mjs';

// 直近作業の文（射影テーブルを読む新しい文）を、変更前の本番 SQL（search-projection.test.mjs に固定）と同じ束縛値で実行し、
// 行の並び・総数が完全に一致し、読み取り行数が変更前以下で、対象行数＋上限の定数倍以内であること、
// 候補を部分索引だけで作る（memories を全走査しない）ことを確かめる。
function compareRecent(f, statements, label) {
  const results = [];
  for (const { sql, values } of statements) {
    const before = beforeRecentStatement(sql);
    if (!before) continue;
    const current = f.db.storage.sql.exec(sql, ...values);
    const currentRows = current.toArray();
    const old = f.db.storage.sql.exec(before.sql, ...values);
    const oldRows = old.toArray();
    assert.deepEqual(currentRows, oldRows, `${label} ${before.kind}`);
    assert.ok(current.rowsRead <= old.rowsRead, `${label} ${before.kind} rows ${current.rowsRead} > ${old.rowsRead}`);
    const plan = f.db.storage.sql.exec(`EXPLAIN QUERY PLAN ${sql}`, ...values).toArray().map((row) => row.detail);
    assert.ok(plan.some((detail) => detail.includes('USING COVERING INDEX idx_memory_search_projection_recent')), `${label} ${before.kind}: ${plan.join(' | ')}`);
    assert.ok(!plan.some((detail) => /\bSCAN (m|memories)\b/.test(detail)), `${label} ${before.kind}: ${plan.join(' | ')}`);
    results.push({ kind: before.kind, current: current.rowsRead, before: old.rowsRead, rows: currentRows.length });
  }
  return results;
}

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
    f.DB.hooks.all = (sql, values) => { statements.push({ sql, values }); };
    f.DB.hooks.first = (sql, values) => { statements.push({ sql, values }); };
    const scans = () => statements.map(({ sql }) => sql).filter((sql) => sql.includes('task_rank') || /GROUP BY/.test(sql) && sql.includes('COUNT(*) AS total'));
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
      // 変更前の文と並び・総数・読み取り行数を比べる。読み取りは対象行（16行）＋上限の定数倍以内。
      for (const result of compareRecent(f, statements, JSON.stringify(body))) console.log(JSON.stringify({ event: 'recent_rows_read', body, ...result }));
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

// 境界データ（空白だけ・数値の sessionId、空白の sm_project_id、real/text の turn・part、不正な日付・タイムゾーン付き日付・
// 非文字列の日付、チェックポイント、想起除外、忘却行）を含む状態で、全頁・除外あり/なし・範囲外・enrichmentEligible の
// どの組み合わせでも、変更前の文と並び・総数が完全に一致すること。
test('recent tasks over boundary data match the pre-projection statements on every page', async () => {
  const f = fixture();
  try {
    insertBoundary(f.db);
    for (let i = 0; i < 6; i++) {
      const r = await f.api('/v3/documents', { containerTag: i % 2 ? 'space-a' : 'space-b', content: `# 境界 ${i}\n完了`,
        metadata: capture(`edge-${i % 3}`, i % 2 ? `2026-10-0${i + 1}T10:00:00Z` : 'not a date', { turn: i, part: 1, sm_project_id: i % 3 ? 'load-project' : ' ' }) });
      assert.equal(r.status, 201, JSON.stringify(r.body));
    }
    await f.drain();
    const statements = [];
    f.DB.hooks.all = (sql, values) => { statements.push({ sql, values }); };
    f.DB.hooks.first = (sql, values) => { statements.push({ sql, values }); };
    let compared = 0;
    for (const extra of [{}, { excludeSessionId: 'edge-a' }, { excludeSessionId: 'edge-0' }, { enrichmentEligible: true }]) {
      for (const page of [1, 2, 3, 4, 40]) {
        statements.length = 0;
        const response = await f.api('/v3/documents/list', { projection: 'index', recentTasks: true, page, limit: 3, ...extra });
        assert.equal(response.status, 200, JSON.stringify(response.body));
        assert.equal(response.body.pagination.totalItems, legacyTotal(f.db, extra.excludeSessionId));
        compared += compareRecent(f, statements, JSON.stringify({ page, ...extra })).length;
      }
    }
    assert.ok(compared >= 20, `compared ${compared} statements`);
  } finally { f.DB.hooks.all = undefined; f.DB.hooks.first = undefined; await f.drain(); }
});
