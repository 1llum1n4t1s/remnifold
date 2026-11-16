// 検索用の射影テーブル memory_search_projection（migration 0014）が、変更前の検索文と同じ値・同じ結果を保つことを確かめる。
// - (a) 値の一致: 境界データ（型の揺れ・不正な日付・古い改訂の分類など）を入れた状態で、生きている全行について
//   射影の各列と「変更前の本番コードの式」の値・型が一致し、射影の欠落と忘却行の残存が0件であること。
//   埋め戻しは 0014 を適用する前にデータを入れてから適用して確かめる。
// - (b) 保守: 書込み経路を1つ実行するたびに、射影テーブルとビュー memory_search_projection_source の差分が両方向で0行であること。
// - (c) 文面の固定: 派生式の唯一の定義であるビューの文面と、比較の基準に使う変更前の式の文面をハッシュで固定する。
// - 読み取り側: 変更前の本番 SQL（このファイルに固定）を同じ束縛値で直接実行し、id の並びと件数が完全に一致すること、
//   D1 の読み取り行数（Durable Object SQLite の rowsRead）が変更前以下で、分類数・検索語数に比例して増えないこと、
//   実行計画で memories を全走査しないこと。時間は測らない。
import test, { assert } from './support/test.mjs';
import { fixture, migrations } from './worker-fixture.mjs';
import { buildMemoryIndex } from '../src/shared/memory-index.ts';

// ---- 変更前の本番コードの式（比較の基準。本番コードからは削除済みで、ここだけに残す） ----
// 文面を変えると (c) のハッシュが変わる。基準を変える必要があるのは、ビューの式を変える新しいマイグレーションを
// 追加するとき（ビューを作り直して全行を再度埋め戻す）だけで、そのときは新旧の式を両方ここで比べてから更新する。
export const BEFORE_V1 = (column) => `(json_type(${column}, '$.memoryIndex.version') = 'integer' AND
           json_extract(${column}, '$.memoryIndex.version') = 1)`;
export const BEFORE_HAYSTACK = (alias) => `replace(lower(COALESCE(json_extract(${alias}.metadata_json, '$.memoryIndex.title'), '') || ' ' ||
                      COALESCE(json_extract(${alias}.metadata_json, '$.title'), '') || ' ' ||
                      COALESCE(json_extract(${alias}.metadata_json, '$.memoryIndex.description'), '') || ' ' ||
                      COALESCE(json_extract(${alias}.metadata_json, '$.memoryIndex.sections'), '') || char(10) ||
                      COALESCE((SELECT group_concat(topic.topic, char(10)) FROM memory_topics AS topic
                                WHERE topic.memory_id = ${alias}.id AND topic.source_revision = ${alias}.topic_revision), '')), 'İ', 'i̇')`;
const BEFORE_CONTENT = (alias) => {
  const prefix = alias ? `${alias}.` : '';
  return `CASE WHEN ${BEFORE_V1(`${prefix}metadata_json`)}
               THEN '' ELSE ${prefix}content END AS content`;
};
const BEFORE_SOURCE_DATE = (path) => {
  const value = `json_extract(m.metadata_json, '${path}')`;
  return `CASE WHEN json_type(m.metadata_json, '${path}') = 'text'
      AND ${value} GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*'
      AND strftime('%Y-%m-%d', substr(${value}, 1, 10), '+0 days') = substr(${value}, 1, 10)
      THEN julianday(${value}) END`;
};
export const BEFORE_RECENT_CONDITIONS = [
  "json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0",
  "json_extract(m.metadata_json, '$.sm_consolidation') IS NOT 1",
  "json_extract(m.metadata_json, '$.type') = 'conversation'",
  "json_type(m.metadata_json, '$.sessionId') = 'text'",
  "length(trim(json_extract(m.metadata_json, '$.sessionId'))) > 0",
];
export const BEFORE_TASK = {
  project: `COALESCE(CASE WHEN json_type(m.metadata_json, '$.sm_project_id') = 'text'
        THEN NULLIF(trim(json_extract(m.metadata_json, '$.sm_project_id')), '') END, m.container_tag)`,
  session: `json_extract(m.metadata_json, '$.sessionId')`,
  turn: `CASE WHEN json_type(m.metadata_json, '$.turn') = 'integer' THEN json_extract(m.metadata_json, '$.turn') ELSE 0 END`,
  part: `CASE WHEN json_type(m.metadata_json, '$.part') = 'integer' THEN json_extract(m.metadata_json, '$.part') ELSE 1 END`,
  sourceDate: `COALESCE(${['$.memoryIndex.sourceUpdatedAt', '$.sourceTimestamp', '$.sourceUpdatedAt', '$.completedAt'].map(BEFORE_SOURCE_DATE).join(', ')},
      julianday(m.created_at))`,
};

// 射影の各列に対応する、変更前の式（別名 m の memories から計算）。
const REFERENCE = {
  memory_id: 'm.id',
  container_tag: 'm.container_tag',
  created_at: 'm.created_at',
  updated_at: 'm.updated_at',
  topic_revision: 'm.topic_revision',
  recallable: "json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0",
  manual_only_marker: "json_extract(m.metadata_json, '$.sm_organization_manual_only') IS 1",
  sm_scope: "json_extract(m.metadata_json, '$.sm_scope')",
  index_v1: BEFORE_V1('m.metadata_json'),
  recent_eligible: `CASE WHEN ${BEFORE_RECENT_CONDITIONS.join(' AND ')} THEN 1 ELSE 0 END`,
  task_project: BEFORE_TASK.project,
  task_session: BEFORE_TASK.session,
  task_turn: BEFORE_TASK.turn,
  task_part: BEFORE_TASK.part,
  source_date: BEFORE_TASK.sourceDate,
  index_text: BEFORE_HAYSTACK('m'),
};
export const PROJECTION_COLUMNS = ['memory_rowid', ...Object.keys(REFERENCE)];

// ---- 変更前の検索文（server/src/index.ts の変更前の版を固定したもの） ----
// 新しい文の文面から検索条件の有無を読み取り、同じ条件の変更前の文を組み立てる。束縛値の順序は新旧で同じ。
export function beforeFlags(sql) {
  const indexOnly = sql.includes('p.recallable = 1');
  return { container: sql.includes('p.container_tag = ?') || sql.includes('m.container_tag = ?'), scope: sql.includes('p.sm_scope = ?'), indexOnly,
    manualContext: indexOnly && !sql.includes('p.manual_only_marker'), semantic: sql.includes('memory_organization_hidden') };
}
function beforeConditions({ container, scope, indexOnly, manualContext, semantic }, alias) {
  const prefix = alias ? `${alias}.` : '';
  const conditions = [`${prefix}is_forgotten = 0`, `NOT EXISTS(SELECT 1 FROM ${semantic ? 'memory_organization_hidden' : 'memory_organization_complete'} h WHERE h.memory_id=${alias || 'memories'}.id)`];
  if (container) conditions.push(`${prefix}container_tag = ?`);
  if (scope) conditions.push(`json_extract(${prefix}metadata_json, '$.sm_scope') = ?`);
  if (indexOnly) conditions.push(`json_extract(${prefix}metadata_json, '$.memoryIndex.recallable') IS NOT 0`);
  if (indexOnly && !manualContext) conditions.push(`NOT (json_extract(${prefix}metadata_json, '$.sm_organization_manual_only') IS 1 AND EXISTS(SELECT 1 FROM memory_organization_versions WHERE memory_id=${prefix}id))`);
  return conditions.join(' AND ');
}
const beforePriority = (v1, text, count) => `CASE WHEN NOT ${v1}${Array.from({ length: count }, () => ` OR ${text} GLOB ?`).join('')} THEN 0 ELSE 1 END`;
const beforePriorityColumns = (alias) => `${BEFORE_V1(`${alias}.metadata_json`)} AS index_v1,
                    CASE WHEN ${BEFORE_V1(`${alias}.metadata_json`)} IS NOT 0 THEN ${BEFORE_HAYSTACK(alias)} END AS index_text`;

// 新しい文を分類し、同じ束縛値で実行できる変更前の文を返す。対象外の文は null。
export function beforeStatement(sql) {
  const globs = (sql.match(/ GLOB \?/g) ?? []).length;
  if (sql.includes(') AS ranked') && sql.includes('memory_search_projection')) {
    const weights = [...sql.matchAll(/GLOB \? THEN (\d+) ELSE/g)].map((match) => Number(match[1]));
    const weight = weights.length ? weights.map((w) => `CASE WHEN candidate.index_text GLOB ? THEN ${w} ELSE 0 END`).join(' + ') : '0';
    return { kind: 'indexed', sql: `SELECT m.id, m.container_tag, ${BEFORE_CONTENT('m')}, m.metadata_json, m.created_at, m.updated_at, m.topic_revision
     FROM (
       SELECT candidate.row_order, candidate.updated_at, ${weight} AS weight
       FROM (
         SELECT m.rowid AS row_order, m.updated_at, ${BEFORE_HAYSTACK('m')} AS index_text
         FROM memories AS m WHERE ${beforeConditions(beforeFlags(sql), 'm')}
         LIMIT -1 OFFSET 0
       ) AS candidate
       LIMIT -1 OFFSET 0
     ) AS ranked
     CROSS JOIN memories AS m ON m.rowid = ranked.row_order
     WHERE ranked.weight > 0
     ORDER BY ranked.weight DESC, ranked.updated_at DESC, ranked.row_order LIMIT ?` };
  }
  if (sql.includes('WITH fts AS') && sql.includes('memory_search_projection')) {
    return { kind: 'fts', sql: `SELECT id, container_tag, content, metadata_json, created_at, updated_at, topic_revision, rank
           FROM (
             SELECT m.id, m.container_tag, ${BEFORE_CONTENT('m')}, m.metadata_json, m.created_at, m.updated_at,
                    m.topic_revision, bm25(memories_fts) AS rank,
                    ${beforePriorityColumns('m')}
             FROM memories_fts
             JOIN memories AS m ON m.rowid = memories_fts.rowid
             WHERE memories_fts MATCH ? AND ${beforeConditions(beforeFlags(sql), 'm')}
             LIMIT -1 OFFSET 0
           ) AS fts
           ORDER BY ${beforePriority('fts.index_v1', 'fts.index_text', globs)}, fts.rank LIMIT ?` };
  }
  if (sql.includes(') AS lexical') && sql.includes('memory_search_projection')) {
    return { kind: 'instr', sql: `SELECT id, container_tag, content, metadata_json, created_at, updated_at, topic_revision
           FROM (
             SELECT id, container_tag, ${BEFORE_CONTENT('')}, metadata_json, created_at, updated_at, topic_revision,
                    ${beforePriorityColumns('memories')}
             FROM memories
             WHERE ${beforeConditions(beforeFlags(sql), '')} AND instr(lower(content), lower(?)) > 0
             LIMIT -1 OFFSET 0
           ) AS lexical
           ORDER BY ${beforePriority('lexical.index_v1', 'lexical.index_text', globs)}, lexical.updated_at DESC LIMIT ?` };
  }
  return null;
}

// 直近作業の変更前の文。exclude は除外セッションの有無（束縛値は新旧で同じ順序: [除外セッション], 上限, 位置）。
const BEFORE_RECENT_COLUMNS = `m.id, m.container_tag, m.metadata_json, m.status, m.created_at, m.updated_at,
       substr(m.content, 1, 300) AS content_preview`;
export function beforeRecentStatement(sql) {
  const exclude = sql.includes('p.task_session <> ?');
  const where = ['m.is_forgotten = 0', ...BEFORE_RECENT_CONDITIONS, ...(exclude ? ["json_extract(m.metadata_json, '$.sessionId') <> ?"] : [])].join(' AND ');
  if (sql.includes('task_rank')) return { kind: 'recent', sql: `WITH candidates AS (
    SELECT m.id, m.created_at, m.updated_at,
      ${BEFORE_TASK.turn} AS task_turn,
      ${BEFORE_TASK.part} AS task_part,
      ${BEFORE_TASK.sourceDate} AS source_date,
      ${BEFORE_TASK.project} AS task_project,
      ${BEFORE_TASK.session} AS task_session
    FROM memories AS m WHERE ${where}
  ), ranked AS (
    SELECT candidates.*, ROW_NUMBER() OVER (
      PARTITION BY task_project, task_session
      ORDER BY source_date DESC,
        task_turn DESC, task_part ASC,
        created_at DESC, updated_at DESC, id DESC
    ) AS task_rank FROM candidates
  ), recent AS (
    SELECT id, source_date, COUNT(*) OVER () AS recent_total FROM ranked WHERE task_rank = 1
    ORDER BY source_date DESC, id DESC LIMIT ? OFFSET ?
  ) SELECT ${BEFORE_RECENT_COLUMNS}, recent.recent_total FROM memories AS m JOIN recent ON recent.id = m.id WHERE 1 = 1
       ORDER BY recent.source_date DESC, m.id DESC` };
  if (sql.includes('COUNT(*) AS total') && sql.includes('GROUP BY p.task_project')) return { kind: 'recent-count',
    sql: `SELECT COUNT(*) AS total FROM (SELECT 1 FROM memories AS m WHERE ${where} GROUP BY ${BEFORE_TASK.project}, ${BEFORE_TASK.session})` };
  return null;
}

// ---- 射影の検査 ----
const exec = (db, sql, ...values) => db.storage.sql.exec(sql, ...values);
export function projectionMismatches(db) {
  const checks = Object.entries(REFERENCE).map(([column, expression]) =>
    `CASE WHEN p.${column} IS NOT (${expression}) OR typeof(p.${column}) IS NOT typeof(${expression}) THEN '${column}' END`);
  const missingOrDifferent = exec(db, `SELECT m.id, CASE WHEN p.memory_rowid IS NULL THEN 'missing' ELSE concat_ws(',', ${checks.join(', ')}) END AS columns
    FROM memories AS m LEFT JOIN memory_search_projection AS p ON p.memory_rowid = m.rowid WHERE m.is_forgotten = 0`).toArray()
    .filter((row) => row.columns);
  const stale = exec(db, `SELECT p.memory_id AS id, 'stale' AS columns FROM memory_search_projection AS p
    LEFT JOIN memories AS m ON m.rowid = p.memory_rowid WHERE m.rowid IS NULL OR m.is_forgotten <> 0 OR m.id IS NOT p.memory_id`).toArray();
  return [...missingOrDifferent, ...stale];
}
export function projectionDiff(db) {
  const columns = PROJECTION_COLUMNS.join(', ');
  const count = (left, right) => exec(db, `SELECT COUNT(*) AS n FROM (SELECT ${columns} FROM ${left} EXCEPT SELECT ${columns} FROM ${right})`).one().n;
  return { tableOnly: count('memory_search_projection', 'memory_search_projection_source'), viewOnly: count('memory_search_projection_source', 'memory_search_projection') };
}
export function assertProjection(db, label) {
  assert.deepEqual(projectionDiff(db), { tableOnly: 0, viewOnly: 0 }, `${label}: projection table and view differ`);
  const mismatches = projectionMismatches(db);
  assert.deepEqual(mismatches, [], `${label}: projection differs from the pre-0014 expressions`);
  const live = exec(db, 'SELECT COUNT(*) AS n FROM memories WHERE is_forgotten = 0').one().n;
  assert.equal(exec(db, 'SELECT COUNT(*) AS n FROM memory_search_projection').one().n, live, `${label}: projection row count`);
}

// ---- 境界データ ----
const BOUNDARY_METADATA = [
  '{}',
  '{"title":42,"memoryIndex":{"version":1,"title":["A","b"],"description":null,"sections":{"Head":"Line1\\nLINE2"}}}',
  '{"title":"Line1\\nLINE2 ÉCOLE İstanbul","memoryIndex":{"version":"1","title":"Ünïcode TITLE","recallable":false}}',
  '{"memoryIndex":{"version":1.0,"title":"Real version","description":"CAFÉ İ","recallable":0}}',
  '{"memoryIndex":{"version":2,"title":"Version two","recallable":"0"},"sm_scope":"public"}',
  '{"memoryIndex":{"title":"No version","recallable":0.0},"sm_scope":7}',
  '{"memoryIndex":{"version":1,"recallable":true,"sections":[1,"Two",null]},"sm_scope":1.5}',
  '{"memoryIndex":{"version":1,"title":null,"description":"","sections":""},"sm_scope":true}',
  '{"memoryIndex":"not an object","sm_scope":{"nested":"scope"}}',
  '{"type":"conversation","sessionId":"   ","turn":1,"part":1}',
  '{"type":"conversation","sessionId":12,"turn":1,"part":1}',
  '{"type":"conversation","sessionId":"edge-a","sm_project_id":"  ","turn":1.5,"part":"2"}',
  '{"type":"conversation","sessionId":"edge-a","sm_project_id":" edge-project ","turn":"2","part":2.0}',
  '{"type":"conversation","sessionId":"edge-b","sm_project_id":5,"turn":3,"part":1,"memoryIndex":{"version":1,"sourceUpdatedAt":"2026-02-30T00:00:00Z"}}',
  '{"type":"conversation","sessionId":"edge-b","sourceTimestamp":"2026-10-05T10:00:00+09:00","turn":4,"part":1}',
  '{"type":"conversation","sessionId":"edge-c","sourceUpdatedAt":20261005,"completedAt":"2026-10-06T00:00:00Z"}',
  '{"type":"conversation","sessionId":"edge-c","sourceUpdatedAt":"not a date","completedAt":"garbage"}',
  '{"type":"conversation","sessionId":"edge-d","memoryIndex":{"sourceUpdatedAt":"2026-13-01T00:00:00Z"},"sourceTimestamp":"2026-10-07"}',
  '{"type":"conversation","sessionId":"edge-d","sm_consolidation":true}',
  '{"type":"conversation","sessionId":"edge-d","sm_consolidation":1}',
  '{"type":"conversation","sessionId":"edge-e","memoryIndex":{"recallable":false}}',
  '{"type":"note","sessionId":"edge-e"}',
  '{"type":"conversation","sessionId":"edge-f","sm_organization_manual_only":true}',
  '{"sm_organization_manual_only":1,"memoryIndex":{"version":1,"title":"Manual marker"}}',
];
// 作成日時のフォールバックに不正な値・タイムゾーン付きの値も混ぜる。
const BOUNDARY_CREATED = ['2026-10-01T00:00:00.000Z', 'not-a-date', '2026-10-02T09:00:00+09:00', '2026-10-03T00:00:00.000Z'];

export function insertBoundary(db, prefix = 'edge') {
  exec(db, "INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES ('edge', 'Edge', '2026-01-01', '2026-01-01'), ('edge-2', 'Edge 2', '2026-01-01', '2026-01-01')");
  const ids = [];
  BOUNDARY_METADATA.forEach((metadata, index) => {
    const id = `${prefix}-${String(index).padStart(3, '0')}`;
    const created = BOUNDARY_CREATED[index % BOUNDARY_CREATED.length];
    // topic_revision は NULL の行も作る（分類の改訂一致は NULL では成立しない）。
    exec(db, `INSERT INTO memories (id, custom_id, container_tag, content, metadata_json, created_at, updated_at, topic_revision, is_forgotten)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`, id, id, index % 2 ? 'edge' : 'edge-2', `Edge body ${index} 東京 evidence`, metadata,
      created, index % 3 ? created : '2026-11-01T00:00:00.000Z', index % 7 === 6 ? null : `rev-${index % 3}`, index % 11 === 10 ? 1 : 0);
    ids.push(id);
    // 現行改訂と古い改訂の分類を混在させる。大文字・İ・非ASCII大文字も入れる。
    exec(db, 'INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, ?, ?, ?, ?)',
      id, `rev-${index % 3}`, ['Validation', 'ÉCOLE 分類', 'İstanbul Topic', 'Line\nBreak'][index % 4], `k-${index % 4}`, '2026-10-01');
    exec(db, 'INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, ?, ?, ?, ?)',
      id, 'stale-revision', 'Stale TOPIC', 'k-stale', '2026-10-01');
  });
  return ids;
}

const sha256 = async (text) => [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text)))]
  .map((byte) => byte.toString(16).padStart(2, '0')).join('');

// (c) ビューの文面（派生式の唯一の定義）と、比較の基準に使う変更前の式の文面を固定する。
// これが変わったら、0014 を書き換えたか基準の式を変えたことになる。適用済みのデータベースには反映されないため、
// 式を変えるときは 0014 を戻し、「ビューを作り直して全行を再度埋め戻す」新しいマイグレーションを追加してから、
// 新しいビューの文面と基準の式で値の一致 (a) を確かめ、ここのハッシュを更新する。
const VIEW_SQL_SHA256 = '6123db8518d5070150a7913aa09df777461f553de14479a5e8e75eba0eb31793';
const REFERENCE_SQL_SHA256 = 'f9bf4c38cc10ca6698eba9a74ceb6250e79a50af95bc0457f5b87c18482691d9';

test('search projection: 0014 backfills boundary data with values equal to the pre-0014 expressions', async () => {
  const f = fixture({ beforeMigration: '0014_' });
  try {
    insertBoundary(f.db);
    const saved = await f.api('/v3/documents', { containerTag: 'edge', content: '# API\nbody', metadata: { title: 'API', memoryIndex: buildMemoryIndex({ title: 'API', request: 'API body', sourceUpdatedAt: '2026-10-01T00:00:00Z' }) } });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    await f.drain();
    for (const migration of migrations) if (migration.name >= '0014_') f.db.exec(migration.sql);
    assertProjection(f.db, 'backfill');
    // 忘却行は射影に入らない（境界データの index%11==10 の行）。
    assert.equal(exec(f.db, "SELECT COUNT(*) AS n FROM memory_search_projection WHERE memory_id = 'edge-010'").one().n, 0);
    // 長い照合対象を前方の列だけの走査で読まないよう、index_text は最後の列に置く。
    const columns = exec(f.db, "SELECT name FROM pragma_table_info('memory_search_projection') ORDER BY cid").toArray().map((row) => row.name);
    assert.deepEqual(columns, PROJECTION_COLUMNS.filter((name) => name !== 'index_text').concat('index_text'));
    // (c) 文面のハッシュ。
    const viewSql = exec(f.db, "SELECT sql FROM sqlite_master WHERE type = 'view' AND name = 'memory_search_projection_source'").one().sql;
    const referenceSql = JSON.stringify(REFERENCE);
    assert.deepEqual({ view: await sha256(viewSql), reference: await sha256(referenceSql) }, { view: VIEW_SQL_SHA256, reference: REFERENCE_SQL_SHA256 },
      'Do not edit 0014 or the reference expressions; add a migration that recreates the view and backfills again');
  } finally { await f.drain(); }
});

test('search projection: every write path keeps the table equal to the view', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    insertBoundary(f.db);
    assertProjection(f.db, 'boundary insert');
    f.env.AI_ENRICHMENT_MODE = 'on';
    const check = (label) => assertProjection(f.db, label);
    const save = async (body) => { const r = await f.api('/v3/documents', body); assert.equal(r.status, 201, JSON.stringify(r.body)); await f.drain(); return r.body.id; };
    const id = await save({ containerTag: 'flow', customId: 'flow-1', content: '# Flow\nbody', metadata: { title: 'Flow', type: 'conversation', sessionId: 's-flow', turn: 1, part: 1, sm_scope: 'public' } });
    check('save');
    await save({ containerTag: 'flow', customId: 'flow-1', content: '# Flow\nupdated body', metadata: { title: 'Flow 2', type: 'conversation', sessionId: 's-flow', turn: 2, part: 1, sm_scope: 'private' } });
    check('upsert');
    const patched = await f.api(`/v3/documents/${id}`, { content: '# Flow\npatched', metadata: { title: 'Patched', memoryIndex: buildMemoryIndex({ title: 'Patched', request: 'patched', sourceUpdatedAt: '2026-10-09T00:00:00Z' }) } }, 'PATCH');
    assert.equal(patched.status, 200, JSON.stringify(patched.body)); await f.drain();
    check('patch');
    const enriched = await f.api('/v4/enrich', { id }); assert.ok(enriched.status < 300, JSON.stringify(enriched.body)); await f.drain();
    assert.ok(exec(f.db, 'SELECT COUNT(*) AS n FROM memory_topics WHERE memory_id = ?', id).one().n > 0, 'enrichment stored topics');
    check('enrich');
    // 分類が空で返ると failTopics が現行改訂の分類を消す。
    const ai = f.env.AI.run;
    f.env.AI.run = async (model, input) => input.text ? ai(model, input) : { choices: [{ finish_reason: 'stop', message: { content: JSON.stringify({ facts: [], topics: [] }) } }] };
    await f.api('/v4/enrich', { id }); await f.drain();
    f.env.AI.run = ai;
    check('failTopics');
    await f.api('/v4/enrich', { id }); await f.drain();
    check('reclassify');
    const forgotten = await f.api('/v4/memories', { containerTag: 'flow', documentId: id }, 'DELETE'); assert.equal(forgotten.status, 200, JSON.stringify(forgotten.body)); await f.drain();
    assert.equal(exec(f.db, 'SELECT COUNT(*) AS n FROM memory_search_projection WHERE memory_id = ?', id).one().n, 0);
    check('forget');
    await save({ containerTag: 'flow', customId: 'flow-1', content: '# Flow\nrestored', metadata: { title: 'Restored', type: 'conversation', sessionId: 's-flow' } });
    check('restore');
    const removable = await save({ containerTag: 'flow', customId: 'flow-delete', content: 'to delete', metadata: { title: 'Delete me' } });
    assert.equal((await f.api(`/v3/documents/${removable}`, undefined, 'DELETE')).status, 204); await f.drain();
    assert.equal(exec(f.db, 'SELECT COUNT(*) AS n FROM memory_search_projection WHERE memory_id = ?', removable).one().n, 0);
    check('hard delete');
    // コンテナ削除の外部キー連鎖（記憶と分類の両方が連鎖で消える）。
    exec(f.db, "DELETE FROM container_tags WHERE tag = 'edge-2'");
    check('container cascade');
    // チェックポイントの公開と supersede。
    const sources = await f.seed(2); await f.drain();
    assert.equal((await f.consolidate()).status, 200); await f.drain();
    check('consolidate publish');
    await save({ containerTag: 'memories', customId: 'source-extra', content: 'Verified source extra', metadata: { sm_project_id: 'log-project' } });
    await f.api(`/v3/documents/${sources[0]}`, { content: 'Verified source 0 changed' }, 'PATCH'); await f.drain();
    assert.equal((await f.consolidate()).status, 200); await f.drain();
    check('consolidate supersede');
    // 整理の commit と、原本の編集で 0009 のトリガーが要約を入れ子で忘却・分類削除する経路。
    f.env.AI_ENRICHMENT_MODE = 'off';
    const original = await save({ containerTag: 'organize', content: 'Original conclusion and its reasons 😀', metadata: { type: 'conversation', sessionId: 'organize-session' } });
    const batch = (await f.api('/v4/organize/prepare', { sourceIds: [original] })).body;
    assert.ok(batch.sources?.some((source) => source.id === original), JSON.stringify(batch));
    const groups = [...new Set(batch.sources.map((source) => source.partition))];
    const committed = await f.api('/v4/organize/commit', { batchId: batch.batchId,
      summaries: groups.map((partition, group) => ({ ref: `s${group}`, title: 'Conclusion', topic: 'Evidence', content: 'Final conclusion: selected the verified approach because the evidence supports it. 😀',
        sources: batch.sources.filter((source) => source.partition === partition).map((source) => source.id) })),
      coverage: batch.sources.map((source) => ({ id: source.id, revision: source.revision, status: 'complete', summaries: [`s${groups.indexOf(source.partition)}`] })) });
    assert.equal(committed.status, 200, JSON.stringify(committed.body)); await f.drain();
    check('organize commit');
    const summary = exec(f.db, "SELECT memory_id FROM memory_organization_summaries WHERE status = 'active'").toArray()[0]?.memory_id;
    assert.ok(summary, 'summary memory published');
    exec(f.db, "INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) SELECT id, topic_revision, 'Summary topic', 'k-summary', '2026-10-01' FROM memories WHERE id = ? AND topic_revision IS NOT NULL", summary);
    check('summary topic');
    assert.equal((await f.api(`/v3/documents/${original}`, { content: 'Original conclusion edited' }, 'PATCH')).status, 200); await f.drain();
    assert.equal(exec(f.db, 'SELECT is_forgotten FROM memories WHERE id = ?', summary).one().is_forgotten, 1, 'nested 0009 trigger forgot the summary');
    check('source edit invalidates summary');
    // 直接 SQL による各列の変更。
    exec(f.db, `UPDATE memories SET metadata_json = json_set(metadata_json, '$.title', 'Direct TITLE', '$.sm_scope', 'direct') WHERE id = 'edge-001'`);
    check('direct metadata');
    exec(f.db, "UPDATE memories SET topic_revision = 'stale-revision' WHERE id = 'edge-003'");
    check('direct topic_revision');
    exec(f.db, "UPDATE memories SET updated_at = '2026-12-01T00:00:00.000Z' WHERE id = 'edge-005'");
    check('direct updated_at');
    exec(f.db, "UPDATE memories SET created_at = '2025-01-01T00:00:00.000Z' WHERE id = 'edge-009'");
    check('direct created_at');
    exec(f.db, "UPDATE memories SET container_tag = 'edge' WHERE id = 'edge-011'");
    check('direct container_tag');
    exec(f.db, "UPDATE memories SET is_forgotten = 1 WHERE id = 'edge-013'");
    check('direct forget');
    exec(f.db, "UPDATE memories SET is_forgotten = 0 WHERE id = 'edge-013'");
    check('direct restore');
    // 分類行の追加・更新・削除（現行改訂と古い改訂の両方）。
    exec(f.db, "INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) SELECT id, topic_revision, 'Added TOPIC', 'k-added', '2026-10-01' FROM memories WHERE id = 'edge-005'");
    check('topic insert');
    exec(f.db, "INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES ('edge-005', 'other-revision', 'Other', 'k-other', '2026-10-01')");
    check('stale topic insert');
    exec(f.db, "UPDATE memory_topics SET topic = 'Renamed TOPIC' WHERE memory_id = 'edge-005' AND topic_key = 'k-added'");
    check('topic update');
    exec(f.db, "UPDATE memory_topics SET source_revision = (SELECT topic_revision FROM memories WHERE id = 'edge-005') WHERE memory_id = 'edge-005' AND topic_key = 'k-other'");
    check('topic revision update');
    exec(f.db, "UPDATE memory_topics SET memory_id = 'edge-007' WHERE memory_id = 'edge-005' AND topic_key = 'k-other'");
    check('topic moved between memories');
    exec(f.db, "DELETE FROM memory_topics WHERE memory_id = 'edge-005'");
    check('topic delete');
    // ベクトル・埋め込み・enrichment 状態だけの更新ではトリガーが走らない（射影行を目印で汚しても残る）。
    exec(f.db, "UPDATE memory_search_projection SET index_text = 'sentinel' WHERE memory_id = 'edge-001'");
    exec(f.db, "UPDATE memories SET vector_status = 'indexed', vector_attempted_at = '2026-10-01', embedding_status = 'done', embedding_json = '[1]', enrichment_error = 'x', topic_status = 'done' WHERE id = 'edge-001'");
    assert.equal(exec(f.db, "SELECT index_text FROM memory_search_projection WHERE memory_id = 'edge-001'").one().index_text, 'sentinel');
    exec(f.db, "UPDATE memories SET updated_at = updated_at || '' WHERE id = 'edge-001'");
    assert.equal(exec(f.db, "SELECT index_text FROM memory_search_projection WHERE memory_id = 'edge-001'").one().index_text, 'sentinel', 'unchanged values do not rebuild');
    exec(f.db, "UPDATE memories SET metadata_json = json_set(metadata_json, '$.touch', 1) WHERE id = 'edge-001'");
    check('rebuild after sentinel');
  } finally { await f.drain(); }
});

// FTS の同点（同じ本文で bm25 が等しい行）と本文部分一致の同点（同じ updated_at）が、変更前と同じ並びになること。
// 新しい文は row_order（rowid）を最後の並べ替えキーにしており、変更前の「走査順のまま」と一致する前提を確かめる。
test('search projection: ties keep the pre-0014 order for FTS, instr and indexed statements', async () => {
  const f = fixture();
  try {
    const ids = [];
    for (let i = 0; i < 14; i++) {
      const title = ['Tie alpha', 'Other title', 'Tie beta'][i % 3];
      const memoryIndex = i % 4 === 3 ? { version: 'legacy' } : i % 5 === 4 ? undefined : buildMemoryIndex({ title, request: `${title} request`, sourceUpdatedAt: '2026-10-01T00:00:00Z' });
      const r = await f.api('/v3/documents', { containerTag: i % 2 ? 'tie-a' : 'tie-b', content: 'Tie evidence alphabet 東京 shared body',
        metadata: { title, ...(memoryIndex ? { memoryIndex } : {}) } });
      assert.equal(r.status, 201, JSON.stringify(r.body));
      ids.push(r.body.id);
    }
    await f.drain();
    exec(f.db, "UPDATE memories SET updated_at = '2026-10-02T00:00:00.000Z', topic_revision = 'r1'");
    // 分類の一致だけで優先される行と、一致しない分類だけを持つ行を混ぜる。
    ids.forEach((id, i) => { if (i % 3 === 1) exec(f.db, "INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, 'r1', ?, 'k', '2026-10-01')", id, i % 2 ? 'Alphabet topic' : 'Unrelated'); });
    const captured = [];
    f.DB.hooks.all = (sql, values) => { if (beforeStatement(sql)) captured.push({ sql, values }); };
    const tied = {};
    for (const q of ['alphabet', '東京', 'tie']) for (const scope of [{ allContainers: true }, { containerTag: 'tie-a' }]) {
      captured.length = 0;
      const response = await f.api('/v4/search', { q, limit: 40, indexOnly: true, ...scope });
      assert.equal(response.status, 200, JSON.stringify(response.body));
      for (const { sql, values } of captured) {
        const before = beforeStatement(sql);
        const current = exec(f.db, sql, ...values).toArray();
        const old = exec(f.db, before.sql, ...values).toArray();
        tied[before.kind] = Math.max(tied[before.kind] ?? 0, old.length);
        assert.deepEqual(current.map((row) => row.id), old.map((row) => row.id), `${before.kind} ${q} ${JSON.stringify(scope)}`);
        if (before.kind === 'fts') assert.ok(new Set(current.map((row) => row.rank)).size < current.length, 'bm25 ties exist');
      }
    }
    // どの文でも、同点を含む複数行の結果を比べたこと。
    assert.deepEqual(Object.keys(tied).sort(), ['fts', 'indexed', 'instr']);
    assert.ok(Object.values(tied).every((rows) => rows > 3), JSON.stringify(tied));
  } finally { f.DB.hooks.all = undefined; await f.drain(); }
});

const LONG_PROMPT = '検索APIでD1の負荷が高い原因を調査して、記憶整理コマンドと東京の設計メモ、Orion transport の検証手順、CAFÉ evidence と İstanbul の分類も確認して最適化して';
const plan = (db, sql, values) => exec(db, `EXPLAIN QUERY PLAN ${sql}`, ...values).toArray().map((row) => row.detail);

// 読み取り行数と実行計画。分類数（0件と6件）や検索語数（短い問いと長い依頼文）を変えても、
// 新しい文の読み取り行数はほぼ変わらない（照合対象は保存済みで、分類を読まない）。
test('search projection: rows read stay bounded across topic counts and query lengths, with no memories scan', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const ids = [];
    for (let i = 0; i < 60; i++) {
      const title = ['検索APIの負荷 調査', 'Orion 設計', 'plain note', 'CAFÉ 東京 Evidence'][i % 4];
      const r = await f.api('/v3/documents', { containerTag: ['space-a', 'space-b'][i % 2], content: `# ${title}\n本文 ${i} 負荷と D1 の調査 evidence 東京 alpha`,
        metadata: { title, memoryIndex: buildMemoryIndex({ title, request: `${title} の説明`, sourceUpdatedAt: '2026-10-01T00:00:00Z' }), sm_scope: i % 2 ? 'public' : 'private',
          type: 'conversation', sessionId: `rows-${i % 12}`, turn: i, part: 1 } });
      assert.equal(r.status, 201, JSON.stringify(r.body));
      ids.push(r.body.id);
    }
    await f.drain();
    exec(f.db, "UPDATE memories SET topic_revision = 'r1'");
    const live = exec(f.db, 'SELECT COUNT(*) AS n FROM memories WHERE is_forgotten = 0').one().n;
    const violations = [];
    const measure = async (body) => {
      const captured = [];
      f.DB.hooks.all = (sql, values) => { if (beforeStatement(sql)) captured.push({ sql, values }); };
      try { assert.equal((await f.api('/v4/search', body)).status, 200); } finally { f.DB.hooks.all = undefined; }
      const result = {};
      for (const { sql, values } of captured) {
        const before = beforeStatement(sql);
        const current = exec(f.db, sql, ...values); current.toArray();
        const old = exec(f.db, before.sql, ...values); old.toArray();
        // 上限は「変更前 + 選んだ候補数（上限 MAX_LIMIT = 50）」。理由は下の assert 側のコメントを参照。
        if (current.rowsRead > old.rowsRead + 50) violations.push(`${before.kind} ${body.q.slice(0, 12)} rows ${current.rowsRead} > ${old.rowsRead} + 50`);
        result[before.kind] = { current: current.rowsRead, before: old.rowsRead, plan: plan(f.db, sql, values) };
      }
      return result;
    };
    const bodies = [{ q: '負荷 evidence', limit: 40, indexOnly: true, allContainers: true }, { q: LONG_PROMPT, limit: 40, indexOnly: true, allContainers: true }];
    // フックを差し替えて文を捕まえるため、1件ずつ順に測る。
    const without = [];
    for (const body of bodies) without.push(await measure(body));
    // 一致しない分類を各行に6件ずつ足す（照合結果は変えず、変更前の文では分類の読み取りが増える）。
    ids.forEach((id) => { for (let k = 0; k < 6; k++) exec(f.db, "INSERT INTO memory_topics (memory_id, source_revision, topic, topic_key, created_at) VALUES (?, 'r1', ?, ?, '2026-10-01')", id, `unrelated ${k}`, `u-${k}`); });
    const withTopics = [];
    for (const body of bodies) withTopics.push(await measure(body));
    const summary = { live, without: without.map((r) => Object.fromEntries(Object.entries(r).map(([k, v]) => [k, [v.current, v.before]]))),
      withTopics: withTopics.map((r) => Object.fromEntries(Object.entries(r).map(([k, v]) => [k, [v.current, v.before]]))) };
    console.log(JSON.stringify({ event: 'projection_rows_read', ...summary, plans: Object.fromEntries(Object.entries(withTopics[1]).map(([k, v]) => [k, v.plan])) }));
    // 新しい文は、一致行ごとに射影を1行読み、選んだ候補（上限 50）だけ memories を読み直す。
    // 変更前は、一致行ごとに memories を1行と分類を読んでいた。射影を読む代わりに memories と分類の読みが減る構造なので、
    // 分類が少ないデータ（0件）では差が最大でも「選んだ候補数」（+50）に収まり、分類が多いデータでは変更前を下回る。
    assert.deepEqual(violations, [], 'rows read must not exceed the pre-0014 statements plus the selected candidates (MAX_LIMIT 50)');
    // 分類が各行6件のデータ（本番の平均は約4件/行）では、どの文も変更前より厳密に少ないこと。
    withTopics.forEach((r, index) => {
      for (const kind of Object.keys(r)) assert.ok(r[kind].current < r[kind].before, `${kind} must read fewer rows than before with 6 topics per row (case ${index}) ${JSON.stringify(summary)}`);
    });
    for (const kind of ['indexed', 'fts']) {
      bodies.forEach((_, index) => {
        const zero = without[index][kind]?.current, six = withTopics[index][kind]?.current;
        assert.ok(zero !== undefined && six !== undefined, `${kind} measured for every case ${JSON.stringify(summary)}`);
        // 同じ問い合わせなら、分類が0件でも6件でも読み取り行数はほぼ同じ（照合対象は保存済みで分類を読まない）。
        assert.ok(Math.abs(six - zero) <= 10, `${kind} rows read depend on topic count ${JSON.stringify(summary)}`);
      });
      // 検索語数（短い問いと長い依頼文）による差は、一致行を rowid で読み直す数（上限 50 行）以内に収まる。
      // 照合対象の読み取りは語数に比例しない。
      for (const set of [without, withTopics]) assert.ok(Math.abs(set[1][kind].current - set[0][kind].current) <= 50, `${kind} rows read depend on query length ${JSON.stringify(summary)}`);
    }
    for (const r of [...without, ...withTopics]) {
      // 索引一致は生きている行数＋上限の定数倍以内（射影の走査1回と、上限内の行の読み直し）。
      assert.ok(r.indexed.current <= live + 3 * 50, `indexed rows ${r.indexed.current} live ${live}`);
      for (const kind of ['indexed', 'fts']) {
        const details = r[kind].plan;
        assert.ok(!details.some((detail) => /\bSCAN (m|memories)\b/.test(detail)), `${kind} scans memories: ${details.join(' | ')}`);
        assert.ok(details.filter((detail) => /\b(m|memories)\b/.test(detail)).every((detail) => detail.startsWith('SEARCH')), `${kind}: ${details.join(' | ')}`);
      }
    }
  } finally { f.DB.hooks.all = undefined; await f.drain(); }
});
