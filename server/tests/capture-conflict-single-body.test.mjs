// migration 0018（競合トリガー2本の本文を1つの INSTEAD OF トリガーへ集約し、本文内のビュー参照を平らに書き直す）が、
// 結果を変えず、memories への INSERT / upsert / 忘却の準備費だけを減らすことを確かめる。
// - 構造: 集約した本文は、R・S を元のビュー参照へ戻すと 0016 の conflict_update の本文と一字違わず一致する。WHEN・OF・
//   射影トリガー2本は文面が不変で、INSERT・UPDATE それぞれの発火順（作成順）が不変。D1 の1文の上限（100KB）に収まる。
// - 同値: 0017 までの DB と 0018 までの DB に、直接の SQL で作った競合・要約・coverage・checkpoint・group などの操作列を流し、
//   全表と全文検索が一致すること（エラー文も一致）。recursive_triggers の OFF/ON の両方で比べる。
// - 検出力: 本文7文と条件を1つずつ落とした版（メモリ上で作る。コミット物は変えない）が、同じ操作列の比較で必ず検出されること。
// - 準備費: 同一 DB の前後で EXPLAIN の命令数を比べる（upsert 1/4 以下、忘却 1/2 以下）。時間は測らない。
// - 深さ: 依存する要約の鎖がある競合が workerd で成功すること。
import test, { assert } from './support/test.mjs';
import { fixture, migrations } from './worker-fixture.mjs';

const migration = (prefix) => migrations.find((item) => item.name.startsWith(prefix)).sql;

// 本文の置換規則（0018 の先頭コメントと同じ）。逆置換で 0016 の本文に戻ることを確かめる。
const SIB = 'SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id';
const SUM = 'SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id';
const P = `SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))`;
const R = `${P}
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0`;
const S = `SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(${R})
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(${P})`;

const triggerBlock = (sql, name) => {
  const start = sql.lastIndexOf(`CREATE TRIGGER ${name}`);
  assert.ok(start >= 0, `${name} is created`);
  return sql.slice(start, sql.indexOf('\nEND;', start) + 5);
};
const head = (block) => block.slice(0, block.indexOf('\nBEGIN\n'));
const body = (block) => block.slice(block.indexOf('\nBEGIN\n') + 7, block.length - '\nEND;'.length);
const INVALIDATE = 'memory_organization_capture_invalidate';

// ---- 構造 ----

test('0018 single body is the 0016 conflict body with only the two view references replaced', { timeout: 60_000 }, async () => {
  const sql = migration('0018_');
  const original = body(triggerBlock(migration('0016_'), 'memory_organization_capture_conflict_update'));
  const merged = body(triggerBlock(sql, INVALIDATE));
  // 置換は S（R を含む）を先に戻し、次に R を戻す。
  assert.equal(merged.split(S).length - 1, 5, 'S replaces the five conflict_summaries references');
  assert.equal(merged.split(R).length - 1, 3 + 5, 'R replaces the three siblings references and the one inside each S');
  const restored = merged.split(S).join(SUM).split(R).join(SIB);
  assert.equal(restored, original);
  assert.ok(!restored.includes('NEW.memory_id'), 'no other reference to the invalidation row remains');
  // 本文は1か所だけ。競合トリガー2本は識別を渡す INSERT 1文で、本文を持たない。
  for (const name of ['memory_organization_capture_conflict_insert', 'memory_organization_capture_conflict_update']) {
    const block = triggerBlock(sql, name);
    assert.equal(body(block).trim().split(';').filter((part) => part.trim()).length, 1, `${name} has a single statement`);
    assert.ok(/INSERT OR ABORT INTO memory_organization_capture_invalidations\(/.test(body(block)), `${name} keeps OR ABORT`);
    assert.ok(!/capture_siblings|capture_conflict_summaries/.test(body(block)));
  }
  // 競合トリガー2本は BEGIN…END が一字一致する（WHEN と OF だけが違う）。
  assert.equal(body(triggerBlock(sql, 'memory_organization_capture_conflict_insert')), body(triggerBlock(sql, 'memory_organization_capture_conflict_update')));
  assert.ok(sql.indexOf('INSTEAD OF INSERT ON memory_organization_capture_invalidations') < sql.indexOf('CREATE TRIGGER memory_organization_capture_conflict_insert'));
});

test('0018 keeps the WHEN, OF, projection triggers and relative firing order, adds only the view and the INSTEAD OF trigger', { timeout: 60_000 }, async () => {
  const f = fixture({ beforeMigration: '0018_' });
  try {
    const read = () => f.db.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY rowid").all();
    const event = (sql) => /(?:AFTER|BEFORE|INSTEAD OF) (INSERT|UPDATE|DELETE)/.exec(sql)[1];
    const orders = (rows) => Object.fromEntries(['INSERT', 'UPDATE', 'DELETE'].map((kind) => [kind,
      rows.filter((row) => row.type === 'trigger' && row.tbl_name === 'memories' && event(row.sql) === kind).map((row) => row.name)]));
    const before = read();
    f.db.exec(migration('0018_'));
    const after = read();
    const key = (row) => `${row.type}:${row.name}`;
    const byName = (rows) => new Map(rows.map((row) => [key(row), row.sql]));
    const beforeMap = byName(before), afterMap = byName(after);
    const added = [...afterMap.keys()].filter((name) => !beforeMap.has(name)).sort();
    assert.deepEqual(added, [`trigger:${INVALIDATE}`, 'view:memory_organization_capture_invalidations']);
    assert.deepEqual([...beforeMap.keys()].filter((name) => !afterMap.has(name)), []);
    // 作り直した4本以外は文面も不変。
    const recreated = new Set(['memory_organization_capture_conflict_insert', 'memory_organization_capture_conflict_update', 'memory_search_projection_memory_insert', 'memory_search_projection_memory_update']);
    for (const [name, sql] of beforeMap) {
      if (!recreated.has(name.replace(/^trigger:/, ''))) assert.equal(afterMap.get(name), sql, `${name} unchanged`);
    }
    // 射影2本は一字も変えない。競合2本は WHEN（と OF）を含む見出しが同じで、本文だけが集約される。
    for (const name of ['memory_search_projection_memory_insert', 'memory_search_projection_memory_update']) assert.equal(afterMap.get(`trigger:${name}`), beforeMap.get(`trigger:${name}`), name);
    for (const name of ['memory_organization_capture_conflict_insert', 'memory_organization_capture_conflict_update']) {
      assert.equal(head(afterMap.get(`trigger:${name}`)), head(beforeMap.get(`trigger:${name}`)), `${name} WHEN/OF unchanged`);
      assert.notEqual(afterMap.get(`trigger:${name}`), beforeMap.get(`trigger:${name}`));
    }
    assert.deepEqual(orders(after), orders(before), 'relative firing order per event is preserved');
    // 同じ種類の中で、競合 → 射影の順（発火は逆順）に並ぶ。
    assert.deepEqual(orders(after).INSERT.slice(-2), ['memory_organization_capture_conflict_insert', 'memory_search_projection_memory_insert']);
    assert.deepEqual(orders(after).UPDATE.slice(-2), ['memory_organization_capture_conflict_update', 'memory_search_projection_memory_update']);
    // D1 の1文の上限（100KB）。インポートは1つの SQL として送られる。
    assert.ok(Math.max(...after.map((row) => row.sql?.length ?? 0)) < 100 * 1024);
    assert.ok(new TextEncoder().encode(migration('0018_')).length < 100 * 1024);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 同値 ----

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|\b[0-9a-f]{64}\b|\b[0-9a-f]{32}\b/g;
const TIME = /\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z/g;
function snapshot(f) {
  const tables = f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' AND name NOT LIKE 'memories_fts%' ORDER BY name").all().map((row) => row.name);
  const dump = {};
  for (const name of tables) {
    let rows;
    try { rows = f.db.prepare(`SELECT rowid AS _rowid, * FROM "${name}" ORDER BY rowid`).all(); } catch { rows = f.db.prepare(`SELECT * FROM "${name}"`).all(); }
    dump[name] = rows;
  }
  for (const term of ['body', 'edited']) dump[`fts:${term}`] = f.db.prepare('SELECT rowid FROM memories_fts WHERE memories_fts MATCH ? ORDER BY rowid').all(term);
  f.db.exec("INSERT INTO memories_fts(memories_fts, rank) VALUES('integrity-check', 1)");
  return JSON.stringify(dump).replace(UUID, 'U').replace(TIME, 'T').replace(/"source_date":[\d.]+/g, '"source_date":"J"');
}

// fixture の close() は外部キーの都合で一部の表とビューを残す。同じ case で2つ目の fixture を作る前に、残りを全て消す。
function wipe(f) {
  f.db.close();
  for (const { name } of f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'view'").all()) f.db.exec(`DROP VIEW "${name}"`);
  for (const name of ['memory_organization_summaries', 'facts', 'memories']) f.db.exec(`DROP TABLE IF EXISTS "${name}"`);
  assert.deepEqual(f.db.prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all(), []);
}

const T0 = '2026-01-01T00:00:00.000Z';
const hash = (c) => c.repeat(64);
const v2key = (session, turn, c) => `${session}:${turn}:${hash(c)}:1:${hash(c).slice(0, 16)}`;
const meta = (session, o = {}) => JSON.stringify({ sessionId: session, turn: o.turn ?? 1, part: o.part ?? 1,
  ...(o.project === null ? {} : { sm_project_id: o.project ?? 'proj' }), ...(o.key ? { captureKey: o.key } : {}),
  ...(o.v2 ? { captureVersion: 2, parts: 1 } : {}), ...(o.extra ?? {}) });

// 直接の SQL で、競合・要約・coverage・checkpoint・group・progress を作る。各操作のエラー文は状態と一緒に比べる。
async function scenario(f) {
  const steps = [];
  let errors = [];
  const ex = (sql, ...values) => { try { f.db.prepare(sql).run(...values); } catch (error) { errors.push(String(error).replace(/^Error: /, '')); } };
  const step = (name, run) => { run(); steps.push([name, `${snapshot(f)}|${JSON.stringify(errors)}`]); errors = []; };
  const tag = (value) => ex('INSERT OR IGNORE INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)', value, value, T0, T0);
  const mem = (id, metadata, o = {}) => {
    tag(o.tag ?? 'memories');
    ex(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, topic_revision, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`, id, o.custom ?? id, o.tag ?? 'memories', o.content ?? `body ${id}`, metadata,
    o.forgotten ? 'superseded' : 'done', o.forgotten ? 1 : 0, o.topicRevision ?? null, T0, T0);
  };
  const summary = (id, memoryId, deps, status = 'active') => {
    ex('INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES (?, ?, ?, ?, ?)', id, memoryId, 'r', status, T0);
    for (const dep of deps) ex('INSERT INTO memory_organization_dependencies(summary_id, memory_id, source_revision) VALUES (?, ?, ?)', id, dep, T0);
  };
  const derived = (memoryId) => {
    ex(`INSERT INTO facts(id, container_tag, source_memory_id, subject, predicate, object, subject_key, predicate_key, object_key, created_at, updated_at)
        VALUES (?, 'memories', ?, 's', 'p', 'o', 's', 'p', 'o', ?, ?)`, `fact-${memoryId}`, memoryId, T0, T0);
    ex("INSERT INTO memory_topics(memory_id, source_revision, topic, topic_key, created_at) VALUES (?, 'r', 't', 't', ?)", memoryId, T0);
  };
  const coverage = (memoryId, o = {}) => {
    ex('INSERT INTO memory_organization_coverage(memory_id, source_revision, status, capture_verified_revision) VALUES (?, ?, ?, ?)', memoryId, o.revision ?? T0, o.status ?? 'active', 'verified' in o ? o.verified : T0);
    for (const id of o.summaries ?? []) ex('INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, ?)', memoryId, id);
  };
  const progress = (memoryId, reason = null) => ex("INSERT INTO memory_organization_progress(memory_id, source_revision, next_offset, memo, next_attempt, defer_reason) VALUES (?, ?, 0, '', ?, ?)", memoryId, T0, T0, reason);
  const group = (id, batch, session, members, summaries) => {
    ex("INSERT INTO memory_organization_groups(id, prefix, session_id, turn, expected_parts, status, batch_id) VALUES (?, ?, ?, 1, 2, 'ready', ?)", id, id, session, batch);
    members.forEach((member, index) => ex('INSERT INTO memory_organization_group_members(group_id, part, memory_id, source_revision) VALUES (?, ?, ?, ?)', id, index + 1, member, T0));
    for (const summaryId of summaries) ex('INSERT INTO memory_organization_group_summaries(group_id, summary_id) VALUES (?, ?)', id, summaryId);
  };
  const checkpoint = (p, summaryId, source) => {
    ex("INSERT OR IGNORE INTO memory_consolidation_projects(project_key, source_container_tag, created_at, updated_at) VALUES ('proj', 'memories', ?, ?)", T0, T0);
    mem(`${p}-cm`, '{"sm_consolidation":true}');
    ex("INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at) VALUES (?, 'proj', ?, ?, 'superseded', ?, ?)", `${p}-c`, `${p}-cm`, 1, T0, T0);
    ex('INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision) VALUES (?, ?, ?)', `${p}-c`, source, T0);
    ex('INSERT INTO memory_organization_checkpoints(summary_id, consolidation_id) VALUES (?, ?)', summaryId, `${p}-c`);
  };
  // 競合の相手 E（v2 の鍵）と、それに結び付く要約・事実・分類・coverage・progress・group（通常と reset:）を作る。
  const seed = (p, o = {}) => {
    const session = `${p}-session`;
    mem(`${p}-e`, meta(session, { key: o.eKey ?? v2key(session, 1, 'a'), v2: o.eV2 ?? true, project: o.project }), { topicRevision: o.eTopicRevision, tag: o.tag });
    mem(`${p}-sm`, '{"sm_consolidation":true}');
    summary(`${p}-s`, `${p}-sm`, [`${p}-e`]);
    derived(`${p}-sm`);
    coverage(`${p}-e`, { summaries: o.link === false ? [] : [`${p}-s`], verified: 'verified' in o ? o.verified : T0 });
    progress(`${p}-e`, o.defer ?? null);
    group(`${p}-g1`, 'batch', session, [`${p}-e`], [`${p}-s`]);
    group(`${p}-g2`, 'reset:x', session, [`${p}-e`], [`${p}-s`]);
    return session;
  };
  const conflict = (p, session, o = {}) => mem(`${p}-n`, meta(session, { key: o.key, v2: o.v2, project: o.project }), { tag: o.tag });

  step('plain', () => { const session = seed('p01'); mem('p01-lone', meta('other-session', { key: v2key('other-session', 1, 'c'), v2: true })); assert.ok(session); });
  step('v2 pair keeps the peer', () => { const session = seed('p02'); conflict('p02', session, { key: v2key(session, 1, 'b'), v2: true }); });
  step('v2 pair invalidates only checkpoint summaries', () => {
    const session = seed('p03');
    mem('p03-sm2', '{"sm_consolidation":true}'); summary('p03-s2', 'p03-sm2', []); derived('p03-sm2');
    checkpoint('p03', 'p03-s2', 'p03-e');
    conflict('p03', session, { key: v2key(session, 1, 'b'), v2: true });
  });
  step('strict peer invalidates everything', () => { const session = seed('p04'); conflict('p04', session, { key: 'legacy-key' }); });
  step('null capture key', () => { const session = seed('p05'); conflict('p05', session, {}); });
  step('unsafe update', () => {
    seed('p06');
    for (const [id, turn] of [['p06-u1', 2 ** 53], ['p06-u2', 1e300]]) {
      mem(id, meta('p06-unsafe', { turn, key: 'k' })); mem(`${id}-sm`, '{"sm_consolidation":true}');
      summary(`${id}-s`, `${id}-sm`, [id]); derived(`${id}-sm`); coverage(id, { summaries: [`${id}-s`] }); progress(id);
      group(`${id}-g`, 'batch', 'p06-unsafe', [id], [`${id}-s`]);
      ex("UPDATE memories SET content = 'edited body' WHERE id = ?", id);
    }
  });
  step('update joins an identity', () => {
    const session = seed('p07');
    mem('p07-n', meta('p07-away', { key: 'k' })); mem('p07-nsm', '{"sm_consolidation":true}');
    summary('p07-ns', 'p07-nsm', ['p07-n']); derived('p07-nsm'); coverage('p07-n', { summaries: ['p07-ns'] }); progress('p07-n');
    group('p07-ng', 'batch', 'p07-away', ['p07-n'], ['p07-ns']);
    ex('UPDATE memories SET metadata_json = ? WHERE id = ?', meta(session, { key: 'k2' }), 'p07-n');
  });
  step('stale coverage makes a v2 peer a sibling', () => {
    const session = seed('p08', { verified: null });
    conflict('p08', session, { key: v2key(session, 1, 'b'), v2: true });
  });
  step('coverage older than the original revision', () => {
    const session = seed('p09', { eTopicRevision: 'r1' });
    conflict('p09', session, { key: v2key(session, 1, 'b'), v2: true });
  });
  step('capture-hash-unverifiable progress is kept', () => { const session = seed('p10', { defer: 'capture-hash-unverifiable' }); conflict('p10', session, { key: 'legacy-key' }); });
  step('the changed row is itself a summary and is forgotten mid-body', () => {
    const session = seed('p11');
    mem('p11-n', '{}'); mem('p11-nm', '{"sm_consolidation":true}');
    summary('p11-ns', 'p11-n', ['p11-e']); derived('p11-n');
    ex('UPDATE memories SET metadata_json = ? WHERE id = ?', meta(session, { key: 'legacy-key' }), 'p11-n');
  });
  step('the only peer is a summary and disappears mid-body', () => {
    const session = seed('p12');
    summary('p12-se', 'p12-e', ['p12-e']); derived('p12-e');
    conflict('p12', session, { key: 'legacy-key' });
  });
  step('forgotten summary keeps facts and topics', () => {
    const session = seed('p13');
    mem('p13-fm', '{"sm_consolidation":true}', { forgotten: true });
    summary('p13-fs', 'p13-fm', ['p13-e']); derived('p13-fm');
    conflict('p13', session, { key: 'legacy-key' });
  });
  step('revival and forgetting', () => {
    const session = seed('p14');
    mem('p14-n', meta(session, { key: 'legacy-key' }), { forgotten: true });
    ex('UPDATE memories SET is_forgotten = 0, status = ? WHERE id = ?', 'done', 'p14-n');
    ex("UPDATE memories SET is_forgotten = 1, status = 'superseded' WHERE id = ?", 'p14-n');
    ex('UPDATE memories SET is_forgotten = 0 WHERE id = ?', 'p14-n');
  });
  step('container change enters a project key', () => {
    const session = seed('p15', { project: null, tag: 'space-a' });
    mem('p15-n', meta(session, { key: 'legacy-key', project: null }), { tag: 'space-b' });
    ex("UPDATE memories SET container_tag = 'space-a' WHERE id = ?", 'p15-n');
  });
  step('versions rows are not identities', () => {
    const session = seed('p16');
    ex("INSERT INTO memory_organization_versions(memory_id, summary_id) VALUES ('p16-e', 'p16-s')");
    conflict('p16', session, { key: 'legacy-key' });
  });
  step('v2 update joins a v2 peer', () => {
    const session = seed('p24');
    mem('p24-n', meta('p24-away', { key: v2key('p24-away', 1, 'd'), v2: true }));
    ex('UPDATE memories SET metadata_json = ? WHERE id = ?', meta(session, { key: v2key(session, 1, 'b'), v2: true }), 'p24-n');
  });
  step('summary chain', () => {
    const session = seed('p17');
    mem('p17-sm2', '{"sm_consolidation":true}'); summary('p17-s2', 'p17-sm2', ['p17-sm']); derived('p17-sm2');
    conflict('p17', session, { key: 'legacy-key' });
  });
  step('v2 changed row against a legacy peer', () => {
    const session = seed('p22', { eKey: 'legacy-key', eV2: false });
    conflict('p22', session, { key: v2key(session, 1, 'b'), v2: true });
  });
  step('coverage without a summary link', () => { const session = seed('p23', { link: false }); conflict('p23', session, { key: 'legacy-key' }); });
  step('malformed metadata', () => { mem('p18-bad', 'not json'); });
  step('delete of a conflicting row', () => { const session = seed('p19'); conflict('p19', session, { key: 'legacy-key' }); ex('DELETE FROM memories WHERE id = ?', 'p19-n'); });
  step('multi-row update and insert', () => {
    const session = seed('p20');
    tag('memories');
    ex(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, created_at, updated_at)
        SELECT 'p20-a', 'p20-a', 'memories', 'body a', ?, 'done', 0, ?, ? UNION ALL SELECT 'p20-b', 'p20-b', 'memories', 'body b', ?, 'done', 0, ?, ?`,
    meta(session, { key: 'ka' }), T0, T0, meta(session, { key: 'kb' }), T0, T0);
    ex("UPDATE memories SET content = content || ' edited' WHERE id IN ('p20-e', 'p20-a', 'p20-b')");
  });
  step('upsert shape', () => {
    const session = seed('p21');
    mem('p21-n', meta('p21-away', { key: 'k' }));
    ex(`INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, created_at, updated_at)
        VALUES ('p21-x', 'p21-n', 'memories', 'edited body', ?, 'done', 0, ?, ?)
        ON CONFLICT(container_tag, custom_id) DO UPDATE SET content = excluded.content, metadata_json = excluded.metadata_json, is_forgotten = 0`,
    meta(session, { key: 'legacy-key' }), T0, T0);
  });
  return steps;
}

const runScenario = async (recursive, sql) => {
  const f = fixture({ beforeMigration: '0018_' });
  try {
    f.db.exec(`PRAGMA recursive_triggers = ${recursive ? 'ON' : 'OFF'}`);
    if (sql) f.db.exec(sql);
    return await scenario(f);
  } finally { await f.drain(); wipe(f); }
};
const firstDifference = (before, after) => {
  let at = 0;
  while (before[at] === after[at]) at++;
  return `at ${at}: before ${before.slice(Math.max(0, at - 160), at + 160)} / after ${after.slice(Math.max(0, at - 160), at + 160)}`;
};
const baselines = new Map();
const baseline = async (recursive) => { if (!baselines.has(recursive)) baselines.set(recursive, await runScenario(recursive)); return baselines.get(recursive); };

for (const recursive of [false, true]) {
  test(`0017 and 0018 keep every table, full-text match and error identical with recursive_triggers ${recursive ? 'ON' : 'OFF'}`, { timeout: 180_000 }, async () => {
    const before = await baseline(recursive);
    const after = await runScenario(recursive, migration('0018_'));
    assert.deepEqual(after.map(([name]) => name), before.map(([name]) => name));
    after.forEach(([name, state], index) => {
      if (state !== before[index][1]) assert.ok(false, `step ${name} differs ${firstDifference(before[index][1], state)}`);
    });
    // 操作列が表を実際に変えていること。要約・coverage・progress・group の無効化と忘却、事実・分類の削除が起きている。
    const last = JSON.parse(after.at(-1)[1].split('|')[0]);
    const count = (table, predicate = () => true) => last[table].filter(predicate).length;
    assert.ok(count('memory_organization_summaries', (row) => row.status === 'invalid') >= 8);
    assert.ok(count('memory_organization_summaries', (row) => row.status === 'active') >= 3);
    assert.ok(count('memory_organization_coverage', (row) => row.status === 'invalid') >= 5);
    assert.ok(count('memories', (row) => row.is_forgotten === 1) >= 8);
    assert.ok(count('memory_organization_groups', (row) => row.status === 'invalid') >= 5 && count('memory_organization_groups', (row) => row.batch_id?.startsWith('reset:') && row.status === 'ready') >= 5);
    assert.ok(count('memory_organization_progress') >= 8 && count('memory_organization_progress', (row) => row.defer_reason === 'capture-hash-unverifiable') === 1);
    assert.ok(count('facts') > 0 && count('memory_topics') > 0);
    const states = (name) => JSON.parse(after.find(([step]) => step === name)[1].split('|')[0]);
    // 同じ識別の2本目が v2 の別の鍵なら競合にならず、要約は残る。checkpoint の要約だけが無効になる。
    const pair = states('v2 pair invalidates only checkpoint summaries').memory_organization_summaries.filter((row) => row.id.startsWith('p03'));
    assert.deepEqual(pair.map((row) => [row.id, row.status]).sort(), [['p03-s', 'active'], ['p03-s2', 'invalid']]);
    // 忘却済み要約に残った facts は、競合で削除される（本文の DELETE FROM facts の欠落を検出する事例）。
    assert.deepEqual(states('forgotten summary keeps facts and topics').facts.filter((row) => row.source_memory_id === 'p13-fm'), []);
    assert.equal(states('plain').facts.filter((row) => row.source_memory_id === 'p13-fm').length, 0);
  });
}

// 検出力: 本文7文と条件を1つずつ落とした版は、同じ比較で必ず差が出る。
const mutants = () => {
  const sql = migration('0018_');
  const block = triggerBlock(sql, INVALIDATE);
  const statements = body(block).split(/;\n(?=  [A-Z])/).map((part, index, parts) => index < parts.length - 1 ? `${part};\n` : part);
  assert.equal(statements.length, 7);
  const replace = (text, from, to) => {
    assert.ok(text.includes(from), `mutation target exists: ${from.slice(0, 60)}`);
    return text.split(from).join(to);
  };
  const result = statements.map((statement, index) => [`drop statement ${index + 1}`, sql.replace(block, block.replace(statement, ''))]);
  const mutateBody = (name, edit) => { const next = block.replace(body(block), edit(body(block))); assert.notEqual(next, block); result.push([name, sql.replace(block, next)]); };
  const live = 'AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0';
  mutateBody('condition: self alive', (text) => replace(text, live, 'AND 1=1'));
  mutateBody('condition: other peer', (text) => replace(text, 'OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p', 'OR 0 AND EXISTS(SELECT 1 FROM memory_organization_capture_identities p'));
  mutateBody('condition: coverage re-evaluation', (text) => replace(text, 'OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage', 'OR 0 AND EXISTS(SELECT 1 FROM memory_organization_coverage coverage'));
  mutateBody('condition: v2 variant of the changed row', (text) => replace(text, 'b.id=NEW.memory_id OR NEW.single_part_v2=0', 'b.id=NEW.memory_id OR 0'));
  mutateBody('condition: v2 variant of the peer', (text) => replace(text, 'AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))', 'AND 1=1)'));
  mutateBody('condition: unsafe', (text) => replace(text, 'WHERE NEW.unsafe=1', 'WHERE 0'));
  mutateBody('condition: checkpoint', (text) => replace(text, `UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(${P})`, ''));
  // 受け渡し側: 競合トリガーが INSTEAD OF の本文へ渡す single_part_v2 を 0 に固定した版（insert・update を別々に）。
  const handoff = 'EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=a.id),0';
  for (const name of ['memory_organization_capture_conflict_insert', 'memory_organization_capture_conflict_update']) {
    const trigger = triggerBlock(sql, name);
    result.push([`handoff: ${name} single_part_v2 fixed to 0`, sql.replace(trigger, replace(trigger, handoff, '0,0'))]);
  }
  return result;
};

test('0018 mutations of each body statement and condition are detected by the equivalence scenario', { timeout: 600_000 }, async () => {
  const before = await baseline(false);
  const undetected = [];
  for (const [name, sql] of mutants()) {
    // 変異版は適用でき（exec が投げない）、全 step を完走したうえで、状態に差がある場合だけ検出とする。
    const after = await runScenario(false, sql);
    assert.deepEqual(after.map(([step]) => step), before.map(([step]) => step), `${name} runs every step`);
    const differs = after.some(([, state], index) => state !== before[index][1]);
    if (!differs) undetected.push(name);
  }
  assert.deepEqual(undetected, []);
});

// ---- 準備費 ----

let explained = 0;
const instructions = (f, sql) => f.db.storage.sql.exec(`EXPLAIN /* ${++explained} */ ${sql}`).toArray().length;
const UPSERT = `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, entity_context, status, is_forgotten,
    embedding_status, fact_status, vector_status, topic_status, topic_revision, created_at, updated_at)
  VALUES ('x', 'x', 'memories', 'c', '{}', NULL, 'done', 0, 'pending', 'pending', 'pending', 'pending', 'r', 't', 't')
  ON CONFLICT(container_tag, custom_id) DO UPDATE SET content = excluded.content, metadata_json = excluded.metadata_json,
    entity_context = excluded.entity_context, status = 'done', is_forgotten = 0, embedding_status = excluded.embedding_status,
    fact_status = excluded.fact_status, vector_status = excluded.vector_status, topic_status = excluded.topic_status,
    topic_revision = excluded.topic_revision, embedding_model = NULL, fact_model = NULL, embedded_at = NULL,
    facts_extracted_at = NULL, embedding_json = NULL, vector_mutation_id = NULL, vector_attempted_at = NULL, topic_model = NULL,
    topics_extracted_at = NULL, enrichment_error = NULL, updated_at = excluded.updated_at
  WHERE NOT EXISTS (SELECT 1 FROM memory_consolidations WHERE memory_id = memories.id)
    AND NOT EXISTS (SELECT 1 FROM memory_organization_versions WHERE memory_id = memories.id)`;
const INSERT = `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, created_at, updated_at)
  VALUES ('x', 'x', 'memories', 'c', '{}', 'done', 0, 't', 't')`;
const FORGET = `UPDATE memories SET status = 'superseded', is_forgotten = 1, vector_status = 'pending', vector_mutation_id = NULL,
    vector_attempted_at = NULL, updated_at = 't' WHERE id IN ('x')`;

test('0018 cuts the prepare cost of upsert and forgetting on the same database', { timeout: 60_000 }, async () => {
  const f = fixture({ beforeMigration: '0018_' });
  try {
    const before = { upsert: instructions(f, UPSERT), insert: instructions(f, INSERT), forget: instructions(f, FORGET) };
    f.db.exec(migration('0018_'));
    const after = { upsert: instructions(f, UPSERT), insert: instructions(f, INSERT), forget: instructions(f, FORGET) };
    assert.ok(before.upsert > 20_000 && before.forget > 10_000, `the comparison measures real trigger expansion: ${JSON.stringify(before)}`);
    assert.ok(after.upsert * 4 <= before.upsert, JSON.stringify({ before, after }));
    assert.ok(after.forget * 2 <= before.forget, JSON.stringify({ before, after }));
    assert.ok(after.insert < before.insert, JSON.stringify({ before, after }));
    console.log(JSON.stringify({ event: 'capture_conflict_single_body_instructions', before, after }));
  } finally { await f.drain(); f.db.close(); }
});

// ---- 深さ ----

for (const recursive of [false, true]) {
  test(`0018 conflict succeeds through a dependent summary chain with recursive_triggers ${recursive ? 'ON' : 'OFF'}`, { timeout: 60_000 }, async () => {
    const f = fixture();
    try {
      f.db.exec(`PRAGMA recursive_triggers = ${recursive ? 'ON' : 'OFF'}`);
      const run = (sql, ...values) => f.db.prepare(sql).run(...values);
      const insert = (id, metadata) => run("INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten, created_at, updated_at) VALUES (?, ?, 'memories', ?, ?, 'done', 0, ?, ?)", id, id, `body ${id}`, metadata, T0, T0);
      run('INSERT INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)', 'memories', 'memories', T0, T0);
      insert('e', meta('chain', { key: 'legacy-a' }));
      for (const id of ['m1', 'm2']) insert(id, '{"sm_consolidation":true}');
      run("INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s1', 'm1', 'r', 'active', ?), ('s2', 'm2', 'r', 'active', ?)", T0, T0);
      run("INSERT INTO memory_organization_dependencies(summary_id, memory_id, source_revision) VALUES ('s1', 'e', 'r'), ('s2', 'm1', 'r')");
      insert('n', meta('chain', { key: 'legacy-b' }));
      const states = f.db.prepare('SELECT id, is_forgotten FROM memories ORDER BY id').all().map((row) => `${row.id}:${row.is_forgotten}`);
      assert.deepEqual(states, ['e:0', 'm1:1', 'm2:1', 'n:0']);
      assert.deepEqual(f.db.prepare('SELECT id, status FROM memory_organization_summaries ORDER BY id').all().map((row) => `${row.id}:${row.status}`), ['s1:invalid', 's2:invalid']);
    } finally { await f.drain(); f.db.close(); }
  });
}
