// 版付きの経路（replaceFacts・addMemory・PATCH・retryEnrichment・忘却・ハード削除）の復帰は、
// 消える自分の排他的な事実と同じ (container, subject_key, predicate_key) の組だけを見直す。
// 全件版（restoreUnsupportedFacts）との差分、連鎖、乱択、例外注入、版不一致、実行計画をAPI経由で確かめる。
import test, { assert } from "./support/test.mjs";
import { fixture, output, embedding } from "./worker-fixture.mjs";

function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const pick = (random, values) => values[Math.floor(random() * values.length)];
const at = (ms) => `2026-05-01T00:00:00.${String(ms).padStart(3, "0")}Z`;

// 本文の目印から返す事実を決める AI。plan は { facts: [{ subject, predicate, object, exclusive }] }。
function setup(f) {
  const plans = new Map();
  f.env.AI = { run: async (_model, input) => {
    if (input.text) return { data: [embedding] };
    const text = JSON.stringify(input);
    for (const [token, plan] of plans) {
      if (text.includes(token)) {
        return output({ facts: plan.facts.map((fact) => ({ confidence: 1, ...fact })), topics: ["T"] });
      }
    }
    return output({ facts: [], topics: ["T"] });
  } };
  return plans;
}
const fact = (object, exclusive = true, subject = "S", predicate = "P") => ({ subject, predicate, object, exclusive });

async function save(f, plans, container, name, facts, options = {}) {
  const token = `rec_${container}_${name}_end`;
  f.env.AI_ENRICHMENT_MODE = "off";
  const saved = await f.api("/v3/documents", { containerTag: container, customId: name, content: token,
    metadata: options.project ? { sm_project_id: options.project } : {} });
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  f.env.AI_ENRICHMENT_MODE = "on";
  if (options.ms !== undefined) f.db.prepare("UPDATE memories SET updated_at = ? WHERE id = ?").run(at(options.ms), saved.body.id);
  plans.set(token, { facts });
  await f.drain();
  return { id: saved.body.id, token, name, container };
}
async function enrich(f, record) {
  assert.equal((await f.api("/v4/enrich", { id: record.id })).status, 202);
  await f.drain();
}
const states = (f, container) => f.db.prepare(
  "SELECT m.custom_id AS name, x.object, x.status FROM facts AS x JOIN memories AS m ON m.id = x.source_memory_id WHERE x.container_tag = ? ORDER BY m.custom_id, x.object",
).all(container).map((row) => `${row.name}:${row.object}:${row.status}`);

// 生きた記録の supersedes 関係を1つも持たない superseded。
const unsupported = (f) => f.db.prepare(
  `SELECT x.id, x.container_tag AS container, x.object FROM facts AS x
   WHERE x.status = 'superseded' AND NOT EXISTS (
     SELECT 1 FROM fact_relations AS r JOIN facts AS n ON n.id = r.from_fact_id
     JOIN memories AS m ON m.id = n.source_memory_id
     WHERE r.to_fact_id = x.id AND r.relation = 'supersedes' AND m.is_forgotten = 0)`,
).all();

// ---- 1. a<-b<-c の連鎖: 無関係な記録の保存・抽出で a は戻らない ----
test("fact restore: an unrelated record does not revive the oldest fact of a chain a<-b<-c", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const records = [];
    for (const [name, ms] of [["a", 0], ["b", 1], ["c", 2]]) records.push(await save(f, plans, "chain", name, [fact(name)], { ms }));
    for (const record of records) await enrich(f, record);
    const chain = ["a:a:superseded", "b:b:superseded", "c:c:active"];
    assert.deepEqual(states(f, "chain"), chain);
    const chainOnly = () => states(f, "chain").filter((s) => /^[abc]:/u.test(s));
    const unrelated = await save(f, plans, "chain", "u", [fact("u", true, "U", "Q")]);
    assert.deepEqual(chainOnly(), chain, "after saving an unrelated record");
    await enrich(f, unrelated);
    assert.deepEqual(chainOnly(), chain, "after extracting an unrelated record");
    // 同じ組の非排他、subject だけ・predicate だけが同じ排他の記録。PATCH（消える事実を含む）しても a は戻らない。
    const near = [
      await save(f, plans, "chain", "n", [fact("n", false)], { ms: 5 }),
      await save(f, plans, "chain", "q", [fact("q", true, "S", "Q")], { ms: 6 }),
      await save(f, plans, "chain", "p", [fact("p", true, "U", "P")], { ms: 7 }),
      unrelated,
    ];
    for (const record of near.slice(0, 3)) await enrich(f, record);
    assert.deepEqual(chainOnly(), chain, "after extracting the near records");
    for (const record of near) {
      const edited = await f.api(`/v3/documents/${record.id}`, { content: `${record.token} edited` }, "PATCH");
      assert.equal(edited.status, 200, JSON.stringify(edited.body));
      await f.drain();
      assert.deepEqual(chainOnly(), chain, `after editing ${record.name}`);
    }
    assert.equal(states(f, "chain").filter((s) => !/^[abc]:/u.test(s)).length, 4);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 2. 旧の全件SQLとの差分 ----
const FULL = `UPDATE facts SET status = 'active', updated_at = ?
  WHERE container_tag = ? AND status = 'superseded'
    AND NOT EXISTS (
      SELECT 1 FROM fact_relations AS relation
      JOIN facts AS newer ON newer.id = relation.from_fact_id
      JOIN memories AS source ON source.id = newer.source_memory_id
      WHERE relation.to_fact_id = facts.id AND relation.relation = 'supersedes'
        AND newer.status = 'active' AND source.is_forgotten = 0)`;
const isScoped = (op) => op.sql.includes("UPDATE facts INDEXED BY idx_facts_identity");
const snapshot = (f) => f.db.prepare(
  "SELECT id, container_tag AS container, source_memory_id AS source, subject_key AS s, predicate_key AS p, is_exclusive AS ex, status FROM facts",
).all();
const kindOf = (sql) => sql.includes("custom_id") ? "custom" : sql.includes("is_forgotten = 1") ? "forget"
  : sql.includes("topic_revision") ? "id" : "hard";

// batch を、版付きの復帰を含めて（withScoped）または含めず旧の全件SQLを DELETE の後に足して（旧の順）実行し、
// 事実の状態を返す。トランザクションは必ず巻き戻す。
function simulate(f, ops, withScoped) {
  const scoped = ops.find(isScoped);
  const hard = ops.at(-1).sql.startsWith("DELETE FROM memories");
  const cut = hard ? ops.length : ops.findIndex((op, i) => i > ops.indexOf(scoped) && op.sql.startsWith("DELETE FROM facts")) + 1;
  let result;
  try {
    f.db.transaction(() => {
      for (const op of ops.slice(0, cut)) if (withScoped || op !== scoped) op.execute();
      if (!withScoped) f.db.prepare(FULL).run(scoped.values[0], scoped.values[1]);
      result = snapshot(f);
      throw new Error("rollback");
    });
  } catch (error) { if (error.message !== "rollback") throw error; }
  return result;
}
function observeBatches(f) {
  const observed = [];
  f.DB.hooks.batch = (ops) => {
    const scoped = ops.find(isScoped);
    if (!scoped) return;
    observed.push({ kind: kindOf(scoped.sql), before: snapshot(f), withScoped: simulate(f, ops, true), full: simulate(f, ops, false) });
  };
  return observed;
}
// 戻った行の差を、新・旧の全件版・消えた排他的事実の組で分ける。
function compareRestore({ before, withScoped, full }) {
  const was = new Map(before.map((row) => [row.id, row.status]));
  const after = new Map(withScoped.map((row) => [row.id, row.status]));
  const oldAfter = new Map(full.map((row) => [row.id, row.status]));
  const pairs = new Set(before.filter((row) => !after.has(row.id) && row.ex === 1).map((row) => `${row.container}|${row.s}|${row.p}`));
  const restored = (map) => new Set(before.filter((row) => map.get(row.id) === "active" && was.get(row.id) === "superseded").map((row) => row.id));
  const byId = new Map(before.map((row) => [row.id, row]));
  const inPair = (id) => pairs.has(`${byId.get(id).container}|${byId.get(id).s}|${byId.get(id).p}`);
  const next = restored(after), old = restored(oldAfter);
  return {
    newOnly: [...next].filter((id) => !old.has(id)),
    oldOnlyInPair: [...old].filter((id) => !next.has(id) && inPair(id)),
    oldOnlyOutside: [...old].filter((id) => !next.has(id) && !inPair(id)),
    both: [...next].filter((id) => old.has(id)),
  };
}

// 前提: 種に統合（checkpoint）の記憶を入れない（hardDelete の派生記憶の事実削除は、後続の全件見直しの対象で、この比較に含めない）。
function seedRandom(f, plans, random, container) {
  const memories = [];
  return (async () => {
    for (let i = 0; i < 6; i++) {
      memories.push(await save(f, plans, container, `m${i}`, [fact(pick(random, ["a", "b", "c"]), random() < 0.7)],
        { project: pick(random, [null, "p1"]) }));
    }
    const insert = f.db.prepare(`INSERT OR IGNORE INTO facts(id, container_tag, source_memory_id, subject, predicate, object,
      subject_key, predicate_key, object_key, is_exclusive, confidence, status, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)`);
    const ids = [];
    for (const memory of memories) {
      for (let n = Math.floor(random() * 4); n > 0; n--) {
        const [s, p, o] = [pick(random, ["s1", "s2"]), pick(random, ["p1", "p2"]), pick(random, ["a", "b", "c", "d"])];
        const id = `${container}-${ids.length}`;
        const inserted = insert.run(id, container, memory.id, s, p, o, s, p, o, random() < 0.6 ? 1 : 0,
          random() < 0.35 ? "superseded" : "active", at(1), at(1));
        if (inserted.changes === 1) ids.push({ id, source: memory.id });
      }
    }
    const relate = f.db.prepare(`INSERT OR IGNORE INTO fact_relations(id, container_tag, from_fact_id, relation, to_fact_id, source_memory_id, confidence, created_at)
      VALUES (?, ?, ?, ?, ?, ?, 0.5, ?)`);
    for (let n = 0; n < 10 && ids.length > 1; n++) {
      const from = pick(random, ids), to = pick(random, ids);
      if (from.source !== to.source) relate.run(`${container}-r${n}`, container, from.id, random() < 0.7 ? "supersedes" : "supports", to.id, from.source, at(1));
    }
    if (random() < 0.5) f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(memories[5].id);
    return memories;
  })();
}

test("fact restore: the scoped statement only differs from the old full statement outside the vanishing pairs", { timeout: 600000 }, async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const random = prng(20261126);
    const kinds = { id: 0, custom: 0, forget: 0, hard: 0 };
    const totals = { both: 0, oldOnlyOutside: 0, oldOnlyInPair: 0, newOnly: 0 };
    for (let n = 0; n < 40; n++) {
      const container = `d${n}`;
      const memories = await seedRandom(f, plans, random, container);
      const live = memories.slice(0, 5);
      const target = pick(random, live);
      const observed = observeBatches(f);
      const operation = ["forget", "patch", "hard", "upsert", "enrich"][n % 5];
      if (operation === "forget") await f.api("/v4/memories", { containerTag: container, documentId: target.id }, "DELETE");
      if (operation === "patch") await f.api(`/v3/documents/${target.id}`, { content: `${target.token} patched` }, "PATCH");
      if (operation === "hard") await f.api(`/v3/documents/${target.id}`, undefined, "DELETE");
      if (operation === "upsert") await f.api("/v3/documents", { containerTag: container, customId: target.name, content: `${target.token} again` });
      if (operation === "enrich") await f.api("/v4/enrich", { id: target.id });
      await f.drain();
      f.DB.hooks.batch = undefined;
      for (const batch of observed) {
        kinds[batch.kind]++;
        const diff = compareRestore(batch);
        for (const key of Object.keys(totals)) totals[key] += diff[key].length;
        assert.deepEqual(diff.newOnly, [], `${container} ${batch.kind}: rows only the scoped statement restores`);
        assert.deepEqual(diff.oldOnlyInPair, [], `${container} ${batch.kind}: rows of a vanishing pair the scoped statement missed`);
      }
    }
    for (const [kind, count] of Object.entries(kinds)) assert.ok(count >= 6, `${kind} batches ${count}`);
    assert.ok(totals.both >= 8, `rows restored by both ${totals.both}`);
    assert.ok(totals.oldOnlyOutside >= 8, `rows only the old statement restores ${totals.oldOnlyOutside}`);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 3. 乱択 ----
test("fact restore: random enrich, edit, upsert and forget never leave a superseded fact without a live superseding record", { timeout: 600000 }, async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const random = prng(20261127);
    const counts = { enrich: 0, patch: 0, upsert: 0, forget: 0, hard: 0 };
    let checks = 0, supersededSeen = 0;
    // 件数は dev/src/server_tests.rs のHTTPタイムアウト（90秒）に収まる規模にしてある。増やさない。
    for (let n = 0; n < 20; n++) {
      const container = `r${n}`;
      const live = [];
      for (let i = 0; i < 4; i++) {
        const facts = Array.from({ length: pick(random, [1, 1, 2]) }, () => fact(pick(random, ["a", "b", "c"]), random() < 0.7,
          "S", pick(random, ["P", "Q"])));
        live.push(await save(f, plans, container, `m${i}`, facts, { ms: pick(random, [0, 1, 2, 3]), project: pick(random, [null, "p1"]) }));
      }
      for (const record of [...live].sort(() => random() - 0.5)) await enrich(f, record);
      for (let step = 0; step < 10; step++) {
        const operation = pick(random, ["enrich", "patch", "upsert", "forget", "hard", "enrich", "patch", "upsert"]);
        const index = Math.floor(random() * live.length);
        const record = live[index];
        if ((operation === "forget" || operation === "hard") && live.length <= 2) continue;
        counts[operation]++;
        if (operation !== "forget" && operation !== "hard") {
          plans.set(record.token, { facts: [fact(pick(random, ["a", "b", "c"]), random() < 0.7, "S", pick(random, ["P", "Q"]))] });
        }
        if (operation === "enrich") await f.api("/v4/enrich", { id: record.id });
        if (operation === "patch") await f.api(`/v3/documents/${record.id}`, { content: `${record.token} p${step}` }, "PATCH");
        if (operation === "upsert") await f.api("/v3/documents", { containerTag: container, customId: record.name, content: `${record.token} u${step}` });
        if (operation === "forget") await f.api("/v4/memories", { containerTag: container, documentId: record.id }, "DELETE");
        if (operation === "hard") await f.api(`/v3/documents/${record.id}`, undefined, "DELETE");
        await f.drain();
        if (operation === "forget" || operation === "hard") live.splice(index, 1);
        checks++;
        supersededSeen += f.db.prepare("SELECT COUNT(*) AS n FROM facts WHERE container_tag = ? AND status = 'superseded'").get(container).n;
        assert.deepEqual(unsupported(f), [], `${container} step ${step} ${operation}`);
      }
    }
    for (const [operation, count] of Object.entries(counts)) assert.ok(count >= 5, `${operation} ${count}`);
    assert.ok(checks >= 100, `checks ${checks}`);
    assert.ok(supersededSeen >= 50, `superseded rows seen ${supersededSeen}`);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 4. 忘却・ハード削除の batch 直後の例外 ----
async function replacedPair(f, plans, container) {
  const older = await save(f, plans, container, "old", [fact("a")], { ms: 0 });
  const newer = await save(f, plans, container, "new", [fact("b")], { ms: 5 });
  await enrich(f, older);
  await enrich(f, newer);
  assert.deepEqual(states(f, container), ["new:b:active", "old:a:superseded"]);
  return { older, newer };
}
const failAfterBatch = (f) => { f.DB.hooks.all = (sql) => { if (sql.includes("consolidation.status = 'invalid'")) throw new Error("injected-after-batch"); }; };
async function requestWithInjectedFailure(f, send) {
  failAfterBatch(f);
  try { return await send(); } catch (error) { return { status: 500, error: String(error) }; } finally { f.DB.hooks.all = undefined; }
}

test("fact restore: forgetting restores the directly replaced fact even when the request fails right after the batch", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const { newer } = await replacedPair(f, plans, "forget");
    const response = await requestWithInjectedFailure(f, () => f.api("/v4/memories", { containerTag: "forget", documentId: newer.id }, "DELETE"));
    assert.notEqual(response.status, 200, "the injected failure must break the request");
    assert.deepEqual(states(f, "forget"), ["old:a:active"]);
  } finally { await f.drain(); f.db.close(); }
});

test("fact restore: hard delete restores the directly replaced fact even when the request fails right after the batch", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const { newer } = await replacedPair(f, plans, "hard");
    const response = await requestWithInjectedFailure(f, () => f.api(`/v3/documents/${newer.id}`, undefined, "DELETE"));
    assert.notEqual(response.status, 204, "the injected failure must break the request");
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memories WHERE id = ?").get(newer.id).n, 0);
    assert.deepEqual(states(f, "hard"), ["old:a:active"]);
  } finally { await f.drain(); f.db.close(); }
});

test("fact restore: hard delete returns 404 and changes nothing when the record is gone", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const { newer } = await replacedPair(f, plans, "gone");
    const missing = await f.api("/v3/documents/no-such-record", undefined, "DELETE");
    assert.equal(missing.status, 404);
    // 存在確認の後、batch の前に記録が消えた場合も 404 で、他の事実は変わらない。
    f.DB.hooks.batch = () => { f.DB.hooks.batch = undefined; f.db.prepare("DELETE FROM memories WHERE id = ?").run(newer.id); };
    const raced = await f.api(`/v3/documents/${newer.id}`, undefined, "DELETE");
    assert.equal(raced.status, 404);
    assert.deepEqual(states(f, "gone"), ["old:a:superseded"]);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 5. 版不一致なら0件 ----
test("fact restore: a stale revision restores nothing", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const { newer } = await replacedPair(f, plans, "stale");
    const captured = [];
    f.DB.hooks.batch = (ops) => { for (const op of ops) if (isScoped(op)) captured.push({ sql: op.sql, values: [...op.values] }); };
    await enrich(f, newer);
    f.DB.hooks.batch = undefined;
    assert.equal(captured.length, 1);
    const { sql, values } = captured[0];
    // 事実が置き換え済みの状態へ戻す（再抽出の結果は同じ）。
    assert.deepEqual(states(f, "stale"), ["new:b:active", "old:a:superseded"]);
    const stale = values.map((value, index) => index >= 3 && index % 2 === 1 ? "stale-revision" : value);
    assert.equal(f.db.prepare(sql).run(...stale).changes, 0);
    assert.deepEqual(states(f, "stale"), ["new:b:active", "old:a:superseded"]);
    // 版が合えば、自分の事実を置き換え元として数えずに a を戻す（比較の基準）。
    assert.equal(f.db.prepare(sql).run(...values).changes, 1);
    assert.deepEqual(states(f, "stale"), ["new:b:active", "old:a:active"]);
  } finally { await f.drain(); f.db.close(); }
});

// ---- 6. 実行計画 ----
test("fact restore: the four scoped statements plan idx_facts_identity and idx_facts_source without scanning facts", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const captured = {};
    f.DB.hooks.batch = (ops) => { for (const op of ops) if (isScoped(op)) captured[kindOf(op.sql)] = { sql: op.sql, values: [...op.values] }; };
    const first = await save(f, plans, "plan", "p0", [fact("a")], { ms: 0 });
    const second = await save(f, plans, "plan", "p1", [fact("b")], { ms: 5 });
    await enrich(f, first);
    await enrich(f, second);
    const again = await f.api("/v3/documents", { containerTag: "plan", customId: "p0", content: `${first.token} again` });
    assert.equal(again.status, 201, JSON.stringify(again.body));
    await f.drain();
    const forgotten = await f.api("/v4/memories", { containerTag: "plan", documentId: first.id }, "DELETE");
    assert.equal(forgotten.status, 200, JSON.stringify(forgotten.body));
    const removed = await f.api(`/v3/documents/${second.id}`, undefined, "DELETE");
    assert.equal(removed.status, 204);
    await f.drain();
    f.DB.hooks.batch = undefined;
    assert.deepEqual(Object.keys(captured).sort(), ["custom", "forget", "hard", "id"]);
    for (const [kind, { sql, values }] of Object.entries(captured)) {
      const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...values).map((row) => String(row.detail)).join("\n");
      assert.ok(plan.includes("idx_facts_identity"), `${kind}\n${plan}`);
      assert.ok(plan.includes("idx_facts_source"), `${kind}\n${plan}`);
      assert.ok(!plan.includes("idx_facts_container_status"), `${kind}\n${plan}`);
      assert.ok(plan.includes("subject_key=?") && plan.includes("predicate_key=?"), `${kind}\n${plan}`);
      assert.ok(!plan.split("\n").some((line) => /SCAN (facts|gone|newer)\b/u.test(line)), `${kind}\n${plan}`);
    }
  } finally { await f.drain(); f.db.close(); }
});
