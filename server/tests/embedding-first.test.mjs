// 埋め込みの先行保存: embedText の完了時点で埋め込みを保存し、抽出の完了を待たない。
// 抽出が遅く応答後の実行が打ち切られても、終わった埋め込みを失わない。
import test, { assert } from "./support/test.mjs";
import { fixture, output, embedding } from "./worker-fixture.mjs";

const gate = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };

// 抽出の応答だけを gate で止める。本文に "Edited" を含む版は別の事実を返し、版ごとの事実を区別する。
function setup(f, { failExtraction = false, holdEmbedding = false } = {}) {
  const extraction = gate();
  const embedGate = gate();
  if (!holdEmbedding) embedGate.release();
  const vectors = new Map();
  let extractions = 0;
  let embeddings = 0;
  f.env.AI_ENRICHMENT_MODE = "on";
  f.env.AI = { run: async (_model, input) => {
    if (input.text) { embeddings++; await embedGate.promise; return { data: [embedding] }; }
    extractions++;
    const edited = JSON.stringify(input).includes("Edited");
    await extraction.promise;
    if (failExtraction) throw new Error("injected-extraction-failure");
    return output({ facts: [{ subject: edited ? "Edited" : "Original", predicate: "has", object: "fact", confidence: 1, exclusive: false }], topics: ["Validation"] });
  } };
  f.env.MEMORY_VECTORS = {
    upsert: async (rows) => { for (const row of rows) vectors.set(row.id, row); return { mutationId: "mutation" }; },
    deleteByIds: async (ids) => { for (const id of ids) vectors.delete(id); return { mutationId: "delete" }; },
    getByIds: async (ids) => ids.flatMap((id) => vectors.has(id) ? [vectors.get(id)] : []),
  };
  return { extraction, embedGate, vectors, counts: () => ({ extractions, embeddings }) };
}

const save = async (f, content) => {
  const saved = await f.api("/v3/documents", { containerTag: "memories", customId: "embedding-first", content });
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  return saved.body.id;
};
const row = (f, id) => f.db.prepare("SELECT embedding_status, vector_status, fact_status, topic_revision, enrichment_error, embedding_json IS NOT NULL AS has_embedding FROM memories WHERE id = ?").get(id);
const facts = (f, id) => f.db.prepare("SELECT subject FROM facts WHERE source_memory_id = ? ORDER BY subject").all(id).map((fact) => fact.subject);
async function until(check, message) {
  for (let i = 0; i < 400; i++) { if (check()) return; await new Promise((resolve) => setTimeout(resolve, 5)); }
  assert.ok(false, message);
}

test("embedding is saved and queued while fact extraction is still pending", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f);
    const id = await save(f, "Original body.");
    await until(() => row(f, id).embedding_status === "done", "embedding saved before extraction finishes");
    const early = row(f, id);
    assert.equal(early.vector_status, "queued");
    assert.equal(early.has_embedding, 1);
    assert.equal(early.fact_status, "processing");
    assert.deepEqual(facts(f, id), []);
    assert.equal(s.vectors.has(id), true);
    s.extraction.release();
    await f.drain();
    const done = row(f, id);
    assert.equal(done.fact_status, "done");
    assert.equal(done.enrichment_error, null);
    assert.deepEqual(facts(f, id), ["Original"]);
    assert.deepEqual(s.counts(), { extractions: 1, embeddings: 1 });
  } finally { s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("embedding stays saved when fact extraction fails", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f, { failExtraction: true });
    const id = await save(f, "Original body.");
    await until(() => row(f, id).embedding_status === "done", "embedding saved before extraction fails");
    s.extraction.release();
    await f.drain();
    const done = row(f, id);
    assert.equal(done.embedding_status, "done");
    assert.equal(done.vector_status, "queued");
    assert.equal(done.fact_status, "failed");
    assert.match(done.enrichment_error, /fact extraction: .*injected-extraction-failure/);
    assert.equal(s.vectors.has(id), true);
  } finally { s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("a version change during extraction leaves no facts or stale vector of the old version", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f);
    const id = await save(f, "Original body.");
    await until(() => row(f, id).embedding_status === "done", "old version embedding saved");
    const oldRevision = row(f, id).topic_revision;
    assert.equal(s.vectors.get(id).metadata.topic_revision, oldRevision);
    assert.equal(await save(f, "Edited body."), id);
    const newRevision = row(f, id).topic_revision;
    assert.notEqual(newRevision, oldRevision);
    s.extraction.release();
    await f.drain();
    assert.deepEqual(facts(f, id), ["Edited"]);
    const done = row(f, id);
    assert.equal(done.fact_status, "done");
    assert.equal(done.embedding_status, "done");
    assert.equal(s.vectors.get(id).metadata.topic_revision, newRevision);
  } finally { s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("forgetting during extraction leaves no facts or vector", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f);
    const id = await save(f, "Original body.");
    await until(() => row(f, id).embedding_status === "done", "embedding saved before forgetting");
    assert.equal(s.vectors.has(id), true);
    const forgotten = await f.api("/v4/memories", { containerTag: "memories", documentId: id }, "DELETE");
    assert.equal(forgotten.status, 200, JSON.stringify(forgotten.body));
    s.extraction.release();
    await f.drain();
    assert.deepEqual(facts(f, id), []);
    assert.equal(s.vectors.has(id), false);
    assert.equal(f.db.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten, 1);
  } finally { s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("an exception in the embedding save chain stores no facts", async () => {
  const f = fixture(); let s;
  const errors = [];
  const original = console.error;
  console.error = (...values) => errors.push(values.map(String).join(" "));
  try {
    s = setup(f);
    f.DB.hooks.run = async (sql) => { if (sql.includes("SET embedding_status = 'done'")) throw new Error("injected-save-failure"); };
    const id = await save(f, "Original body.");
    await until(() => f.db.prepare("SELECT 1 FROM memories WHERE id = ? AND embedding_status = 'processing'").get(id), "enrichment started");
    s.extraction.release();
    const results = await f.drain();
    assert.ok(errors.some((line) => line.includes("memory_enrichment_failed")), "enrichment failure is reported");
    f.DB.hooks.run = undefined;
    assert.ok(results.some((result) => result.status === "rejected" && /injected-save-failure/.test(result.error)), JSON.stringify(results));
    assert.deepEqual(facts(f, id), []);
    const failed = row(f, id);
    assert.equal(failed.fact_status, "failed");
    assert.equal(failed.embedding_status, "failed");
    assert.match(failed.enrichment_error, /injected-save-failure/);
  } finally { console.error = original; f.DB.hooks.run = undefined; s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("recallable:false set during extraction stores no facts and records no error", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f);
    const id = await save(f, "Original body.");
    await until(() => row(f, id).embedding_status === "done", "embedding saved before the update");
    f.db.prepare("UPDATE memories SET metadata_json = json_set(metadata_json, '$.memoryIndex.recallable', json('false')) WHERE id = ?").run(id);
    s.extraction.release();
    await f.drain();
    assert.deepEqual(facts(f, id), []);
    assert.equal(row(f, id).enrichment_error, null);
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM memory_topics WHERE memory_id = ?").get(id)?.n ?? 0, 0);
  } finally { s?.extraction.release(); await f.drain(); f.db.close(); }
});

test("a memory hidden by organization while its embedding is in flight leaves no queued vector", async () => {
  const f = fixture(); let s;
  try {
    s = setup(f, { holdEmbedding: true });
    const id = await save(f, "Original body.");
    await until(() => s.counts().embeddings === 1, "embedding started");
    // 原本の被覆が完了した要約を作り、原本を整理で隠す。
    f.env.AI_ENRICHMENT_MODE = "off";
    const control = await f.api("/v3/documents", { containerTag: "memories", customId: "cover", content: "Cover body." });
    f.env.AI_ENRICHMENT_MODE = "on";
    assert.equal(control.status, 201, JSON.stringify(control.body));
    const revision = row(f, id).topic_revision;
    const old = new Date(Date.now() - 30 * 60_000).toISOString();
    f.db.prepare("INSERT INTO memory_organization_summaries(id, memory_id, revision, status, updated_at) VALUES ('s-cover', ?, 'r', 'active', ?)").run(control.body.id, old);
    f.db.prepare("INSERT INTO memory_organization_coverage(memory_id, source_revision, status) VALUES (?, ?, 'active')").run(id, revision);
    f.db.prepare("INSERT INTO memory_organization_coverage_summaries(memory_id, summary_id) VALUES (?, 's-cover')").run(id);
    // 被覆する要約の記憶が索引済みなら、原本は埋め込みが保存された後も隠れたままになる。
    f.db.prepare("UPDATE memories SET embedding_json = '[1]', embedding_status = 'done', vector_status = 'indexed' WHERE id = ?").run(control.body.id);
    assert.ok(f.db.prepare("SELECT 1 FROM memory_organization_hidden WHERE memory_id = ?").get(id), "the source memory is hidden");
    s.embedGate.release();
    s.extraction.release();
    await f.drain();
    assert.equal(s.vectors.has(id), false);
    assert.notEqual(row(f, id).vector_status, "queued");
  } finally { s?.embedGate.release(); s?.extraction.release(); await f.drain(); f.db.close(); }
});
