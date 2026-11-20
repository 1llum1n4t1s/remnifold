// 検索 API の任意引数 sessionId（記録のメタデータ sessionId の完全一致）の検証。
// 他セッションの高関連度の記録が候補窓（索引一致・全文・Vectorize の上限）を埋めても、指定セッションの記録が返ること、
// sessionId の無い記録・型の違う値は返らないこと、不正な値は 400、引数なしは従来どおりであること。
// 追加的な変更のため、マイグレーションは増やさない（射影テーブル memory_search_projection の task_session を使う）。
import test, { assert } from './support/test.mjs';
import { fixture, migrations } from './worker-fixture.mjs';
import { buildMemoryIndex } from '../src/shared/memory-index.ts';

const sourceDate = '2026-10-01T00:00:00Z';
const OTHERS = 60; // MAX_LIMIT（50）より多くして、他セッションだけで候補窓を埋める。

async function seed(f, container, title, content, metadata) {
  const r = await f.api('/v3/documents', { containerTag: container, content,
    metadata: { title, type: 'conversation', ...metadata, memoryIndex: buildMemoryIndex({ title, request: title, sourceUpdatedAt: sourceDate }) } });
  assert.equal(r.status, 201, JSON.stringify(r.body));
  return r.body.id;
}

// 他セッションは索引（タイトル）に検索語を持つ高関連度、今のセッションは本文にだけ検索語を持つ低関連度にする。
async function corpus(f, word) {
  const others = [];
  for (let i = 0; i < OTHERS; i++) others.push(await seed(f, i % 2 ? 'space-a' : 'space-c', `${word} topic ${i}`, `# ${word} topic ${i}\n${word} detail`, { sessionId: `other-${i % 3}`, turn: i }));
  const mine = [];
  mine.push(await seed(f, 'space-b', 'current note one', `# note one\n${word} in the body only`, { sessionId: 'current', turn: 1 }));
  mine.push(await seed(f, 'space-a', 'current note two', `# note two\n${word} in the body only`, { sessionId: 'current', turn: 2 }));
  const noSession = await seed(f, 'space-b', `${word} orphan`, `# orphan\n${word} detail`, {});
  const numeric = await seed(f, 'space-b', `${word} numeric`, `# numeric\n${word} detail`, { sessionId: 42 });
  await f.drain();
  return { others, mine, noSession, numeric };
}

const ids = (response) => response.body.results.map((row) => row.id);

test('search sessionId keeps the current session records when other sessions fill the candidate window', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const { others, mine, noSession, numeric } = await corpus(f, 'alpha');
    for (const scope of [{ allContainers: true }, { containerTag: 'space-a' }, { containerTag: 'space-b' }])
      for (const indexOnly of [true, false]) {
        const body = { q: 'alpha', limit: 40, indexOnly, ...scope };
        const before = await f.api('/v4/search', body);
        assert.equal(before.status, 200, JSON.stringify(before.body));
        const filtered = await f.api('/v4/search', { ...body, sessionId: 'current' });
        assert.equal(filtered.status, 200, JSON.stringify(filtered.body));
        const expected = scope.containerTag === 'space-b' ? [mine[0]] : scope.containerTag === 'space-a' ? [mine[1]] : mine;
        assert.deepEqual(ids(filtered).sort(), [...expected].sort(), JSON.stringify({ scope, indexOnly }));
        for (const row of filtered.body.results) assert.equal(row.metadata.sessionId, 'current');
        assert.ok(!ids(filtered).includes(noSession) && !ids(filtered).includes(numeric));
        if (scope.allContainers && indexOnly) {
          // 引数なしでは他セッションが候補窓を埋め、今のセッションの記録は取りこぼされる（従来の挙動）。
          assert.ok(ids(before).every((id) => !mine.includes(id)), 'unfiltered window should be filled by other sessions');
          assert.ok(ids(before).every((id) => others.includes(id) || id === noSession || id === numeric));
        }
      }
    // 検索語なしの直近一覧でも同じ絞り込みになる。
    const recent = await f.api('/v4/search', { allContainers: true, q: '', limit: 40, sessionId: 'current' });
    assert.equal(recent.status, 200);
    assert.deepEqual(ids(recent).sort(), [...mine].sort());
    // 別の値・長さ上限ちょうどの値は一致なし・200。
    const none = await f.api('/v4/search', { allContainers: true, q: 'alpha', indexOnly: true, sessionId: 'nobody' });
    assert.deepEqual([none.status, none.body.results.length], [200, 0]);
    assert.equal((await f.api('/v4/search', { allContainers: true, q: 'alpha', indexOnly: true, sessionId: 'a'.repeat(160) })).status, 200);
    // 型の違う値（整数 42）は文字列 "42" と一致しない。
    const typed = await f.api('/v4/search', { allContainers: true, q: 'alpha', indexOnly: true, sessionId: '42' });
    assert.equal(typed.body.results.length, 0);
  } finally { await f.drain(); f.db.close(); }
});

test('search sessionId rejects invalid values and leaves requests without it unchanged', async () => {
  const f = fixture();
  try {
    await seed(f, 'space-a', 'alpha one', '# one\nalpha', { sessionId: 'one' });
    await seed(f, 'space-a', 'alpha two', '# two\nalpha', { sessionId: 'two' });
    await f.drain();
    for (const sessionId of [null, 1, true, '', '   ', 'a'.repeat(161), {}, ['one']]) {
      const response = await f.api('/v4/search', { allContainers: true, q: 'alpha', indexOnly: true, sessionId });
      assert.equal(response.status, 400, JSON.stringify(sessionId));
    }
    const plain = await f.api('/v4/search', { allContainers: true, q: 'alpha', indexOnly: true });
    assert.equal(plain.status, 200);
    assert.equal(plain.body.results.length, 2);
    assert.deepEqual(new Set(plain.body.results.map((row) => row.metadata.sessionId)), new Set(['one', 'two']));
  } finally { await f.drain(); f.db.close(); }
});

test('search sessionId reaches the semantic path when Vectorize topK is filled by other sessions', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    f.env.AI_ENRICHMENT_MODE = 'on';
    const { others, mine } = await corpus(f, 'omega');
    // 全行を Vectorize 登録済みとして、D1 の未登録行の補完に頼らず、topK が他セッションだけで埋まる状況にする。
    f.db.prepare("UPDATE memories SET vector_status = 'indexed' WHERE embedding_json IS NOT NULL").run();
    assert.ok(f.db.prepare('SELECT COUNT(*) AS n FROM memories WHERE embedding_json IS NOT NULL').get().n >= OTHERS);
    f.env.MEMORY_VECTORS.query = async () => ({ matches: others.slice(0, 50).map((id) => ({ id, score: 0.99, metadata: {} })), count: 50 });
    // 語彙の重ならない検索語で、全文・索引一致を使わず意味検索だけの結果にする。
    const body = { q: 'zzqq unrelated phrase', limit: 40, indexOnly: true, allContainers: true };
    const before = await f.api('/v4/search', body);
    assert.equal(before.status, 200, JSON.stringify(before.body));
    assert.ok(ids(before).every((id) => !mine.includes(id)), 'unfiltered semantic window is filled by other sessions');
    const filtered = await f.api('/v4/search', { ...body, sessionId: 'current' });
    assert.equal(filtered.status, 200, JSON.stringify(filtered.body));
    assert.deepEqual(ids(filtered).sort(), [...mine].sort());
  } finally { await f.drain(); f.db.close(); }
});

// 同じ sessionId を別クライアントが使っても混ざらないよう、sessionSource（metadata.sm_source）でも完全一致させる。
// sm_source を持たない旧記録は種別不明として、種別を指定した絞り込みでは返さない（fail closed）。
test('search sessionSource separates clients sharing a session id and treats legacy records as unknown', async () => {
  const f = fixture();
  try {
    const codex = await seed(f, 'space-a', 'sigma codex turn', '# turn\nsigma detail', { sessionId: 'shared', sm_source: 'codex', sm_client: 'codex-kagayoi-memory' });
    const claude = await seed(f, 'space-b', 'sigma claude turn', '# turn\nsigma detail', { sessionId: 'shared', sm_source: 'claude-code', sm_client: 'claude-code-remnifold' });
    const legacy = await seed(f, 'space-a', 'sigma legacy turn', '# turn\nsigma detail', { sessionId: 'shared' });
    const otherSession = await seed(f, 'space-a', 'sigma other turn', '# turn\nsigma detail', { sessionId: 'elsewhere', sm_source: 'codex' });
    await f.drain();
    for (const scope of [{ allContainers: true }, { containerTag: 'space-a' }])
      for (const indexOnly of [true, false]) {
        const search = (extra) => f.api('/v4/search', { q: 'sigma', limit: 20, indexOnly, ...scope, ...extra });
        const only = async (extra) => ids(await search(extra)).sort();
        const label = JSON.stringify({ scope, indexOnly });
        assert.deepEqual(await only({ sessionId: 'shared', sessionSource: 'codex' }), [codex], label);
        assert.deepEqual(await only({ sessionId: 'shared', sessionSource: 'claude-code' }), scope.containerTag ? [] : [claude], label);
        // 種別なしは sessionId の完全一致だけ（旧記録も含む）。
        assert.deepEqual(await only({ sessionId: 'shared' }), (scope.containerTag ? [codex, legacy] : [codex, claude, legacy]).sort(), label);
        assert.ok(!(await only({ sessionId: 'shared', sessionSource: 'codex' })).includes(otherSession));
      }
    // 検索結果の索引に種別が載る（旧サーバーの結果には無く、クライアントは種別なしの候補を sessionId だけで通す）。
    const compact = await f.api('/v4/search', { q: 'sigma', allContainers: true, indexOnly: true, sessionId: 'shared', sessionSource: 'codex' });
    assert.equal(compact.body.results[0].metadata.sm_source, 'codex');
    for (const body of [{ sessionSource: 'codex' }, { sessionId: 'shared', sessionSource: '' }, { sessionId: 'shared', sessionSource: ' ' },
      { sessionId: 'shared', sessionSource: 1 }, { sessionId: 'shared', sessionSource: 'a'.repeat(65) }, { sessionId: 'shared', sessionSource: null }]) {
      const response = await f.api('/v4/search', { q: 'sigma', allContainers: true, indexOnly: true, ...body });
      assert.equal(response.status, 400, JSON.stringify(body));
    }
  } finally { await f.drain(); f.db.close(); }
});

// 再開で ID が変わった同じ会話の ID 群（sessionIds）。どれかに一致すれば返し、他セッションが候補窓を埋めても落とさない。
test('search sessionIds matches any of the ids across every path and validates the array', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const { others, mine } = await corpus(f, 'delta');
    const resumed = await seed(f, 'space-b', 'resumed note', '# note\ndelta in the body only', { sessionId: 'resumed-old', sm_source: 'claude-code' });
    const current = await seed(f, 'space-b', 'current claude note', '# note\ndelta in the body only', { sessionId: 'current', sm_source: 'claude-code' });
    await f.drain();
    const expected = [...mine, resumed, current].sort();
    for (const scope of [{ allContainers: true }, { containerTag: 'space-b' }])
      for (const indexOnly of [true, false]) {
        const response = await f.api('/v4/search', { q: 'delta', limit: 40, indexOnly, ...scope, sessionIds: ['current', 'resumed-old', 'current'] });
        assert.equal(response.status, 200, JSON.stringify(response.body));
        const want = scope.containerTag ? [mine[0], resumed, current].sort() : expected;
        assert.deepEqual(ids(response).sort(), want, JSON.stringify({ scope, indexOnly }));
        assert.ok(ids(response).every((id) => !others.includes(id)));
      }
    // 種別との組み合わせ: sessionIds にも sessionSource がかかる（sm_source の無い mine は落ちる）。
    const typed = await f.api('/v4/search', { q: 'delta', allContainers: true, indexOnly: true, sessionIds: ['current', 'resumed-old'], sessionSource: 'claude-code' });
    assert.deepEqual(ids(typed).sort(), [resumed, current].sort());
    // 検索語なしの一覧と、意味検索（Vectorize の topK が他セッションで埋まる場合）。
    const recent = await f.api('/v4/search', { allContainers: true, q: '', limit: 40, sessionIds: ['resumed-old', 'nobody'] });
    assert.deepEqual(ids(recent), [resumed]);
    f.env.AI_ENRICHMENT_MODE = 'on';
    const semantic = await seed(f, 'space-b', 'resumed semantic', '# s\nplain', { sessionId: 'resumed-old' });
    await f.drain();
    f.db.prepare("UPDATE memories SET vector_status = 'indexed' WHERE embedding_json IS NOT NULL").run();
    f.env.MEMORY_VECTORS.query = async () => ({ matches: others.slice(0, 50).map((id) => ({ id, score: 0.99, metadata: {} })), count: 50 });
    const viaVectors = await f.api('/v4/search', { q: 'zzqq unrelated phrase', allContainers: true, indexOnly: true, limit: 40, sessionIds: ['resumed-old', 'nobody'] });
    assert.equal(viaVectors.status, 200);
    assert.ok(ids(viaVectors).includes(semantic), JSON.stringify(ids(viaVectors)));
    assert.ok(ids(viaVectors).every((id) => !others.includes(id)));
    // 検証: 空・9 件・非配列・不正な要素・sessionId との併用・sessionSource のみは 400。
    for (const body of [{ sessionIds: [] }, { sessionIds: Array.from({ length: 9 }, (_, i) => `s${i}`) }, { sessionIds: 'current' }, { sessionIds: [1] },
      { sessionIds: [''] }, { sessionIds: ['  '] }, { sessionIds: ['a'.repeat(161)] }, { sessionIds: [null] }, { sessionIds: ['a'], sessionId: 'a' }]) {
      const response = await f.api('/v4/search', { q: 'delta', allContainers: true, indexOnly: true, ...body });
      assert.equal(response.status, 400, JSON.stringify(body).slice(0, 80));
    }
    assert.equal((await f.api('/v4/search', { q: 'delta', allContainers: true, indexOnly: true, sessionIds: Array.from({ length: 8 }, (_, i) => `s${i}`) })).status, 200);
  } finally { await f.drain(); f.db.close(); }
});

// フォーク・再開で引き継いだ旧 ID の記録は、ID ごとの時刻の上限（sessionUntil）以下の元ターンだけが一致する。
// 親が上限より後に保存した記録（並行作業）は、候補窓を決める前に除かれ、今の ID の記録は時刻に関係なく返る。
test('search sessionUntil keeps only old-id records up to the bound before the candidate window', { timeout: 120_000 }, async () => {
  const f = fixture();
  try {
    const at = (title, content, sessionId, when) => f.api('/v3/documents', { containerTag: 'space-a', content, metadata: {
      title, type: 'conversation', sessionId, ...(when ? { sourceTimestamp: when } : {}),
      memoryIndex: buildMemoryIndex({ title, request: title, ...(when ? { sourceUpdatedAt: when } : {}) }) } }).then((r) => { assert.equal(r.status, 201); return r.body.id; });
    const cap = '2026-09-01T10:05:00Z';
    const before = await at('kappa plan before fork', '# a\nkappa', 'parent', '2026-09-01T10:00:00Z');
    const boundary = await at('kappa plan at the fork', '# a\nkappa', 'parent', cap);
    const untimed = await at('kappa plan untimed', '# a\nkappa', 'parent');
    const child = await at('kappa child later work', '# a\nkappa', 'child', '2030-01-01T00:00:00Z');
    const after = [];
    // 親が引き継ぎ後に保存した記録を候補窓（50）より多く作り、上限がなければ child と親の古い記録を押し出す状況にする。
    for (let i = 0; i < OTHERS; i++) after.push(await at(`kappa parallel work ${i}`, `# a\nkappa ${i}`, 'parent', `2026-09-02T10:00:${String(i).padStart(2, '0')}Z`));
    await f.drain();
    for (const scope of [{ allContainers: true }, { containerTag: 'space-a' }])
      for (const indexOnly of [true, false]) {
        const response = await f.api('/v4/search', { q: 'kappa', limit: 40, indexOnly, ...scope, sessionIds: ['child', 'parent'], sessionUntil: { parent: cap } });
        assert.equal(response.status, 200, JSON.stringify(response.body));
        assert.deepEqual(ids(response).sort(), [before, boundary, child].sort(), JSON.stringify({ scope, indexOnly }));
        assert.ok(!ids(response).includes(untimed) && after.every((id) => !ids(response).includes(id)));
      }
    // 上限なしの sessionIds は従来どおり親の全記録を返す（旧クライアントの挙動）。
    const unbounded = await f.api('/v4/search', { q: 'kappa', limit: 40, indexOnly: true, allContainers: true, sessionIds: ['child', 'parent'] });
    assert.ok(unbounded.body.results.length > 3);
    // 解釈できない上限の ID は何も一致しない（fail closed）。
    const bad = await f.api('/v4/search', { q: 'kappa', indexOnly: true, allContainers: true, sessionIds: ['parent'], sessionUntil: { parent: 'not a time' } });
    assert.deepEqual([bad.status, bad.body.results.length], [200, 0]);
    for (const body of [{ sessionUntil: { parent: cap } }, { sessionIds: ['child'], sessionUntil: { parent: cap } }, { sessionIds: ['parent'], sessionUntil: { parent: '' } },
      { sessionIds: ['parent'], sessionUntil: { parent: 1 } }, { sessionIds: ['parent'], sessionUntil: { parent: 'x'.repeat(65) } }, { sessionIds: ['parent'], sessionUntil: [cap] },
      { sessionIds: ['parent'], sessionUntil: null }]) {
      const response = await f.api('/v4/search', { q: 'kappa', allContainers: true, indexOnly: true, ...body });
      assert.equal(response.status, 400, JSON.stringify(body));
    }
  } finally { await f.drain(); f.db.close(); }
});

// migration 0015 の索引 idx_memory_search_projection_session: セッション絞り込みの文が射影の全走査ではなくこの索引を使うこと、
// 未適用のサーバー（0015 より前）でも同じ結果を返すこと（遅いだけ）を確かめる。実行計画は EXPLAIN QUERY PLAN で見る。
test('search session statements use the 0015 task_session index and return the same rows without it', { timeout: 120_000 }, async () => {
  const index = 'idx_memory_search_projection_session';
  const results = {};
  // 0015 より前の状態で記録を入れ、同じデータのまま 0015 を適用して前後を比べる（フィクスチャは同じ記憶域を共有するため1つだけ作る）。
  const f = fixture({ beforeMigration: '0015_' });
  try {
    {
      for (let i = 0; i < 30; i++) await seed(f, i % 2 ? 'space-a' : 'space-b', `theta other ${i}`, `# t\ntheta ${i}`, { sessionId: `other-${i % 5}`, sm_source: 'codex' });
      await seed(f, 'space-a', 'theta mine one', '# t\ntheta', { sessionId: 'mine', sm_source: 'codex' });
      await seed(f, 'space-b', 'theta mine two', '# t\ntheta body', { sessionId: 'mine', sm_source: 'codex' });
      // 2 文字の検索語は全文検索（trigram）の語にならず、本文の部分一致の文になる。
      await seed(f, 'space-b', 'plain note', '# t\nzzabcdefghijzz', { sessionId: 'mine', sm_source: 'codex' });
      await seed(f, 'space-b', 'theta old fork', '# t\ntheta', { sessionId: 'parent', sm_source: 'codex' });
      await f.drain();
    }
    for (const withIndex of [false, true]) {
      if (withIndex) f.db.exec(migrations.find((migration) => migration.name.startsWith('0015_')).sql);
      assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE name = ?").get(index).n, withIndex ? 1 : 0);
      const captured = [];
      f.DB.hooks.all = (sql, values) => { if (sql.includes('task_session')) captured.push({ sql, values }); };
      const bodies = [];
      for (const scope of [{ allContainers: true }, { containerTag: 'space-b' }])
        for (const indexOnly of [true, false]) {
          bodies.push({ q: 'theta', limit: 40, indexOnly, ...scope, sessionId: 'mine' });
          bodies.push({ q: 'theta', limit: 40, indexOnly, ...scope, sessionIds: ['mine', 'parent'], sessionUntil: { parent: '2999-01-01T00:00:00Z' }, sessionSource: 'codex' });
        }
      bodies.push({ q: '', limit: 40, allContainers: true, sessionId: 'mine' });
      bodies.push({ q: 'ab', limit: 40, indexOnly: true, allContainers: true, sessionId: 'mine' });
      bodies.push({ q: 'ab', limit: 40, indexOnly: true, containerTag: 'space-b', sessionIds: ['mine', 'parent'], sessionUntil: { parent: '2999-01-01T00:00:00Z' } });
      const found = [];
      for (const body of bodies) {
        const response = await f.api('/v4/search', body);
        assert.equal(response.status, 200, JSON.stringify(response.body));
        found.push(response.body.results.map((row) => row.title ?? row.metadata?.title).sort());
      }
      const plans = captured.map(({ sql, values }) => ({
        kind: sql.includes('WITH fts') ? 'fts' : sql.includes('instr(') ? 'instr' : sql.includes('ranked') ? 'indexed' : 'memories-subquery',
        plan: f.db.storage.sql.exec(`EXPLAIN QUERY PLAN ${sql}`, ...values).toArray().map((row) => row.detail),
      }));
      results[withIndex ? 'after' : 'before'] = { found, plans };
      for (const { kind, plan } of plans) {
        const uses = plan.some((detail) => detail.includes(index));
        assert.equal(uses, withIndex, `${withIndex ? 'after' : 'before'} 0015 ${kind}: ${plan.join(' | ')}`);
        // 索引を使う文は、射影の全走査（SCAN p / SCAN memory_search_projection）を含まない。
        if (withIndex) assert.ok(!plan.some((detail) => /\bSCAN (p|memory_search_projection)\b/.test(detail)), `${kind}: ${plan.join(' | ')}`);
      }
      assert.ok(new Set(plans.map((p) => p.kind)).size >= 4, JSON.stringify(plans.map((p) => p.kind)));
    }
  } finally { f.DB.hooks.all = undefined; await f.drain(); f.db.close(); }
  assert.deepEqual(results.after.found, results.before.found);
  assert.ok(results.after.found.some((titles) => titles.length >= 2));
  const summary = {};
  for (const phase of ['before', 'after']) for (const { kind, plan } of results[phase].plans) summary[`${phase} ${kind}`] ??= plan;
  console.log(JSON.stringify({ event: 'session_index_plans', summary }));
});
