import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import worker from "../src/index.ts";

function d1Adapter(database) {
  let beforeFirst;
  let beforeAll;
  let beforeRun;
  let beforeBatch;

  function prepare(sql) {
    const operation = (values = []) => ({
      sql,
      values,
      bind: (...nextValues) => operation(nextValues),
      first: async () => {
        const row = database.prepare(sql).get(...values);
        if (beforeFirst) return beforeFirst(sql, row);
        return row;
      },
      all: async () => {
        const results = database.prepare(sql).all(...values);
        return { results: beforeAll ? await beforeAll(sql, results) : results };
      },
      run: async () => {
        if (beforeRun) await beforeRun(sql, values);
        const result = database.prepare(sql).run(...values);
        return { meta: { changes: Number(result.changes) } };
      },
      batch: () => {
        if (/^\s*(?:SELECT|WITH|PRAGMA)\b/iu.test(sql)) {
          return { results: database.prepare(sql).all(...values), meta: { changes: 0 } };
        }
        const result = database.prepare(sql).run(...values);
        return { results: [], meta: { changes: Number(result.changes) } };
      },
    });
    return operation();
  }

  return {
    prepare,
    batch: async (operations) => {
      if (beforeBatch) await beforeBatch(operations);
      database.exec("BEGIN");
      try {
        const results = operations.map((operation) => operation.batch());
        database.exec("COMMIT");
        return results;
      } catch (error) {
        database.exec("ROLLBACK");
        throw error;
      }
    },
    interceptFirst(callback) {
      beforeFirst = callback;
    },
    interceptAll(callback) {
      beforeAll = callback;
    },
    interceptRun(callback) {
      beforeRun = callback;
    },
    interceptBatch(callback) {
      beforeBatch = callback;
    },
  };
}

function openDatabase() {
  const database = new DatabaseSync(":memory:");
  database.exec("PRAGMA foreign_keys = ON");
  const migrations = resolve(import.meta.dirname, "..", "migrations");
  for (const name of readdirSync(migrations).filter((name) => name.endsWith(".sql")).sort()) {
    database.exec(readFileSync(resolve(import.meta.dirname, "..", "migrations", name), "utf8"));
  }
  return database;
}

function deferred() {
  let resolvePromise;
  const promise = new Promise((resolve) => {
    resolvePromise = resolve;
  });
  return { promise, resolve: resolvePromise };
}

function embedding(first = 1, second = 0) {
  const values = Array(1_024).fill(0);
  values[0] = first;
  values[1] = second;
  return values;
}

function enrichment(facts, topics) {
  return {
    choices: [{ message: { content: JSON.stringify({ facts, topics }) } }],
  };
}

function baseEnvironment(DB, overrides = {}) {
  return {
    DB,
    MEMORY_API_KEY: "test-key",
    AI_ENRICHMENT_MODE: "off",
    AI: { run: async () => { throw new Error("AI should not run"); } },
    MEMORY_VECTORS: {},
    ...overrides,
  };
}

function request(path, body, method = "POST") {
  return new Request(`https://memory.example${path}`, {
    method,
    headers: { Authorization: "Bearer test-key", "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

test("same-millisecond retry cannot restore stale topics after a content patch", async () => {
  const OriginalDate = globalThis.Date;
  const fixedTime = "2026-09-11T00:00:00.000Z";
  globalThis.Date = class extends OriginalDate {
    constructor(...args) {
      super(...(args.length === 0 ? [fixedTime] : args));
    }
    static now() {
      return OriginalDate.parse(fixedTime);
    }
  };

  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const waits = [];
    const ctx = { waitUntil: (promise) => waits.push(promise) };
    const env = baseEnvironment(DB);

    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "legacy-folder",
      customId: "race",
      content: "Old content",
      metadata: { topics: ["Old"] },
    }), env, ctx);
    assert.equal(saved.status, 201);
    const { id } = await saved.json();

    let intercepted = false;
    DB.interceptFirst(async (sql, row) => {
      const isRetryRead = sql.includes("SELECT id, container_tag,") &&
        sql.includes("metadata_json, updated_at, topic_revision") &&
        sql.includes("FROM memories WHERE id = ? AND is_forgotten = 0");
      if (intercepted || !isRetryRead) {
        return row;
      }
      intercepted = true;
      env.AI_ENRICHMENT_MODE = "off";
      const patched = await worker.fetch(request(`/v3/documents/${id}`, { content: "New content" }, "PATCH"), env, ctx);
      assert.equal(patched.status, 200);
      env.AI_ENRICHMENT_MODE = "on";
      return row;
    });

    env.AI_ENRICHMENT_MODE = "on";
    const retry = await worker.fetch(request("/v4/enrich", { id }), env, ctx);
    assert.equal(retry.status, 409);
    assert.equal(intercepted, true);

    const current = await worker.fetch(new Request(`https://memory.example/v3/documents/${id}`, {
      headers: { Authorization: "Bearer test-key" },
    }), env, ctx);
    const currentBody = await current.json();
    assert.equal(currentBody.content, "New content");
    assert.deepEqual(currentBody.topics, []);
    assert.equal(currentBody.metadata.topics, undefined);
  } finally {
    database.close();
    globalThis.Date = OriginalDate;
  }
});

test("accepts a maximum-length multibyte document body", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const response = await worker.fetch(request("/v3/documents", {
      containerTag: "multibyte-body",
      customId: "maximum-content",
      content: "あ".repeat(200_000),
      entityContext: "文脈".repeat(8_000),
      metadata: { note: "メタデータ".repeat(4_000) },
    }), env, { waitUntil() {} });

    assert.equal(response.status, 201);
  } finally {
    database.close();
  }
});

test("forget accepts a document ID only within its named container", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const ctx = { waitUntil() {} };
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-by-id",
      content: "# Imported title\n\nSession: old-session\nTurn: 1\n\nSanitized body.",
      metadata: { captureVersion: 2, title: "Imported title", sessionId: "old-session", turn: 1 },
    }), env, ctx);
    const id = (await saved.json()).id;

    const wrongContainer = await worker.fetch(request("/v4/memories", {
      containerTag: "memories",
      documentId: id,
    }, "DELETE"), env, ctx);
    assert.equal((await wrongContainer.json()).id, null);
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten, 0);

    const forgotten = await worker.fetch(request("/v4/memories", {
      containerTag: "forget-by-id",
      documentId: id,
    }, "DELETE"), env, ctx);
    assert.equal(forgotten.status, 200);
    assert.deepEqual(await forgotten.json(), { id, message: "Memory forgotten" });
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten, 1);
  } finally {
    database.close();
  }
});

test("forget cleanup restores a vector recreated before the pending delete completes", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const deleteStarted = deferred();
    const releaseDelete = deferred();
    const upserts = [];
    let storedVector;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding(0, 1)] }
          : enrichment([], ["Restore race"]),
      },
      MEMORY_VECTORS: {
        deleteByIds: async () => {
          deleteStarted.resolve();
          await releaseDelete.promise;
          storedVector = undefined;
          return { mutationId: "forgotten-delete" };
        },
        upsert: async (vectors) => {
          upserts.push(vectors);
          [storedVector] = vectors;
          return { mutationId: `restore-${upserts.length}` };
        },
      },
    });

    env.AI_ENRICHMENT_MODE = "off";
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-race",
      customId: "canonical",
      content: "Original memory.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();
    const originalRevision = database.prepare(
      "SELECT topic_revision FROM memories WHERE id = ?",
    ).get(id).topic_revision;
    storedVector = { id, values: embedding(1, 0), metadata: { topic_revision: originalRevision } };

    env.AI_ENRICHMENT_MODE = "on";
    const forgetWaits = [];
    const forgotten = await worker.fetch(request("/v4/memories", {
      containerTag: "forget-vector-race",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => forgetWaits.push(promise) });
    assert.equal(forgotten.status, 200);
    await deleteStarted.promise;

    const restoreWaits = [];
    const restored = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-race",
      customId: "canonical",
      content: "Restored memory.",
    }), env, { waitUntil: (promise) => restoreWaits.push(promise) });
    assert.equal(restored.status, 201);
    assert.equal((await restored.json()).id, id);
    await Promise.all(restoreWaits);
    const restoredRevision = database.prepare(
      "SELECT topic_revision FROM memories WHERE id = ?",
    ).get(id).topic_revision;
    assert.equal(storedVector.metadata.topic_revision, restoredRevision);

    releaseDelete.resolve();
    await Promise.all(forgetWaits);

    assert.notEqual(restoredRevision, originalRevision);
    assert.equal(upserts.length, 2);
    assert.equal(storedVector.metadata.topic_revision, restoredRevision);
    assert.deepEqual(storedVector.values, embedding(0, 1));
    assert.deepEqual(
      { ...database.prepare(
        "SELECT is_forgotten, vector_status, vector_mutation_id FROM memories WHERE id = ?",
      ).get(id) },
      { is_forgotten: 0, vector_status: "queued", vector_mutation_id: "restore-2" },
    );
  } finally {
    database.close();
  }
});

test("forget cleanup deletes a repair upsert superseded by another forget", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const firstDeleteStarted = deferred();
    const releaseFirstDelete = deferred();
    const repairStarted = deferred();
    const releaseRepair = deferred();
    let deletes = 0;
    let upserts = 0;
    let storedVector;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding(0, 1)] }
          : enrichment([], ["Restore race"]),
      },
      MEMORY_VECTORS: {
        deleteByIds: async () => {
          deletes += 1;
          if (deletes === 1) {
            firstDeleteStarted.resolve();
            await releaseFirstDelete.promise;
          }
          storedVector = undefined;
          return { mutationId: `delete-${deletes}` };
        },
        upsert: async (vectors) => {
          upserts += 1;
          if (upserts === 2) {
            repairStarted.resolve();
            await releaseRepair.promise;
          }
          [storedVector] = vectors;
          return { mutationId: `upsert-${upserts}` };
        },
      },
    });

    env.AI_ENRICHMENT_MODE = "off";
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-twice",
      customId: "canonical",
      content: "Original memory.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();

    env.AI_ENRICHMENT_MODE = "on";
    const firstForgetWaits = [];
    await worker.fetch(request("/v4/memories", {
      containerTag: "forget-vector-twice",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => firstForgetWaits.push(promise) });
    await firstDeleteStarted.promise;

    const restoreWaits = [];
    await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-twice",
      customId: "canonical",
      content: "Restored memory.",
    }), env, { waitUntil: (promise) => restoreWaits.push(promise) });
    await Promise.all(restoreWaits);
    releaseFirstDelete.resolve();
    await repairStarted.promise;

    const secondForgetWaits = [];
    const forgottenAgain = await worker.fetch(request("/v4/memories", {
      containerTag: "forget-vector-twice",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => secondForgetWaits.push(promise) });
    assert.equal(forgottenAgain.status, 200);
    await Promise.all(secondForgetWaits);

    releaseRepair.resolve();
    await Promise.all(firstForgetWaits);

    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(id).is_forgotten, 1);
    assert.equal(upserts, 2);
    assert.equal(deletes, 3);
    assert.equal(storedVector, undefined);
  } finally {
    database.close();
  }
});

test("forget cleanup leaves a failed repair immediately eligible for reconciliation", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const deleteStarted = deferred();
    const releaseDelete = deferred();
    let upserts = 0;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding(0, 1)] }
          : enrichment([], ["Restore race"]),
      },
      MEMORY_VECTORS: {
        deleteByIds: async () => {
          deleteStarted.resolve();
          await releaseDelete.promise;
          return { mutationId: "delete" };
        },
        upsert: async () => {
          upserts += 1;
          if (upserts === 2) throw new Error("Vectorize repair unavailable");
          return { mutationId: "restore" };
        },
      },
    });

    env.AI_ENRICHMENT_MODE = "off";
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-retry",
      customId: "canonical",
      content: "Original memory.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();

    env.AI_ENRICHMENT_MODE = "on";
    const forgetWaits = [];
    await worker.fetch(request("/v4/memories", {
      containerTag: "forget-vector-retry",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => forgetWaits.push(promise) });
    await deleteStarted.promise;

    const restoreWaits = [];
    await worker.fetch(request("/v3/documents", {
      containerTag: "forget-vector-retry",
      customId: "canonical",
      content: "Restored memory.",
    }), env, { waitUntil: (promise) => restoreWaits.push(promise) });
    await Promise.all(restoreWaits);
    releaseDelete.resolve();
    await Promise.all(forgetWaits);

    assert.deepEqual(
      { ...database.prepare(
        `SELECT is_forgotten, vector_status, vector_mutation_id, vector_attempted_at
         FROM memories WHERE id = ?`,
      ).get(id) },
      {
        is_forgotten: 0,
        vector_status: "failed",
        vector_mutation_id: null,
        vector_attempted_at: null,
      },
    );

    env.MEMORY_VECTORS = {
      getByIds: async () => [],
      upsert: async () => {
        upserts += 1;
        return { mutationId: "reconciled" };
      },
    };
    const reconcileWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => reconcileWaits.push(promise) });
    await Promise.all(reconcileWaits);
    assert.equal(upserts, 3);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "queued");
  } finally {
    database.close();
  }
});

test("forgotten reconciliation removes an enrichment upsert accepted after forget cleanup", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const upsertStarted = deferred();
    const releaseUpsert = deferred();
    let deleteCalls = 0;
    let storedVector;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding()] }
          : enrichment([], ["Late vector"]),
      },
      MEMORY_VECTORS: {
        upsert: async (vectors) => {
          upsertStarted.resolve();
          await releaseUpsert.promise;
          [storedVector] = vectors;
          return { mutationId: "late-upsert" };
        },
        deleteByIds: async () => {
          deleteCalls += 1;
          if (deleteCalls === 2) throw new Error("late cleanup unavailable");
          storedVector = undefined;
          return { mutationId: `delete-${deleteCalls}` };
        },
        getByIds: async () => storedVector ? [storedVector] : [],
      },
    });

    const enrichmentWaits = [];
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "late-forgotten-vector",
      customId: "canonical",
      content: "Forget while vector upsert is waiting.",
    }), env, { waitUntil: (promise) => enrichmentWaits.push(promise) });
    const { id } = await saved.json();
    await upsertStarted.promise;

    const forgetWaits = [];
    await worker.fetch(request("/v4/memories", {
      containerTag: "late-forgotten-vector",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => forgetWaits.push(promise) });
    await Promise.all(forgetWaits);
    releaseUpsert.resolve();
    await Promise.all(enrichmentWaits);

    assert.equal(storedVector.id, id);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "pending");

    const reconcileWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => reconcileWaits.push(promise) });
    await Promise.all(reconcileWaits);
    assert.equal(deleteCalls, 3);
    assert.equal(storedVector, undefined);
  } finally {
    database.close();
  }
});

test("AI-disabled forget retries a failed vector deletion during reconciliation", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    let deleteCalls = 0;
    let storedVector;
    const env = baseEnvironment(DB, {
      MEMORY_VECTORS: {
        deleteByIds: async () => {
          deleteCalls += 1;
          if (deleteCalls === 1) throw new Error("Vectorize unavailable");
          storedVector = undefined;
          return { mutationId: "retried-delete" };
        },
        getByIds: async () => storedVector ? [storedVector] : [],
      },
    });

    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "disabled-forget-vector",
      customId: "canonical",
      content: "Stored without enrichment.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();
    storedVector = { id, values: embedding(), metadata: { topic_revision: "legacy" } };

    const forgetWaits = [];
    await worker.fetch(request("/v4/memories", {
      containerTag: "disabled-forget-vector",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => forgetWaits.push(promise) });
    await Promise.all(forgetWaits);
    assert.equal(deleteCalls, 1);
    assert.equal(storedVector.id, id);

    const reconcileWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => reconcileWaits.push(promise) });
    await Promise.all(reconcileWaits);
    assert.equal(deleteCalls, 2);
    assert.equal(storedVector, undefined);
  } finally {
    database.close();
  }
});

test("forgotten reconciliation rechecks an accepted asynchronous delete until the vector disappears", async () => {
  const database = openDatabase();
  try {
    let deleteCalls = 0;
    let storedVector;
    const env = baseEnvironment(d1Adapter(database), {
      MEMORY_VECTORS: {
        deleteByIds: async () => {
          deleteCalls += 1;
          if (deleteCalls === 3) storedVector = undefined;
          return { mutationId: `delete-${deleteCalls}` };
        },
        getByIds: async () => storedVector ? [storedVector] : [],
      },
    });
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "async-forget-vector",
      customId: "canonical",
      content: "Deletion becomes visible later.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();
    storedVector = { id, values: embedding(), metadata: { topic_revision: "legacy" } };

    const forgetWaits = [];
    await worker.fetch(request("/v4/memories", {
      containerTag: "async-forget-vector",
      documentId: id,
    }, "DELETE"), env, { waitUntil: (promise) => forgetWaits.push(promise) });
    await Promise.all(forgetWaits);
    assert.equal(deleteCalls, 1);

    for (let pass = 0; pass < 2; pass += 1) {
      const waits = [];
      worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
      await Promise.all(waits);
    }
    assert.equal(deleteCalls, 3);
    assert.equal(storedVector, undefined);
  } finally {
    database.close();
  }
});

test("forgotten reconciliation repairs a canonical vector restored after lookup but before bulk delete", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const created = await worker.fetch(request("/v3/documents", {
      containerTag: "forgotten-scan-restore",
      customId: "canonical",
      content: "Original memory.",
    }), baseEnvironment(DB), { waitUntil() {} });
    const { id } = await created.json();
    const originalRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    database.prepare(
      "UPDATE memories SET is_forgotten = 1, vector_status = 'pending', vector_attempted_at = NULL WHERE id = ?",
    ).run(id);

    const lookupStarted = deferred();
    const releaseLookup = deferred();
    const oldVector = { id, values: embedding(1, 0), metadata: { topic_revision: originalRevision } };
    let storedVector = oldVector;
    let lookups = 0;
    const upserts = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding(0, 1)] }
          : enrichment([], ["Restored"]),
      },
      MEMORY_VECTORS: {
        getByIds: async () => {
          lookups += 1;
          if (lookups > 1) return storedVector ? [storedVector] : [];
          lookupStarted.resolve();
          await releaseLookup.promise;
          return [oldVector];
        },
        deleteByIds: async () => {
          storedVector = undefined;
          return { mutationId: "bulk-delete" };
        },
        upsert: async (vectors) => {
          upserts.push(vectors);
          [storedVector] = vectors;
          return { mutationId: `restore-${upserts.length}` };
        },
      },
    });

    const reconcileWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => reconcileWaits.push(promise) });
    await lookupStarted.promise;

    const restoreWaits = [];
    const restored = await worker.fetch(request("/v3/documents", {
      containerTag: "forgotten-scan-restore",
      customId: "canonical",
      content: "Restored memory.",
    }), env, { waitUntil: (promise) => restoreWaits.push(promise) });
    assert.equal((await restored.json()).id, id);
    await Promise.all(restoreWaits);
    const restoredRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    assert.equal(storedVector.metadata.topic_revision, restoredRevision);

    releaseLookup.resolve();
    await Promise.all(reconcileWaits);
    assert.notEqual(restoredRevision, originalRevision);
    assert.equal(upserts.length, 2);
    assert.equal(storedVector.metadata.topic_revision, restoredRevision);
    assert.deepEqual(storedVector.values, embedding(0, 1));
  } finally {
    database.close();
  }
});

test("AI-disabled hard delete still submits vector deletion", async () => {
  const database = openDatabase();
  try {
    let deletedIds;
    const env = baseEnvironment(d1Adapter(database), {
      MEMORY_VECTORS: {
        deleteByIds: async (ids) => {
          deletedIds = ids;
          return { mutationId: "hard-delete" };
        },
      },
    });
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "disabled-hard-delete",
      customId: "canonical",
      content: "Stored without enrichment.",
    }), env, { waitUntil() {} });
    const { id } = await saved.json();

    const waits = [];
    const deleted = await worker.fetch(request(`/v3/documents/${id}`, undefined, "DELETE"), env, {
      waitUntil: (promise) => waits.push(promise),
    });
    assert.equal(deleted.status, 204);
    await Promise.all(waits);
    assert.deepEqual(deletedIds, [id]);
    assert.equal(database.prepare("SELECT id FROM memories WHERE id = ?").get(id), undefined);
  } finally {
    database.close();
  }
});

test("capture reuse ignores forgotten rows and restores a forgotten shared capture", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const ctx = { waitUntil() {} };
    const captureKey = "capture-session:forgotten:0123456789abcdef";
    const customId = `codex-turn-v2:${captureKey}`;
    const metadata = {
      captureVersion: 2,
      captureKey,
      sm_project_id: "forgotten-project",
    };

    const legacy = await worker.fetch(request("/v3/documents", {
      containerTag: "forgotten-project",
      customId,
      content: "Forgotten legacy capture.",
      metadata,
    }), env, ctx);
    const legacyId = (await legacy.json()).id;
    const forgottenLegacy = await worker.fetch(request("/v4/memories", {
      containerTag: "forgotten-project",
      content: "Forgotten legacy capture.",
    }, "DELETE"), env, ctx);
    assert.equal(forgottenLegacy.status, 200);

    const active = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId,
      content: "Active shared capture.",
      metadata,
    }), env, ctx);
    const activeId = (await active.json()).id;
    assert.notEqual(activeId, legacyId);

    const reused = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId,
      content: "This content must not replace the active capture.",
      metadata,
      reuseExistingCapture: true,
    }), env, ctx);
    const reusedBody = await reused.json();
    assert.equal(reused.status, 201);
    assert.equal(reusedBody.id, activeId);
    assert.equal(reusedBody.reusedExistingCapture, true);
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(legacyId).is_forgotten, 1);

    const restoreKey = "capture-session:restore:fedcba9876543210";
    const restoreCustomId = `codex-turn-v2:${restoreKey}`;
    const restoreMetadata = {
      captureVersion: 2,
      captureKey: restoreKey,
      sm_project_id: "memories",
    };
    const original = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId: restoreCustomId,
      content: "Capture before forget.",
      metadata: restoreMetadata,
    }), env, ctx);
    const originalId = (await original.json()).id;
    const forgotten = await worker.fetch(request("/v4/memories", {
      containerTag: "memories",
      content: "Capture before forget.",
    }, "DELETE"), env, ctx);
    assert.equal(forgotten.status, 200);

    const restored = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId: restoreCustomId,
      content: "Capture after forget.",
      metadata: restoreMetadata,
      reuseExistingCapture: true,
    }), env, ctx);
    const restoredBody = await restored.json();
    assert.equal(restored.status, 201);
    assert.equal(restoredBody.id, originalId);
    assert.equal(restoredBody.reusedExistingCapture, undefined);

    const restoredDocument = await worker.fetch(new Request(`https://memory.example/v3/documents/${originalId}`, {
      headers: { Authorization: "Bearer test-key" },
    }), env, ctx);
    assert.equal(restoredDocument.status, 200);
    assert.equal((await restoredDocument.json()).content, "Capture after forget.");
    assert.deepEqual(
      { ...database.prepare("SELECT is_forgotten, content FROM memories WHERE id = ?").get(originalId) },
      { is_forgotten: 0, content: "Capture after forget." },
    );
  } finally {
    database.close();
  }
});

test("patch rejects forgotten documents without changing them", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const ctx = { waitUntil() {} };
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forgotten-patch",
      customId: "target",
      content: "Original forgotten content.",
      metadata: { note: "original" },
    }), env, ctx);
    const id = (await saved.json()).id;
    const forgotten = await worker.fetch(request("/v4/memories", {
      containerTag: "forgotten-patch",
      content: "Original forgotten content.",
    }, "DELETE"), env, ctx);
    assert.equal(forgotten.status, 200);
    const beforePatch = { ...database.prepare("SELECT * FROM memories WHERE id = ?").get(id) };

    const patched = await worker.fetch(request(`/v3/documents/${id}`, {
      content: "Mutated forgotten content.",
      metadata: { note: "mutated" },
    }, "PATCH"), env, ctx);
    assert.equal(patched.status, 404);
    assert.deepEqual(
      { ...database.prepare("SELECT * FROM memories WHERE id = ?").get(id) },
      beforePatch,
    );
  } finally {
    database.close();
  }
});

test("patch loses to a same-millisecond forget after its initial read", async () => {
  const OriginalDate = globalThis.Date;
  const fixedTime = "2026-09-11T01:00:00.000Z";
  globalThis.Date = class extends OriginalDate {
    constructor(...args) {
      super(...(args.length === 0 ? [fixedTime] : args));
    }
    static now() {
      return OriginalDate.parse(fixedTime);
    }
  };

  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const env = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "forget-patch-race",
      customId: "target",
      content: "Original race content.",
      metadata: { note: "original" },
    }), env, ctx);
    const id = (await saved.json()).id;
    let intercepted = false;
    let rowAfterForget;
    DB.interceptFirst(async (sql, row) => {
      if (intercepted || !sql.includes("SELECT container_tag, content, metadata_json, updated_at, topic_revision FROM memories WHERE id = ? AND is_forgotten = 0")) {
        return row;
      }
      intercepted = true;
      const forgotten = await worker.fetch(request("/v4/memories", {
        containerTag: "forget-patch-race",
        content: "Original race content.",
      }, "DELETE"), env, ctx);
      assert.equal(forgotten.status, 200);
      rowAfterForget = { ...database.prepare("SELECT * FROM memories WHERE id = ?").get(id) };
      return row;
    });

    const patched = await worker.fetch(request(`/v3/documents/${id}`, {
      content: "Race mutation must not persist.",
      metadata: { note: "mutated" },
    }, "PATCH"), env, ctx);
    assert.equal(patched.status, 409);
    assert.equal(intercepted, true);
    assert.deepEqual(
      { ...database.prepare("SELECT * FROM memories WHERE id = ?").get(id) },
      rowAfterForget,
    );
  } finally {
    database.close();
    globalThis.Date = OriginalDate;
  }
});

test("stores valid facts while empty normalized topics fail independently", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const waits = [];
    const fact = {
      subject: "User",
      predicate: "uses",
      object: "D1",
      confidence: 0.9,
      exclusive: false,
    };
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding()] }
          : enrichment([fact], ["__unclassified__", "0123456789abcdef"]),
      },
      MEMORY_VECTORS: {
        upsert: async () => ({ mutationId: "topic-empty" }),
      },
    });

    const response = await worker.fetch(request("/v3/documents", {
      containerTag: "empty-topics",
      customId: "facts-survive",
      content: "The user uses D1.",
    }), env, { waitUntil: (promise) => waits.push(promise) });
    const { id } = await response.json();
    await Promise.all(waits);

    const stored = database.prepare(
      "SELECT fact_status, topic_status FROM memories WHERE id = ?",
    ).get(id);
    const facts = database.prepare("SELECT subject, predicate, object FROM facts WHERE source_memory_id = ?").all(id);
    const topics = database.prepare("SELECT topic FROM memory_topics WHERE memory_id = ?").all(id);
    assert.deepEqual({ ...stored }, { fact_status: "done", topic_status: "failed" });
    assert.deepEqual(facts.map((row) => ({ ...row })), [{ subject: "User", predicate: "uses", object: "D1" }]);
    assert.deepEqual(topics.map((row) => ({ ...row })), []);

    waits.length = 0;
    const explicitResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "empty-topics",
      customId: "explicit-topics",
      content: "The user uses D1.",
      metadata: { topics: ["Database"] },
    }), env, { waitUntil: (promise) => waits.push(promise) });
    const explicitId = (await explicitResponse.json()).id;
    await Promise.all(waits);
    assert.equal(
      database.prepare("SELECT topic_status FROM memories WHERE id = ?").get(explicitId).topic_status,
      "done",
    );
    assert.deepEqual(
      database.prepare("SELECT topic FROM memory_topics WHERE memory_id = ?").all(explicitId)
        .map((row) => row.topic),
      ["Database"],
    );
  } finally {
    database.close();
  }
});

test("upsert and patch atomically clear derived facts and restore unsupported facts", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const ctx = { waitUntil() {} };
    const previousResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-updates",
      customId: "previous",
      content: "The color was green.",
    }), env, ctx);
    const targetResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-updates",
      customId: "target",
      content: "The color is blue.",
    }), env, ctx);
    const previousId = (await previousResponse.json()).id;
    const targetId = (await targetResponse.json()).id;
    const now = new Date().toISOString();

    const seedSupersedingFact = (suffix) => {
      const previousFactId = `previous-${suffix}`;
      const targetFactId = `target-${suffix}`;
      database.prepare(
        `INSERT INTO facts(
           id, container_tag, source_memory_id, subject, predicate, object,
           subject_key, predicate_key, object_key, is_exclusive, confidence, status,
           created_at, updated_at
         ) VALUES (?, 'fact-updates', ?, 'User', 'color', 'green', 'user', 'color', 'green', 1, 0.8, 'superseded', ?, ?)`,
      ).run(previousFactId, previousId, now, now);
      database.prepare(
        `INSERT INTO facts(
           id, container_tag, source_memory_id, subject, predicate, object,
           subject_key, predicate_key, object_key, is_exclusive, confidence, status,
           created_at, updated_at
         ) VALUES (?, 'fact-updates', ?, 'User', 'color', 'blue', 'user', 'color', 'blue', 1, 0.9, 'active', ?, ?)`,
      ).run(targetFactId, targetId, now, now);
      database.prepare(
        `INSERT INTO fact_relations(
           id, container_tag, from_fact_id, relation, to_fact_id,
           source_memory_id, confidence, created_at
         ) VALUES (?, 'fact-updates', ?, 'supersedes', ?, ?, 0.8, ?)`,
      ).run(`relation-${suffix}`, targetFactId, previousFactId, targetId, now);
      return previousFactId;
    };

    let previousFactId = seedSupersedingFact("upsert");
    const upserted = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-updates",
      customId: "target",
      content: "The color changed without enrichment.",
    }), env, ctx);
    assert.equal(upserted.status, 201);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(targetId).count, 0);
    assert.equal(database.prepare("SELECT status FROM facts WHERE id = ?").get(previousFactId).status, "active");

    database.prepare("DELETE FROM facts WHERE id = ?").run(previousFactId);
    previousFactId = seedSupersedingFact("patch");
    const patched = await worker.fetch(request(`/v3/documents/${targetId}`, {
      content: "The color changed again without enrichment.",
    }, "PATCH"), env, ctx);
    assert.equal(patched.status, 200);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(targetId).count, 0);
    assert.equal(database.prepare("SELECT status FROM facts WHERE id = ?").get(previousFactId).status, "active");
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM fact_relations").get().count, 0);
  } finally {
    database.close();
  }
});

test("stale enrichment cannot mutate current facts or enqueue a stale vector", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const ctx = { waits: [], waitUntil(promise) { this.waits.push(promise); } };
    const env = baseEnvironment(DB);
    const previousResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-race",
      customId: "previous",
      content: "The color was green.",
    }), env, ctx);
    const targetResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-race",
      customId: "target",
      content: "The color is blue.",
    }), env, ctx);
    const previousId = (await previousResponse.json()).id;
    const targetId = (await targetResponse.json()).id;
    const now = new Date().toISOString();
    database.prepare(
      `INSERT INTO facts(
         id, container_tag, source_memory_id, subject, predicate, object,
         subject_key, predicate_key, object_key, is_exclusive, confidence, status,
         created_at, updated_at
       ) VALUES ('previous-green', 'fact-race', ?, 'User', 'color', 'green',
                 'user', 'color', 'green', 1, 0.8, 'active', ?, ?)`,
    ).run(previousId, now, now);

    const embeddingResult = deferred();
    const enrichmentResult = deferred();
    const aiStarted = deferred();
    let aiCalls = 0;
    let vectorUpserts = 0;
    env.AI_ENRICHMENT_MODE = "on";
    env.AI = {
      run: async (model) => {
        aiCalls += 1;
        if (aiCalls === 2) aiStarted.resolve();
        return model.includes("bge-m3") ? embeddingResult.promise : enrichmentResult.promise;
      },
    };
    env.MEMORY_VECTORS = {
      upsert: async () => {
        vectorUpserts += 1;
        return { mutationId: "stale" };
      },
    };

    const retry = await worker.fetch(request("/v4/enrich", { id: targetId }), env, ctx);
    assert.equal(retry.status, 202);
    await aiStarted.promise;

    let raced = false;
    DB.interceptRun(async (sql) => {
      if (raced || !sql.includes("SET embedding_status = 'done'")) return;
      raced = true;
      env.AI_ENRICHMENT_MODE = "off";
      const patched = await worker.fetch(request(`/v3/documents/${targetId}`, {
        content: "The current color is red.",
      }, "PATCH"), env, ctx);
      assert.equal(patched.status, 200);
      const currentNow = new Date().toISOString();
      database.prepare(
        `INSERT INTO facts(
           id, container_tag, source_memory_id, subject, predicate, object,
           subject_key, predicate_key, object_key, is_exclusive, confidence, status,
           created_at, updated_at
         ) VALUES ('current-red', 'fact-race', ?, 'User', 'color', 'red',
                   'user', 'color', 'red', 1, 0.95, 'active', ?, ?)`,
      ).run(targetId, currentNow, currentNow);
      database.prepare("UPDATE facts SET status = 'superseded' WHERE id = 'previous-green'").run();
      database.prepare(
        `INSERT INTO fact_relations(
           id, container_tag, from_fact_id, relation, to_fact_id,
           source_memory_id, confidence, created_at
         ) VALUES ('current-relation', 'fact-race', 'current-red', 'supersedes',
                   'previous-green', ?, 0.8, ?)`,
      ).run(targetId, currentNow);
      env.AI_ENRICHMENT_MODE = "on";
    });

    embeddingResult.resolve({ data: [embedding()] });
    enrichmentResult.resolve(enrichment([{
      subject: "User",
      predicate: "color",
      object: "blue",
      confidence: 0.9,
      exclusive: true,
    }], ["Color"]));
    await Promise.all(ctx.waits);

    assert.equal(raced, true);
    assert.equal(vectorUpserts, 0);
    assert.deepEqual(
      database.prepare("SELECT id, object, status FROM facts ORDER BY id").all().map((row) => ({ ...row })),
      [
        { id: "current-red", object: "red", status: "active" },
        { id: "previous-green", object: "green", status: "superseded" },
      ],
    );
    assert.deepEqual(
      database.prepare("SELECT id, from_fact_id, to_fact_id FROM fact_relations").all().map((row) => ({ ...row })),
      [{ id: "current-relation", from_fact_id: "current-red", to_fact_id: "previous-green" }],
    );
  } finally {
    database.close();
  }
});

test("semantic search ignores stale vector revisions and reconciliation replaces them", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const response = await worker.fetch(request("/v3/documents", {
      containerTag: "vector-revision",
      customId: "semantic",
      content: "Unrelated stored text.",
    }), baseEnvironment(DB), { waitUntil() {} });
    const { id } = await response.json();
    const row = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id);
    database.prepare(
      "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'pending' WHERE id = ?",
    ).run(JSON.stringify(embedding(0, 1)), id);

    let storedVector = { id, values: embedding(1, 0), metadata: { topic_revision: "stale-revision" } };
    const upserts = [];
    let queryOptions;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async (_vector, options) => {
          queryOptions = options;
          return {
            matches: [{ id, score: 0.99, metadata: storedVector.metadata }],
            count: 1,
          };
        },
        getByIds: async () => [storedVector],
        upsert: async (vectors) => {
          upserts.push(vectors);
          [storedVector] = vectors;
          return { mutationId: "reconciled" };
        },
      },
    });

    const search = await worker.fetch(request("/v4/search", {
      containerTag: "vector-revision",
      q: "semantic needle",
    }), env, { waitUntil() {} });
    const searchBody = await search.json();
    assert.equal(searchBody.results.length, 1);
    assert.equal(searchBody.results[0].similarity, 0);
    assert.equal(searchBody.results[0].semanticSimilarity, 0);
    assert.equal(queryOptions.returnMetadata, "all");

    const firstWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => firstWaits.push(promise) });
    await Promise.all(firstWaits);
    assert.equal(upserts.length, 1);
    assert.equal(upserts[0][0].metadata.topic_revision, row.topic_revision);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "queued");

    const secondWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => secondWaits.push(promise) });
    await Promise.all(secondWaits);
    assert.equal(upserts.length, 1);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "indexed");

    const currentSearch = await worker.fetch(request("/v4/search", {
      containerTag: "vector-revision",
      q: "semantic needle",
    }), env, { waitUntil() {} });
    const currentSearchBody = await currentSearch.json();
    assert.equal(currentSearchBody.results[0].similarity, 0.99);
    assert.equal(currentSearchBody.results[0].semanticSimilarity, 0.99);
  } finally {
    database.close();
  }
});

test("vector status and reconciliation split Vectorize lookups into batches of 20", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    database.prepare(
      "INSERT INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)",
    ).run("vector-batches", "Vector batches", "2026-01-01", "2026-01-01");
    const insert = database.prepare(
      `INSERT INTO memories(
         id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
         embedding_status, fact_status, embedding_json, vector_status, topic_status,
         topic_revision, created_at, updated_at
       ) VALUES (?, ?, 'vector-batches', 'Stored memory', '{}', 'done', 0,
         'done', 'done', ?, 'queued', 'done', ?, '2026-01-01', '2026-01-01')`,
    );
    for (let index = 0; index < 45; index += 1) {
      const id = `vector-batch-${index}`;
      insert.run(id, id, JSON.stringify(embedding()), `revision-${index}`);
    }

    const batches = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      MEMORY_VECTORS: {
        describe: async () => ({ name: "test-index" }),
        getByIds: async (ids) => {
          assert.ok(ids.length <= 20);
          batches.push(ids.length);
          return ids.map((id) => ({ id, metadata: { topic_revision: `revision-${id.slice("vector-batch-".length)}` } }));
        },
      },
    });

    const status = await worker.fetch(request("/v4/vector-status", undefined, "GET"), env, { waitUntil() {} });
    assert.equal(status.status, 200);
    assert.equal((await status.json()).visibleActiveVectors, 45);
    assert.deepEqual(batches, [20, 20, 5]);

    batches.length = 0;
    const waits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
    await Promise.all(waits);
    assert.deepEqual(batches, [20, 20, 5]);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM memories WHERE vector_status = 'indexed'").get().count, 45);
  } finally {
    database.close();
  }
});

test("reconciliation status updates cannot overwrite a newer document revision", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const env = baseEnvironment(DB);
    const response = await worker.fetch(request("/v3/documents", {
      containerTag: "reconcile-race",
      customId: "race",
      content: "Old vector content.",
    }), env, { waitUntil() {} });
    const { id } = await response.json();
    const oldRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    database.prepare(
      "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'pending' WHERE id = ?",
    ).run(JSON.stringify(embedding()), id);

    const getResult = deferred();
    const getStarted = deferred();
    const upserts = [];
    env.AI_ENRICHMENT_MODE = "on";
    env.MEMORY_VECTORS = {
      getByIds: async () => {
        getStarted.resolve();
        return getResult.promise;
      },
      upsert: async (vectors) => {
        upserts.push(vectors);
        return { mutationId: "old-revision" };
      },
    };

    const waits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
    await getStarted.promise;
    env.AI_ENRICHMENT_MODE = "off";
    const patched = await worker.fetch(request(`/v3/documents/${id}`, {
      content: "New vector content.",
    }, "PATCH"), env, { waitUntil() {} });
    assert.equal(patched.status, 200);
    getResult.resolve([{ id, values: embedding(), metadata: { topic_revision: "stale" } }]);
    await Promise.all(waits);

    assert.equal(upserts.length, 1);
    assert.equal(upserts[0][0].metadata.topic_revision, oldRevision);
    assert.deepEqual(
      { ...database.prepare("SELECT embedding_json, vector_status FROM memories WHERE id = ?").get(id) },
      { embedding_json: null, vector_status: "disabled" },
    );
  } finally {
    database.close();
  }
});

test("reconciliation deletes a stale vector upsert that completes after hard delete", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const created = await worker.fetch(request("/v3/documents", {
      containerTag: "reconcile-hard-delete",
      customId: "canonical",
      content: "Stored memory.",
    }), baseEnvironment(DB), { waitUntil() {} });
    const { id } = await created.json();
    database.prepare(
      "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'pending' WHERE id = ?",
    ).run(JSON.stringify(embedding()), id);

    const upsertStarted = deferred();
    const releaseUpsert = deferred();
    let deleteCalls = 0;
    let storedVector = { id, values: embedding(0, 1), metadata: { topic_revision: "stale" } };
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      MEMORY_VECTORS: {
        getByIds: async () => [storedVector].filter(Boolean),
        upsert: async (vectors) => {
          upsertStarted.resolve();
          await releaseUpsert.promise;
          [storedVector] = vectors;
          return { mutationId: "stale-upsert" };
        },
        deleteByIds: async () => {
          deleteCalls += 1;
          storedVector = undefined;
          return { mutationId: `delete-${deleteCalls}` };
        },
      },
    });

    const reconcileWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => reconcileWaits.push(promise) });
    await upsertStarted.promise;

    const deleteWaits = [];
    const deleted = await worker.fetch(request(`/v3/documents/${id}`, undefined, "DELETE"), env, {
      waitUntil: (promise) => deleteWaits.push(promise),
    });
    assert.equal(deleted.status, 204);
    await Promise.all(deleteWaits);
    releaseUpsert.resolve();
    await Promise.all(reconcileWaits);

    assert.equal(deleteCalls, 2);
    assert.equal(storedVector, undefined);
    assert.equal(database.prepare("SELECT id FROM memories WHERE id = ?").get(id), undefined);
  } finally {
    database.close();
  }
});

test("reconciliation repairs a stale upsert when the active revision changes", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const created = await worker.fetch(request("/v3/documents", {
      containerTag: "reconcile-new-revision",
      customId: "canonical",
      content: "Old memory.",
    }), baseEnvironment(DB), { waitUntil() {} });
    const { id } = await created.json();
    const oldRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    database.prepare(
      "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'pending' WHERE id = ?",
    ).run(JSON.stringify(embedding(1, 0)), id);

    const lookupStarted = deferred();
    const releaseLookup = deferred();
    const upserts = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      MEMORY_VECTORS: {
        getByIds: async () => {
          lookupStarted.resolve();
          return releaseLookup.promise;
        },
        upsert: async (vectors) => {
          upserts.push(vectors);
          return { mutationId: `revision-${upserts.length}` };
        },
      },
    });

    const waits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
    await lookupStarted.promise;
    const currentRevision = "current-revision";
    database.prepare(
      `UPDATE memories
       SET content = 'Current memory.', embedding_json = ?, vector_status = 'pending', topic_revision = ?
       WHERE id = ?`,
    ).run(JSON.stringify(embedding(0, 1)), currentRevision, id);
    releaseLookup.resolve([{ id, values: embedding(), metadata: { topic_revision: "stale" } }]);
    await Promise.all(waits);

    assert.notEqual(currentRevision, oldRevision);
    assert.equal(upserts.length, 2);
    assert.equal(upserts[0][0].metadata.topic_revision, oldRevision);
    assert.equal(upserts[1][0].metadata.topic_revision, currentRevision);
    assert.deepEqual(upserts[1][0].values, embedding(0, 1));
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "queued");
  } finally {
    database.close();
  }
});

test("fact replacement limits candidates before relation classification and supersedes only related facts", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const baseEnv = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const previousFacts = [
      ["non-match-old", "preference", "match", "2026-01-01T00:00:00.000Z"],
      ["non-recent-3", "preference", "other-3", "2026-01-02T00:00:00.000Z"],
      ["non-recent-2", "preference", "other-2", "2026-01-03T00:00:00.000Z"],
      ["non-recent-1", "preference", "other-1", "2026-01-04T00:00:00.000Z"],
      ["exclusive-old-c", "backend", "old-c", "2026-01-05T00:00:00.000Z"],
      ["exclusive-old-b", "backend", "old-b", "2026-01-06T00:00:00.000Z"],
      ["exclusive-old-a", "backend", "old-a", "2026-01-07T00:00:00.000Z"],
      ["exclusive-same", "backend", "new", "2026-01-08T00:00:00.000Z"],
    ];
    for (const [customId, predicate, object, updatedAt] of previousFacts) {
      const response = await worker.fetch(request("/v3/documents", {
        containerTag: "fact-candidates",
        customId,
        content: `${predicate}: ${object}`,
      }), baseEnv, ctx);
      const { id } = await response.json();
      database.prepare(
        `INSERT INTO facts(
           id, container_tag, source_memory_id, subject, predicate, object,
           subject_key, predicate_key, object_key, is_exclusive, confidence, status,
           created_at, updated_at
         ) VALUES (?, 'fact-candidates', ?, 'User', ?, ?, 'user', ?, ?, 0, 0.8, 'active', ?, ?)`,
      ).run(`fact-${customId}`, id, predicate, object, predicate, object, updatedAt, updatedAt);
    }

    const targetResponse = await worker.fetch(request("/v3/documents", {
      containerTag: "fact-candidates",
      customId: "target",
      content: "Current durable facts.",
      metadata: { topics: ["Facts"] },
    }), baseEnv, ctx);
    const targetId = (await targetResponse.json()).id;
    const waits = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model) => model.includes("bge-m3")
          ? { data: [embedding()] }
          : enrichment([
            {
              subject: "User",
              predicate: "preference",
              object: "match",
              confidence: 0.9,
              exclusive: false,
            },
            {
              subject: "User",
              predicate: "backend",
              object: "new",
              confidence: 0.95,
              exclusive: true,
            },
          ], ["Facts"]),
      },
      MEMORY_VECTORS: { upsert: async () => ({ mutationId: "facts" }) },
    });
    const retry = await worker.fetch(request("/v4/enrich", { id: targetId }), env, {
      waitUntil: (promise) => waits.push(promise),
    });
    assert.equal(retry.status, 202);
    await Promise.all(waits);

    const relations = database.prepare(
      `SELECT newer.predicate, relation.relation, older.object
       FROM fact_relations AS relation
       JOIN facts AS newer ON newer.id = relation.from_fact_id
       JOIN facts AS older ON older.id = relation.to_fact_id
       WHERE relation.source_memory_id = ?
       ORDER BY newer.predicate, relation.relation, older.object`,
    ).all(targetId).map((row) => ({ ...row }));
    assert.deepEqual(relations, [
      { predicate: "backend", relation: "supersedes", object: "old-a" },
      { predicate: "backend", relation: "supersedes", object: "old-b" },
      { predicate: "backend", relation: "supports", object: "new" },
    ]);
    assert.deepEqual(
      database.prepare(
        "SELECT object, status FROM facts WHERE predicate = 'backend' AND source_memory_id <> ? ORDER BY object",
      ).all(targetId).map((row) => ({ ...row })),
      [
        { object: "new", status: "active" },
        { object: "old-a", status: "superseded" },
        { object: "old-b", status: "superseded" },
        { object: "old-c", status: "active" },
      ],
    );
    assert.equal(
      database.prepare(
        "SELECT COUNT(*) AS count FROM fact_relations AS r JOIN facts AS f ON f.id = r.from_fact_id WHERE f.predicate = 'preference'",
      ).get().count,
      0,
    );
  } finally {
    database.close();
  }
});

test("legacy vector hits beyond the D1 scan are cosine-scored and indexed vectors migrate fairly", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const containerTag = "legacy-vectors";
    database.prepare(
      "INSERT INTO container_tags(tag, name, created_at, updated_at) VALUES (?, ?, ?, ?)",
    ).run(containerTag, "Legacy vectors", "2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z");
    const insert = database.prepare(
      `INSERT INTO memories(
         id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
         embedding_status, fact_status, embedding_json, vector_status, topic_status,
         topic_revision, created_at, updated_at
       ) VALUES (?, ?, ?, ?, '{}', 'done', 0, 'done', 'done', ?, 'indexed', 'done', ?, ?, ?)`,
    );
    const vectors = new Map();
    database.exec("BEGIN");
    try {
      for (let index = 0; index < 201; index += 1) {
        const suffix = String(index).padStart(3, "0");
        const id = `legacy-${suffix}`;
        const revision = `revision-${suffix}`;
        const timestamp = new Date(Date.UTC(2026, 0, 1, 0, 0, index)).toISOString();
        const values = index === 0 ? embedding(1, 0) : embedding(0, 1);
        insert.run(id, id, containerTag, `Stored legacy memory ${suffix}.`, JSON.stringify(values), revision, timestamp, timestamp);
        vectors.set(id, { id, values, metadata: {} });
      }
      database.exec("COMMIT");
    } catch (error) {
      database.exec("ROLLBACK");
      throw error;
    }

    const migrated = new Set();
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async () => ({
          matches: [{ id: "legacy-000", score: -0.5, metadata: {} }],
          count: 1,
        }),
        getByIds: async (ids) => ids.map((id) => vectors.get(id)).filter(Boolean),
        upsert: async (nextVectors) => {
          for (const vector of nextVectors) {
            vectors.set(vector.id, vector);
            migrated.add(vector.id);
          }
          return { mutationId: `legacy-${migrated.size}` };
        },
      },
    });

    const search = await worker.fetch(request("/v4/search", {
      containerTag,
      q: "semantic needle",
    }), env, { waitUntil() {} });
    const searchBody = await search.json();
    assert.equal(searchBody.results[0].id, "legacy-000");
    assert.equal(searchBody.results[0].similarity, 0.99);

    for (let pass = 0; pass < 22; pass += 1) {
      const waits = [];
      worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
      await Promise.all(waits);
    }
    assert.equal(migrated.size, 201);
    assert.equal(
      database.prepare("SELECT COUNT(*) AS count FROM memories WHERE vector_status <> 'indexed'").get().count,
      0,
    );
    for (const [id, vector] of vectors) {
      assert.equal(vector.metadata.topic_revision, database.prepare(
        "SELECT topic_revision FROM memories WHERE id = ?",
      ).get(id).topic_revision);
    }
  } finally {
    database.close();
  }
});

test("a late old vector upsert is repaired while the current D1 revision stays authoritative", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const oldUpsertStarted = deferred();
    const releaseOldUpsert = deferred();
    let upsertCalls = 0;
    let storedVector;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (model, input) => {
          if (!model.includes("bge-m3")) return enrichment([], ["Race"]);
          const content = input.text[0];
          return { data: [content.includes("Old") ? embedding(1, 0) : embedding(0, 1)] };
        },
      },
      MEMORY_VECTORS: {
        upsert: async (vectors) => {
          upsertCalls += 1;
          if (upsertCalls === 1) {
            oldUpsertStarted.resolve();
            await releaseOldUpsert.promise;
          }
          [storedVector] = vectors;
          return { mutationId: `mutation-${upsertCalls}` };
        },
        getByIds: async () => storedVector ? [storedVector] : [],
      },
    });

    const oldWaits = [];
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "late-vector",
      customId: "race",
      content: "Old vector content.",
    }), env, { waitUntil: (promise) => oldWaits.push(promise) });
    const { id } = await saved.json();
    await oldUpsertStarted.promise;
    const oldRevision = storedVector?.metadata?.topic_revision ?? database.prepare(
      "SELECT topic_revision FROM memories WHERE id = ?",
    ).get(id).topic_revision;

    const currentWaits = [];
    const patched = await worker.fetch(request(`/v3/documents/${id}`, {
      content: "Current vector content.",
    }, "PATCH"), env, { waitUntil: (promise) => currentWaits.push(promise) });
    assert.equal(patched.status, 200);
    await Promise.all(currentWaits);
    const currentRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    assert.equal(storedVector.metadata.topic_revision, currentRevision);

    let waits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
    await Promise.all(waits);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "indexed");

    releaseOldUpsert.resolve();
    await Promise.all(oldWaits);
    assert.notEqual(oldRevision, currentRevision);
    assert.equal(storedVector.metadata.topic_revision, currentRevision);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "queued");

    waits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => waits.push(promise) });
    await Promise.all(waits);
    assert.equal(storedVector.metadata.topic_revision, currentRevision);
    assert.equal(database.prepare("SELECT vector_status FROM memories WHERE id = ?").get(id).vector_status, "indexed");
  } finally {
    database.close();
  }
});

test("search and document-list compact projections omit full content without changing defaults", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const metadata = {
      title: "Projection index",
      filepath: "docs/projection.md",
      memoryIndex: {
        version: 1,
        title: "Projection index",
        description: "Find the compact projection",
        sections: ["Projection"],
        recallable: true,
        sourceKind: "conversation",
      },
      topics: ["Projection"],
    };
    const saved = await worker.fetch(request("/v3/documents", {
      containerTag: "projection-space",
      customId: "projection-document",
      content: "Projection needle with full private detail.",
      metadata,
    }), env, { waitUntil() {} });
    const { id } = await saved.json();

    const fullSearch = await worker.fetch(request("/v4/search", {
      containerTag: "projection-space",
      q: "Projection needle",
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.match(fullSearch.results[0].content, /full private detail/);
    assert.equal(fullSearch.results[0].memory, fullSearch.results[0].content);

    const indexSearch = await worker.fetch(request("/v4/search", {
      containerTag: "projection-space",
      q: "Projection needle",
      indexOnly: true,
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.equal(indexSearch.results[0].id, id);
    assert.equal("content" in indexSearch.results[0], false);
    assert.equal("memory" in indexSearch.results[0], false);
    assert.equal("summary" in indexSearch.results[0], false);
    for (const field of [
      "metadata", "topics", "provenance", "title", "filepath", "containerTag",
      "createdAt", "updatedAt", "similarity", "score",
    ]) assert.equal(field in indexSearch.results[0], true, field);

    const fullList = await worker.fetch(request("/v3/documents/list", {
      containerTag: "projection-space",
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.match(fullList.documents[0].content, /full private detail/);
    assert.equal(fullList.memoryEntries[0].content, fullList.documents[0].content);

    const indexList = await worker.fetch(request("/v3/documents/list", {
      containerTag: "projection-space",
      projection: "index",
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.equal(indexList.documents[0].id, id);
    assert.match(indexList.documents[0].summary, /Projection needle/);
    assert.equal("content" in indexList.documents[0], false);
    assert.equal("enrichment" in indexList.documents[0], false);
    assert.equal("memoryEntries" in indexList, false);

    const idsList = await worker.fetch(request("/v3/documents/list", {
      containerTag: "projection-space",
      projection: "ids",
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.deepEqual(idsList.documents, [{ id }]);
    assert.equal("memoryEntries" in idsList, false);

    const captureList = await worker.fetch(request("/v3/documents/list", {
      containerTag: "projection-space",
      projection: "capture",
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.deepEqual(captureList.documents, [{ id, metadata }]);
    assert.equal("memoryEntries" in captureList, false);

    const invalidProjection = await worker.fetch(request("/v3/documents/list", {
      projection: "unknown",
    }), env, { waitUntil() {} });
    assert.equal(invalidProjection.status, 400);
    const invalidIndexOnly = await worker.fetch(request("/v4/search", {
      containerTag: "projection-space",
      indexOnly: "yes",
    }), env, { waitUntil() {} });
    assert.equal(invalidIndexOnly.status, 400);

    const legacyContent = `${"old prefix ".repeat(700)}\n# Tail heading\nTailNeedle appears near the end.`;
    const legacy = await worker.fetch(request("/v3/documents", {
      containerTag: "legacy-summary-space",
      customId: "legacy-tail",
      content: legacyContent,
    }), env, { waitUntil() {} }).then((response) => response.json());
    const legacySearch = await worker.fetch(request("/v4/search", {
      containerTag: "legacy-summary-space",
      q: "TailNeedle",
      indexOnly: true,
    }), env, { waitUntil() {} }).then((response) => response.json());
    assert.equal(legacySearch.results[0].id, legacy.id);
    assert.equal("content" in legacySearch.results[0], false);
    assert.equal("memory" in legacySearch.results[0], false);
    assert.equal(legacySearch.results[0].metadata.memoryIndex.version, 1);
    assert.equal(legacySearch.results[0].metadata.memoryIndex.title, "Tail heading");
    assert.match(legacySearch.results[0].searchExcerpt, /# Tail heading\nTailNeedle/);
    assert.ok(legacySearch.results[0].searchExcerpt.length <= 240);
    assert.equal("summary" in legacySearch.results[0], false);
  } finally {
    database.close();
  }
});

test("FTS score preserves stronger-first order and indexOnly prioritizes index hits before body excerpts", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    let ftsPages = 0;
    let instrPages = 0;
    DB.interceptAll((sql, rows) => {
      if (sql.includes("FROM memories_fts")) ftsPages += 1;
      if (sql.includes("instr(lower(content)")) instrPages += 1;
      return rows;
    });
    const env = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const stronger = await worker.fetch(request("/v3/documents", {
      containerTag: "bm25-order",
      customId: "stronger",
      content: "RankNeedle ".repeat(30),
    }), env, ctx).then((response) => response.json());
    const weaker = await worker.fetch(request("/v3/documents", {
      containerTag: "bm25-order",
      customId: "weaker",
      content: `RankNeedle ${"unrelated padding ".repeat(200)}`,
    }), env, ctx).then((response) => response.json());
    const ranked = await worker.fetch(request("/v4/search", {
      containerTag: "bm25-order",
      q: "RankNeedle",
      limit: 5,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(ranked.results.map(({ id }) => id), [stronger.id, weaker.id]);
    assert.ok(ranked.results[0].similarity > ranked.results[1].similarity);
    assert.ok(ranked.results.every(({ similarity }) => similarity < 0.61));
    assert.deepEqual(ranked.results.map(({ lexicalSimilarity }) => lexicalSimilarity),
      ranked.results.map(({ similarity }) => similarity));
    assert.equal(ranked.results[0].semanticSimilarity, null);

    const unrelatedIndex = (title) => ({
      version: 1,
      title,
      description: "別件の索引",
      sections: ["別件"],
      recallable: true,
      sourceKind: "conversation",
    });
    for (let index = 0; index < 70; index += 1) {
      await worker.fetch(request("/v3/documents", {
        containerTag: "index-crowding",
        customId: `body-only-${index}`,
        content: `${"CrowdingNeedle ".repeat(20)}本文だけの候補 ${index}`,
        metadata: { memoryIndex: unrelatedIndex(`無関係 ${index}`) },
      }), env, ctx);
    }
    const indexed = await worker.fetch(request("/v3/documents", {
      containerTag: "index-crowding",
      customId: "indexed",
      content: `CrowdingNeedle 索引にも一致する候補 ${"padding ".repeat(100)}`,
      metadata: { memoryIndex: unrelatedIndex("CrowdingNeedle の索引") },
    }), env, ctx).then((response) => response.json());
    const topic = await worker.fetch(request("/v3/documents", {
      containerTag: "index-crowding",
      customId: "topic",
      content: `CrowdingNeedle topicにも一致する候補 ${"padding ".repeat(100)}`,
      metadata: {
        memoryIndex: unrelatedIndex("別名の索引"),
        topics: ["CrowdingNeedle topic"],
      },
    }), env, ctx).then((response) => response.json());
    ftsPages = 0;
    const compact = await worker.fetch(request("/v4/search", {
      containerTag: "index-crowding",
      q: "CrowdingNeedle",
      limit: 20,
      indexOnly: true,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(new Set(compact.results.slice(0, 2).map(({ id }) => id)), new Set([indexed.id, topic.id]));
    assert.ok(compact.results.slice(2).every((result) => result.searchExcerpt?.includes("CrowdingNeedle")));
    assert.ok(compact.results.every((result) => !("content" in result) && !("memory" in result)));
    assert.equal(ftsPages, 1, "index priority should select useful entries in one bounded FTS read");

    const ecole = await worker.fetch(request("/v3/documents", {
      containerTag: "unicode-index",
      customId: "ecole",
      content: "école lexical body",
      metadata: { memoryIndex: unrelatedIndex("ÉCOLE deployment") },
    }), env, ctx).then((response) => response.json());
    const athens = await worker.fetch(request("/v3/documents", {
      containerTag: "unicode-index",
      customId: "athens",
      content: "αθήνα lexical body",
      metadata: {
        memoryIndex: unrelatedIndex("別名の索引"),
        topics: ["ΑΘΉΝΑ"],
      },
    }), env, ctx).then((response) => response.json());
    const ecoleSearch = await worker.fetch(request("/v4/search", {
      containerTag: "unicode-index",
      q: "école",
      indexOnly: true,
    }), env, ctx).then((response) => response.json());
    const athensSearch = await worker.fetch(request("/v4/search", {
      containerTag: "unicode-index",
      q: "αθήνα",
      indexOnly: true,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(ecoleSearch.results.map(({ id }) => id), [ecole.id]);
    assert.deepEqual(athensSearch.results.map(({ id }) => id), [athens.id]);

    const fullwidthTarget = await worker.fetch(request("/v3/documents", {
      containerTag: "fts-instr-fallback",
      customId: "fullwidth-target",
      content: "ＡＢＣ の設定",
      metadata: { memoryIndex: unrelatedIndex("ＡＢＣ の設定") },
    }), env, ctx).then((response) => response.json());
    await worker.fetch(request("/v3/documents", {
      containerTag: "fts-instr-fallback",
      customId: "ascii-body-only",
      content: "ABC の unrelated body",
      metadata: { memoryIndex: unrelatedIndex("別件の索引") },
    }), env, ctx);
    ftsPages = 0;
    instrPages = 0;
    const fullwidthSearch = await worker.fetch(request("/v4/search", {
      containerTag: "fts-instr-fallback",
      q: "ＡＢＣ",
      indexOnly: true,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(fullwidthSearch.results.map(({ id }) => id), [fullwidthTarget.id]);
    assert.equal(fullwidthSearch.results[0].lexicalSimilarity, 0);
    assert.equal(fullwidthSearch.results[0].similarity, 0);
    assert.ok(ftsPages > 0, "normalized FTS should observe the ASCII body-only distractor");
    assert.ok(instrPages > 0, "zero accepted FTS rows should fall back to exact instr matching");
  } finally {
    database.close();
  }
});

test("indexOnly SQL projects v1 content away across recent, lexical, semantic, and vector hydration paths", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    let selected = [];
    DB.interceptAll((sql, rows) => {
      if (sql.includes("THEN '' ELSE") && sql.includes("AS content")) {
        selected.push({ sql, rows: rows.map((row) => ({ id: row.id, content: row.content })) });
      }
      return rows;
    });
    const plainEnv = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const v1Content = "FtsNeedle Qz v1 private body";
    const legacyContent = "FtsNeedle Qz legacy body retained for summary";
    const v1 = await worker.fetch(request("/v3/documents", {
      containerTag: "content-projection",
      customId: "v1",
      content: v1Content,
      metadata: {
        memoryIndex: {
          version: 1,
          title: "FtsNeedle Qz index",
          description: "projection path",
          sections: [],
          recallable: true,
          sourceKind: "conversation",
        },
      },
    }), plainEnv, ctx).then((response) => response.json());
    const legacy = await worker.fetch(request("/v3/documents", {
      containerTag: "content-projection",
      customId: "legacy",
      content: legacyContent,
    }), plainEnv, ctx).then((response) => response.json());
    const booleanVersionContent = "FtsNeedle Qz boolean version remains legacy";
    const booleanVersion = await worker.fetch(request("/v3/documents", {
      containerTag: "content-projection",
      customId: "boolean-version",
      content: booleanVersionContent,
      metadata: { memoryIndex: { version: true, title: "FtsNeedle Qz boolean" } },
    }), plainEnv, ctx).then((response) => response.json());
    const unknownVersionContent = "FtsNeedle Qz unknown version remains legacy";
    const unknownVersion = await worker.fetch(request("/v3/documents", {
      containerTag: "content-projection",
      customId: "unknown-version",
      content: unknownVersionContent,
      metadata: { memoryIndex: { version: "future", title: "FtsNeedle Qz unknown" } },
    }), plainEnv, ctx).then((response) => response.json());

    const assertProjection = (record) => {
      assert.ok(record, "expected an indexOnly content projection query");
      assert.equal(record.rows.find((row) => row.id === v1.id)?.content, "");
      assert.equal(record.rows.find((row) => row.id === legacy.id)?.content, legacyContent);
      assert.equal(record.rows.find((row) => row.id === booleanVersion.id)?.content, booleanVersionContent);
      assert.equal(record.rows.find((row) => row.id === unknownVersion.id)?.content, unknownVersionContent);
    };

    selected = [];
    await worker.fetch(request("/v4/search", {
      containerTag: "content-projection",
      q: "",
      indexOnly: true,
      limit: 10,
    }), plainEnv, ctx);
    assertProjection(selected.find(({ sql }) => sql.includes("ORDER BY updated_at DESC LIMIT ?")));

    selected = [];
    const lexicalProjection = await worker.fetch(request("/v4/search", {
      containerTag: "content-projection",
      q: "FtsNeedle",
      indexOnly: true,
      limit: 10,
    }), plainEnv, ctx).then((response) => response.json());
    assertProjection(selected.find(({ sql }) => sql.includes("FROM memories_fts")));
    assert.match(lexicalProjection.results.find(({ id }) => id === booleanVersion.id)?.metadata.memoryIndex.description ?? "", /boolean version/);
    assert.match(lexicalProjection.results.find(({ id }) => id === unknownVersion.id)?.metadata.memoryIndex.description ?? "", /unknown version/);

    selected = [];
    await worker.fetch(request("/v4/search", {
      containerTag: "content-projection",
      q: "Qz",
      indexOnly: true,
      limit: 10,
    }), plainEnv, ctx);
    assertProjection(selected.find(({ sql }) => sql.includes("instr(lower(content)")));

    for (const id of [v1.id, legacy.id, booleanVersion.id, unknownVersion.id]) {
      database.prepare(
        "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'failed' WHERE id = ?",
      ).run(JSON.stringify(embedding(1, 0)), id);
    }
    const semanticEnv = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: { query: async () => ({ matches: [], count: 0 }) },
    });
    selected = [];
    const semantic = await worker.fetch(request("/v4/search", {
      containerTag: "content-projection",
      q: "MeaningNeedle",
      indexOnly: true,
      limit: 10,
    }), semanticEnv, ctx).then((response) => response.json());
    assertProjection(selected.find(({ sql }) => sql.includes("vector_status <> 'indexed'")));
    assert.ok(semantic.results.every((result) => result.semanticSimilarity === 1));

    const revisions = new Map(database.prepare(
      "SELECT id, topic_revision FROM memories WHERE id IN (?, ?, ?, ?)",
    ).all(v1.id, legacy.id, booleanVersion.id, unknownVersion.id).map((row) => [row.id, row.topic_revision]));
    database.prepare("UPDATE memories SET vector_status = 'indexed' WHERE id IN (?, ?, ?, ?)")
      .run(v1.id, legacy.id, booleanVersion.id, unknownVersion.id);
    const vectorEnv = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async () => ({
          matches: [v1.id, legacy.id, booleanVersion.id, unknownVersion.id].map((id) => ({
            id,
            score: 0.9,
            metadata: { topic_revision: revisions.get(id) },
          })),
          count: 4,
        }),
      },
    });
    selected = [];
    await worker.fetch(request("/v4/search", {
      containerTag: "content-projection",
      q: "VectorNeedle",
      indexOnly: true,
      limit: 10,
    }), vectorEnv, ctx);
    assertProjection(selected.find(({ sql }) => sql.includes("id IN (") && sql.includes("embedding_json")));
  } finally {
    database.close();
  }
});

test("enrichment-eligible list skips newer recallable-false rows and advances to older targets", async () => {
  const database = openDatabase();
  try {
    const env = baseEnvironment(d1Adapter(database));
    const ctx = { waitUntil() {} };
    const eligible = await worker.fetch(request("/v3/documents", {
      containerTag: "classification-space",
      customId: "eligible",
      content: "Older enrichment target.",
    }), env, ctx).then((response) => response.json());
    const excluded = await worker.fetch(request("/v3/documents", {
      containerTag: "classification-space",
      customId: "excluded",
      content: "Newer manual-only target.",
      metadata: { memoryIndex: { version: 1, title: "Manual only", recallable: false } },
    }), env, ctx).then((response) => response.json());
    database.prepare("UPDATE memories SET created_at = ?, updated_at = ? WHERE id = ?")
      .run("2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z", eligible.id);
    database.prepare("UPDATE memories SET created_at = ?, updated_at = ? WHERE id = ?")
      .run("2026-02-01T00:00:00.000Z", "2026-02-01T00:00:00.000Z", excluded.id);

    const ordinary = await worker.fetch(request("/v3/documents/list", {
      containerTag: "classification-space",
      topic: "__unclassified__",
      projection: "ids",
      page: 1,
      limit: 1,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(ordinary.documents, [{ id: excluded.id }]);
    assert.equal(ordinary.pagination.totalItems, 2);

    const eligibleOnly = await worker.fetch(request("/v3/documents/list", {
      containerTag: "classification-space",
      topic: "__unclassified__",
      projection: "ids",
      enrichmentEligible: true,
      page: 1,
      limit: 1,
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(eligibleOnly.documents, [{ id: eligible.id }]);
    assert.equal(eligibleOnly.pagination.totalItems, 1);
  } finally {
    database.close();
  }
});

test("semantic fallback scans only unindexed rows after Vectorize succeeds and all rows when it fails", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const baseEnv = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const ids = {};
    for (const [customId, content] of [["indexed", "Archived alpha."], ["failed", "Archived beta."]]) {
      const response = await worker.fetch(request("/v3/documents", {
        containerTag: "fallback-space",
        customId,
        content,
      }), baseEnv, ctx);
      ids[customId] = (await response.json()).id;
      database.prepare(
        "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = ? WHERE id = ?",
      ).run(JSON.stringify(embedding(1, 0)), customId, ids[customId]);
    }

    let failVectorQuery = false;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async () => {
          if (failVectorQuery) throw new Error("Vectorize unavailable");
          return { matches: [], count: 0 };
        },
      },
    });
    const successfulVectorize = await worker.fetch(request("/v4/search", {
      containerTag: "fallback-space",
      q: "semantic needle",
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(successfulVectorize.results.map((result) => result.id), [ids.failed]);

    failVectorQuery = true;
    const failedVectorize = await worker.fetch(request("/v4/search", {
      containerTag: "fallback-space",
      q: "semantic needle",
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(new Set(failedVectorize.results.map((result) => result.id)), new Set([ids.indexed, ids.failed]));
  } finally {
    database.close();
  }
});

test("scope search D1 fallback retains indexed rows when other scopes occupy Vectorize matches", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const baseEnv = baseEnvironment(DB);
    const ctx = { waitUntil() {} };
    const target = await worker.fetch(request("/v3/documents", {
      containerTag: "scope-space",
      customId: "target-scope",
      content: "Archived alpha.",
      metadata: { sm_scope: "target" },
    }), baseEnv, ctx).then((response) => response.json());
    const foreign = await worker.fetch(request("/v3/documents", {
      containerTag: "scope-space",
      customId: "foreign-scope",
      content: "Archived beta.",
      metadata: { sm_scope: "foreign" },
    }), baseEnv, ctx).then((response) => response.json());
    for (const id of [target.id, foreign.id]) {
      database.prepare(
        "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'indexed' WHERE id = ?",
      ).run(JSON.stringify(embedding(1, 0)), id);
    }
    const foreignRevision = database.prepare(
      "SELECT topic_revision FROM memories WHERE id = ?",
    ).get(foreign.id).topic_revision;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async () => ({
          matches: [{ id: foreign.id, score: 0.99, metadata: { topic_revision: foreignRevision } }],
          count: 1,
        }),
      },
    });
    const search = await worker.fetch(request("/v4/search", {
      containerTag: "scope-space",
      q: "semantic needle",
      filters: {
        AND: [{ key: "sm_scope", filterType: "metadata", value: "target" }],
      },
    }), env, ctx).then((response) => response.json());
    assert.deepEqual(search.results.map((result) => result.id), [target.id]);
    assert.equal(search.results[0].similarity, 0.99);
  } finally {
    database.close();
  }
});

test("Vectorize hits are hydrated even when the speculative D1 fallback query fails", async () => {
  const database = openDatabase();
  try {
    const baseDB = d1Adapter(database);
    const response = await worker.fetch(request("/v3/documents", {
      containerTag: "fallback-error",
      customId: "vector-hit",
      content: "Stored without lexical query terms.",
    }), baseEnvironment(baseDB), { waitUntil() {} });
    const { id } = await response.json();
    database.prepare(
      "UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'indexed' WHERE id = ?",
    ).run(JSON.stringify(embedding(1, 0)), id);
    const revision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(id).topic_revision;
    let fallbackFailed = false;
    const DB = {
      ...baseDB,
      prepare(sql) {
        if (sql.includes("vector_status <> 'indexed'")) {
          return {
            bind() { return this; },
            async all() {
              fallbackFailed = true;
              throw new Error("D1 fallback unavailable");
            },
          };
        }
        return baseDB.prepare(sql);
      },
    };
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => ({ data: [embedding(1, 0)] }) },
      MEMORY_VECTORS: {
        query: async () => ({
          matches: [{ id, score: 0.98, metadata: { topic_revision: revision } }],
          count: 1,
        }),
      },
    });
    const search = await worker.fetch(request("/v4/search", {
      containerTag: "fallback-error",
      q: "semantic needle",
    }), env, { waitUntil() {} }).then((result) => result.json());
    assert.equal(fallbackFailed, true);
    assert.equal(search.results[0].id, id);
    assert.equal(search.results[0].similarity, 0.98);
  } finally {
    database.close();
  }
});

test("recallable false preserves storage, explicit topics, and manual search without enrichment", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    let aiCalls = 0;
    let vectorCalls = 0;
    const waits = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async () => { aiCalls += 1; return { data: [embedding()] }; } },
      MEMORY_VECTORS: {
        getByIds: async () => { vectorCalls += 1; return []; },
        upsert: async () => { vectorCalls += 1; return { mutationId: "unexpected" }; },
      },
    });
    const ctx = { waitUntil: (promise) => waits.push(promise) };
    const metadata = {
      memoryIndex: { version: 1, title: "Manual memory", recallable: false },
      topics: ["Manual topic"],
    };
    const first = await worker.fetch(request("/v3/documents", {
      containerTag: "manual-space",
      customId: "manual-document",
      content: "Manual searchable first version.",
      metadata,
    }), env, ctx);
    const firstBody = await first.json();
    assert.equal(firstBody.enrichmentStatus, "disabled");
    assert.equal(firstBody.topicStatus, "done");
    assert.deepEqual(firstBody.topics, ["Manual topic"]);

    const second = await worker.fetch(request("/v3/documents", {
      containerTag: "manual-space",
      customId: "manual-document",
      content: "Manual searchable second version.",
      metadata,
    }), env, ctx);
    const secondBody = await second.json();
    assert.equal(secondBody.id, firstBody.id);
    assert.equal(secondBody.enrichmentStatus, "disabled");
    assert.equal(waits.length, 0);
    assert.equal(aiCalls, 0);

    const stored = await worker.fetch(new Request(
      `https://memory.example/v3/documents/${firstBody.id}`,
      { headers: { Authorization: "Bearer test-key" } },
    ), env, ctx).then((response) => response.json());
    assert.deepEqual(stored.topics, ["Manual topic"]);
    assert.deepEqual(
      {
        embeddingStatus: stored.enrichment.embeddingStatus,
        factStatus: stored.enrichment.factStatus,
        vectorStatus: stored.enrichment.vectorStatus,
        topicStatus: stored.enrichment.topicStatus,
      },
      { embeddingStatus: "disabled", factStatus: "disabled", vectorStatus: "disabled", topicStatus: "done" },
    );

    const patched = await worker.fetch(request(`/v3/documents/${firstBody.id}`, {
      content: "Patched manual keyword.",
    }, "PATCH"), env, ctx).then((response) => response.json());
    assert.equal(patched.enrichment.embeddingStatus, "disabled");
    assert.equal(patched.enrichment.factStatus, "disabled");
    assert.equal(patched.enrichment.vectorStatus, "disabled");
    assert.equal(patched.enrichment.topicStatus, "disabled");
    assert.equal(waits.length, 0);
    assert.equal(aiCalls, 0);

    const retry = await worker.fetch(request("/v4/enrich", { id: firstBody.id }), env, ctx);
    assert.equal(retry.status, 409);
    assert.match((await retry.json()).error.message, /excluded/);
    assert.equal(waits.length, 0);
    assert.equal(aiCalls, 0);

    database.prepare(
      "UPDATE memories SET embedding_json = ?, vector_status = 'failed' WHERE id = ?",
    ).run(JSON.stringify(embedding()), firstBody.id);
    const scheduledWaits = [];
    worker.scheduled({}, env, { waitUntil: (promise) => scheduledWaits.push(promise) });
    await Promise.all(scheduledWaits);
    assert.equal(vectorCalls, 0);

    const manualSearch = await worker.fetch(request("/v4/search", {
      containerTag: "manual-space",
      q: "Patched manual keyword",
    }), { ...env, AI_ENRICHMENT_MODE: "off" }, ctx).then((response) => response.json());
    assert.equal(manualSearch.results[0].id, firstBody.id);
    assert.match(manualSearch.results[0].content, /Patched manual keyword/);
    assert.equal(typeof manualSearch.results[0].lexicalSimilarity, "number");
    assert.equal(manualSearch.results[0].semanticSimilarity, null);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM facts").get().count, 0);
  } finally {
    database.close();
  }
});

test("consolidation is leased, supersedes the prior checkpoint, and invalidates on source mutation", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const disabledEnv = baseEnvironment(DB);
    const idleContext = { waitUntil() {} };
    const sourceIds = [];
    for (let index = 0; index < 20; index += 1) {
      const saved = await worker.fetch(request("/v3/documents", {
        containerTag: "memories",
        customId: `consolidation-source-${index}`,
        content: `Verified source ${index}`,
        metadata: { sm_project_id: "consolidation-project" },
      }), disabledEnv, idleContext).then((response) => response.json());
      sourceIds.push(saved.id);
    }

    const payload = {
      title: "Project checkpoint",
      overview: "A safe restart checkpoint.",
      verified: ["Twenty source records were stored."],
      unverified: ["A future choice is not verified."],
      unresolved: ["Choose the next milestone."],
      nextActions: ["Review the open milestone."],
    };
    const waits = [];
    const context = { waitUntil: (promise) => waits.push(promise) };
    const ai = {
      run: async (_model, input) => {
        if (input.text) return { data: [embedding()] };
        if (input.response_format?.json_schema?.name === "memory_consolidation") {
          return { choices: [{ message: { content: JSON.stringify(payload) } }] };
        }
        return enrichment([{ subject: "duplicate", predicate: "must", object: "not publish", confidence: 1, exclusive: true }], ["Checkpoint"]);
      },
    };
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: ai,
      MEMORY_VECTORS: { upsert: async () => ({ mutationId: "checkpoint-vector" }) },
    });
    const first = await worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), env, context);
    assert.equal(first.status, 200);
    const firstBody = await first.json();
    assert.equal(firstBody.status, "consolidated");
    assert.equal(firstBody.revision, 1);
    assert.equal(firstBody.sourceCount, 20);
    assert.deepEqual(new Set(firstBody.sourceMemoryIds), new Set(sourceIds));
    await Promise.all(waits.splice(0));
    const firstSummary = database.prepare(
      "SELECT is_forgotten, fact_status, metadata_json FROM memories WHERE id = ?",
    ).get(firstBody.memoryId);
    assert.equal(firstSummary.is_forgotten, 0);
    assert.equal(firstSummary.fact_status, "disabled");
    assert.equal(JSON.parse(firstSummary.metadata_json).sm_consolidation, true);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM facts WHERE source_memory_id = ?").get(firstBody.memoryId).count, 0);

    const additional = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId: "consolidation-source-20",
      content: "Verified source 20",
      metadata: { sm_project_id: "consolidation-project" },
    }), disabledEnv, idleContext).then((response) => response.json());
    sourceIds.push(additional.id);

    const failed = await worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), { ...env, AI: { run: async () => { throw new Error("AI unavailable"); } } }, context);
    assert.equal(failed.status, 500);
    assert.equal(database.prepare("SELECT status FROM memory_consolidations WHERE id = ?").get(firstBody.consolidationId).status, "active");
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(firstBody.memoryId).is_forgotten, 0);

    const entered = deferred();
    const release = deferred();
    const racingEnv = { ...env, AI: {
      run: async (model, input) => {
        if (input.response_format?.json_schema?.name === "memory_consolidation") {
          entered.resolve();
          await release.promise;
        }
        return ai.run(model, input);
      },
    } };
    const secondPromise = worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), racingEnv, context);
    await entered.promise;
    const competing = await worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), racingEnv, context).then((response) => response.json());
    assert.equal(competing.status, "busy");
    release.resolve();
    const secondBody = await secondPromise.then((response) => response.json());
    assert.equal(secondBody.revision, 2);
    assert.equal(secondBody.sourceCount, 21);
    await Promise.all(waits.splice(0));
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(firstBody.memoryId).is_forgotten, 1);

    const patched = await worker.fetch(request(`/v3/documents/${sourceIds[0]}`, {
      content: "Mutated source invalidates its checkpoint.",
    }, "PATCH"), disabledEnv, idleContext);
    assert.equal(patched.status, 200);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM memory_consolidations WHERE status = 'active'").get().count, 0);
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = ?").get(secondBody.memoryId).is_forgotten, 1);

    const thirdBody = await worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), env, context).then((response) => response.json());
    await Promise.all(waits.splice(0));
    const forgottenSummary = await worker.fetch(request("/v4/memories", {
      containerTag: "memories",
      documentId: thirdBody.memoryId,
    }, "DELETE"), disabledEnv, idleContext);
    assert.equal(forgottenSummary.status, 200);
    assert.equal(database.prepare("SELECT status FROM memory_consolidations WHERE id = ?").get(thirdBody.consolidationId).status, "invalid");
    assert.equal(database.prepare("SELECT active_consolidation_id FROM memory_consolidation_projects WHERE project_key = ?").get("project:consolidation-project").active_consolidation_id, null);
    const afterSelfForget = await worker.fetch(request("/v4/consolidate", {
      projectId: "consolidation-project",
      force: true,
    }), env, context).then((response) => response.json());
    assert.equal(afterSelfForget.status, "consolidated");
    await Promise.all(waits.splice(0));
  } finally {
    database.close();
  }
});

test("hard delete atomically invalidates a consolidation published after its pre-read", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const env = baseEnvironment(DB);
    const context = { waitUntil() {} };
    const source = await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId: "hard-delete-source",
      content: "Source deleted during publication.",
      metadata: { sm_project_id: "hard-delete-project" },
    }), env, context).then((response) => response.json());
    const sourceRevision = database.prepare("SELECT topic_revision FROM memories WHERE id = ?").get(source.id).topic_revision;
    let injected = false;
    DB.interceptBatch(async (operations) => {
      if (injected || !operations.some((operation) => operation.sql.trim() === "DELETE FROM memories WHERE id = ?")) return;
      injected = true;
      const now = "2026-09-12T00:00:00.000Z";
      database.prepare(
        `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
           embedding_status, fact_status, vector_status, topic_status, topic_revision, created_at, updated_at)
         VALUES (?, ?, 'memories', 'Racing summary', ?, 'done', 0, 'disabled', 'disabled', 'disabled',
           'disabled', ?, ?, ?)`,
      ).run("racing-summary", "racing-summary", JSON.stringify({ sm_consolidation: true }), "racing-revision", now, now);
      database.prepare(
        `INSERT INTO memory_consolidation_projects(project_key, project_id, source_container_tag, revision,
           active_consolidation_id, last_success_at, created_at, updated_at)
         VALUES ('project:hard-delete-project', 'hard-delete-project', 'memories', 1, 'racing-consolidation', ?, ?, ?)`,
      ).run(now, now, now);
      database.prepare(
        `INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
         VALUES ('racing-consolidation', 'project:hard-delete-project', 'racing-summary', 1, 'active', ?, ?)`,
      ).run(now, now);
      database.prepare(
        `INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision)
         VALUES ('racing-consolidation', ?, ?)`,
      ).run(source.id, sourceRevision);
    });

    const deleted = await worker.fetch(new Request(`https://memory.example/v3/documents/${source.id}`, {
      method: "DELETE",
      headers: { Authorization: "Bearer test-key" },
    }), env, context);
    assert.equal(deleted.status, 204);
    assert.equal(injected, true);
    assert.equal(database.prepare("SELECT status FROM memory_consolidations WHERE id = 'racing-consolidation'").get().status, "invalid");
    assert.equal(database.prepare("SELECT is_forgotten FROM memories WHERE id = 'racing-summary'").get().is_forgotten, 1);
    assert.equal(database.prepare("SELECT active_consolidation_id FROM memory_consolidation_projects WHERE project_key = 'project:hard-delete-project'").get().active_consolidation_id, null);
  } finally {
    database.close();
  }
});

test("large source history stays bounded on disk and active checkpoint wins index-only recall", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const now = "2026-09-12T00:00:00.000Z";
    database.prepare(
      "INSERT INTO container_tags(tag, name, created_at, updated_at) VALUES ('memories', 'Memories', ?, ?)",
    ).run(now, now);
    database.prepare(
      `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
         embedding_status, fact_status, vector_status, topic_status, topic_revision, created_at, updated_at)
       VALUES ('large-summary-1', 'large-summary-1', 'memories', 'Previous needle checkpoint', ?, 'done', 0,
         'disabled', 'disabled', 'disabled', 'disabled', 'large-summary-revision', ?, ?)`,
    ).run(JSON.stringify({ sm_consolidation: true, sm_project_id: "large-project" }), now, now);
    database.prepare(
      `INSERT INTO memory_consolidation_projects(project_key, project_id, source_container_tag, revision,
         active_consolidation_id, last_success_at, created_at, updated_at)
       VALUES ('project:large-project', 'large-project', 'memories', 1, 'large-consolidation-1', ?, ?, ?)`,
    ).run(now, now, now);
    database.prepare(
      `INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
       VALUES ('large-consolidation-1', 'project:large-project', 'large-summary-1', 1, 'active', ?, ?)`,
    ).run(now, now);
    const insertMemory = database.prepare(
      `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
         embedding_status, fact_status, vector_status, topic_status, topic_revision, created_at, updated_at)
       VALUES (?, ?, 'memories', ?, ?, 'done', 0, 'disabled', 'disabled', 'disabled', 'disabled', ?, ?, ?)`,
    );
    const insertSource = database.prepare(
      "INSERT INTO memory_consolidation_sources(consolidation_id, memory_id, source_revision) VALUES ('large-consolidation-1', ?, ?)",
    );
    database.exec("BEGIN");
    for (let index = 0; index < 1_720; index += 1) {
      const id = `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;
      const revision = `revision-${index}`;
      insertMemory.run(id, `large-${index}`, `Archived source ${index}`, JSON.stringify({ sm_project_id: "large-project" }), revision, now, now);
      insertSource.run(id, revision);
    }
    database.exec("COMMIT");
    const envOff = baseEnvironment(DB);
    await worker.fetch(request("/v3/documents", {
      containerTag: "memories",
      customId: "large-new-source",
      content: "New source for the bounded checkpoint.",
      metadata: { sm_project_id: "large-project" },
    }), envOff, { waitUntil() {} });
    const waits = [];
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: { run: async (_model, input) => {
        if (input.text) return { data: [embedding()] };
        if (input.response_format?.json_schema?.name === "memory_consolidation") {
          return { choices: [{ message: { content: JSON.stringify({
            title: "Needle checkpoint",
            overview: "Bounded metadata checkpoint.",
            verified: [], unverified: [], unresolved: [], nextActions: [],
          }) } }] };
        }
        return enrichment([], ["Checkpoint"]);
      } },
      MEMORY_VECTORS: { upsert: async () => ({ mutationId: "large-checkpoint" }) },
    });
    const consolidated = await worker.fetch(request("/v4/consolidate", {
      projectId: "large-project",
      force: true,
    }), env, { waitUntil: (promise) => waits.push(promise) }).then((response) => response.json());
    await Promise.all(waits);
    assert.equal(consolidated.sourceCount, 1_721);
    assert.equal(consolidated.sourceMemoryIds.length, 1_000);
    assert.equal(consolidated.sourceMemoryIdsTruncated, true);
    const storedMetadata = database.prepare("SELECT metadata_json FROM memories WHERE id = ?").get(consolidated.memoryId).metadata_json;
    assert.ok(Buffer.byteLength(storedMetadata, "utf8") < 64 * 1024);
    assert.equal(JSON.parse(storedMetadata).sourceMemoryIds.length, 1_000);
    assert.equal(database.prepare("SELECT COUNT(*) AS count FROM memory_consolidation_sources WHERE consolidation_id = ?").get(consolidated.consolidationId).count, 1_721);

    const insertCheckpoint = database.prepare(
      `INSERT INTO memories(id, custom_id, container_tag, content, metadata_json, status, is_forgotten,
         embedding_status, fact_status, vector_status, topic_status, topic_revision, created_at, updated_at)
       VALUES (?, ?, 'memories', 'Unrelated checkpoint', ?, 'done', 0, 'disabled', 'disabled',
         'disabled', 'disabled', ?, ?, ?)`,
    );
    const insertProject = database.prepare(
      `INSERT INTO memory_consolidation_projects(project_key, project_id, source_container_tag, revision,
         active_consolidation_id, last_success_at, created_at, updated_at)
       VALUES (?, ?, 'memories', 1, ?, ?, ?, ?)`,
    );
    const insertConsolidation = database.prepare(
      `INSERT INTO memory_consolidations(id, project_key, memory_id, revision, status, created_at, updated_at)
       VALUES (?, ?, ?, 1, 'active', ?, ?)`,
    );
    database.exec("BEGIN");
    for (let index = 0; index < 101; index += 1) {
      const memoryId = `unrelated-summary-${index}`;
      const projectId = `unrelated-project-${index}`;
      const projectKey = `project:${projectId}`;
      const consolidationId = `unrelated-consolidation-${index}`;
      const metadata = JSON.stringify({
        sm_consolidation: true,
        sm_project_id: projectId,
        memoryIndex: { version: 1, title: "Unrelated checkpoint", description: "Other subject", sections: [], recallable: true },
      });
      insertCheckpoint.run(memoryId, memoryId, metadata, `unrelated-revision-${index}`, now, now);
      insertProject.run(projectKey, projectId, consolidationId, now, now, now);
      insertConsolidation.run(consolidationId, projectKey, memoryId, now, now);
    }
    database.exec("COMMIT");

    for (let index = 0; index < 60; index += 1) {
      await worker.fetch(request("/v3/documents", {
        containerTag: "memories",
        customId: `needle-crowd-${index}`,
        content: `needle crowd ${index}`,
        metadata: { sm_project_id: `crowd-${index}` },
      }), envOff, { waitUntil() {} });
    }
    const recalled = await worker.fetch(request("/v4/search", {
      containerTag: "memories",
      q: "needle",
      indexOnly: true,
      limit: 20,
    }), envOff, { waitUntil() {} }).then((response) => response.json());
    assert.equal(recalled.results[0].id, consolidated.memoryId);
    assert.equal(recalled.results[0].metadata.sourceMemoryIds.length, 1_721);
  } finally {
    database.close();
  }
});

test("daily consolidation runs at twenty memories or three days and skips nineteen fresh memories", async () => {
  const database = openDatabase();
  try {
    const DB = d1Adapter(database);
    const disabledEnv = baseEnvironment(DB);
    const idleContext = { waitUntil() {} };
    const seed = async (projectId, count) => {
      const ids = [];
      for (let index = 0; index < count; index += 1) {
        const body = await worker.fetch(request("/v3/documents", {
          containerTag: "memories",
          customId: `${projectId}-${index}`,
          content: `${projectId} memory ${index}`,
          metadata: { sm_project_id: projectId },
        }), disabledEnv, idleContext).then((response) => response.json());
        ids.push(body.id);
      }
      return ids;
    };
    await seed("fresh-nineteen", 19);
    await seed("fresh-twenty", 20);
    const bulkIds = await seed("initial-bulk", 81);
    database.prepare(
      `UPDATE memories SET created_at = '2026-09-01T00:00:00.000Z'
       WHERE json_extract(metadata_json, '$.sm_project_id') = 'initial-bulk'`,
    ).run();
    const [agedId] = await seed("aged-one", 1);
    database.prepare("UPDATE memories SET created_at = ? WHERE id = ?").run("2026-09-01T00:00:00.000Z", agedId);

    let consolidationCalls = 0;
    const env = baseEnvironment(DB, {
      AI_ENRICHMENT_MODE: "on",
      AI: {
        run: async (_model, input) => {
          if (input.text) return { data: [embedding()] };
          if (input.response_format?.json_schema?.name === "memory_consolidation") {
            consolidationCalls += 1;
            return { choices: [{ message: { content: JSON.stringify({
              title: "Scheduled checkpoint",
              overview: "Scheduled consolidation.",
              verified: [],
              unverified: [],
              unresolved: [],
              nextActions: [],
            }) } }] };
          }
          return enrichment([], ["Checkpoint"]);
        },
      },
      MEMORY_VECTORS: { upsert: async () => ({ mutationId: "scheduled-checkpoint" }) },
    });
    const waits = [];
    const context = { waitUntil: (promise) => waits.push(promise) };
    worker.scheduled({ cron: "0 18 * * *" }, env, context);
    await waits[0];
    await Promise.all(waits.slice(1));

    assert.equal(consolidationCalls, 5);
    const activeProjects = new Set(database.prepare(
      "SELECT project_key FROM memory_consolidations WHERE status = 'active'",
    ).all().map((row) => row.project_key));
    assert.equal(activeProjects.has("project:fresh-nineteen"), false);
    assert.equal(activeProjects.has("project:fresh-twenty"), true);
    assert.equal(activeProjects.has("project:aged-one"), true);
    assert.equal(activeProjects.has("project:initial-bulk"), true);
    assert.equal(database.prepare(
      "SELECT initial_backfill_completed FROM memory_consolidation_projects WHERE project_key = 'project:initial-bulk'",
    ).get().initial_backfill_completed, 1);
    const bulkActive = database.prepare(
      "SELECT id FROM memory_consolidations WHERE project_key = 'project:initial-bulk' AND status = 'active'",
    ).get();
    assert.equal(database.prepare(
      "SELECT COUNT(*) AS count FROM memory_consolidation_sources WHERE consolidation_id = ?",
    ).get(bulkActive.id).count, bulkIds.length);
  } finally {
    database.close();
  }
});
