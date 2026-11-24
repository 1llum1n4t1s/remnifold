// 統合対象の集計（consolidationTargets）の変更が、結果（順序・値・件数）を変えないことを確かめる。
// - 旧文はこのテストに固定した版（memories 全走査の WITH raw / unconsolidated）。新文は製品コード（server/src/index.ts）が
//   実際に発行した文をフックで捕まえたものを使う。projectKey 指定時の文は旧文と空白を畳んで一致すること、束縛は [projectKey]。
//   指定なしの文は射影から始まり、束縛なし。
// - 同値: 同じ DB・同じ乱択の操作列（recallable 0/1・型違い metadata・sm_consolidation・topic_revision NULL・forget/復元・
//   versions・sources の有無と版違い・複数 container・project 有無）で、新旧の全行を順序込みで比べる。
//   スキーマは 0018 まで（旧索引・新索引なし）と 0019 まで（新索引）の両方。
// - 計画: 0019 では anti-join が新しい被覆索引を使い、旧索引は消えて本数は変わらないこと。時間は測らない。
// - 1 ケースにつき fixture は1つだけ。変種ごとにケースを分ける。
import test, { assert } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';
import worker from '../src/index.ts';

const norm = (sql) => sql.replace(/\s+/gu, ' ').trim();
const KEY_SQL = `CASE
  WHEN json_type(m.metadata_json, '$.sm_project_id') = 'text'
       AND length(trim(json_extract(m.metadata_json, '$.sm_project_id'))) > 0
    THEN 'project:' || json_extract(m.metadata_json, '$.sm_project_id')
  ELSE 'container:' || m.container_tag
END`;
const oldSql = (withKey) => norm(`WITH raw AS (
       SELECT m.id, m.container_tag, m.created_at,
              COALESCE(m.topic_revision, m.updated_at) AS source_revision,
              ${KEY_SQL} AS project_key,
              CASE WHEN json_type(m.metadata_json, '$.sm_project_id') = 'text'
                    AND length(trim(json_extract(m.metadata_json, '$.sm_project_id'))) > 0
                   THEN json_extract(m.metadata_json, '$.sm_project_id') END AS project_id
       FROM memories AS m
       WHERE m.is_forgotten = 0
         AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0
         AND json_extract(m.metadata_json, '$.sm_consolidation') IS NOT 1
         AND NOT EXISTS(SELECT 1 FROM memory_organization_versions WHERE memory_id=m.id)
         ${withKey ? `AND ${KEY_SQL} = ?` : ''}
     ), unconsolidated AS (
       SELECT raw.* FROM raw
       WHERE NOT EXISTS (
         SELECT 1 FROM memory_consolidation_sources AS source
         JOIN memory_consolidations AS consolidation
           ON consolidation.id = source.consolidation_id AND consolidation.status = 'active'
         WHERE source.memory_id = raw.id AND source.source_revision = raw.source_revision
       )
     )
     SELECT u.project_key AS projectKey, MAX(u.project_id) AS projectId,
            MIN(u.container_tag) AS sourceContainerTag, COUNT(*) AS unconsolidatedCount,
            MIN(u.created_at) AS oldestUnconsolidatedAt,
            project.last_success_at AS lastSuccessAt,
            project.initial_backfill_completed AS initialBackfillCompleted
     FROM unconsolidated AS u
     LEFT JOIN memory_consolidation_projects AS project ON project.project_key = u.project_key
     GROUP BY u.project_key, project.last_success_at, project.initial_backfill_completed
     ORDER BY oldestUnconsolidatedAt ASC`);

function capture(f) {
  const log = [];
  f.DB.hooks.all = (sql, values) => { log.push({ sql: norm(sql), values: [...values] }); };
  return log;
}
const targetsOf = (log) => log.filter((e) => /^WITH (raw|candidate) AS/u.test(e.sql));

// 製品の発行文を捕まえる。指定なしは定期実行、指定ありは手動 consolidate（unconsolidatedTarget）から。
async function productAll(f) {
  f.env.AI_ENRICHMENT_MODE = 'on';
  const log = capture(f);
  worker.scheduled({ cron: '0 18 * * *' }, f.env, f.ctx);
  await f.drain();
  const hits = targetsOf(log).filter((e) => e.values.length === 0);
  assert.ok(hits.length >= 1, 'product statement without projectKey was issued');
  return hits[0];
}
async function productProject(f) {
  f.env.AI_ENRICHMENT_MODE = 'on';
  const log = capture(f);
  await f.api('/v4/consolidate', { projectId: 'p1', force: true });
  await f.drain();
  const hits = targetsOf(log).filter((e) => e.values.length === 1);
  assert.deepEqual(hits.map((e) => e.values[0]), ['project:p1', 'container:p1'], 'both keys are tried');
  return hits;
}

function mulberry32(seed) {
  let a = seed;
  return () => { a = (a + 0x6D2B79F5) | 0; let t = Math.imul(a ^ (a >>> 15), 1 | a); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const pick = (rng, values) => values[Math.floor(rng() * values.length)];

const METADATA = ['{}', '{}', '{"memoryIndex":{"recallable":0}}', '{"memoryIndex":{"recallable":1}}', '{"memoryIndex":"x"}',
  '{"memoryIndex":{"recallable":false}}', '{"sm_consolidation":1}', '{"sm_consolidation":0}', '{"sm_consolidation":true}',
  '{"sm_project_id":"p1"}', '{"sm_project_id":"p1","memoryIndex":{"recallable":0}}', '{"sm_project_id":"p2"}', '{"sm_project_id":"  "}',
  '{"sm_project_id":5}', '[]', '{"sm_project_id":"p1","sm_consolidation":1}'];
const CONTAINERS = ['c1', 'c2', 'c3'];
const REVISIONS = [null, null, 'r1', 'r2'];
const CREATED = ['2026-01-05T00:00:00.000Z', '2026-01-05T00:00:00.000Z', '2026-01-03T00:00:00.000Z', '2026-01-09T00:00:00.000Z', '2026-01-01T00:00:00.000Z'];
const KEYS = ['project:p1', 'project:p2', 'container:c1', 'container:c2', 'container:c3', 'project:5', 'container:none'];

function buildWorld(f, rng) {
  const run = (sql, ...args) => f.db.prepare(sql).run(...args);
  const world = { memories: [], summaries: [], consolidations: [] };
  for (const tag of CONTAINERS) run(`INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, 't', 't')`, tag, tag);
  for (let i = 0; i < 40; i += 1) {
    const id = `m-${i}`;
    run(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, entity_context, status, is_forgotten, embedding_status, fact_status,
      vector_status, topic_status, topic_revision, created_at, updated_at) VALUES (?, ?, ?, ?, ?, NULL, 'done', ?, 'pending', 'pending', 'disabled', 'pending', ?, ?, ?)`,
      id, `c-${i}`, pick(rng, CONTAINERS), `content ${i}`, pick(rng, METADATA), rng() < 0.15 ? 1 : 0, pick(rng, REVISIONS), pick(rng, CREATED), pick(rng, ['u1', 'u2', 'u3']));
    world.memories.push(id);
  }
  const projects = [['project:p1', 1, '2026-01-02T00:00:00.000Z'], ['project:p2', 0, null], ['container:c1', 1, null], ['container:c2', 1, 'bad-date']];
  for (const [key, backfill, last] of projects) {
    run(`INSERT INTO memory_consolidation_projects(project_key, source_container_tag, initial_backfill_completed, last_success_at, created_at, updated_at) VALUES (?, 'c1', ?, ?, 't', 't')`, key, backfill, last);
  }
  let n = 0;
  for (const [key] of projects) for (const status of ['active', 'superseded', 'invalid']) {
    const memoryId = `s-${n}`;
    run(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, embedding_status, fact_status, vector_status, topic_status, created_at, updated_at)
      VALUES (?, ?, 'c1', 'summary', '{"sm_consolidation":1}', 'done', 0, 'pending', 'pending', 'disabled', 'pending', 't', 't')`, memoryId, `sc-${n}`);
    run(`INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 't', 't')`, `k-${n}`, key, memoryId, n + 1, status);
    world.consolidations.push({ id: `k-${n}`, status });
    n += 1;
  }
  return world;
}

function mutate(f, world, rng) {
  const run = (sql, ...args) => f.db.prepare(sql).run(...args);
  const id = pick(rng, world.memories);
  switch (Math.floor(rng() * 8)) {
    case 0: run('UPDATE memories SET is_forgotten = 1 WHERE id = ?', id); break;
    case 1: run('UPDATE memories SET is_forgotten = 0 WHERE id = ?', id); break;
    case 2: run('UPDATE memories SET topic_revision = ? WHERE id = ?', pick(rng, REVISIONS), id); break;
    case 3: run('UPDATE memories SET metadata_json = ? WHERE id = ?', pick(rng, METADATA), id); break;
    case 4: run('UPDATE memories SET updated_at = ? WHERE id = ?', pick(rng, ['u1', 'u2', 'u9']), id); break;
    case 5: run('UPDATE memories SET container_tag = ? WHERE id = ?', pick(rng, CONTAINERS), id); break;
    case 6: run('UPDATE memories SET created_at = ? WHERE id = ?', pick(rng, CREATED), id); break;
    default: {
      if (rng() < 0.5) run('DELETE FROM memory_organization_versions WHERE memory_id = ?', id);
      else run('INSERT OR REPLACE INTO memory_organization_versions(memory_id, summary_id) VALUES (?, ?)', id, 'sum');
    }
  }
}

// 統合元の登録（現在の版・古い版・無し、active/superseded/invalid の統合）。
function settle(f, world, rng) {
  const run = (sql, ...args) => f.db.prepare(sql).run(...args);
  for (const id of world.memories) {
    if (rng() < 0.55) continue;
    const row = f.db.prepare('SELECT COALESCE(topic_revision, updated_at) AS revision FROM memories WHERE id = ?').get(id);
    const consolidation = pick(rng, world.consolidations);
    run('DELETE FROM memory_consolidation_sources WHERE memory_id = ? AND consolidation_id = ?', id, consolidation.id);
    run('INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision) VALUES (?, ?, ?)', consolidation.id, id, rng() < 0.25 ? 'stale' : row.revision);
  }
}

const rows = (f, sql, values) => JSON.stringify(f.db.prepare(sql).all(...values));
const countRows = (f, sql, values) => f.db.prepare(sql).all(...values).length;

function compare(f, world, newAll, newProject, { rounds = 30, seed = 5, newAllOverride } = {}) {
  const rng = mulberry32(seed);
  const at = (label, work) => { try { return work(); } catch (error) { throw Error(`${label}: ${error.message}`); } };
  const stats = { compared: 0, mismatches: 0, nonEmpty: 0, multi: 0, sourced: 0 };
  const oldAll = oldSql(false);
  const oldProject = oldSql(true);
  for (let round = 0; round < rounds; round += 1) {
    for (let step = 0; step < 5; step += 1) at(`mutate round ${round}`, () => mutate(f, world, rng));
    if (round % 2 === 1 || round === 0) at(`settle round ${round}`, () => settle(f, world, rng));
    const before = rows(f, oldAll, []);
    const after = rows(f, newAllOverride ?? newAll, []);
    stats.compared += 1;
    if (before !== after) stats.mismatches += 1;
    const count = countRows(f, oldAll, []);
    if (count > 0) stats.nonEmpty += 1;
    if (count > 2) stats.multi += 1;
    stats.sourced += Number(f.db.prepare('SELECT COUNT(*) AS n FROM memory_consolidation_sources').get().n) > 0 ? 1 : 0;
    for (const key of KEYS) {
      for (const sql of newProject) {
        stats.compared += 1;
        if (rows(f, oldProject, [key]) !== rows(f, sql, [key])) stats.mismatches += 1;
      }
    }
  }
  return stats;
}
const assertCovered = (stats) => assert.ok(stats.compared > 200 && stats.nonEmpty > 15 && stats.multi > 8 && stats.sourced > 10, JSON.stringify(stats));

for (const schema of ['0018', '0019']) {
  test(`consolidation targets: without projectKey matches the old statement exactly (${schema} schema)`, async () => {
    const f = fixture({ beforeMigration: schema === '0018' ? '0019_' : undefined });
    try {
      const hit = await productAll(f);
      assert.deepEqual(hit.values, []);
      assert.notEqual(hit.sql, oldSql(false));
      assert.match(hit.sql, /FROM memory_search_projection AS p WHERE p\.recallable = 1/u);
      assert.match(hit.sql, /LIMIT -1 OFFSET 0/u);
      const world = buildWorld(f, mulberry32(1));
      const stats = compare(f, world, hit.sql, [oldSql(true)]);
      assert.equal(stats.mismatches, 0, JSON.stringify(stats));
      assertCovered(stats);
      // 検出力: recallable の条件を外した文は食い違う。
      const loose = compare(f, world, hit.sql.replace('WHERE p.recallable = 1', 'WHERE 1'), [oldSql(true)], { rounds: 12, seed: 9 });
      assert.ok(loose.mismatches > 0, `recallable sensitivity ${JSON.stringify(loose)}`);
    } finally { f.db.close(); }
  });
}

test('consolidation targets: with projectKey the product statement is the old statement', async () => {
  const f = fixture();
  try {
    const hits = await productProject(f);
    for (const hit of hits) assert.equal(hit.sql, oldSql(true));
    const all = await productAll(f);
    const stats = compare(f, buildWorld(f, mulberry32(2)), all.sql, hits.map((hit) => hit.sql), { rounds: 12, seed: 21 });
    assert.equal(stats.mismatches, 0, JSON.stringify(stats));
  } finally { f.db.close(); }
});

test('consolidation targets: 0019 swaps the lookup index in place and the anti-join uses it', async () => {
  const f = fixture();
  try {
    const names = (db) => db.prepare(`SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'memory_consolidation_sources' AND name NOT LIKE 'sqlite_autoindex%' ORDER BY name`).all().map((r) => r.name);
    assert.deepEqual(names(f.db), ['idx_memory_consolidation_sources_lookup']);
    const hit = await productAll(f);
    const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${hit.sql}`).all().map((r) => String(r.detail));
    assert.ok(plan.some((d) => /idx_memory_consolidation_sources_lookup \(memory_id=\? AND source_revision=\?\)/u.test(d)), plan.join('\n'));
    assert.ok(plan.some((d) => /^SCAN p$/u.test(d)), plan.join('\n'));
    f.db.exec('DROP INDEX idx_memory_consolidation_sources_lookup');
    f.db.exec('CREATE INDEX idx_memory_consolidation_sources_memory ON memory_consolidation_sources(memory_id, source_revision)');
    assert.deepEqual(names(f.db), ['idx_memory_consolidation_sources_memory']);
  } finally { f.db.close(); }
});

test('consolidation targets: 0019 is idempotent and 0018 keeps only the old index', async () => {
  const f = fixture({ beforeMigration: '0019_' });
  try {
    const list = () => f.db.prepare(`SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'memory_consolidation_sources' AND name NOT LIKE 'sqlite_autoindex%'`).all().map((r) => r.name);
    assert.deepEqual(list(), ['idx_memory_consolidation_sources_memory']);
    const { migrations } = await import('./worker-fixture.mjs');
    const sql = migrations.find((m) => m.name.startsWith('0019_')).sql;
    f.db.exec(sql);
    f.db.exec(sql);
    assert.deepEqual(list(), ['idx_memory_consolidation_sources_lookup']);
  } finally { f.db.close(); }
});
