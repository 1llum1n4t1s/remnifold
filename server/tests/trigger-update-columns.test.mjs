// migration 0016（memories の UPDATE トリガーを AFTER UPDATE OF <列> に作り直す）と 0017（速度用の索引）が、
// 結果を変えず、状態列だけの UPDATE から重いトリガー本文の展開を外すことを確かめる。
// - 同値: 0015 までの DB と 0017 までの DB に同じ操作列を流し、全表（memories・射影・facts・分類・整理の表）と全文検索の一致が
//   一致すること。recursive_triggers の ON/OFF の両方で比べる。UUID と時刻は出現順の記号に置き換えて比べる（値の違いだけを無視する）。
// - 展開: EXPLAIN で、状態列だけの UPDATE には Program 命令（トリガー本文）が無く、本文・射影の列の UPDATE にはあること。
// - 計画: 0017 の索引は reconcile の索引済み分岐と reuse 検索だけの実行計画を変え、ほかの文は 0015 と同じ計画のままであること。
// 時間は測らない。
import test, { assert } from './support/test.mjs';
import { fixture, migrations } from './worker-fixture.mjs';
import worker from '../src/index.ts';

const migration = (prefix) => migrations.find((item) => item.name.startsWith(prefix)).sql;
const identity = { sessionId: 'session-1', turn: 1, part: 1, parts: 1, sm_project_id: 'project' };

const variant = async (body, extra = {}) => {
  const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(body)))].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  const metadata = { ...identity, title: 'Decision', captureVersion: 2, ...extra };
  metadata.captureKey = `${metadata.sessionId}:${metadata.turn}:${hash}:1:${hash.slice(0, 16)}`;
  return { metadata, content: `# ${metadata.title}\n\n${body}` };
};
const payload = (batch) => ({ batchId: batch.batchId,
  summaries: [{ ref: 's', title: 'Decision', topic: 'Evidence', content: 'The complete verified decision follows from all important reasons.', sources: batch.sources.map((source) => source.id) }],
  coverage: batch.sources.map((source) => ({ id: source.id, revision: source.revision, status: 'complete', summaries: ['s'] })) });

// 表ごとの全行（rowid 順）と全文検索の一致を文字列にする。UUID と時刻は出現順の記号に置き換える。
// 32・64 桁の 16 進は UUID を含む入力から導出した ID・ハッシュ（fact_relations・整理の request_json など）。
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|\b[0-9a-f]{64}\b|\b[0-9a-f]{32}\b/g;
const TIME = /\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z/g;
function snapshot(f) {
  const tables = f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' AND name NOT LIKE 'memories_fts%' AND name <> 'memory_embedding_retries' ORDER BY name").all().map((row) => row.name);
  const dump = {};
  for (const name of tables) {
    let rows;
    try { rows = f.db.prepare(`SELECT rowid AS _rowid, * FROM "${name}" ORDER BY rowid`).all(); } catch { rows = f.db.prepare(`SELECT * FROM "${name}"`).all(); }
    dump[name] = rows;
  }
  for (const term of ['alpha', 'unique', 'edited', 'space']) dump[`fts:${term}`] = f.db.prepare('SELECT rowid FROM memories_fts WHERE memories_fts MATCH ? ORDER BY rowid').all(term);
  f.db.exec("INSERT INTO memories_fts(memories_fts, rank) VALUES('integrity-check', 1)");
  const ids = new Map();
  const symbol = (kind) => (value) => ids.has(value) ? ids.get(value) : (ids.set(value, `${kind}${ids.size}`), ids.get(value));
  // 射影の source_date は保存時刻のユリウス日（実行ごとに違う）。ほかの列と全文検索の一致は値のまま比べる。
  return JSON.stringify(dump).replace(UUID, symbol('U')).replace(TIME, 'T').replace(/"source_date":[\d.]+/g, '"source_date":"J"');
}

// fixture の close() は外部キーの都合で memories・facts・memory_organization_summaries を残し、ビューも消さない
// （各 case は専用の Durable Object なので通常は問題にならない）。同じ case で2つ目の fixture を作る前に、残りを全て消す。
function wipe(f) {
  f.db.close();
  for (const { name } of f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'view'").all()) f.db.exec(`DROP VIEW "${name}"`);
  for (const name of ['memory_organization_summaries', 'facts', 'memories']) f.db.exec(`DROP TABLE IF EXISTS "${name}"`);
  assert.deepEqual(f.db.prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all(), []);
}

// 同じ操作列。各段の後に snapshot を取り、どの段でずれたかを分かるようにする。
async function scenario(f) {
  const steps = [];
  const mark = (name) => steps.push([name, snapshot(f)]);
  const save = async (body) => { const response = await f.api('/v3/documents', body); assert.ok(response.status < 300, JSON.stringify(response.body)); await f.drain(); return response.body.id; };
  const row = (id) => f.db.prepare('SELECT * FROM memories WHERE id = ?').get(id);
  f.env.AI_ENRICHMENT_MODE = 'on';
  // 挿入（分類・事実・ベクトルの状態更新を含む）。
  const a = await save({ containerTag: 'memories', customId: 'plain-a', content: 'alpha unique body', metadata: { title: 'A', sm_project_id: 'project' } });
  const b = await save({ containerTag: 'memories', customId: 'plain-b', content: 'bravo unique body', metadata: { title: 'B', sm_project_id: 'project' } });
  const c = await save({ containerTag: 'space-b', customId: 'plain-c', content: 'charlie unique body', metadata: { title: 'C' } });
  mark('insert');
  // upsert: 本文の変更、metadata だけの変更、同一内容。
  await save({ containerTag: 'memories', customId: 'plain-a', content: 'alpha unique edited body', metadata: { title: 'A', sm_project_id: 'project' } });
  await save({ containerTag: 'memories', customId: 'plain-b', content: 'bravo unique body', metadata: { title: 'B2', sm_project_id: 'project', extra: 1 } });
  await save({ containerTag: 'space-b', customId: 'plain-c', content: 'charlie unique body', metadata: { title: 'C' } });
  mark('upsert');
  // 本文の PATCH。
  assert.equal((await f.api(`/v3/documents/${b}`, { content: 'bravo unique edited body' }, 'PATCH')).status, 200);
  await f.drain();
  mark('patch');
  // 状態列だけの更新（トリガーは何もしない）。
  f.db.prepare("UPDATE memories SET vector_status = 'indexed', vector_mutation_id = 'm1', vector_attempted_at = '2026-01-01T00:00:00.000Z' WHERE id = ?").run(a);
  f.db.prepare("UPDATE memories SET embedding_status = 'failed', enrichment_error = 'x', fact_status = 'failed' WHERE id = ?").run(b);
  mark('state');
  // topic_revision・updated_at・created_at・container_tag の変更。
  f.db.prepare("UPDATE memories SET topic_revision = 'revision-x' WHERE id = ?").run(c);
  f.db.prepare("UPDATE memories SET updated_at = '2026-02-02T00:00:00.000Z' WHERE id = ?").run(b);
  f.db.prepare("UPDATE memories SET created_at = '2026-01-02T00:00:00.000Z' WHERE id = ?").run(a);
  f.db.prepare("INSERT INTO container_tags(tag, name, created_at, updated_at) VALUES ('space-c', 'space-c', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')").run();
  f.db.prepare("UPDATE memories SET container_tag = 'space-c' WHERE id = ?").run(c);
  mark('columns');
  // 分類の差し替え。
  f.db.prepare('DELETE FROM memory_topics WHERE memory_id = ?').run(a);
  f.db.prepare("INSERT INTO memory_topics(memory_id, source_revision, topic, topic_key, created_at) VALUES (?, ?, 'Replaced', 'replaced', '2026-01-01T00:00:00.000Z')").run(a, row(a).topic_revision ?? '');
  f.db.prepare("UPDATE memory_topics SET topic = 'Renamed' WHERE memory_id = ?").run(a);
  mark('topics');
  // 忘却と復活（API の忘却、API の upsert による復活、直接の is_forgotten 切替）。
  assert.equal((await f.api('/v4/memories', { containerTag: 'memories', documentId: a }, 'DELETE')).status, 200);
  await f.drain();
  mark('forget');
  await save({ containerTag: 'memories', customId: 'plain-a', content: 'alpha unique edited body', metadata: { title: 'A', sm_project_id: 'project' } });
  mark('revive');
  f.db.prepare('UPDATE memories SET is_forgotten = 1 WHERE id = ?').run(b);
  f.db.prepare('UPDATE memories SET is_forgotten = 0 WHERE id = ?').run(b);
  mark('toggle');
  // capture 競合の発生と整理（要約・coverage・依存）。
  const first = await variant('First independent conclusion.');
  const second = await variant('Second unresolved contradictory conclusion.');
  const ids = [await save({ containerTag: 'memories', content: first.content, metadata: first.metadata }), await save({ containerTag: 'memories', content: second.content, metadata: second.metadata })];
  mark('conflict');
  const prepared = await f.api('/v4/organize/prepare', { sourceIds: ids });
  assert.equal(prepared.status, 200, JSON.stringify(prepared.body));
  assert.equal((await f.api('/v4/organize/commit', payload(prepared.body))).status, 200);
  await f.drain();
  mark('organize');
  // 整理済みの原本の編集（依存する要約・coverage・分類の無効化）と、競合している記録の編集・忘却。
  assert.equal((await f.api(`/v3/documents/${ids[0]}`, { content: `${first.content} edited` }, 'PATCH')).status, 200);
  await f.drain();
  mark('source-edit');
  assert.equal((await f.api('/v4/memories', { containerTag: 'memories', documentId: ids[1] }, 'DELETE')).status, 200);
  await f.drain();
  mark('conflict-forget');
  // hard delete。
  for (const id of [c, ids[0]]) assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'DELETE')).status, 204);
  await f.drain();
  mark('hard-delete');
  return steps;
}

// 状態列だけの UPDATE にはトリガーの Program 命令が無い。fixture は workerd の SQLite なので EXPLAIN をそのまま使える。
// 同じ文面の EXPLAIN をスキーマ変更の前後で使い回さないよう、呼び出しごとに別のコメントを付ける。
let explained = 0;
const hasProgram = (f, sql, ...values) => f.db.storage.sql.exec(`EXPLAIN /* ${++explained} */ ${sql}`, ...values).toArray().some((instruction) => instruction.opcode === 'Program');

for (const recursive of [false, true]) {
  test(`0016/0017 keep every table and full-text match identical with recursive_triggers ${recursive ? 'ON' : 'OFF'}`, { timeout: 120_000 }, async () => {
    const runs = {};
    for (const [state, options] of [['0015', { beforeMigration: '0016_' }], ['0017', {}]]) {
      const f = fixture(options);
      try {
        f.db.exec(`PRAGMA recursive_triggers = ${recursive ? 'ON' : 'OFF'}`);
        if (state === '0017') {
          const names = f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name IN ('idx_memories_custom_id', 'idx_memories_vector_sweep') ORDER BY name").all().map((row) => row.name);
          assert.deepEqual(names, ['idx_memories_custom_id', 'idx_memories_vector_sweep']);
        }
        runs[state] = await scenario(f);
      } finally { await f.drain(); wipe(f); }
    }
    assert.deepEqual(runs['0017'].map(([name]) => name), runs['0015'].map(([name]) => name));
    runs['0017'].forEach(([name, state], index) => {
      if (state !== runs['0015'][index][1]) {
        const before = runs['0015'][index][1], after = state;
        let at = 0;
        while (before[at] === after[at]) at++;
        assert.ok(false, `step ${name} differs at ${at}: before ${before.slice(Math.max(0, at - 160), at + 160)} / after ${after.slice(Math.max(0, at - 160), at + 160)}`);
      }
    });
    // 操作列が表を実際に変えていること（空の比較で通らない）。
    const finalState = JSON.parse(runs['0017'].at(-1)[1]);
    assert.ok(Object.values(finalState).flat().length > 20 && finalState.memory_organization_summaries.length > 0 && finalState.facts.length > 0);
  });
}

test('0016 triggers are column-limited with unchanged bodies, keep their relative order, and skip expansion for state-only updates', { timeout: 60_000 }, async () => {
  const names = ['memories_after_update', 'memory_organization_source_update', 'memory_organization_capture_conflict_update', 'memory_search_projection_memory_update'];
  const columns = {
    memories_after_update: ['content', 'container_tag'],
    memory_organization_source_update: ['content', 'metadata_json', 'container_tag', 'topic_revision', 'is_forgotten', 'status'],
    memory_organization_capture_conflict_update: ['content', 'metadata_json', 'container_tag', 'is_forgotten'],
    memory_search_projection_memory_update: ['metadata_json', 'topic_revision', 'is_forgotten', 'container_tag', 'created_at', 'updated_at', 'id'],
  };
  const f = fixture({ beforeMigration: '0016_' });
  try {
    const read = () => Object.fromEntries(f.db.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'memories' ORDER BY rowid").all().map((row) => [row.name, row.sql]));
    const before = read();
    const order = (triggers) => f.db.prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'memories' ORDER BY rowid").all().map((row) => row.name).filter((name) => names.includes(name));
    const orderBefore = order();
    const state = [["UPDATE memories SET vector_status = 'queued', vector_mutation_id = ?, vector_attempted_at = ? WHERE id = ?", 'm', 't', 'x'],
      ["UPDATE memories SET embedding_status = 'processing', vector_status = 'pending', vector_mutation_id = NULL, vector_attempted_at = NULL WHERE id = ?", 'x'],
      ["UPDATE memories SET embedding_json = ?, enrichment_error = ?, embedded_at = ?, fact_status = 'done', topic_status = 'done' WHERE id = ?", 'e', null, 't', 'x']];
    const bodies = [["UPDATE memories SET content = ? WHERE id = ?", 'c', 'x'], ["UPDATE memories SET metadata_json = ?, updated_at = ? WHERE id = ?", '{}', 't', 'x'],
      ["UPDATE memories SET is_forgotten = 1, status = 'superseded', vector_status = 'pending' WHERE id = ?", 'x'], ["UPDATE memories SET topic_revision = ? WHERE id = ?", 'r', 'x'],
      ["UPDATE memories SET container_tag = ? WHERE id = ?", 'space', 'x']];
    for (const [sql, ...values] of state) assert.ok(hasProgram(f, sql, ...values), `before 0016: ${sql}`);
    f.db.exec(migration('0016_'));
    const after = read();
    assert.deepEqual(order(), orderBefore, 'relative order of the four triggers is preserved');
    for (const name of names) {
      const stripped = after[name].replace(/AFTER UPDATE OF [a-z_, ]+ ON memories/, 'AFTER UPDATE ON memories');
      assert.equal(stripped, before[name], `${name} body and WHEN are unchanged`);
      const found = /AFTER UPDATE OF ([a-z_, ]+) ON memories/.exec(after[name])?.[1].split(', ');
      assert.deepEqual(found, columns[name]);
      // WHEN が比較する列は OF の列に含まれる（OLD.<列> を全て拾う。rowid は OF に書けないので射影の制約として別に扱う）。
      const when = after[name].slice(0, after[name].indexOf('\nBEGIN'));
      const compared = [...when.matchAll(/(?:OLD|NEW|old|new)\.([a-z_]+)\s+IS\s+NOT\s+(?:NEW|new)\./g)].map((match) => match[1]).filter((column) => column !== 'rowid');
      assert.deepEqual([...new Set(compared)].sort(), [...columns[name]].sort());
    }
    for (const [sql, ...values] of state) assert.ok(!hasProgram(f, sql, ...values), `after 0016 (no trigger expansion): ${sql}`);
    for (const [sql, ...values] of bodies) assert.ok(hasProgram(f, sql, ...values), `after 0016 (trigger expanded): ${sql}`);
  } finally { await f.drain(); f.db.close(); }
});

// 実行計画。Worker が実際に出した文を捕まえ、0015 の状態と 0017 の後で同じ文の計画を比べる。
test('0017 indexes change only the reconcile indexed branch and the reuse lookup plans', { timeout: 120_000 }, async () => {
  const f = fixture({ beforeMigration: '0016_' });
  try {
    f.env.AI_ENRICHMENT_MODE = 'on';
    f.env.MEMORY_VECTORS.describe = async () => ({ vectorCount: 0 });
    const captured = [];
    const capture = (sql, values) => captured.push({ sql, values });
    Object.assign(f.DB.hooks, { all: capture, first: capture });
    const metadata = { captureVersion: 2, captureKey: 'reuse-key', sm_project_id: 'project', sessionId: 'session-1', turn: 1, part: 1, parts: 1 };
    const body = { containerTag: 'memories', customId: 'codex-turn-v2:reuse-key', content: 'alpha unique body', metadata };
    for (let i = 0; i < 6; i++) {
      assert.equal((await f.api('/v3/documents', { containerTag: i % 2 ? 'space-a' : 'memories', customId: `doc-${i}`, content: `alpha unique ${i}`, metadata: { title: `T${i}` } })).status, 201);
    }
    assert.equal((await f.api('/v3/documents', body)).status, 201);
    await f.drain();
    f.db.prepare("UPDATE memories SET vector_status = 'indexed' WHERE custom_id = 'doc-1'").run();
    captured.length = 0;
    // 意味検索（Vectorize が空を返す経路と、失敗して全体の fallback になる経路）、container 一覧、再 upsert（container_tag+custom_id）、reuse、件数。
    f.env.MEMORY_VECTORS.query = async () => ({ matches: [], count: 0 });
    assert.equal((await f.api('/v4/search', { q: 'alpha unique', allContainers: true, limit: 5 })).status, 200);
    f.env.MEMORY_VECTORS.query = async () => { throw new Error('vector-unavailable'); };
    assert.equal((await f.api('/v4/search', { q: 'alpha unique', containerTag: 'memories', limit: 5 })).status, 200);
    assert.equal((await f.api('/v3/documents', { containerTag: 'memories', customId: 'doc-0', content: 'alpha unique 0 again', metadata: { title: 'again' } })).status, 201);
    assert.equal((await f.api('/v3/documents', { ...body, reuseExistingCapture: true })).status, 201);
    assert.equal((await f.api('/v4/vector-status', undefined, 'GET')).status, 200);
    worker.scheduled({ cron: '*/5 * * * *' }, f.env, f.ctx);
    await f.drain();
    const kinds = {
      semanticFallback: (sql) => /embedding_json IS NOT NULL\s+AND vector_status IN/.test(sql),
      semanticFullFallback: (sql) => /embedding_json IS NOT NULL\s+ORDER BY updated_at DESC LIMIT/.test(sql),
      containerCustom: (sql) => /m\.container_tag=\? AND m\.custom_id=\?/.test(sql),
      count: (sql) => /GROUP BY vector_status/.test(sql),
      containers: (sql) => /SELECT DISTINCT container_tag/.test(sql),
      reuse: (sql) => /WHERE custom_id = \?\s+AND is_forgotten = 0/.test(sql),
      reconcile: (sql) => /vector_status <> 'indexed'/.test(sql) && /UNION ALL/.test(sql),
    };
    const statements = {};
    for (const [kind, matches] of Object.entries(kinds)) {
      const found = captured.filter(({ sql }) => matches(sql));
      assert.ok(found.length > 0, `statement captured: ${kind}`);
      if (kind === 'reconcile') {
        const parts = found[0].sql.split(/\n\s*UNION ALL\n/);
        assert.equal(parts.length, 2, 'reconcile has exactly two branches');
        statements.reconcileOther = { sql: parts[0], values: [] };
        statements.reconcileIndexed = { sql: parts[1], values: [] };
      } else statements[kind] = found[0];
    }
    const plan = ({ sql, values }) => f.db.storage.sql.exec(`EXPLAIN QUERY PLAN /* ${++explained} */ ${sql}`, ...values).toArray().map((row) => row.detail);
    const before = Object.fromEntries(Object.entries(statements).map(([kind, statement]) => [kind, plan(statement)]));
    f.db.exec(migration('0016_'));
    f.db.exec(migration('0017_'));
    const after = Object.fromEntries(Object.entries(statements).map(([kind, statement]) => [kind, plan(statement)]));
    const changed = Object.keys(statements).filter((kind) => JSON.stringify(before[kind]) !== JSON.stringify(after[kind])).sort();
    assert.deepEqual(changed, ['reconcileIndexed', 'reuse'], JSON.stringify({ before, after }));
    assert.ok(after.reconcileIndexed.some((detail) => detail.includes('idx_memories_vector_sweep')), JSON.stringify(after.reconcileIndexed));
    assert.ok(after.reuse.some((detail) => detail.includes('idx_memories_custom_id')), JSON.stringify(after.reuse));
    assert.ok(!before.reuse.some((detail) => detail.includes('idx_memories_custom_id')));
    console.log(JSON.stringify({ event: 'trigger_update_columns_plans', before, after }));
  } finally { await f.drain(); f.db.close(); }
});
