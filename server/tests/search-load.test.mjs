// 索引・分類一致の検索文が、旧実装と同じ行・同じ順序を返しつつ D1 の読み取り行数を減らすことを確認する。
// - 旧実装は検索語ごとに JSON 抽出と memory_topics の索引探索を繰り返していた（本番で1文あたり百万行規模）。
// - 新実装は1行につき1回だけ照合対象（索引テキスト＋現行分類）を作り、各語は GLOB の並びで判定する。
// 比較の基準として、旧実装の SQL をこのファイルに固定し、同じ検索条件・束縛値で直接実行する。
// 読み取り行数は Durable Object SQLite の cursor.rowsRead（D1 の rows read と同じ指標）で数える。時間は測らない。
import test, {assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';
import {buildMemoryIndex} from '../src/shared/memory-index.ts';

const LONG_PROMPT = '検索APIでD1の負荷が高い原因を調査して、記憶整理コマンドと東京の設計メモ、Orion transport の検証手順、CAFÉ evidence と İstanbul の分類も確認して最適化して';

// 旧実装の索引照合（検索語ごとに評価する形）。比較専用で、本番コードには残っていない。
const legacyPredicate = (alias) => {
  const p = alias ? `${alias}.` : '';
  return `(replace(lower(COALESCE(json_extract(${p}metadata_json, '$.memoryIndex.title'), '') || ' ' ||
    COALESCE(json_extract(${p}metadata_json, '$.title'), '') || ' ' ||
    COALESCE(json_extract(${p}metadata_json, '$.memoryIndex.description'), '') || ' ' ||
    COALESCE(json_extract(${p}metadata_json, '$.memoryIndex.sections'), '')), 'İ', 'i̇') GLOB json_extract(term.value, '$.pattern')
    OR EXISTS (SELECT 1 FROM memory_topics AS topic WHERE topic.memory_id = ${p}id AND topic.source_revision = ${p}topic_revision
      AND replace(lower(topic.topic), 'İ', 'i̇') GLOB json_extract(term.value, '$.pattern')))`;
};
const legacyMatch = (alias) => `EXISTS (SELECT 1 FROM json_each(?) AS term WHERE ${legacyPredicate(alias)})`;
const v1 = (column) => `(json_type(${column}, '$.memoryIndex.version') = 'integer' AND json_extract(${column}, '$.memoryIndex.version') = 1)`;
const legacyPriority = (alias) => `CASE WHEN NOT ${v1(`${alias ? `${alias}.` : ''}metadata_json`)} OR ${legacyMatch(alias)} THEN 0 ELSE 1 END`;

// 新しい文から検索条件・束縛値を取り出し、旧実装の文と束縛値を組み立てる。
function legacyFor(sql, values) {
  const globs = (sql.match(/ GLOB \?/g) ?? []).length;
  if (sql.includes(') AS ranked')) {
    const clause = sql.split('FROM memories AS m WHERE ')[1].split(/\s+LIMIT -1 OFFSET 0/)[0];
    const weights = [...sql.matchAll(/GLOB \? THEN (\d+) ELSE/g)].map((match) => Number(match[1]));
    // 新しい文は検索語、検索条件、上限の順に束縛する。
    const parameters = values.slice(globs, values.length - 1);
    const patterns = JSON.stringify(values.slice(0, globs).map((pattern, index) => ({ weight: weights[index], pattern })));
    return { kind: 'indexed', sql: `SELECT m.id FROM memories AS m WHERE ${clause} AND ${legacyMatch('m')}
      ORDER BY (SELECT COALESCE(SUM(json_extract(term.value, '$.weight')), 0) FROM json_each(?) AS term WHERE ${legacyPredicate('m')}) DESC,
      m.updated_at DESC LIMIT ?`, values: [...parameters, patterns, patterns, values.at(-1)] };
  }
  const patterns = JSON.stringify(values.slice(values.length - 1 - globs, values.length - 1).map((pattern) => ({ weight: 1, pattern })));
  if (sql.includes(') AS fts')) {
    const clause = sql.split('WHERE memories_fts MATCH ? AND ')[1].split(/\s+LIMIT -1 OFFSET 0/)[0];
    return { kind: 'fts', sql: `SELECT m.id FROM memories_fts JOIN memories AS m ON m.rowid = memories_fts.rowid
      WHERE memories_fts MATCH ? AND ${clause} ORDER BY ${legacyPriority('m')}, bm25(memories_fts) LIMIT ?`,
      values: [...values.slice(0, values.length - 1 - globs), patterns, values.at(-1)] };
  }
  if (sql.includes(') AS lexical')) {
    const clause = sql.split(/FROM memories\s+WHERE /)[1].split(' AND instr(lower(content)')[0];
    return { kind: 'instr', sql: `SELECT id FROM memories WHERE ${clause} AND instr(lower(content), lower(?)) > 0
      ORDER BY ${legacyPriority('')}, updated_at DESC LIMIT ?`, values: [...values.slice(0, values.length - 1 - globs), patterns, values.at(-1)] };
  }
  return null;
}

test('index and topic matching keeps legacy rows and order while reading far fewer rows', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const titles = ['検索APIの負荷 調査', '記憶整理 コマンド', 'CAFÉ 東京 Evidence', 'İstanbul transport', 'Orion 設計', '負荷 試験 手順', 'plain note'];
    const ids = [];
    for (let i = 0; i < 90; i++) {
      const title = `${titles[i % titles.length]} ${i % 4}`;
      // 索引の版を混在させる: 版なし（NULL 判定）・文字列版・整数2（偽判定）の行も旧実装と同じ優先度になることを比べる。
      const memoryIndex = i % 5 === 4 ? { version: 'legacy' } : i % 11 === 10 ? { version: 2, title } : i % 7 === 6 ? undefined : buildMemoryIndex({ title, request: `${title} の説明`, sourceUpdatedAt: '2026-10-01T00:00:00Z' });
      const r = await f.api('/v3/documents', { containerTag: ['space-a', 'space-b', 'space-c'][i % 3], content: `# ${title}\n本文 ${i} 負荷と D1 の調査 evidence café 東京 alpha`,
        metadata: { title, ...(memoryIndex ? { memoryIndex } : {}), sm_scope: i % 2 ? 'public' : 'private' } });
      assert.equal(r.status, 201, JSON.stringify(r.body));
      ids.push(r.body.id);
    }
    await f.drain();
    // 同時刻の行を多く作り、同点の並び（走査順）まで旧実装と一致することを確かめる。
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
    f.DB.hooks.all = (sql, values) => { if (/\) AS (ranked|fts|lexical)/.test(sql)) captured.push({ sql, values }); };
    const kinds = new Set();
    const bodies = [];
    // 最後の長い依頼文は自動想起の典型で、検索語が多い（旧実装は語数に比例して読み取りが増える）。
    for (const q of ['検索APIの負荷を減らす方法は？（D1）', 'evidence café 東京 alpha', 'ecole validation', 'istanbul İstanbul', '最適化', 'これ それ', 'Orion transport plain',
      LONG_PROMPT])
      for (const scope of [{ allContainers: true }, { containerTag: 'space-b' }, { allContainers: true, filters: { AND: [{ key: 'sm_scope', filterType: 'metadata', value: 'public' }] } }])
        for (const indexOnly of [true, false]) bodies.push({ q, limit: 40, indexOnly, ...scope });
    let legacyRows = 0, currentRows = 0;
    const byKind = {};
    for (const body of bodies) {
      captured.length = 0;
      const response = await f.api('/v4/search', body);
      assert.equal(response.status, 200, JSON.stringify(response.body));
      for (const statement of captured) {
        const legacy = legacyFor(statement.sql, statement.values);
        assert.ok(legacy, statement.sql);
        kinds.add(legacy.kind);
        const current = f.db.storage.sql.exec(statement.sql, ...statement.values);
        const currentIds = current.toArray().map((row) => row.id);
        const old = f.db.storage.sql.exec(legacy.sql, ...legacy.values);
        const oldIds = old.toArray().map((row) => row.id);
        assert.deepEqual(currentIds, oldIds, `${legacy.kind} ${JSON.stringify(body)}`);
        legacyRows += old.rowsRead; currentRows += current.rowsRead;
        const tally = byKind[`${legacy.kind} ${body.q.slice(0, 12)}`] ??= { current: 0, legacy: 0 };
        tally.current += current.rowsRead; tally.legacy += old.rowsRead;
        // 文ごとにも読み取り行数が旧実装を超えないこと。
        assert.ok(current.rowsRead <= old.rowsRead, `${legacy.kind} ${JSON.stringify(body)} rows ${current.rowsRead} > ${old.rowsRead}`);
      }
    }
    assert.deepEqual([...kinds].sort(), ['fts', 'indexed', 'instr']);
    console.log(JSON.stringify({ event: 'search_rows_read', current: currentRows, legacy: legacyRows, byKind }));
    // 90行の小さなコーパスでも全体で3分の1未満、検索語の多い長い依頼文の索引一致では8分の1未満になる。
    // 旧実装は行数×検索語数に比例し、新実装は行数にだけ比例するため、本番規模（数千行・最大30語）では差がさらに広がる。
    assert.ok(currentRows * 3 < legacyRows, `rowsRead current=${currentRows} legacy=${legacyRows}`);
    const longPrompt = byKind[`indexed ${LONG_PROMPT.slice(0, 12)}`];
    assert.ok(longPrompt.current * 8 < longPrompt.legacy, JSON.stringify(longPrompt));
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
