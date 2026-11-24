// 非表示判定を束縛 id にした UPDATE 7文と、忘却 SELECT の2枝 UNION ALL 化が、結果を変えないことを確かめる。
// - 旧文はこのテストに固定した版（h.memory_id=memories.id、旧の忘却 SELECT）。新文は製品コード（server/src/index.ts）が
//   実際に発行した文を、フックで捕まえたものを使う。文面は期待する新文と空白を畳んで一致すること、束縛値は流した値と
//   位置まで一致することを確かめる（束縛の順を入れ替えると落ちる）。
// - 同値: 同じ DB・同じ乱択の操作列（NULL・同点・型違い metadata・forget・復元・整理の公開/失効・hidden 行）で、
//   UPDATE はトランザクション内の changes と対象行の全列、SELECT は順序込み LIMIT 1/7/100/700 を新旧で比べる。
// - 検出力: 最後の束縛を別 id にした文と、束縛を入れ替えた文が、同じ比較で必ず食い違うこと。
// - 計画: 新しい UPDATE の非表示判定が (memory_id=?) の SEARCH になること。時間は測らない。
import test, { assert } from './support/test.mjs';
import { fixture, embedding } from './worker-fixture.mjs';
import worker from '../src/index.ts';

const norm = (sql) => sql.replace(/\s+/gu, ' ').trim();
const vis = (view, idExpr) => `NOT EXISTS(SELECT 1 FROM ${view} h WHERE h.memory_id=${idExpr})`;
const VIEWS = ['memory_organization_hidden', 'memory_organization_complete'];

// 7文の固定文面。v は非表示判定の式。旧は memories.id、新は束縛の ?。
// 束縛の順（in は {id, mut, at, rev, updatedAt, status}）。新は判定の ? を attempt 用の ? より前（本テストは attempt なしの形）に置く。
const SITES = {
  'vector-queued': { tpl: (v) => `UPDATE memories SET vector_status = 'queued', vector_mutation_id = ?, vector_attempted_at = ?
    WHERE id = ? AND updated_at = ? AND topic_revision = ? AND is_forgotten = 0 AND ${v}`,
    old: (i) => [i.mut, i.at, i.id, i.updatedAt, i.rev], next: (i) => [i.mut, i.at, i.id, i.updatedAt, i.rev, i.id], views: VIEWS },
  'forgotten-lookup-failed': { tpl: (v) => `UPDATE memories SET vector_status = 'pending', vector_attempted_at = ?
    WHERE id = ? AND (is_forgotten = 1 OR NOT ${v})`,
    old: (i) => [i.at, i.id], next: (i) => [i.at, i.id, i.id], views: [VIEWS[0]] },
  'forgotten-status': { tpl: (v) => `UPDATE memories SET vector_status = ?, vector_mutation_id = ?, vector_attempted_at = ?
    WHERE id = ? AND (is_forgotten = 1 OR NOT ${v})`,
    old: (i) => [i.status, i.mut, i.at, i.id], next: (i) => [i.status, i.mut, i.at, i.id, i.id], views: [VIEWS[0]] },
  'reconcile-indexed': { tpl: (v) => `UPDATE memories SET vector_status = 'indexed', vector_attempted_at = ?
    WHERE id = ? AND topic_revision IS ? AND is_forgotten = 0 AND ${v}`,
    old: (i) => [i.at, i.id, i.rev], next: (i) => [i.at, i.id, i.rev, i.id], views: VIEWS },
  'reconcile-queued': { tpl: (v) => `UPDATE memories SET vector_status = 'queued', vector_mutation_id = ?, vector_attempted_at = ?
    WHERE id = ? AND topic_revision IS ? AND is_forgotten = 0 AND ${v}`,
    old: (i) => [i.mut, i.at, i.id, i.rev], next: (i) => [i.mut, i.at, i.id, i.rev, i.id], views: VIEWS },
  'stale-upsert-pending': { tpl: (v) => `UPDATE memories SET vector_status = 'pending', vector_mutation_id = NULL, vector_attempted_at = NULL
    WHERE id = ? AND (is_forgotten = 1 OR NOT ${v})`,
    old: (i) => [i.id], next: (i) => [i.id, i.id], views: [VIEWS[0]] },
  'repair-queued': { tpl: (v) => `UPDATE memories SET vector_status = 'queued', vector_mutation_id = ?, vector_attempted_at = ?
    WHERE id = ? AND topic_revision IS ? AND is_forgotten = 0 AND ${v}`,
    old: (i) => [i.mut, i.at, i.id, i.rev], next: (i) => [i.mut, i.at, i.id, i.rev, i.id], views: [VIEWS[0]] },
};
const oldSql = (site, view) => norm(SITES[site].tpl(vis(view, 'memories.id')));
const expectedNewSql = (site, view) => norm(SITES[site].tpl(vis(view, '?')));

// 旧の忘却 SELECT（固定）。新は製品が発行した文。
const OLD_FORGOTTEN = norm(`SELECT id FROM memories
  WHERE is_forgotten = 1 OR NOT ${vis('memory_organization_hidden', 'memories.id')}
  ORDER BY COALESCE(vector_attempted_at, updated_at) ASC
  LIMIT ?`);
const NEW_FORGOTTEN = norm(`SELECT id FROM (
  SELECT * FROM (SELECT id, COALESCE(vector_attempted_at, updated_at) AS k, rowid AS r
    FROM memories WHERE is_forgotten = 1
    ORDER BY COALESCE(vector_attempted_at, updated_at), rowid LIMIT ?)
  UNION ALL
  SELECT * FROM (SELECT m.id, COALESCE(m.vector_attempted_at, m.updated_at) AS k, m.rowid AS r
    FROM memory_organization_hidden AS h CROSS JOIN memories AS m ON m.id = h.memory_id
    ORDER BY k, r LIMIT ?)
) ORDER BY k, r LIMIT ?`);

// ---- 製品が発行した文の捕捉 ----
function capture(f) {
  const log = [];
  for (const hook of ['first', 'all', 'run', 'execute']) f.DB.hooks[hook] = (sql, values) => { log.push({ sql: norm(sql), values: [...values] }); };
  return log;
}
const find = (log, sql) => log.filter((entry) => entry.sql === sql);
const rowOf = (f, id) => f.db.prepare('SELECT updated_at AS updatedAt, topic_revision AS rev FROM memories WHERE id = ?').get(id);
const ISO = /^\d{4}-\d\d-\d\dT[\d:.]+Z$/u;

async function addMemory(f, name) {
  const saved = await f.api('/v3/documents', { containerTag: 'space', customId: name, content: `content ${name}`, metadata: {} });
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  return saved.body.id;
}

// 保存 → 埋め込み → ベクトル upsert の流れ。interfere は最初の upsert の中で DB を書き換え、後続の UPDATE を不成立にする。
async function vectorFlow({ offAtUpsert = false, interfere } = {}) {
  const f = fixture();
  f.env.AI_ENRICHMENT_MODE = 'on';
  const seen = { before: null, upserts: 0 };
  f.env.MEMORY_VECTORS = { deleteByIds: async () => ({}), getByIds: async () => [], upsert: async (rows) => {
    seen.upserts += 1;
    if (seen.upserts === 1) {
      seen.id = rows[0].id;
      seen.before = rowOf(f, seen.id);
      if (offAtUpsert) f.env.AI_ENRICHMENT_MODE = 'off';
      interfere?.(f, seen.id);
    }
    return { mutationId: `mut-${seen.upserts}` };
  } };
  const log = capture(f);
  await addMemory(f, 'flow');
  await f.drain();
  assert.ok(seen.id, 'vector upsert ran');
  return { f, log, seen };
}

// 再照合（scheduled）の流れ。mode は先頭の AI_ENRICHMENT_MODE。
async function reconcile(f) {
  worker.scheduled({ cron: '*/5 * * * *' }, f.env, f.ctx);
  await f.drain();
}

async function forgottenFlow(lookupFails) {
  const f = fixture();
  f.env.AI_ENRICHMENT_MODE = 'off';
  const forgotten = [await addMemory(f, 'a'), await addMemory(f, 'b')];
  await addMemory(f, 'c');
  for (const id of forgotten) f.db.prepare('UPDATE memories SET is_forgotten = 1 WHERE id = ?').run(id);
  f.env.MEMORY_VECTORS = { upsert: async () => ({}), deleteByIds: async () => ({}), getByIds: async () => {
    if (lookupFails) throw Error('lookup-failed');
    return [];
  } };
  const log = capture(f);
  await reconcile(f);
  return { f, log, forgotten };
}

async function activeFlow({ stale, offAtVector }) {
  const f = fixture();
  f.env.AI_ENRICHMENT_MODE = 'off';
  const id = await addMemory(f, 'active');
  f.db.prepare(`UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'pending', vector_attempted_at = NULL WHERE id = ?`)
    .run(JSON.stringify(embedding), id);
  f.env.AI_ENRICHMENT_MODE = 'on';
  const row = rowOf(f, id);
  const flip = () => { if (offAtVector) f.env.AI_ENRICHMENT_MODE = 'off'; };
  f.env.MEMORY_VECTORS = { deleteByIds: async () => ({}),
    getByIds: async (ids) => { flip(); return stale ? [] : ids.map((value) => ({ id: value, metadata: { topic_revision: row.rev ?? '' } })); },
    upsert: async () => { flip(); return { mutationId: 'mut-active' }; } };
  const log = capture(f);
  await reconcile(f);
  return { f, log, id, row };
}

// ---- 新旧の比較 ----
function mulberry32(seed) {
  let a = seed;
  return () => { a = (a + 0x6D2B79F5) | 0; let t = Math.imul(a ^ (a >>> 15), 1 | a); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const pick = (rng, values) => values[Math.floor(rng() * values.length)];

// 比較用の状態。source 8件と summary 用 4件を作る。
async function randomWorld(f) {
  f.env.AI_ENRICHMENT_MODE = 'off';
  const sources = [];
  const summaries = [];
  for (let i = 0; i < 8; i += 1) sources.push(await addMemory(f, `src-${i}`));
  for (let i = 0; i < 4; i += 1) {
    const memoryId = await addMemory(f, `sum-${i}`);
    f.db.prepare(`INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES (?, ?, 'r', 'active', 't')`).run(`summary-${i}`, memoryId);
    summaries.push({ id: `summary-${i}`, memoryId });
  }
  return { sources, summaries, all: [...sources, ...summaries.map((s) => s.memoryId)] };
}

const METADATA = ['{}', '{"memoryIndex":{"recallable":0}}', '{"memoryIndex":"x"}', '[]', '{"sm_scope":1}', '{"memoryIndex":{"recallable":false}}'];
const ATTEMPTED = [null, null, 'a', 'a', 'b', '', 5, 2.5];
const UPDATED = ['t1', 't1', 't2', 't0', ''];

function mutate(f, world, rng) {
  const id = pick(rng, world.all);
  // 更新トリガーの対象列（is_forgotten・topic_revision・metadata_json）は source にだけ書く。summary の記憶に書くと、
  // トリガーが summary を失効させて記憶を忘却し、復元もできず、公開（complete・hidden）の行が残らなくなる。
  const source = pick(rng, world.sources);
  const run = (sql, ...args) => f.db.prepare(sql).run(...args);
  switch (Math.floor(rng() * 11)) {
    case 0: run('UPDATE memories SET is_forgotten = 1 WHERE id = ?', source); break;
    case 1: run('UPDATE memories SET is_forgotten = 0 WHERE id = ?', source); break;
    case 2: run('UPDATE memories SET vector_attempted_at = ? WHERE id = ?', pick(rng, ATTEMPTED), id); break;
    case 3: run('UPDATE memories SET updated_at = ? WHERE id = ?', pick(rng, UPDATED), id); break;
    case 4: run('UPDATE memories SET topic_revision = ? WHERE id = ?', pick(rng, [null, 'r1', 'r2']), source); break;
    case 5: run('UPDATE memories SET embedding_json = ? WHERE id = ?', pick(rng, [null, '[1]']), id); break;
    case 6: run('UPDATE memories SET embedding_status = ?, vector_status = ? WHERE id = ?', pick(rng, ['done', 'pending']), pick(rng, ['indexed', 'pending', 'queued', 'failed', 'disabled']), id); break;
    case 7: run('UPDATE memories SET metadata_json = ? WHERE id = ?', pick(rng, METADATA), source); break;
    case 8: run('UPDATE memory_organization_summaries SET status = ? WHERE id = ?', pick(rng, ['active', 'active', 'invalid']), pick(rng, world.summaries).id); break;
    default: {
      const source = pick(rng, world.sources);
      run('DELETE FROM memory_organization_coverage WHERE memory_id = ?', source);
      const kind = pick(rng, ['none', 'active', 'active', 'stale', 'invalid']);
      if (kind === 'none') break;
      const current = f.db.prepare('SELECT COALESCE(topic_revision, updated_at) AS revision FROM memories WHERE id = ?').get(source).revision;
      run('INSERT INTO memory_organization_coverage(memory_id, source_revision, status) VALUES (?, ?, ?)', source, kind === 'stale' ? 'stale' : current, kind === 'invalid' ? 'invalid' : 'active');
      run('INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, ?)', source, pick(rng, world.summaries).id);
    }
  }
}

// 整理の公開/失効を成立させる（summary を復元・active にし、source の coverage を現在の版で作り直す）。
// 乱択の操作だけでは coverage がトリガーで失効し、complete・hidden の行がほとんど残らないため、比較の前に足す。
function settle(f, world, rng) {
  const run = (sql, ...args) => f.db.prepare(sql).run(...args);
  for (const summary of world.summaries) {
    run(`UPDATE memory_organization_summaries SET status = 'active' WHERE id = ?`, summary.id);
    const indexed = rng() < 0.5;
    run('UPDATE memories SET embedding_json = ?, embedding_status = ?, vector_status = ? WHERE id = ?', indexed ? '[1]' : null, indexed ? 'done' : 'pending', indexed ? 'indexed' : 'pending', summary.memoryId);
  }
  for (const source of world.sources) {
    if (rng() < 0.3) continue;
    run('UPDATE memories SET is_forgotten = ?, embedding_json = ? WHERE id = ?', rng() < 0.15 ? 1 : 0, rng() < 0.4 ? '[1]' : null, source);
    run('DELETE FROM memory_organization_coverage WHERE memory_id = ?', source);
    const current = f.db.prepare('SELECT COALESCE(topic_revision, updated_at) AS revision FROM memories WHERE id = ?').get(source).revision;
    run('INSERT INTO memory_organization_coverage(memory_id, source_revision, status) VALUES (?, ?, ?)', source, rng() < 0.15 ? 'stale' : current, rng() < 0.15 ? 'invalid' : 'active');
    run('INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, ?)', source, pick(rng, world.summaries).id);
  }
}

const ROLLBACK = Symbol('rollback');
function inRollback(f, work) {
  let out;
  try { f.db.transaction(() => { out = work(); throw ROLLBACK; }); } catch (error) { if (error !== ROLLBACK) throw error; }
  return out;
}
const runUpdate = (f, sql, args, id) => inRollback(f, () => {
  const changes = Number(f.db.prepare(sql).run(...args).changes);
  return { changes, row: JSON.stringify(f.db.prepare('SELECT * FROM memories WHERE id = ?').get(id) ?? null) };
});

// 新旧の UPDATE を、状態を進めながら比べ、食い違いの数を返す。nextArgs で新文の束縛を差し替えられる。
function compareUpdates(f, world, site, { oldText, newText, nextArgs = SITES[site].next, rounds = 14, seed = 7 }) {
  const rng = mulberry32(seed);
  const stats = { compared: 0, mismatches: 0, hiddenSeen: 0, changed: 0 };
  for (let round = 0; round < rounds; round += 1) {
    for (let step = 0; step < 4; step += 1) mutate(f, world, rng);
    if (round % 2 === 1) settle(f, world, rng);
    stats.hiddenSeen += Number(f.db.prepare('SELECT COUNT(*) AS n FROM memory_organization_hidden').get().n);
    for (const id of [...world.all, 'missing-id']) {
      const current = f.db.prepare('SELECT updated_at AS updatedAt, topic_revision AS rev FROM memories WHERE id = ?').get(id) ?? { updatedAt: 'x', rev: null };
      for (const variant of [{}, { rev: 'other' }, { updatedAt: 'other' }, { rev: null }]) {
        const input = { id, mut: 'M', at: 'T', status: pick(rng, ['disabled', 'queued', 'pending']), ...current, ...variant };
        const before = runUpdate(f, oldText, SITES[site].old(input), id);
        const after = runUpdate(f, newText, nextArgs(input), id);
        stats.compared += 1;
        stats.changed += before.changes;
        if (before.changes !== after.changes || before.row !== after.row) stats.mismatches += 1;
      }
    }
  }
  return stats;
}

// ---- 各文の確認 ----
// 製品が発行した文を1つだけ取り出し、文面と束縛値を確かめ、乱択の比較に使う。
function productStatement(log, site, view, expectValues) {
  const hits = find(log, expectedNewSql(site, view));
  assert.ok(hits.length >= 1, `${site}/${view}: product statement was issued\n${[...new Set(log.map((e) => e.sql))].filter((s) => s.startsWith('UPDATE memories SET vector_status')).join('\n')}`);
  for (const hit of hits) assert.deepEqual(hit.values, expectValues(hit.values), `${site}/${view}: bind order`);
  return hits[0];
}

async function verifyEquivalence(f, site, view, statement) {
  const world = await randomWorld(f);
  const text = { oldText: oldSql(site, view), newText: statement.sql };
  const stats = compareUpdates(f, world, site, text);
  assert.equal(stats.mismatches, 0, `${site}/${view}: ${JSON.stringify(stats)}`);
  assert.ok(stats.compared > 500 && stats.changed > 20 && stats.hiddenSeen > 5, `coverage of the random run ${JSON.stringify(stats)}`);
  // 検出力: 非表示判定の束縛を別 id にした文は、同じ比較で食い違う。
  const wrongId = compareUpdates(f, world, site, { ...text, nextArgs: (i) => SITES[site].next(i).map((v, index, all) => index === all.length - 1 ? 'other-id' : v), rounds: 6, seed: 11 });
  assert.ok(wrongId.mismatches > 0, `${site}/${view}: wrong id bind is detected ${JSON.stringify(wrongId)}`);
  return stats;
}

// 1 ケースにつき fixture は1つだけ（同じ Durable Object で作り直せない）。変種ごとにケースを分ける。
for (const offAtUpsert of [false, true]) {
  test(`visibility statements: vector-queued binds the id after the revision (${offAtUpsert ? 'complete' : 'hidden'})`, async () => {
    const { f, log, seen } = await vectorFlow({ offAtUpsert });
    try {
      const view = VIEWS[offAtUpsert ? 1 : 0];
      const hit = productStatement(log, 'vector-queued', view, (values) => [
        'mut-1', values[1], seen.id, seen.before.updatedAt, seen.before.rev, seen.id]);
      assert.match(hit.values[1], ISO);
      assert.equal(seen.before.rev === seen.id, false);
      await verifyEquivalence(f, 'vector-queued', view, hit);
    } finally { f.db.close(); }
  });
}

test('visibility statements: stale upsert after forget binds the id twice', async () => {
  const { f, log, seen } = await vectorFlow({ interfere: (g, id) => g.db.prepare('UPDATE memories SET is_forgotten = 1 WHERE id = ?').run(id) });
  try {
    const hit = productStatement(log, 'stale-upsert-pending', VIEWS[0], () => [seen.id, seen.id]);
    await verifyEquivalence(f, 'stale-upsert-pending', VIEWS[0], hit);
  } finally { f.db.close(); }
});

test('visibility statements: repair after a stale upsert binds the id after the revision', async () => {
  const { f, log, seen } = await vectorFlow({ interfere: (g, id) => g.db.prepare(`UPDATE memories SET updated_at = 'moved' WHERE id = ?`).run(id) });
  try {
    const hit = productStatement(log, 'repair-queued', VIEWS[0], (values) => ['mut-2', values[1], seen.id, seen.before.rev, seen.id]);
    assert.match(hit.values[1], ISO);
    await verifyEquivalence(f, 'repair-queued', VIEWS[0], hit);
  } finally { f.db.close(); }
});

for (const lookupFails of [true, false]) {
  test(`visibility statements: forgotten reconcile ${lookupFails ? 'lookup failure' : 'status'} update binds the id twice`, async () => {
    const { f, log, forgotten } = await forgottenFlow(lookupFails);
    try {
      const site = lookupFails ? 'forgotten-lookup-failed' : 'forgotten-status';
      const hits = find(log, expectedNewSql(site, VIEWS[0]));
      assert.deepEqual(hits.map((hit) => hit.values[lookupFails ? 1 : 3]).sort(), [...forgotten].sort(), `${site}: one statement per forgotten id`);
      for (const hit of hits) {
        const tail = hit.values.slice(-2);
        assert.equal(tail[0], tail[1], `${site}: id bound twice`);
        if (lookupFails) assert.match(hit.values[0], ISO);
        else assert.deepEqual([hit.values[0], hit.values[1]], ['disabled', null]);
      }
      await verifyEquivalence(f, site, VIEWS[0], hits[0]);
    } finally { f.db.close(); }
  });
}

for (const offAtVector of [false, true]) {
  for (const stale of [false, true]) {
    test(`visibility statements: reconcile ${stale ? 'queued' : 'indexed'} update binds the id after the revision (${offAtVector ? 'complete' : 'hidden'})`, async () => {
      const view = VIEWS[offAtVector ? 1 : 0];
      const { f, log, id, row } = await activeFlow({ stale, offAtVector });
      try {
        const site = stale ? 'reconcile-queued' : 'reconcile-indexed';
        const hit = productStatement(log, site, view, stale
          ? (values) => ['mut-active', values[1], id, row.rev, id]
          : (values) => [values[0], id, row.rev, id]);
        assert.match(hit.values[stale ? 1 : 0], ISO);
        await verifyEquivalence(f, site, view, hit);
      } finally { f.db.close(); }
    });
  }
}


test('visibility statements: forgotten SELECT returns the same ids in the same order', async () => {
  const { f, log } = await forgottenFlow(false);
  try {
    const hits = find(log, NEW_FORGOTTEN);
    assert.equal(hits.length, 1, 'product forgotten SELECT');
    assert.deepEqual(hits[0].values, [100, 100, 100]);
    const world = await randomWorld(f);
    const rng = mulberry32(3);
    let nonEmpty = 0;
    let hiddenForgottenMix = 0;
    for (let round = 0; round < 40; round += 1) {
      for (let step = 0; step < 4; step += 1) mutate(f, world, rng);
      if (round % 2 === 1) settle(f, world, rng);
      const all = f.db.prepare('SELECT id FROM memories').all().length;
      for (const limit of [1, 7, 100, 700]) {
        const before = f.db.prepare(OLD_FORGOTTEN).all(limit).map((row) => row.id);
        const after = f.db.prepare(hits[0].sql).all(limit, limit, limit).map((row) => row.id);
        assert.deepEqual(after, before, `limit ${limit} round ${round} of ${all}`);
        if (before.length > 1) nonEmpty += 1;
      }
      const hidden = Number(f.db.prepare('SELECT COUNT(*) AS n FROM memory_organization_hidden').get().n);
      const forgotten = Number(f.db.prepare('SELECT COUNT(*) AS n FROM memories WHERE is_forgotten = 1').get().n);
      if (hidden > 0 && forgotten > 0) hiddenForgottenMix += 1;
    }
    assert.ok(nonEmpty > 40 && hiddenForgottenMix > 5, `coverage ${nonEmpty} ${hiddenForgottenMix}`);
    // 検出力: 束縛の LIMIT を小さくした文は食い違う。
    const small = f.db.prepare(hits[0].sql).all(1, 1, 1).map((row) => row.id);
    assert.ok(f.db.prepare(OLD_FORGOTTEN).all(100).length > small.length, 'limit sensitivity');
  } finally { f.db.close(); }
});

test('visibility statements: the id bind becomes a memory_id SEARCH', async () => {
  const { f, log } = await forgottenFlow(false);
  try {
    const hit = find(log, expectedNewSql('forgotten-status', VIEWS[0]))[0];
    const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${hit.sql}`).all(...hit.values).map((row) => String(row.detail));
    assert.ok(plan.some((detail) => /^SEARCH c .*\(memory_id=\?\)/u.test(detail)), plan.join('\n'));
    const before = f.db.prepare(`EXPLAIN QUERY PLAN ${oldSql('forgotten-status', VIEWS[0])}`).all(...SITES['forgotten-status'].old({ id: 'x', mut: null, at: 'T', status: 'queued' })).map((row) => String(row.detail));
    assert.ok(!before.some((detail) => /^SEARCH c .*\(memory_id=\?\)/u.test(detail)), before.join('\n'));
  } finally { f.db.close(); }
});

// 整理のまとめの埋め込み（organizationAttemptFence 付き）。判定の ? は attempt の ? より前に束縛され、
// 新旧の文は同じ状態で同じ changes と行を返す。製品が発行した文を捕捉して比べる。
const ATTEMPT_FENCE = norm(` AND is_forgotten=0 AND EXISTS(
    SELECT 1 FROM memory_organization_embedding_attempts a JOIN memory_organization_summaries s ON s.memory_id=a.memory_id
    WHERE a.memory_id=memories.id AND a.source_revision=memories.topic_revision AND a.token=? AND s.status='active'
      AND a.expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now'))`);

test('visibility statements: organization attempt form binds the id before the token and matches the old statement', async () => {
  const f = fixture();
  try {
    f.env.AI_ENRICHMENT_MODE = 'off';
    const id = await addMemory(f, 'org-own');
    const control = await addMemory(f, 'org-cover');
    f.db.prepare(`INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-own', ?, 'r', 'active', 't')`).run(id);
    f.db.prepare(`UPDATE memories SET embedding_status = 'pending', vector_status = 'pending' WHERE id = ?`).run(id);
    f.env.AI_ENRICHMENT_MODE = 'on';
    f.env.MEMORY_VECTORS = { deleteByIds: async () => ({}), getByIds: async () => [], upsert: async () => ({ mutationId: 'mut-org' }) };
    const log = capture(f);
    const before = rowOf(f, id);
    await reconcile(f);
    const view = VIEWS[0];
    const newSql = norm(`${SITES['vector-queued'].tpl(vis(view, '?'))} ${ATTEMPT_FENCE}`);
    const oldText = norm(`${SITES['vector-queued'].tpl(vis(view, 'memories.id'))} ${ATTEMPT_FENCE}`);
    const hits = find(log, newSql);
    assert.equal(hits.length, 1, `product statement was issued\n${[...new Set(log.map((e) => e.sql))].filter((s) => s.startsWith('UPDATE memories SET vector_status')).join('\n')}`);
    const token = f.db.prepare('SELECT token FROM memory_organization_embedding_attempts WHERE memory_id = ?').get(id).token;
    const values = hits[0].values;
    assert.deepEqual(values, ['mut-org', values[1], id, before.updatedAt, before.rev, id, token], 'bind order: visibility id before the token');
    assert.match(values[1], ISO);
    // 各状態で新旧を比べる。prep は状態を作り、結果は巻き戻す。
    const states = {
      valid: () => f.db.prepare(`UPDATE memory_organization_embedding_attempts SET expires_at = '9999-01-01T00:00:00.000Z' WHERE memory_id = ?`).run(id),
      expired: () => {},
      forgotten: () => { states.valid(); f.db.prepare('UPDATE memories SET is_forgotten = 1 WHERE id = ?').run(id); },
      hidden: () => {
        states.valid();
        f.db.prepare(`INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-cover', ?, 'r', 'active', 't')`).run(control);
        f.db.prepare(`INSERT INTO memory_organization_coverage(memory_id, source_revision, status) VALUES (?, ?, 'active')`).run(id, before.rev);
        f.db.prepare('INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, ?)').run(id, 's-cover');
        // 原本は、被覆する要約の記憶が索引済みになると隠れる。
        f.db.prepare(`UPDATE memories SET embedding_json = '[1]', embedding_status = 'done', vector_status = 'indexed' WHERE id = ?`).run(control);
      },
    };
    const outcome = (sql, args, prep) => inRollback(f, () => {
      prep();
      const changes = Number(f.db.prepare(sql).run(...args).changes);
      return { changes, row: JSON.stringify(f.db.prepare('SELECT * FROM memories WHERE id = ?').get(id)) };
    });
    const oldArgs = (v) => [v[0], v[1], v[2], v[3], v[4], v[6]];
    const seen = {};
    for (const [name, prep] of Object.entries(states)) {
      const old = outcome(oldText, oldArgs(values), prep);
      const next = outcome(hits[0].sql, values, prep);
      assert.deepEqual(next, old, `state ${name}`);
      seen[name] = next.changes;
    }
    assert.deepEqual(seen, { valid: 1, expired: 0, forgotten: 0, hidden: 0 });
    // 検出力: 判定の束縛を別 id にした文は、hidden の状態で食い違う。
    const wrong = outcome(hits[0].sql, values.map((v, index) => index === 5 ? 'other-id' : v), states.hidden);
    assert.notEqual(wrong.changes, 0);
  } finally { f.db.close(); }
});
