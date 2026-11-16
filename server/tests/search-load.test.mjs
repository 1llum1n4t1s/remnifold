// 索引・分類一致の検索文が、変更前と同じ行・同じ順序を返しつつ D1 の読み取り行数を増やさないことを確認する。
// - 変更前（migration 0014 より前）は、検索のたびに全行で照合対象（索引テキスト＋現行分類）を JSON 抽出と分類の
//   相関サブクエリから作っていた。さらに前の版は検索語ごとに同じ評価を繰り返していた（本番で1文あたり百万行規模）。
// - 現在は射影テーブル memory_search_projection に保存済みの照合対象を読み、各語は GLOB の並びで判定する。
// 比較の基準として、変更前の本番 SQL を search-projection.test.mjs に固定し、同じ検索条件・束縛値で直接実行する。
// 読み取り行数は Durable Object SQLite の cursor.rowsRead（D1 の rows read と同じ指標）で数える。時間は測らない。
import test, {assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
import {buildMemoryIndex} from '../src/shared/memory-index.ts';
import {beforeStatement} from './search-projection.test.mjs';

const LONG_PROMPT = '検索APIでD1の負荷が高い原因を調査して、記憶整理コマンドと東京の設計メモ、Orion transport の検証手順、CAFÉ evidence と İstanbul の分類も確認して最適化して';

// 新しい文（射影テーブルを読む文）を分類し、同じ検索条件・束縛値で実行できる変更前の本番 SQL を返す。
const legacyFor = (sql) => beforeStatement(sql);

test('index and topic matching keeps legacy rows and order while reading far fewer rows', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const titles = ['検索APIの負荷 調査', '記憶整理 コマンド', 'CAFÉ 東京 Evidence', 'İstanbul transport', 'Orion 設計', '負荷 試験 手順', 'plain note'];
    const ids = [];
    for (let i = 0; i < 90; i++) {
      const title = `${titles[i % titles.length]} ${i % 4}`;
      // 索引の版を混在させる: 版なし（NULL 判定）・文字列版・整数2（偽判定）の行も変更前と同じ優先度になることを比べる。
      const memoryIndex = i % 5 === 4 ? { version: 'legacy' } : i % 11 === 10 ? { version: 2, title } : i % 7 === 6 ? undefined : buildMemoryIndex({ title, request: `${title} の説明`, sourceUpdatedAt: '2026-10-01T00:00:00Z' });
      const r = await f.api('/v3/documents', { containerTag: ['space-a', 'space-b', 'space-c'][i % 3], content: `# ${title}\n本文 ${i} 負荷と D1 の調査 evidence café 東京 alpha`,
        metadata: { title, ...(memoryIndex ? { memoryIndex } : {}), sm_scope: i % 2 ? 'public' : 'private' } });
      assert.equal(r.status, 201, JSON.stringify(r.body));
      ids.push(r.body.id);
    }
    await f.drain();
    // 同時刻の行を多く作り、同点の並び（走査順）まで変更前と一致することを確かめる。
    f.db.prepare("UPDATE memories SET updated_at = CASE WHEN rowid % 3 = 0 THEN '2026-10-02T00:00:00.000Z' ELSE updated_at END, topic_revision = 'r1'").run();
    const topic = f.db.prepare('INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, ?, ?, ?, ?)');
    ids.forEach((id, i) => {
      // 現行改訂の分類だけが一致に数えられ、古い改訂の分類は無視されることも比較する。
      topic.run(id, i % 6 === 0 ? 'stale' : 'r1', ['Validation', 'ÉCOLE 分類', 'Transport', 'D1 最適化'][i % 4], `k-${i % 4}`, '2026-10-01T00:00:00Z');
      if (i % 3 === 0) topic.run(id, 'r1', 'Unicode İ topic', 'k-extra', '2026-10-01T00:00:00Z');
    });
    f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(ids[5]);
    f.env.AI_ENRICHMENT_MODE = 'on';
    f.env.MEMORY_VECTORS.query = async () => ({ matches: [], count: 0 });

    const captured = [];
    f.DB.hooks.all = (sql, values) => { if (legacyFor(sql)) captured.push({ sql, values }); };
    const kinds = new Set();
    const bodies = [];
    // 最後の長い依頼文は自動想起の典型で、検索語が多い（さらに前の版は語数に比例して読み取りが増えていた）。
    for (const q of ['検索APIの負荷を減らす方法は？（D1）', 'evidence café 東京 alpha', 'ecole validation', 'istanbul İstanbul', '最適化', 'これ それ', 'Orion transport plain',
      LONG_PROMPT])
      for (const scope of [{ allContainers: true }, { containerTag: 'space-b' }, { allContainers: true, filters: { AND: [{ key: 'sm_scope', filterType: 'metadata', value: 'public' }] } }])
        for (const indexOnly of [true, false]) bodies.push({ q, limit: 40, indexOnly, ...scope });
    let legacyRows = 0, currentRows = 0;
    const violations = [];
    const byKind = {};
    for (const body of bodies) {
      captured.length = 0;
      const response = await f.api('/v4/search', body);
      assert.equal(response.status, 200, JSON.stringify(response.body));
      for (const statement of captured) {
        const legacy = legacyFor(statement.sql);
        assert.ok(legacy, statement.sql);
        kinds.add(legacy.kind);
        const current = f.db.storage.sql.exec(statement.sql, ...statement.values);
        const currentIds = current.toArray().map((row) => row.id);
        const old = f.db.storage.sql.exec(legacy.sql, ...statement.values);
        const oldIds = old.toArray().map((row) => row.id);
        assert.deepEqual(currentIds, oldIds, `${legacy.kind} ${JSON.stringify(body)}`);
        legacyRows += old.rowsRead; currentRows += current.rowsRead;
        const tally = byKind[`${legacy.kind} ${body.q.slice(0, 12)}`] ??= { current: 0, legacy: 0 };
        tally.current += current.rowsRead; tally.legacy += old.rowsRead;
        // 文ごとにも読み取り行数が変更前を超えないこと。
        if (current.rowsRead > old.rowsRead) violations.push(`${legacy.kind} ${JSON.stringify(body)} rows ${current.rowsRead} > ${old.rowsRead}`);
      }
    }
    assert.deepEqual([...kinds].sort(), ['fts', 'indexed', 'instr']);
    console.log(JSON.stringify({ event: 'search_rows_read', current: currentRows, legacy: legacyRows, byKind, violations }));
    assert.deepEqual(violations, [], 'rows read must not exceed the pre-0014 statements');
    // 90行の小さなコーパスでも、全体と長い依頼文の索引一致の両方で変更前の3分の2未満になる（実測は約0.6）。
    // 変更前は全行で照合対象を作り直し分類を読んでいたが、現在は保存済みの照合対象を読み、上限内の行だけ memories を読み直す。
    assert.ok(currentRows * 3 < legacyRows * 2, `rowsRead current=${currentRows} legacy=${legacyRows}`);
    const longPrompt = byKind[`indexed ${LONG_PROMPT.slice(0, 12)}`];
    assert.ok(longPrompt.current * 3 < longPrompt.legacy * 2, JSON.stringify(longPrompt));
  } finally { f.DB.hooks.all = undefined; await f.drain(); }
});

// 有効なチェックポイントの分類は、絞り込みで読んだ索引行の分のだけ再利用し、索引を持たない行の分を追加で読む。
// 索引あり・なしのチェックポイントを混在させ、応答の分類が D1 上の現在の分類と一致すること、
// 同じ行の分類を二重に読まないことを確かめる。
test('active checkpoint topics are read once per row while mixing indexed and unindexed checkpoints', async () => {
  const f = fixture();
  try {
    const timestamp = '2026-10-01T00:00:00.000Z';
    const ids = [];
    for (let i = 0; i < 4; i++) {
      const title = `Nebula checkpoint ${i}`;
      const r = await f.api('/v3/documents', { containerTag: 'checkpoints', customId: `checkpoint-${i}`, content: `# ${title}\nNebula evidence ${i}`,
        metadata: { title, sm_consolidation: true, ...(i % 2 ? {} : { memoryIndex: buildMemoryIndex({ title, request: title, sourceUpdatedAt: timestamp }) }) } });
      assert.equal(r.status, 201, JSON.stringify(r.body));
      ids.push(r.body.id);
    }
    await f.drain();
    f.db.prepare("UPDATE memories SET topic_revision = 'r1'").run();
    const topic = f.db.prepare('INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, ?, ?, ?, ?)');
    ids.forEach((id, i) => {
      topic.run(id, 'r1', `Nebula topic ${i}`, `k-${i}`, timestamp);
      f.db.prepare(`INSERT INTO memory_consolidation_projects (project_key, source_container_tag, revision, created_at, updated_at) VALUES (?, 'checkpoints', 1, ?, ?)`)
        .run(`project-${i}`, timestamp, timestamp);
      f.db.prepare(`INSERT INTO memory_consolidations (id, project_key, memory_id, revision, status, created_at, updated_at) VALUES (?, ?, ?, 1, 'active', ?, ?)`)
        .run(`consolidation-${i}`, `project-${i}`, id, timestamp, timestamp);
    });
    const reads = [];
    f.DB.hooks.all = (sql, values) => { if (sql.includes('FROM memory_topics AS mt')) reads.push(...values); };
    const response = await f.api('/v4/search', { q: 'nebula', containerTag: 'checkpoints', indexOnly: true, limit: 10 });
    assert.equal(response.status, 200, JSON.stringify(response.body));
    const returned = new Set(response.body.results.map((row) => row.id));
    for (const id of ids) assert.ok(returned.has(id), `checkpoint ${id} missing`);
    for (const row of response.body.results) {
      const stored = f.db.prepare(`SELECT topic FROM memory_topics WHERE memory_id = ? ORDER BY topic_key`).all(row.id).map((entry) => entry.topic);
      assert.deepEqual(row.topics, stored, row.id);
    }
    // 再読込前の段階（絞り込み＋有効チェックポイント）で各行は1回だけ読む。再読込後の段階で各行をもう1回読む。
    for (const id of ids) assert.equal(reads.filter((value) => value === id).length, 2, `${id} topic reads`);
  } finally { f.DB.hooks.all = undefined; await f.drain(); }
});
