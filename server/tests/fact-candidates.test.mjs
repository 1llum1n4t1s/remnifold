// replaceFacts の関連候補（同一 subject/predicate の既存 active fact 上位3件）の副問い合わせを確認する。
// 本番では旧文が idx_facts_container_status を選び、約4.9万行を読んでいた。
// 新文は idx_facts_identity を固定し、同点の順を rowid で決める。旧の実際の順（container_status 索引の走査順:
// updated_at 降順、同点は rowid 昇順）と同じ3件・同じ順になることを、固定した旧文と比べて確かめる。
// 新旧の文は server/src/index.ts の replaceFacts にある副問い合わせ・INSERT OR IGNORE と同じ形（変更時は合わせる）。
import test, { assert } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';

const projectExpr = `CASE
  WHEN json_type(previous_source.metadata_json, '$.sm_project_id') = 'text'
    THEN json_extract(previous_source.metadata_json, '$.sm_project_id')
  ELSE previous_source.container_tag
END`;
// 新文は元記録の (updated_at, id) が自分より古い候補か、同じ object の候補だけを残す。
const ordered = `AND (candidate.object_key = ? OR (previous_source.updated_at, previous_source.id) < (?, ?))`;
const subquery = (from, order, extra = '') => `SELECT candidate.id, candidate.object_key, candidate.confidence
  FROM facts AS candidate${from}
  JOIN memories AS previous_source ON previous_source.id = candidate.source_memory_id
  WHERE candidate.container_tag = ? AND candidate.subject_key = ? AND candidate.predicate_key = ?
    AND candidate.status = 'active' AND candidate.source_memory_id <> ?
    AND ${projectExpr} = ?
    ${extra}
  ${order}`;
const OLD = subquery('', 'ORDER BY candidate.updated_at DESC LIMIT 3');
// 旧文を本番の計画（container_status 索引）に固定したもの。同点の順の基準。
const OLD_PRODUCTION = subquery(' INDEXED BY idx_facts_container_status', 'ORDER BY candidate.updated_at DESC LIMIT 3');
const NEW = subquery(' INDEXED BY idx_facts_identity', 'ORDER BY candidate.updated_at DESC, candidate.rowid LIMIT 3', ordered);

const relationInsert = (inner) => `INSERT OR IGNORE INTO fact_relations(
    id, container_tag, from_fact_id, relation, to_fact_id, source_memory_id, confidence, created_at)
  SELECT lower(hex(randomblob(16))), ?, ?,
         CASE WHEN previous.object_key = ? THEN 'supports' ELSE 'supersedes' END,
         previous.id, ?, MIN(?, previous.confidence), ?
  FROM (${inner}) AS previous
  WHERE (previous.object_key = ? OR ? = 1)`;

const ids = (rows) => rows.map((row) => row.id);

// 記憶を保存し、指定の fact を rowid 順（配列順）に直接挿入する。
const CURRENT_AT = '2026-06-01T00:00:00.000Z';
async function seed(f, memories, facts, newer = []) {
  f.env.AI_ENRICHMENT_MODE = 'off';
  const memoryIds = {};
  for (const [name, projectId] of Object.entries(memories)) {
    const saved = await f.api('/v3/documents', { containerTag: 'space', customId: name, content: `source ${name}`,
      metadata: projectId ? { sm_project_id: projectId } : {} });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    memoryIds[name] = saved.body.id;
  }
  await f.drain();
  // 既定では、候補の元記録はすべて現在の記録より古い（旧文と同じ3件になる条件）。newer を指定した記録だけ新しくする。
  for (const [name, id] of Object.entries(memoryIds)) {
    f.db.prepare('UPDATE memories SET updated_at = ? WHERE id = ?').run(
      name === 'current' ? CURRENT_AT : newer.includes(name) ? '2027-01-01T00:00:00.000Z' : '2026-01-01T00:00:00.000Z', id);
  }
  const insert = f.db.prepare(`INSERT INTO facts(id, container_tag, source_memory_id, subject, predicate, object,
    subject_key, predicate_key, object_key, is_exclusive, confidence, status, created_at, updated_at)
    VALUES (?, 'space', ?, 's', 'p', ?, ?, 'p', ?, 0, ?, ?, ?, ?)`);
  for (const fact of facts) {
    insert.run(fact.id, memoryIds[fact.source], fact.object ?? fact.id, fact.subject ?? 's', fact.object ?? fact.id,
      fact.confidence ?? 0.8, fact.status ?? 'active', fact.at, fact.at);
  }
  return memoryIds;
}

const params = (current, project, subject = 's') => ['space', subject, 'p', current, project];
// 新文の末尾3つ: 現在の fact の object_key、現在の記録の updated_at、id。
const newParams = (args, object = 'target') => [...args, object, CURRENT_AT, args[3]];

function candidates(f, sql, args) { return ids(f.db.prepare(sql).all(...args)); }

function scenario(name, { memories, facts, project, subject, expected, newer, expectedNew = expected }) {
  test(`fact candidates: ${name}`, async () => {
    const f = fixture();
    try {
      const memoryIds = await seed(f, { current: project, ...memories }, facts, newer);
      const args = params(memoryIds.current, project ?? 'space', subject);
      const base = candidates(f, OLD_PRODUCTION, args);
      assert.deepEqual(base, expected, 'production plan order');
      assert.deepEqual(candidates(f, OLD, args), expected, 'old statement');
      assert.deepEqual(candidates(f, NEW, newParams(args)), expectedNew, 'new statement');
      // 挿入される関連も同じ（from は別にして比べる）。
      for (const exclusive of [0, 1]) {
        const rows = {};
        for (const [label, inner] of [['old', OLD_PRODUCTION], ['new', NEW]]) {
          const from = `from-${label}-${exclusive}`;
          f.db.prepare(`INSERT INTO facts(id, container_tag, source_memory_id, subject, predicate, object,
            subject_key, predicate_key, object_key, is_exclusive, confidence, status, created_at, updated_at)
            VALUES (?, 'space', ?, 's', 'p', ?, 's', 'p', ?, 0, 0.9, 'active', 'z', 'z')`).run(from, memoryIds.current, from, from);
          f.db.prepare(relationInsert(inner)).run('space', from, 'target', memoryIds.current, 0.7, 'now',
            ...(label === 'new' ? newParams(args) : args), 'target', exclusive);
          rows[label] = f.db.prepare(`SELECT relation, to_fact_id, confidence FROM fact_relations WHERE from_fact_id = ?`).all(from)
            .map((row) => `${row.relation}|${row.to_fact_id}|${row.confidence}`);
        }
        // 新しい側の候補を落とす scenario だけは、残った候補（同じ object は supports、他は排他のときだけ supersedes）に限る。
        if (expectedNew === expected) assert.deepEqual(rows.new, rows.old, `relations exclusive=${exclusive}`);
        else assert.deepEqual(rows.new.map((row) => row.split('|')[1]).sort(), expectedNew.filter((id) => exclusive || id === 'same').sort(), `relations exclusive=${exclusive}`);
      }
    } finally { await f.drain(); f.db.close(); }
  });
}

const day = (n) => `2026-01-${String(n).padStart(2, '0')}T00:00:00.000Z`;

scenario('four or more with distinct updated_at return the newest three', {
  memories: { a: null }, project: null,
  facts: [{ id: 'f1', source: 'a', at: day(1) }, { id: 'f2', source: 'a', at: day(4) }, { id: 'f3', source: 'a', at: day(2) },
    { id: 'f4', source: 'a', at: day(5) }, { id: 'f5', source: 'a', at: day(3) }],
  expected: ['f4', 'f2', 'f5'],
});
scenario('all updated_at ties return the first three by rowid', {
  memories: { a: null }, project: null,
  facts: ['f5', 'f1', 'f4', 'f2', 'f3'].map((id) => ({ id, source: 'a', at: day(1) })),
  expected: ['f5', 'f1', 'f4'],
});
scenario('a tie at the boundary is cut by rowid', {
  memories: { a: null }, project: null,
  facts: [{ id: 'old', source: 'a', at: day(1) }, { id: 'tieB', source: 'a', at: day(3) }, { id: 'top', source: 'a', at: day(9) },
    { id: 'tieA', source: 'a', at: day(3) }, { id: 'tieC', source: 'a', at: day(3) }],
  expected: ['top', 'tieB', 'tieA'],
});
scenario('project condition drops newer rows from other projects', {
  memories: { mine1: 'p1', mine2: 'p1', other: 'p2', plain: null }, project: 'p1',
  facts: [{ id: 'x-other', source: 'other', at: day(9) }, { id: 'x-plain', source: 'plain', at: day(8) },
    { id: 'm1', source: 'mine1', at: day(3) }, { id: 'm2', source: 'mine2', at: day(3) }, { id: 'm3', source: 'mine1', at: day(3) },
    { id: 'm4', source: 'mine2', at: day(2) }],
  expected: ['m1', 'm2', 'm3'],
});
scenario('no candidates return nothing', {
  memories: { a: null }, project: null,
  facts: [{ id: 'sup', source: 'a', status: 'superseded', at: day(5) }, { id: 'diff', source: 'a', subject: 'other', at: day(6) }],
  expected: [],
});
scenario('the current memory own facts are excluded', {
  memories: { a: null }, project: null,
  facts: [{ id: 'mine', source: 'current', at: day(9) }, { id: 'a1', source: 'a', at: day(1) }],
  expected: ['a1'],
});

// 元記録が自分より新しい候補は、同じ object のものだけが残る。上位3件の枠を新しい側で埋めない。
scenario('newer sources are dropped unless the object is the same', {
  memories: { old1: null, new1: null, new2: null }, project: null, newer: ['new1', 'new2'],
  facts: [{ id: 'n1', source: 'new1', at: day(9) }, { id: 'same', source: 'new2', object: 'target', at: day(8) },
    { id: 'n2', source: 'new1', at: day(7) }, { id: 'o1', source: 'old1', at: day(1) }],
  expected: ['n1', 'same', 'n2'], expectedNew: ['same', 'o1'],
});

// server/src/index.ts の replaceFacts にある逆向きの関連（自分より新しい排他的な事実が自分を置き換える）と同じ文。
const REVERSE = `INSERT OR IGNORE INTO fact_relations(
    id, container_tag, from_fact_id, relation, to_fact_id, source_memory_id, confidence, created_at)
  SELECT lower(hex(randomblob(16))), own.container_tag, newer.id, 'supersedes', own.id,
         newer.source_memory_id, MIN(own.confidence, newer.confidence), ?
  FROM facts AS own INDEXED BY idx_facts_source
  CROSS JOIN facts AS newer INDEXED BY idx_facts_identity
    ON newer.container_tag = own.container_tag AND newer.subject_key = own.subject_key
   AND newer.predicate_key = own.predicate_key AND newer.status = 'active'
  CROSS JOIN memories AS s ON s.id = newer.source_memory_id
  WHERE own.source_memory_id = ? AND newer.source_memory_id <> own.source_memory_id
    AND newer.is_exclusive = 1 AND newer.object_key <> own.object_key
    AND s.is_forgotten = 0 AND (s.updated_at, s.id) > (?, ?)
    AND ${projectExpr.replaceAll('previous_source', 's')} = ?
    AND EXISTS (SELECT 1 FROM memories WHERE id = ? AND topic_revision = ? AND is_forgotten = 0)`;
const SELF_SUPERSEDE = `UPDATE facts INDEXED BY idx_facts_source SET status = 'superseded'
  WHERE source_memory_id = ? AND status = 'active' AND EXISTS (
    SELECT 1 FROM fact_relations AS relation
    JOIN facts AS newer ON newer.id = relation.from_fact_id
    JOIN memories AS source ON source.id = newer.source_memory_id
    WHERE relation.to_fact_id = facts.id AND relation.relation = 'supersedes'
      AND newer.status = 'active' AND source.is_forgotten = 0)
  AND EXISTS (SELECT 1 FROM memories WHERE id = ? AND topic_revision = ? AND is_forgotten = 0)`;

test('fact candidates: reverse statements plan idx_facts_source and idx_facts_identity', async () => {
  const f = fixture();
  try {
    for (const [name, sql, args] of [['reverse', REVERSE, ['now', 'm', CURRENT_AT, 'm', 'space', 'm', 'r']],
      ['self supersede', SELF_SUPERSEDE, ['m', 'm', 'r']]]) {
      const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...args).map((row) => String(row.detail)).join('\n');
      assert.ok(plan.includes('idx_facts_source'), `${name}\n${plan}`);
      assert.ok(!plan.includes('idx_facts_container_status'), `${name}\n${plan}`);
      if (name === 'reverse') assert.ok(plan.includes('idx_facts_identity'), plan);
    }
  } finally { await f.drain(); f.db.close(); }
});

test('fact candidates: new statement plans idx_facts_identity and the index is plain over the lookup columns', async () => {
  const f = fixture();
  try {
    const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${NEW}`).all(...newParams(params('m', 'space'))).map((row) => String(row.detail)).join('\n');
    assert.ok(plan.includes('idx_facts_identity'), plan);
    assert.ok(!plan.includes('idx_facts_container_status'), plan);
    const index = f.db.prepare(`SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_facts_identity'`).get();
    assert.ok(!/\bWHERE\b/iu.test(index.sql), index.sql);
    const columns = f.db.prepare(`SELECT name FROM pragma_index_info('idx_facts_identity') ORDER BY seqno`).all().map((row) => row.name);
    assert.deepEqual(columns, ['container_tag', 'subject_key', 'predicate_key', 'status']);
  } finally { await f.drain(); f.db.close(); }
});
