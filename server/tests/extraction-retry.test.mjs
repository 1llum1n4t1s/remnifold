// cron での事実・トピック抽出の拾い直し（retryMissingExtractions）。通常の補強が途中で止まった行を、
// 既存の15分 cron が30分に1件だけ、extractEnrichment を1回呼んで回復する。埋め込みは呼ばず、版と updated_at は変えない。
import test, { assert } from "./support/test.mjs";
import { fixture, output, embedding } from "./worker-fixture.mjs";
import worker from "../src/index.ts";

const CRON = "*/15 * * * *";
const SLOT = 900_000;
// 毎時15分・45分の枠（15分枠の番号が奇数）。枠の外は毎時0分・30分。
const IN_SLOT = Date.UTC(2026, 5, 1, 0, 15);
const OUT_SLOT = Date.UTC(2026, 5, 1, 0, 0);
// テストの Worker は時計が 2026-10-21T01:00Z に固定されている（保存した行の created_at がこの日になる）。その日の枠（01:15）。
const WORKER_NOW = "2026-10-21T01:00:00.000Z";
const TODAY_SLOT = Date.UTC(2026, 9, 21, 1, 15);
const FACT = { subject: "Remnifold", predicate: "stores memories in", object: "D1", confidence: 0.9, exclusive: false };

const minutesAgo = (minutes) => new Date(Date.now() - minutes * 60_000).toISOString();
const gate = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };
const tick = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, message) {
  for (let i = 0; i < 400; i++) { if (check()) return; await tick(5); }
  assert.ok(false, message);
}

// 抽出（messages）と埋め込み（text）の AI 呼び出しを数える差し替え。
function setup(f, behavior = async () => ({ facts: [FACT], topics: ["Validation"] })) {
  const state = { extracts: [], embeds: 0, behavior, vectors: new Map() };
  f.env.AI_ENRICHMENT_MODE = "on";
  f.env.AI = { run: async (_model, input) => {
    if (input.text) { state.embeds++; return { data: [embedding] }; }
    const content = input.messages.at(-1).content;
    state.extracts.push(content);
    return output(await state.behavior(content));
  } };
  f.env.MEMORY_VECTORS = {
    upsert: async (rows) => { for (const row of rows) state.vectors.set(row.id, row); return { mutationId: "m" }; },
    deleteByIds: async (ids) => { for (const id of ids) state.vectors.delete(id); return {}; },
    getByIds: async (ids) => ids.flatMap((id) => state.vectors.has(id) ? [state.vectors.get(id)] : []),
  };
  return state;
}

// 抽出が未完了の行。保存は AI なしで行い（応答後の補強を走らせない）、状態を直接書いて「途中で止まった行」にする。
async function pending(f, customId, content, { fact = "pending", topic = "pending", age = 30, metadata } = {}) {
  const mode = f.env.AI_ENRICHMENT_MODE;
  f.env.AI_ENRICHMENT_MODE = "off";
  const saved = await f.api("/v3/documents", { containerTag: "memories", customId, content, ...(metadata ? { metadata } : {}) });
  f.env.AI_ENRICHMENT_MODE = mode;
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  f.db.prepare("UPDATE memories SET embedding_status = 'done', embedded_at = ?, updated_at = ?, fact_status = ?, topic_status = ? WHERE id = ?")
    .run(minutesAgo(age), minutesAgo(age), fact, topic, saved.body.id);
  return saved.body.id;
}
const row = (f, id) => f.db.prepare(`SELECT embedding_status, embedded_at, fact_status, topic_status, fact_model, facts_extracted_at, topic_model, topics_extracted_at,
  topic_revision, updated_at, enrichment_error, is_forgotten, metadata_json FROM memories WHERE id = ?`).get(id);
const retry = (f, id) => f.db.prepare("SELECT source_revision, attempts, next_attempt_at FROM memory_extraction_retries WHERE memory_id = ?").get(id);
const facts = (f, id) => f.db.prepare("SELECT subject, predicate, object, status, updated_at FROM facts WHERE source_memory_id = ?").all(id);
const topics = (f, id) => f.db.prepare("SELECT topic FROM memory_topics WHERE memory_id = ? ORDER BY topic").all(id).map((entry) => entry.topic);
// 既定は、同じ fixture で呼ぶたびに次の枠（30分後）へ進める。同じ枠を続けて使うと2回目は確保できないため。
const nextSlot = (f) => IN_SLOT + (f.slotIndex = (f.slotIndex ?? -1) + 1) * 2 * SLOT;
const cron = async (f, scheduledTime = nextSlot(f)) => { worker.scheduled({ cron: CRON, scheduledTime }, f.env, f.ctx); return f.drain(); };
const withFixture = (name, run, options) => test(name, async () => {
  const f = fixture(options);
  try { await run(f); } finally { await f.drain(); f.db.close(); }
});

// 180秒のタイマーだけを手動にし、片付け（clearTimeout）を観測する。
function manualTimeout() {
  const original = { set: globalThis.setTimeout, clear: globalThis.clearTimeout };
  const timers = [];
  globalThis.setTimeout = (callback, delay, ...rest) => {
    if (delay !== 180_000) return original.set(callback, delay, ...rest);
    const timer = { callback, cleared: false };
    timers.push(timer);
    return timer;
  };
  globalThis.clearTimeout = (handle) => { if (timers.includes(handle)) handle.cleared = true; else original.clear(handle); };
  return { timers, restore() { globalThis.setTimeout = original.set; globalThis.clearTimeout = original.clear; } };
}

withFixture("both sides pending: one extraction call saves facts and topics, keeps the version, clears the retry row and the timer", async (f) => {
  const s = setup(f);
  const clock = manualTimeout();
  try {
    const id = await pending(f, "both", "body both");
    const before = row(f, id);
    assert.deepEqual(await cron(f), [{ status: "fulfilled" }]);
    assert.deepEqual(s.extracts, ["body both"]);
    assert.equal(s.embeds, 0);
    const after = row(f, id);
    assert.equal(after.fact_status, "done");
    assert.equal(after.topic_status, "done");
    assert.equal(after.fact_model, "@cf/zai-org/glm-4.7-flash");
    assert.equal(after.topic_revision, before.topic_revision);
    assert.equal(after.updated_at, before.updated_at);
    assert.equal(after.embedded_at, before.embedded_at);
    assert.equal(after.enrichment_error, null);
    assert.deepEqual(facts(f, id).map(({ subject, object, status, updated_at }) => ({ subject, object, status, updated_at })),
      [{ subject: "Remnifold", object: "D1", status: "active", updated_at: before.updated_at }]);
    assert.deepEqual(topics(f, id), ["Validation"]);
    assert.equal(retry(f, id), undefined);
    assert.equal(clock.timers.length, 1);
    assert.equal(clock.timers[0].cleared, true, "the 180s timer is cleared");
  } finally { clock.restore(); }
});

for (const status of ["done", "disabled"]) {
  withFixture(`fact side ${status}, topics pending: facts are not written and topics are`, async (f) => {
    const s = setup(f);
    const id = await pending(f, "topics-only", "body topics only", { fact: status });
    const before = row(f, id);
    await cron(f);
    assert.equal(s.extracts.length, 1);
    const after = row(f, id);
    assert.equal(after.fact_status, status);
    assert.equal(after.fact_model, before.fact_model);
    assert.equal(after.facts_extracted_at, before.facts_extracted_at);
    assert.deepEqual(facts(f, id), []);
    assert.equal(after.topic_status, "done");
    assert.deepEqual(topics(f, id), ["Validation"]);
    assert.equal(retry(f, id), undefined);
  });

  withFixture(`topic side ${status}, facts pending: topics are not written and facts are`, async (f) => {
    const s = setup(f);
    const id = await pending(f, "facts-only", "body facts only", { topic: status });
    f.db.prepare("INSERT INTO memory_topics(memory_id, source_revision, topic, topic_key, created_at) SELECT id, topic_revision, 'Kept', 'kept', ? FROM memories WHERE id = ?").run(minutesAgo(40), id);
    const before = row(f, id);
    await cron(f);
    assert.equal(s.extracts.length, 1);
    const after = row(f, id);
    assert.equal(after.topic_status, status);
    assert.equal(after.topic_model, before.topic_model);
    assert.equal(after.topics_extracted_at, before.topics_extracted_at);
    assert.deepEqual(topics(f, id), ["Kept"]);
    assert.equal(after.fact_status, "done");
    assert.equal(facts(f, id).length, 1);
    assert.equal(retry(f, id), undefined);
  });
}

withFixture("explicit topics are saved without AI, and facts still use one extraction call", async (f) => {
  const s = setup(f);
  const id = await pending(f, "explicit", "body explicit", { metadata: { topics: ["Explicit label"] } });
  await cron(f);
  assert.equal(s.extracts.length, 1);
  assert.deepEqual(topics(f, id), ["Explicit label"]);
  assert.equal(row(f, id).topic_status, "done");
  assert.equal(row(f, id).fact_status, "done");
  assert.equal(facts(f, id).length, 1);
  assert.equal(retry(f, id), undefined);
});

withFixture("explicit topics with the fact side done need no AI call at all", async (f) => {
  const s = setup(f);
  const id = await pending(f, "explicit-only", "body explicit only", { fact: "done", metadata: { topics: ["Explicit label"] } });
  await cron(f);
  assert.equal(s.extracts.length, 0);
  assert.equal(s.embeds, 0);
  assert.deepEqual(topics(f, id), ["Explicit label"]);
  assert.equal(row(f, id).topic_status, "done");
  assert.equal(retry(f, id), undefined);
});

for (const [name, mutate] of [
  ["recallable becomes false", (f, id) => f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.recallable', json('false')) WHERE id = ?").run(id)],
  ["the version changes (/v4/enrich)", (f, id) => f.db.prepare("UPDATE memories SET topic_revision = 'changed-revision', embedding_status = 'pending' WHERE id = ?").run(id)],
  ["the memory is edited", (f, id) => f.db.prepare("UPDATE memories SET updated_at = ? WHERE id = ?").run(minutesAgo(1), id)],
  ["the memory is forgotten", (f, id) => f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(id)],
]) {
  withFixture(`nothing is written when ${name} while the extraction is in flight`, async (f) => {
    const hold = gate();
    const s = setup(f, async () => { await hold.promise; return { facts: [FACT], topics: ["Validation"] }; });
    const id = await pending(f, "inflight", "body inflight");
    worker.scheduled({ cron: CRON, scheduledTime: IN_SLOT }, f.env, f.ctx);
    await until(() => s.extracts.length === 1, "extraction started");
    mutate(f, id);
    const changed = row(f, id);
    hold.release();
    const results = await f.drain();
    assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
    assert.deepEqual(row(f, id), changed);
    assert.deepEqual(facts(f, id), []);
    assert.deepEqual(topics(f, id), []);
  });
}

withFixture("a failing row stops after 3 attempts with 6h/24h backoff, and a new version starts again", async (f) => {
  const s = setup(f, async () => { throw new Error("injected-extraction-failure"); });
  const id = await pending(f, "bad", "body bad");
  const hours = (iso) => Math.round((Date.parse(iso) - Date.now()) / 3_600_000);
  await cron(f);
  assert.equal(s.extracts.length, 1);
  assert.equal(row(f, id).fact_status, "failed");
  assert.equal(row(f, id).topic_status, "failed");
  assert.match(row(f, id).enrichment_error, /^fact extraction: .*injected-extraction-failure.*topic classification: /u);
  assert.equal(retry(f, id).attempts, 1);
  assert.equal(hours(retry(f, id).next_attempt_at), 6);
  await cron(f);
  assert.equal(s.extracts.length, 1, "backoff: not retried before the next attempt time");
  for (const attempt of [2, 3]) {
    f.db.prepare("UPDATE memory_extraction_retries SET next_attempt_at = ? WHERE memory_id = ?").run(minutesAgo(1), id);
    await cron(f);
    assert.equal(retry(f, id).attempts, attempt);
    assert.equal(hours(retry(f, id).next_attempt_at), 24);
  }
  assert.equal(s.extracts.length, 3);
  f.db.prepare("UPDATE memory_extraction_retries SET next_attempt_at = ? WHERE memory_id = ?").run(minutesAgo(1), id);
  await cron(f);
  assert.equal(s.extracts.length, 3, "no call after 3 attempts");
  assert.equal(row(f, id).fact_status, "failed");
  // 編集や /v4/enrich で版が変わると、1回目から再開する。
  f.db.prepare("UPDATE memories SET topic_revision = 'next-revision' WHERE id = ?").run(id);
  await cron(f);
  assert.equal(s.extracts.length, 4);
  assert.deepEqual({ revision: retry(f, id).source_revision, attempts: retry(f, id).attempts }, { revision: "next-revision", attempts: 1 });
});

withFixture("only extraction errors are rewritten; embedding errors and other stages are kept", async (f) => {
  const s = setup(f);
  const kept = await pending(f, "kept", "body kept", { age: 30 });
  f.db.prepare("UPDATE memories SET enrichment_error = 'embedding: boom' WHERE id = ?").run(kept);
  await cron(f);
  assert.equal(row(f, kept).fact_status, "done");
  assert.equal(row(f, kept).enrichment_error, "embedding: boom");
  const stale = await pending(f, "stale", "body stale", { age: 40 });
  f.db.prepare("UPDATE memories SET enrichment_error = 'fact extraction: old failure; topic classification: old failure' WHERE id = ?").run(stale);
  await cron(f);
  assert.equal(s.extracts.length, 2);
  assert.equal(row(f, stale).fact_status, "done");
  assert.equal(row(f, stale).enrichment_error, null);
});

withFixture("outside the slot, with AI disabled or on a busy day (300 new rows) no AI is called; one row below the threshold runs", async (f) => {
  const s = setup(f);
  const a = await pending(f, "slot-a", "body a", { age: 40 });
  const b = await pending(f, "slot-b", "body b", { age: 30 });
  for (const time of [OUT_SLOT, Number.NaN]) await cron(f, time);
  worker.scheduled({ cron: CRON }, f.env, f.ctx);
  await f.drain();
  f.env.AI_ENRICHMENT_MODE = "off";
  await cron(f);
  f.env.AI_ENRICHMENT_MODE = "on";
  assert.deepEqual(s.extracts, []);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memory_extraction_retries").get().n, 0);
  const fill = (count) => f.db.prepare(`WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?)
    INSERT INTO memories(id, custom_id, container_tag, content, created_at, updated_at) SELECT 'fill-' || i, 'fill-' || i, 'memories', 'x', ?, ? FROM n`)
    .run(count, WORKER_NOW, WORKER_NOW);
  const today = () => f.db.prepare("SELECT COUNT(*) AS n FROM memories WHERE created_at >= '2026-10-21T00:00:00.000Z'").get().n;
  fill(297);
  assert.equal(today(), 299);
  await cron(f, TODAY_SLOT);
  assert.deepEqual(s.extracts, ["body b"], "299 rows today: the newest candidate runs");
  f.db.prepare("INSERT INTO memories(id, custom_id, container_tag, content, created_at, updated_at) VALUES ('fill-last', 'fill-last', 'memories', 'x', ?, ?)").run(WORKER_NOW, WORKER_NOW);
  assert.equal(today(), 300);
  await cron(f, TODAY_SLOT);
  assert.deepEqual(s.extracts, ["body b"], "300 rows today: nothing runs");
  assert.equal(row(f, a).fact_status, "pending");
  assert.equal(s.embeds, 0);
});

withFixture("concurrent crons call the AI once for the same row", async (f) => {
  const hold = gate();
  const s = setup(f, async () => { await hold.promise; return { facts: [FACT], topics: ["Validation"] }; });
  const id = await pending(f, "race", "body race");
  worker.scheduled({ cron: CRON, scheduledTime: IN_SLOT }, f.env, f.ctx);
  worker.scheduled({ cron: CRON, scheduledTime: IN_SLOT }, f.env, f.ctx);
  await until(() => s.extracts.length >= 1, "extraction started");
  await tick(30);
  hold.release();
  const results = await f.drain();
  assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
  assert.equal(s.extracts.length, 1);
  assert.equal(row(f, id).fact_status, "done");
  assert.equal(facts(f, id).length, 1);
});

withFixture("two pending rows: a repeated cron of the same slot calls the AI once, and the next slot processes the second row", async (f) => {
  const s = setup(f);
  const older = await pending(f, "slot-older", "body older", { age: 40 });
  const newer = await pending(f, "slot-newer", "body newer", { age: 30 });
  await cron(f, IN_SLOT);
  await cron(f, IN_SLOT);
  assert.deepEqual(s.extracts, ["body newer"]);
  assert.equal(row(f, newer).fact_status, "done");
  assert.equal(row(f, older).fact_status, "pending");
  assert.equal(f.db.prepare("SELECT slot FROM memory_extraction_retry_slot WHERE id = 1").get().slot, Math.floor(IN_SLOT / SLOT) * 15);
  await cron(f, IN_SLOT + 2 * SLOT);
  assert.deepEqual(s.extracts, ["body newer", "body older"]);
  assert.equal(row(f, older).fact_status, "done");
});

withFixture("after 180 seconds the result is dropped as a failure, the timer is cleared and a late result is not saved", async (f) => {
  const hold = gate();
  const s = setup(f, async () => { await hold.promise; return { facts: [FACT], topics: ["Validation"] }; });
  const clock = manualTimeout();
  try {
    const id = await pending(f, "slow", "body slow");
    worker.scheduled({ cron: CRON, scheduledTime: IN_SLOT }, f.env, f.ctx);
    await until(() => s.extracts.length === 1 && clock.timers.length === 1, "extraction started with a 180s timer");
    clock.timers[0].callback();
    const results = await f.drain();
    assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
    assert.equal(clock.timers[0].cleared, true);
    assert.equal(row(f, id).fact_status, "failed");
    assert.equal(row(f, id).topic_status, "failed");
    assert.match(row(f, id).enrichment_error, /timed out after 180s/);
    assert.equal(retry(f, id).attempts, 1);
    hold.release();
    await tick(30);
    await f.drain();
    assert.deepEqual(facts(f, id), []);
    assert.deepEqual(topics(f, id), []);
    assert.equal(row(f, id).fact_status, "failed");
  } finally { clock.restore(); }
});

withFixture("rows that must not be picked cause no AI call, while an eligible older control row is processed", async (f) => {
  const s = setup(f);
  const control = await pending(f, "control", "body control", { age: 600 });
  const skipped = {
    recent: await pending(f, "recent", "body recent", { age: 5 }),
    enriched: await pending(f, "enriched", "body enriched", { age: 30 }),
    notEmbedded: await pending(f, "notembedded", "body notembedded"),
    forgotten: await pending(f, "forgotten", "body forgotten"),
    recallableFalse: await pending(f, "recallable", "body recallable"),
    summary: await pending(f, "summary", "body summary"),
    noRevision: await pending(f, "norev", "body norev"),
    noEmbeddedAt: await pending(f, "noembedded", "body noembedded"),
    complete: await pending(f, "complete", "body complete", { fact: "done", topic: "done" }),
    disabledBoth: await pending(f, "disabled", "body disabled", { fact: "disabled", topic: "disabled" }),
  };
  f.db.prepare("UPDATE memories SET embedded_at = ? WHERE id = ?").run(minutesAgo(5), skipped.enriched);
  // 埋め込みが未完了の行。埋め込みの拾い直し（別の段）の対象にならないよう、埋め込みの値は持たせておく。
  f.db.prepare("UPDATE memories SET embedding_status = 'processing', embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), skipped.notEmbedded);
  f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(skipped.forgotten);
  f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.recallable', json('false')) WHERE id = ?").run(skipped.recallableFalse);
  f.db.prepare("INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-own', ?, 'r', 'active', ?)").run(skipped.summary, minutesAgo(30));
  f.db.prepare("UPDATE memories SET topic_revision = NULL WHERE id = ?").run(skipped.noRevision);
  f.db.prepare("UPDATE memories SET embedded_at = NULL WHERE id = ?").run(skipped.noEmbeddedAt);
  const snapshot = Object.values(skipped).map((id) => row(f, id));
  await cron(f);
  assert.deepEqual(s.extracts, ["body control"]);
  assert.equal(s.embeds, 0);
  assert.equal(row(f, control).fact_status, "done");
  assert.deepEqual(Object.values(skipped).map((id) => row(f, id)), snapshot);
  const left = f.db.prepare("SELECT memory_id, attempts FROM memory_extraction_retries").all();
  assert.equal(left.length, 0, JSON.stringify({ left, control, row: row(f, control) }));
});

withFixture("the candidate SELECT uses the partial index and neither scans memories nor sorts", async (f) => {
  setup(f);
  await pending(f, "plan", "body plan");
  const captured = [];
  f.DB.hooks.first = (sql) => { captured.push(sql); };
  await cron(f);
  const select = captured.find((sql) => sql.includes("LEFT JOIN memory_extraction_retries"));
  assert.ok(select, "candidate SELECT was issued");
  const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${select}`).all().map((entry) => entry.detail);
  assert.ok(plan.some((detail) => detail.includes("idx_memories_extraction_backlog")), plan.join("\n"));
  assert.ok(!plan.some((detail) => /^SCAN (m|memories)\b/u.test(detail) && !detail.includes("USING")), plan.join("\n"));
  assert.ok(!plan.some((detail) => detail.includes("TEMP B-TREE")), plan.join("\n"));
});

// 期限付き一括モード（EXTRACTION_BACKLOG_UNTIL）。通常の枠（毎時0分・30分）の外の OUT_SLOT でも動くことで、一括モードかどうかを見分ける。
const DAY = 86_400_000;
// pending は AI の設定を一時的に切り替えるので、並行に呼ばず順に作る。
async function pendingMany(f, count, prefix = "bulk") {
  const ids = [];
  for (let i = 0; i < count; i++) ids.push(await pending(f, `${prefix}-${i}`, `body ${prefix} ${i}`, { age: 30 + i }));
  return ids;
}

withFixture("backlog mode: unset, empty, invalid, past, over 7 days and after the deadline stay normal; a valid deadline processes outside the slot", async (f) => {
  const s = setup(f);
  await pendingMany(f, 3);
  for (const value of [undefined, "", "tomorrow", "2026-06-01 00:00:00", new Date(OUT_SLOT - 1000).toISOString(), new Date(OUT_SLOT + 7 * DAY + 1000).toISOString()]) {
    f.env.EXTRACTION_BACKLOG_UNTIL = value;
    await cron(f, OUT_SLOT);
  }
  f.env.EXTRACTION_BACKLOG_UNTIL = new Date(OUT_SLOT + 3_600_000).toISOString();
  await cron(f, OUT_SLOT + 2 * 3_600_000);
  assert.deepEqual(s.extracts, [], "normal mode: nothing runs outside the slot");
  f.env.EXTRACTION_BACKLOG_UNTIL = new Date(OUT_SLOT + 7 * DAY).toISOString();
  await cron(f, OUT_SLOT);
  assert.equal(s.extracts.length, 3, "a deadline exactly 7 days ahead is bulk mode");
});

withFixture("backlog mode: at most 20 rows per slot, 4 at a time, once per slot, and also on a busy day", async (f) => {
  let active = 0;
  let peak = 0;
  const s = setup(f, async () => { active++; peak = Math.max(peak, active); await tick(5); active--; return { facts: [FACT], topics: ["Validation"] }; });
  await pendingMany(f, 25);
  f.db.prepare(`WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300)
    INSERT INTO memories(id, custom_id, container_tag, content, created_at, updated_at) SELECT 'fill-' || i, 'fill-' || i, 'memories', 'x', ?, ?
    FROM n`).run(WORKER_NOW, WORKER_NOW);
  f.env.EXTRACTION_BACKLOG_UNTIL = new Date(TODAY_SLOT + DAY).toISOString();
  await cron(f, TODAY_SLOT);
  assert.equal(s.extracts.length, 20);
  assert.equal(peak, 4);
  await cron(f, TODAY_SLOT);
  assert.equal(s.extracts.length, 20, "the same slot is claimed once");
  await cron(f, TODAY_SLOT + SLOT);
  assert.equal(s.extracts.length, 25, "the next 15-minute slot takes the rest");
  assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memories WHERE fact_status = 'pending' AND custom_id LIKE 'bulk-%'").get().n, 0);
  assert.equal(s.embeds, 0);
});

withFixture("backlog mode: one failing row does not stop the others, and no new row starts after 8 minutes", async (f) => {
  const s = setup(f, async (content) => { if (content === "body bulk 1") throw new Error("injected-extraction-failure"); return { facts: [FACT], topics: ["Validation"] }; });
  const ids = await pendingMany(f, 5);
  f.env.EXTRACTION_BACKLOG_UNTIL = new Date(OUT_SLOT + DAY).toISOString();
  await cron(f, OUT_SLOT);
  assert.equal(s.extracts.length, 5);
  assert.deepEqual(ids.map((id) => row(f, id).fact_status), ["done", "failed", "done", "done", "done"]);
  assert.equal(retry(f, ids[1]).attempts, 1);

  const more = await pendingMany(f, 6, "late");
  const realNow = Date.now;
  let offset = 0;
  Date.now = () => realNow() + offset;
  s.behavior = async () => { offset = 9 * 60_000; return { facts: [FACT], topics: ["Validation"] }; };
  try { await cron(f, OUT_SLOT + SLOT); } finally { Date.now = realNow; }
  assert.equal(s.extracts.length, 5 + 4, "only the 4 rows started before the 8-minute mark");
  assert.equal(more.filter((id) => row(f, id).fact_status === "pending").length, 2);
});

withFixture("without migration 0021 only the third stage fails; reconcile and the embedding retry still succeed", async (f) => {
  const s = setup(f);
  const reconcileRow = await pending(f, "reconcile", "body reconcile", { fact: "done", topic: "done" });
  f.db.prepare("UPDATE memories SET embedding_json = json(?), vector_status = 'pending' WHERE id = ?").run(JSON.stringify(embedding), reconcileRow);
  const extractionRow = await pending(f, "extraction", "body extraction");
  const missing = await pending(f, "missing", "body missing", { fact: "done", topic: "done" });
  f.db.prepare("UPDATE memories SET embedding_status = 'pending', embedded_at = NULL, vector_status = 'pending' WHERE id = ?").run(missing);
  const results = await cron(f);
  assert.equal(results.length, 1);
  assert.equal(results[0].status, "rejected");
  assert.match(results[0].error, /memory_extraction_retries/);
  assert.equal(row(f, reconcileRow).embedding_status, "done");
  assert.equal(f.db.prepare("SELECT vector_status FROM memories WHERE id = ?").get(reconcileRow).vector_status, "queued");
  assert.equal(s.vectors.has(reconcileRow), true);
  assert.equal(row(f, missing).embedding_status, "done", "the embedding retry succeeded");
  assert.equal(f.db.prepare("SELECT embedding_json IS NOT NULL AS has FROM memories WHERE id = ?").get(missing).has, 1);
  assert.deepEqual(s.extracts, []);
  assert.equal(row(f, extractionRow).fact_status, "pending");
}, { beforeMigration: "0021_" });
