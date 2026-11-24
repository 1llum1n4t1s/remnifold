// 事実の新旧は、抽出順ではなく元記録の (updated_at, id) で決まる。
// 同じ組（container・subject・predicate）の active が3件以内・forget なしの範囲で、どの順に抽出しても
// 時刻順に抽出したときの最終 active/superseded と一致することを乱択で確かめる。
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
const permutations = (items) => items.length <= 1 ? [items]
  : items.flatMap((item, i) => permutations([...items.slice(0, i), ...items.slice(i + 1)]).map((rest) => [item, ...rest]));
const at = (ms) => `2026-05-01T00:00:00.${String(ms).padStart(3, "0")}Z`;

// 本文の目印から返す事実を決める AI。
function setup(f) {
  const plans = new Map();
  f.env.AI = { run: async (_model, input) => {
    if (input.text) return { data: [embedding] };
    const text = JSON.stringify(input);
    for (const [token, fact] of plans) {
      if (text.includes(token)) return output({ facts: [{ subject: "S", predicate: "P", object: fact.object, confidence: 1, exclusive: fact.exclusive }], topics: ["T"] });
    }
    return output({ facts: [], topics: ["T"] });
  } };
  return plans;
}

// records: [{ object, exclusive, ms, project }]。記録を保存し、updated_at を指定の値へ固定する。
async function createRecords(f, plans, container, records) {
  f.env.AI_ENRICHMENT_MODE = "off";
  const created = [];
  for (const [index, record] of records.entries()) {
    const token = `rec_${container}_${index}_end`;
    const saved = await f.api("/v3/documents", { containerTag: container, customId: token, content: token,
      metadata: record.project ? { sm_project_id: record.project } : {} });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    f.db.prepare("UPDATE memories SET updated_at = ? WHERE id = ?").run(at(record.ms), saved.body.id);
    plans.set(token, record);
    created.push({ ...record, id: saved.body.id, name: `r${index}` });
  }
  await f.drain();
  f.env.AI_ENRICHMENT_MODE = "on";
  return created;
}

// 指定の順に /v4/enrich で抽出し、最終状態を返す。
async function runOrder(f, container, created, order) {
  f.db.prepare("DELETE FROM fact_relations WHERE container_tag = ?").run(container);
  f.db.prepare("DELETE FROM facts WHERE container_tag = ?").run(container);
  for (const record of order) {
    assert.equal((await f.api("/v4/enrich", { id: record.id })).status, 202);
    await f.drain();
  }
  const names = new Map(created.map((record) => [record.id, record.name]));
  return f.db.prepare("SELECT source_memory_id AS source, object, status FROM facts WHERE container_tag = ?").all(container)
    .map((row) => `${names.get(row.source)}:${row.object}:${row.status}`).sort();
}
// 組の active な object の集合（重複なし）。同じ値の古い記録が裏付けとして active に残る差は、集合では現れない。
const activeValues = (states) => [...new Set(states.filter((state) => state.endsWith(":active")).map((state) => state.split(":")[1]))].sort();
const timeOrder = (created) => [...created].sort((a, b) => a.ms - b.ms || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

// 比べるのは active な object の集合。行単位（どの記録の行が active か）は、同じ値の古い行が残る既知の限界があり比べない
// （下の "known limit: same value kept active" を参照）。
test("fact order: any extraction order gives the same active values as the time order (random, active <= 3, no forget)", { timeout: 600000 }, async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const random = prng(20261125);
    const counterexamples = [];
    let checks = 0;
    // 件数は dev/src/server_tests.rs のHTTPタイムアウト（90秒）に収まる規模にしてある。増やさない。
    for (let n = 0; n < 30; n++) {
      const records = Array.from({ length: pick(random, [2, 3, 3]) }, () => ({
        object: pick(random, ["a", "b", "c"]), exclusive: random() < 0.6, ms: pick(random, [0, 1, 2]),
        project: pick(random, [null, null, "p1"]),
      }));
      const container = `c${n}`;
      const created = await createRecords(f, plans, container, records);
      const expected = await runOrder(f, container, created, timeOrder(created));
      for (const order of permutations(created)) {
        const actual = await runOrder(f, container, created, order);
        checks++;
        if (JSON.stringify(activeValues(actual)) !== JSON.stringify(activeValues(expected))) {
          counterexamples.push({ records: created.map(({ name, object, exclusive, ms, project }) => ({ name, object, exclusive, ms, project })),
            time: timeOrder(created).map((record) => record.name), order: order.map((record) => record.name), expected, actual });
        }
      }
    }
    counterexamples.sort((a, b) => a.records.length - b.records.length);
    assert.deepEqual(counterexamples.slice(0, 6), [], `${counterexamples.length} mismatches of ${checks}`);
    assert.ok(checks >= 50, `checks ${checks}`);
  } finally { await f.drain(); f.db.close(); }
});

// 既知の限界: 行単位では差が出るが、active な値の集合は同じ。
// A=b（古い）、M=c（排他）、C=b（排他、最新）。時刻順では A・M が superseded、C が active。
// M, C, A の順だと、A の抽出時に M が既に C に置き換えられており、C は同じ object なので A への辺ができず、A が active に残る。
test("fact order: known limit, same value kept active", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const specs = [{ object: "b", exclusive: false, ms: 0, project: null }, { object: "c", exclusive: true, ms: 1, project: null },
      { object: "b", exclusive: true, ms: 2, project: null }];
    const forgetNewest = async (container, created) => {
      const forgotten = await f.api("/v4/memories", { containerTag: container, documentId: created[2].id }, "DELETE");
      assert.equal(forgotten.status, 200, JSON.stringify(forgotten.body));
      await f.drain();
      return f.db.prepare("SELECT source_memory_id AS source, object, status FROM facts WHERE container_tag = ?").all(container)
        .map((row) => `${created.find((record) => record.id === row.source).name}:${row.object}:${row.status}`).sort();
    };
    const results = {};
    for (const [label, pick] of [["time", [0, 1, 2]], ["reverse", [1, 2, 0]]]) {
      const created = await createRecords(f, plans, label, specs);
      const before = await runOrder(f, label, created, pick.map((i) => created[i]));
      results[label] = { before, after: await forgetNewest(label, created) };
    }
    assert.deepEqual(results.time.before, ["r0:b:superseded", "r1:c:superseded", "r2:b:active"]);
    assert.deepEqual(results.reverse.before, ["r0:b:active", "r1:c:superseded", "r2:b:active"]);
    assert.deepEqual(activeValues(results.time.before), activeValues(results.reverse.before));
    // C を forget した後は、時刻順・逆順とも同じ状態（C の行が消え、M と A が active に戻る。差は残らない）。
    const afterForget = ["r0:b:active", "r1:c:active"];
    assert.deepEqual(results.time.after, afterForget);
    assert.deepEqual(results.reverse.after, afterForget);
  } finally { await f.drain(); f.db.close(); }
});

// 既知の制約: 順方向は上位3件までなので、同じ組の active が4件以上あると完全な順序独立にはならない。
test("fact order: known limit with four or more active facts in one group", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const records = [0, 1, 2, 3].map((ms) => ({ object: `o${ms}`, exclusive: false, ms, project: null }));
    records.push({ object: "x", exclusive: true, ms: 9, project: null });
    const created = await createRecords(f, plans, "limit", records);
    const activeOld = (states) => states.filter((state) => /^r[0-3]:/u.test(state) && state.endsWith(":active")).length;
    // 時刻順: x は新しい3件だけを置き換え、最も古い1件が active に残る。
    assert.equal(activeOld(await runOrder(f, "limit", created, created)), 1);
    // x が先: 逆向きは上限がないため、4件すべてが置き換えられる。
    assert.equal(activeOld(await runOrder(f, "limit", created, [created[4], ...created.slice(0, 4)])), 0);
  } finally { await f.drain(); f.db.close(); }
});

test("fact order: forgetting the newer exclusive record revives the older fact", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const created = await createRecords(f, plans, "revive", [
      { object: "a", exclusive: true, ms: 0, project: null }, { object: "b", exclusive: true, ms: 5, project: null }]);
    const state = async () => (await runOrder(f, "revive", created, [created[1], created[0]]));
    assert.deepEqual(await state(), ["r0:a:superseded", "r1:b:active"]);
    const forgotten = await f.api("/v4/memories", { containerTag: "revive", documentId: created[1].id }, "DELETE");
    assert.equal(forgotten.status, 200, JSON.stringify(forgotten.body));
    await f.drain();
    const revived = f.db.prepare("SELECT object, status FROM facts WHERE container_tag = 'revive' ORDER BY object").all();
    assert.deepEqual(revived, [{ object: "a", status: "active" }]);
  } finally { await f.drain(); f.db.close(); }
});

test("fact order: an edited old record becomes the newest and supersedes the newer exclusive fact", async () => {
  const f = fixture();
  try {
    const plans = setup(f);
    const created = await createRecords(f, plans, "edit", [
      { object: "a", exclusive: true, ms: 0, project: null }, { object: "b", exclusive: true, ms: 5, project: null }]);
    assert.deepEqual(await runOrder(f, "edit", created, created), ["r0:a:superseded", "r1:b:active"]);
    const edited = await f.api(`/v3/documents/${created[0].id}`, { content: "rec_edit_0_end edited" }, "PATCH");
    assert.equal(edited.status, 200, JSON.stringify(edited.body));
    await f.drain();
    const rows = f.db.prepare("SELECT object, status FROM facts WHERE container_tag = 'edit' ORDER BY object").all();
    assert.deepEqual(rows, [{ object: "a", status: "active" }, { object: "b", status: "superseded" }]);
  } finally { await f.drain(); f.db.close(); }
});
