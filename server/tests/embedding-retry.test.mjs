// cron での埋め込みの拾い直し（retryMissingEmbeddings）。応答後の実行が打ち切られて埋め込みが無いまま残った行を、
// 15分 cron が埋め込みだけ作り直して回復する。事実・トピックは呼ばず、版と updated_at は変えない。
import test, { assert } from "./support/test.mjs";
import { fixture, output, embedding } from "./worker-fixture.mjs";
import worker from "../src/index.ts";

const CRON = "*/15 * * * *";
const minutesAgo = (minutes) => new Date(Date.now() - minutes * 60_000).toISOString();
const gate = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };
const tick = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, message) {
  for (let i = 0; i < 400; i++) { if (check()) return; await tick(5); }
  assert.ok(false, message);
}

// AI と Vectorize の差し替え。embed は本文ごとの挙動（既定は成功）、other は埋め込み以外の AI 呼び出し。
function setup(f, behavior = async () => ({ data: [embedding] })) {
  const state = { embeds: [], other: 0, vectors: new Map(), order: [], behavior };
  f.env.AI_ENRICHMENT_MODE = "on";
  f.env.AI = { run: async (_model, input) => {
    if (input.text) { state.embeds.push(input.text[0]); state.order.push("embed"); return state.behavior(input.text[0]); }
    state.other++;
    return output({ facts: [], topics: ["Validation"] });
  } };
  f.env.MEMORY_VECTORS = {
    upsert: async (rows) => { state.order.push("upsert"); for (const row of rows) state.vectors.set(row.id, row); return { mutationId: "m" }; },
    deleteByIds: async (ids) => { for (const id of ids) state.vectors.delete(id); return {}; },
    getByIds: async (ids) => ids.flatMap((id) => state.vectors.has(id) ? [state.vectors.get(id)] : []),
  };
  return state;
}

// 滞留行を作る。保存は AI なしで行い（応答後の補強を走らせない）、状態を直接書いて「打ち切られた行」にする。
async function stuck(f, customId, content, { status = "processing", age = 30 } = {}) {
  const mode = f.env.AI_ENRICHMENT_MODE;
  f.env.AI_ENRICHMENT_MODE = "off";
  const saved = await f.api("/v3/documents", { containerTag: "memories", customId, content });
  f.env.AI_ENRICHMENT_MODE = mode;
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  f.db.prepare("UPDATE memories SET embedding_status = ?, vector_status = 'pending', updated_at = ? WHERE id = ?").run(status, minutesAgo(age), saved.body.id);
  return saved.body.id;
}
const row = (f, id) => f.db.prepare(`SELECT embedding_status, vector_status, fact_status, topic_status, topic_revision, updated_at, enrichment_error,
  embedding_json IS NOT NULL AS has_embedding, is_forgotten FROM memories WHERE id = ?`).get(id);
const retry = (f, id) => f.db.prepare("SELECT source_revision, attempts, next_attempt_at FROM memory_embedding_retries WHERE memory_id = ?").get(id);
const cron = async (f, name = CRON) => { worker.scheduled({ cron: name, scheduledTime: Date.now() }, f.env, f.ctx); return f.drain(); };
const withFixture = (name, run, options) => test(name, async () => {
  const f = fixture(options);
  try { await run(f); } finally { await f.drain(); f.db.close(); }
});

withFixture("pending, processing and failed rows recover in one cron with a single embedding call each", async (f) => {
  const s = setup(f);
  const ids = [];
  for (const status of ["pending", "processing", "failed"]) ids.push(await stuck(f, status, `body ${status}`, { status }));
  const before = ids.map((id) => row(f, id));
  const results = await cron(f);
  assert.deepEqual(results, [{ status: "fulfilled" }]);
  assert.deepEqual([...s.embeds].sort(), ["body failed", "body pending", "body processing"]);
  assert.equal(s.other, 0);
  ids.forEach((id, index) => {
    const after = row(f, id);
    assert.equal(after.embedding_status, "done");
    assert.equal(after.vector_status, "queued");
    assert.equal(after.has_embedding, 1);
    assert.equal(after.topic_revision, before[index].topic_revision);
    assert.equal(after.updated_at, before[index].updated_at);
    assert.equal(after.fact_status, before[index].fact_status);
    assert.equal(after.topic_status, before[index].topic_status);
    assert.equal(s.vectors.get(id).metadata.topic_revision, before[index].topic_revision);
    assert.equal(retry(f, id), undefined);
  });
});

withFixture("rows that must not be embedded cause no AI call, while an eligible control row is recovered", async (f) => {
  const s = setup(f);
  const control = await stuck(f, "control", "body control");
  const skipped = {
    recallableFalse: await stuck(f, "recallable", "body recallable"),
    disabled: await stuck(f, "disabled", "body disabled", { status: "disabled" }),
    forgotten: await stuck(f, "forgotten", "body forgotten"),
    summary: await stuck(f, "summary", "body summary"),
    hidden: await stuck(f, "hidden", "body hidden"),
    recent: await stuck(f, "recent", "body recent", { age: 5 }),
    noRevision: await stuck(f, "norev", "body norev"),
    embedded: await stuck(f, "embedded", "body embedded", { status: "done" }),
  };
  f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.recallable', json('false')) WHERE id = ?").run(skipped.recallableFalse);
  f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(skipped.forgotten);
  f.db.prepare("UPDATE memories SET topic_revision = NULL WHERE id = ?").run(skipped.noRevision);
  f.db.prepare("UPDATE memories SET embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), skipped.embedded);
  f.db.prepare("INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-own', ?, 'r', 'active', ?)").run(skipped.summary, minutesAgo(30));
  // 整理で隠れた行: 原本の被覆が完了していて、原本の埋め込みが無い。
  const sourceRevision = row(f, skipped.hidden).topic_revision;
  f.db.prepare("INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-cover', ?, 'r', 'active', ?)").run(control, minutesAgo(30));
  f.db.prepare("INSERT INTO memory_organization_coverage(memory_id, source_revision, status) VALUES (?, ?, 'active')").run(skipped.hidden, sourceRevision);
  f.db.prepare("INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, 's-cover')").run(skipped.hidden);
  assert.ok(f.db.prepare("SELECT 1 FROM memory_organization_hidden WHERE memory_id = ?").get(skipped.hidden), "the hidden row is hidden");
  // 被覆の要約側の行（control）はまとめなので対象外になる。対照用には別の通常行を使う。
  const plain = await stuck(f, "plain", "body plain");
  // vector_status は reconcileVectors（忘却・隠れた行の後始末）が触るので比べない。拾い直しが書く列だけを比べる。
  const kept = (id) => { const { embedding_status, has_embedding, enrichment_error, topic_revision, updated_at } = row(f, id); return { embedding_status, has_embedding, enrichment_error, topic_revision, updated_at }; };
  const snapshot = Object.values(skipped).map(kept);
  await cron(f);
  assert.deepEqual(s.embeds, ["body plain"]);
  assert.equal(row(f, plain).embedding_status, "done");
  assert.deepEqual(Object.values(skipped).map(kept), snapshot);
  assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memory_embedding_retries").get().n, 0);
  assert.equal(row(f, control).has_embedding, 0);
});

withFixture("no AI call and no retry-table access while AI enrichment is disabled", async (f) => {
  const s = setup(f);
  const id = await stuck(f, "off", "body off");
  f.env.AI_ENRICHMENT_MODE = "off";
  await cron(f);
  assert.deepEqual(s.embeds, []);
  assert.equal(row(f, id).has_embedding, 0);
  assert.equal(retry(f, id), undefined);
});

withFixture("25 rows recover within 10 per cron, all in three runs", async (f) => {
  const s = setup(f);
  const ids = [];
  for (let i = 0; i < 25; i++) ids.push(await stuck(f, `bulk-${i}`, `body ${i}`));
  const done = () => ids.filter((id) => row(f, id).has_embedding === 1).length;
  const counts = [];
  for (let run = 0; run < 3; run++) { await cron(f); counts.push(s.embeds.length); }
  assert.deepEqual(counts, [10, 20, 25]);
  assert.equal(done(), 25);
  assert.equal(new Set(s.embeds).size, 25);
});

withFixture("a persistent failure stops after 5 attempts, honours backoff and does not block other rows", async (f) => {
  const s = setup(f, async (text) => { if (text.startsWith("bad")) throw new Error("injected-embedding-failure"); return { data: [embedding] }; });
  const bad = await stuck(f, "bad", "bad v1");
  await cron(f);
  assert.equal(s.embeds.length, 1);
  assert.equal(row(f, bad).embedding_status, "failed");
  assert.match(row(f, bad).enrichment_error, /injected-embedding-failure/);
  assert.equal(retry(f, bad).attempts, 1);
  await cron(f);
  assert.equal(s.embeds.length, 1, "backoff: not retried before the next attempt time");
  for (let attempt = 2; attempt <= 5; attempt++) {
    f.db.prepare("UPDATE memory_embedding_retries SET next_attempt_at = ? WHERE memory_id = ?").run(minutesAgo(1), bad);
    await cron(f);
    assert.equal(retry(f, bad).attempts, attempt);
  }
  assert.equal(s.embeds.length, 5);
  f.db.prepare("UPDATE memory_embedding_retries SET next_attempt_at = ? WHERE memory_id = ?").run(minutesAgo(1), bad);
  await cron(f);
  assert.equal(s.embeds.length, 5, "no call after 5 attempts");
  assert.equal(retry(f, bad).attempts, 5);

  // 上限に達した行・確保済みの行が10件の枠を塞がず、古い通常行も回復する。
  const revision = row(f, bad).topic_revision;
  const blockers = [];
  for (let i = 0; i < 20; i++) {
    const id = await stuck(f, `blocker-${i}`, `blocker ${i}`, { age: 10 + i });
    f.db.prepare("INSERT INTO memory_embedding_retries(memory_id, source_revision, attempts, next_attempt_at) VALUES (?, ?, ?, ?)")
      .run(id, row(f, id).topic_revision, i < 10 ? 5 : 1, i < 10 ? minutesAgo(1) : new Date(Date.now() + 3_600_000).toISOString());
    blockers.push(id);
  }
  const oldest = await stuck(f, "oldest", "good oldest", { age: 600 });
  await cron(f);
  assert.equal(row(f, oldest).embedding_status, "done");
  assert.ok(blockers.every((id) => row(f, id).has_embedding === 0));
  assert.equal(s.embeds.filter((text) => text.startsWith("blocker")).length, 0);

  // 版が変わると1回目から再開する。
  await stuck(f, "bad", "bad v2");
  assert.notEqual(row(f, bad).topic_revision, revision);
  await cron(f);
  assert.equal(retry(f, bad).attempts, 1);
  assert.equal(retry(f, bad).source_revision, row(f, bad).topic_revision);
  assert.equal(s.embeds.at(-1), "bad v2");
});

withFixture("concurrent crons call the AI once for the same row", async (f) => {
  const s = setup(f, async () => { await tick(30); return { data: [embedding] }; });
  const id = await stuck(f, "race", "body race");
  worker.scheduled({ cron: CRON }, f.env, f.ctx);
  worker.scheduled({ cron: CRON }, f.env, f.ctx);
  const results = await f.drain();
  assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
  assert.equal(s.embeds.length, 1);
  assert.equal(row(f, id).embedding_status, "done");
});

withFixture("the 18:00 cron does not retry embeddings", async (f) => {
  const s = setup(f);
  const id = await stuck(f, "daily", "body daily");
  await cron(f, "0 18 * * *");
  assert.deepEqual(s.embeds, []);
  assert.equal(row(f, id).has_embedding, 0);
});

// 60秒の打ち切りだけを短くする（他のタイマーは変えない）。
async function withShortTimeout(run) {
  const original = globalThis.setTimeout;
  globalThis.setTimeout = (callback, delay, ...rest) => original(callback, delay === 60_000 ? 30 : delay, ...rest);
  try { await run(); } finally { globalThis.setTimeout = original; }
}

withFixture("an unresponsive AI does not delay reconcileVectors and a late embedding is not saved after the cut-off", async (f) => {
  await withShortTimeout(async () => {
    const hang = gate();
    const s = setup(f, async () => { await hang.promise; return { data: [embedding] }; });
    const reconcileRow = await stuck(f, "reconcile", "body reconcile", { status: "done" });
    f.db.prepare("UPDATE memories SET embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), reconcileRow);
    const id = await stuck(f, "hung", "body hung");
    s.behavior = async (text) => { if (text === "body hung") await hang.promise; return { data: [embedding] }; };
    const results = await cron(f);
    assert.deepEqual(results, [{ status: "fulfilled" }]);
    assert.deepEqual(s.order.slice(0, 2), ["upsert", "embed"], "reconcileVectors finished before the embedding retry began");
    assert.equal(row(f, reconcileRow).vector_status, "queued");
    const cut = row(f, id);
    assert.equal(cut.embedding_status, "failed");
    assert.match(cut.enrichment_error, /timed out/);
    hang.release();
    await tick(30);
    await f.drain();
    const late = row(f, id);
    assert.equal(late.has_embedding, 0);
    assert.equal(late.embedding_status, "failed");
    assert.equal(s.vectors.has(id), false);
    assert.equal(retry(f, id).attempts, 1);
  });
});

for (const [name, mutate] of [
  ["recallable becomes false", (f, id) => f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.recallable', json('false')) WHERE id = ?").run(id)],
  ["the version changes", (f, id) => f.db.prepare("UPDATE memories SET topic_revision = 'changed-revision' WHERE id = ?").run(id)],
  ["the memory is forgotten", (f, id) => f.db.prepare("UPDATE memories SET is_forgotten = 1 WHERE id = ?").run(id)],
]) {
  withFixture(`nothing is written when ${name} while the embedding is in flight`, async (f) => {
    const hold = gate();
    const s = setup(f, async () => { await hold.promise; return { data: [embedding] }; });
    const id = await stuck(f, "inflight", "body inflight");
    const before = row(f, id);
    worker.scheduled({ cron: CRON }, f.env, f.ctx);
    await until(() => s.embeds.length === 1, "embedding started");
    mutate(f, id);
    const changed = row(f, id);
    hold.release();
    const results = await f.drain();
    assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
    const after = row(f, id);
    assert.equal(after.has_embedding, 0);
    assert.equal(after.embedding_status, before.embedding_status);
    assert.equal(after.vector_status, before.vector_status);
    assert.equal(after.enrichment_error, null);
    assert.deepEqual(after, changed);
    assert.equal(s.vectors.has(id), false);
  });
}

withFixture("a failure in reconcileVectors still runs the retry and is rethrown at the end", async (f) => {
  const s = setup(f);
  const reconcileRow = await stuck(f, "reconcile", "body reconcile", { status: "done" });
  f.db.prepare("UPDATE memories SET embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), reconcileRow);
  const id = await stuck(f, "after", "body after");
  f.env.MEMORY_VECTORS.getByIds = async () => { throw new Error("injected-reconcile-failure"); };
  const results = await cron(f);
  assert.equal(results.length, 1);
  assert.equal(results[0].status, "rejected");
  assert.match(results[0].error, /injected-reconcile-failure/);
  assert.equal(row(f, id).embedding_status, "done");
  assert.deepEqual(s.embeds, ["body after"]);
});

withFixture("without migration 0020 the cron still reconciles vectors and reports the failure last", async (f) => {
  const s = setup(f);
  const reconcileRow = await stuck(f, "reconcile", "body reconcile", { status: "done" });
  f.db.prepare("UPDATE memories SET embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), reconcileRow);
  const id = await stuck(f, "pending", "body pending");
  const results = await cron(f);
  assert.equal(results.length, 1);
  assert.equal(results[0].status, "rejected");
  assert.match(results[0].error, /memory_embedding_retries/);
  assert.equal(row(f, reconcileRow).vector_status, "queued");
  assert.equal(s.vectors.has(reconcileRow), true);
  assert.deepEqual(s.embeds, []);
  assert.equal(row(f, id).has_embedding, 0);
}, { beforeMigration: "0020_" });

// 部分索引の追加（0020）で、memories に触れる既存文（コンテナ一覧・忘却・reconcile・保存・補強）の計画が変わらないこと。
withFixture("migration 0020 leaves the query plans of existing memories statements unchanged", async (f) => {
  try {
    setup(f);
    const captured = new Map();
    for (const hook of ["first", "all", "run", "execute"]) f.DB.hooks[hook] = (sql, values) => { if (!captured.has(sql)) captured.set(sql, [...values]); };
    const id = await (async () => {
      const saved = await f.api("/v3/documents", { containerTag: "memories", customId: "plans", content: "body plans" });
      assert.equal(saved.status, 201, JSON.stringify(saved.body));
      return saved.body.id;
    })();
    await f.drain();
    assert.equal((await f.api("/v3/container-tags", undefined, "GET")).status, 200);
    assert.equal((await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE")).status, 200);
    await f.drain();
    f.db.prepare("UPDATE memories SET embedding_status = 'done', vector_status = 'pending', is_forgotten = 0 WHERE id = ?").run(id);
    await cron(f);
    const plan = (db, sql, values) => db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...values).map((entry) => entry.detail);
    // 同じ fixture で、索引ありの計画と、索引を落とした計画（0020 の前の索引集合）を比べる。新表は既存文の計画に影響しない。
    const statements = [...captured].filter(([sql]) => /^\s*(SELECT|UPDATE|DELETE|INSERT|WITH)\b/iu.test(sql) && !sql.includes("memory_embedding_retries") && /\bmemories\b/u.test(sql));
    const withIndex = statements.map(([sql, values]) => plan(f.db, sql, values));
    f.db.exec("DROP INDEX idx_memories_embedding_backlog");
    const withoutIndex = statements.map(([sql, values]) => plan(f.db, sql, values));
    const changed = statements.flatMap(([sql], index) => JSON.stringify(withIndex[index]) === JSON.stringify(withoutIndex[index]) ? []
      : [{ sql: sql.replace(/\s+/gu, " ").slice(0, 160), without: withoutIndex[index], with: withIndex[index] }]);
    assert.ok(statements.length > 20, `compared ${statements.length} statements`);
    assert.deepEqual(changed, []);
  } finally { /* fixture は withFixture が閉じる */ }
});

withFixture("the candidate SELECT uses the partial index and never scans memories", async (f) => {
  setup(f);
  await stuck(f, "plan", "body plan");
  const captured = [];
  f.DB.hooks.all = (sql) => { captured.push(sql); };
  await cron(f);
  const select = captured.find((sql) => sql.includes("LEFT JOIN memory_embedding_retries"));
  assert.ok(select, "candidate SELECT was issued");
  const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${select}`).all().map((entry) => entry.detail);
  assert.ok(plan.some((detail) => detail.includes("idx_memories_embedding_backlog")), plan.join("\n"));
  assert.ok(!plan.some((detail) => /^SCAN (m|memories)\b/u.test(detail)), plan.join("\n"));
});

withFixture("a failed retry does not turn an embedding saved meanwhile by the regular path into failed", async (f) => {
  const hold = gate();
  const s = setup(f, async () => { await hold.promise; throw new Error("injected-late-failure"); });
  const id = await stuck(f, "saved-meanwhile", "body saved meanwhile");
  worker.scheduled({ cron: CRON }, f.env, f.ctx);
  await until(() => s.embeds.length === 1, "embedding started");
  // 通常側が同じ版の埋め込みを先に保存した状態（/v4/enrich は updated_at を変えない）。
  f.db.prepare("UPDATE memories SET embedding_status = 'done', vector_status = 'queued', embedding_json = json(?) WHERE id = ?").run(JSON.stringify(embedding), id);
  const saved = row(f, id);
  hold.release();
  const results = await f.drain();
  assert.equal(results.every((result) => result.status === "fulfilled"), true, JSON.stringify(results));
  assert.deepEqual(row(f, id), saved);
  assert.equal(row(f, id).enrichment_error, null);
});

withFixture("a failed retry writes nothing when the version changed while the embedding was in flight", async (f) => {
  const hold = gate();
  const s = setup(f, async () => { await hold.promise; throw new Error("injected-late-failure"); });
  const id = await stuck(f, "version-moved", "body version moved");
  worker.scheduled({ cron: CRON }, f.env, f.ctx);
  await until(() => s.embeds.length === 1, "embedding started");
  f.db.prepare("UPDATE memories SET topic_revision = 'changed-revision', embedding_status = 'pending', vector_status = 'pending' WHERE id = ?").run(id);
  const moved = row(f, id);
  hold.release();
  await f.drain();
  assert.deepEqual(row(f, id), moved);
});

withFixture("a successful retry clears only an embedding-only enrichment_error", async (f) => {
  setup(f);
  const cases = {
    embeddingOnly: ["embedding: injected-old-failure", null],
    withFact: ["embedding: injected-old-failure; fact extraction: injected-fact", "embedding: injected-old-failure; fact extraction: injected-fact"],
    withTopic: ["embedding: x; topic classification: injected-topic", "embedding: x; topic classification: injected-topic"],
    otherStage: ["fact graph: injected-graph", "fact graph: injected-graph"],
  };
  const ids = {};
  for (const [name, [error]] of Object.entries(cases)) {
    ids[name] = await stuck(f, name, `body ${name}`, { status: "failed" });
    f.db.prepare("UPDATE memories SET enrichment_error = ? WHERE id = ?").run(error, ids[name]);
  }
  await cron(f);
  for (const [name, [, expected]] of Object.entries(cases)) {
    assert.equal(row(f, ids[name]).embedding_status, "done", name);
    assert.equal(row(f, ids[name]).enrichment_error, expected, name);
  }
});
