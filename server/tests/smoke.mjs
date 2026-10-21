import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { createInterface } from "node:readline";
import assert from "node:assert/strict";
import { api, readDocument } from "../../client/memory-client.mjs";
import { runHook } from "../../client/memory-hooks.mjs";

const root = resolve(import.meta.dirname, "..");
const wrangler = join(root, "node_modules", "wrangler", "bin", "wrangler.js");
const state = mkdtempSync(join(tmpdir(), "remnifold-test-"));
const token = "sm_cf_test_only";
const port = 8900 + (process.pid % 500);
const baseUrl = `http://127.0.0.1:${port}`;

function run(args) {
  const result = spawnSync(process.execPath, [wrangler, ...args, "--config", "wrangler.test.jsonc"], {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, MEMORY_API_KEY: token },
  });
  assert.equal(result.status, 0, `${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((resolvePromise) => child.once("exit", resolvePromise));
  // WindowsではWrangler配下のworkerdも終了し、ローカルD1のロックを解放する。
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
      windowsHide: true,
      stdio: "ignore",
    });
  } else {
    child.kill();
  }
  await Promise.race([
    exited,
    new Promise((resolvePromise) => setTimeout(resolvePromise, 2_000)),
  ]);
}

async function waitForServer() {
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/health`);
      if (response.ok) return;
    } catch {
      // The local Worker is still starting.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 125));
  }
  throw new Error("Timed out waiting for wrangler dev");
}

async function request(path, options = {}) {
  return fetch(`${baseUrl}${path}`, {
    ...options,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      ...options.headers,
    },
  });
}

async function callMcp() {
  const child = spawn(process.execPath, [join(root, "..", "client", "mcp-server.mjs")], {
    cwd: root,
    env: { ...process.env, REMNIFOLD_API_URL: baseUrl, REMNIFOLD_API_KEY: token },
    stdio: ["pipe", "pipe", "inherit"],
  });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const responses = [];
  lines.on("line", (line) => responses.push(JSON.parse(line)));
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } })}\n`);
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })}\n`);
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "search_memory", arguments: { query: "Cloudflare", containerTag: "repo_test__0123456789abcdef" } } })}\n`);
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline && responses.length < 3) {
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  }
  await stopChild(child);
  assert.equal(responses.find((item) => item.id === 1)?.result?.serverInfo?.name, "remnifold");
  assert.ok(responses.find((item) => item.id === 2)?.result?.tools?.some((tool) => tool.name === "whoAmI"));
  assert.equal(responses.find((item) => item.id === 3)?.result?.isError, undefined);
}

let worker;
try {
  run(["d1", "migrations", "apply", "remnifold", "--local", "--persist-to", state]);
  worker = spawn(
    process.execPath,
    [
      wrangler,
      "dev",
      "--config",
      "wrangler.test.jsonc",
      "--local",
      "--port",
      String(port),
      "--persist-to",
      state,
      "--var",
      `MEMORY_API_KEY:${token}`,
      "--var",
      "AI_ENRICHMENT_MODE:off",
    ],
    { cwd: root, stdio: ["ignore", "pipe", "pipe"] },
  );
  await waitForServer();

  const health = await fetch(`${baseUrl}/health`).then((response) => response.json());
  assert.equal(health.semanticSearch, "disabled");
  assert.equal(health.factGraph, "disabled");

  const unauthorized = await fetch(`${baseUrl}/v3/session`);
  assert.equal(unauthorized.status, 401);

  const disabledConsolidation = await request("/v4/consolidate", {
    method: "POST",
    body: JSON.stringify({ projectId: "smoke-project", force: true }),
  });
  assert.equal(disabledConsolidation.status, 409);

  const captureKey = "capture-session:1:0123456789abcdef";
  const captureMetadata = {
    captureVersion: 2,
    captureKey,
    sm_project_id: "legacy_capture_folder",
  };
  const legacyCapture = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "legacy capture",
      containerTag: "legacy_capture_folder",
      customId: `codex-turn-v2:${captureKey}`,
      metadata: captureMetadata,
    }),
  }).then((response) => response.json());
  const reusedCapture = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "legacy capture",
      containerTag: "memories",
      customId: `codex-turn-v2:${captureKey}`,
      metadata: captureMetadata,
      reuseExistingCapture: true,
    }),
  }).then((response) => response.json());
  assert.equal(reusedCapture.id, legacyCapture.id);
  assert.equal(reusedCapture.reusedExistingCapture, true);
  const legacyCaptureList = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ containerTag: "legacy_capture_folder" }),
  }).then((response) => response.json());
  const commonCaptureList = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ containerTag: "memories" }),
  }).then((response) => response.json());
  assert.equal(legacyCaptureList.pagination.totalItems, 1);
  assert.equal(commonCaptureList.pagination.totalItems, 0);

  const ordinaryIds = [];
  for (const ordinaryContainer of ["ordinary-a", "ordinary-b"]) {
    const ordinary = await request("/v3/documents", {
      method: "POST",
      body: JSON.stringify({
        content: `ordinary ${ordinaryContainer}`,
        containerTag: ordinaryContainer,
        customId: "same-custom-id",
      }),
    }).then((response) => response.json());
    ordinaryIds.push(ordinary.id);
  }
  assert.notEqual(ordinaryIds[0], ordinaryIds[1]);

  const staleMetadata = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "old content",
      containerTag: "stale-topic-folder",
      customId: "stale-topic",
      metadata: { topics: ["古い分類"] },
    }),
  }).then((response) => response.json());
  const stalePatch = await request(`/v3/documents/${staleMetadata.id}`, {
    method: "PATCH",
    body: JSON.stringify({ content: "new content" }),
  });
  assert.equal(stalePatch.status, 200);
  const stalePatchBody = await stalePatch.json();
  assert.deepEqual(stalePatchBody.topics, []);
  assert.equal(stalePatchBody.metadata.topics, undefined);
  assert.equal(stalePatchBody.enrichment.topicStatus, "disabled");
  const disabledRetry = await request("/v4/enrich", {
    method: "POST",
    body: JSON.stringify({ id: staleMetadata.id }),
  });
  assert.equal(disabledRetry.status, 409);

  for (const [cleanupContainer, cleanupContent] of [
    ["legacy_capture_folder", "legacy capture"],
    ["ordinary-a", "ordinary ordinary-a"],
    ["ordinary-b", "ordinary ordinary-b"],
    ["stale-topic-folder", "new content"],
  ]) {
    const cleanup = await request("/v4/memories", {
      method: "DELETE",
      body: JSON.stringify({ containerTag: cleanupContainer, content: cleanupContent }),
    });
    assert.equal(cleanup.status, 200);
  }

  const unscopedCapture = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "shared capture without project provenance",
      containerTag: "memories",
      customId: "codex-turn-v2:shared:1:fedcba9876543210",
      metadata: { captureVersion: 2, captureKey: "shared:1:fedcba9876543210" },
    }),
  });
  assert.equal(unscopedCapture.status, 201);
  const invalidUnscopedReuse = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "invalid unscoped capture reuse",
      containerTag: "memories",
      customId: "codex-turn-v2:shared:2:fedcba9876543210",
      metadata: { captureVersion: 2, captureKey: "shared:2:fedcba9876543210" },
      reuseExistingCapture: true,
    }),
  });
  assert.equal(invalidUnscopedReuse.status, 400);
  await request("/v4/memories", {
    method: "DELETE",
    body: JSON.stringify({ containerTag: "memories", content: "shared capture without project provenance" }),
  });

  const invalidTopics = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "invalid topic metadata",
      containerTag: "memories",
      metadata: { topics: ["repo_test__0123456789abcdef"] },
    }),
  });
  assert.equal(invalidTopics.status, 400);

  const containerTag = "repo_test__0123456789abcdef";
  const first = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "記憶データはCloudflare D1へ保存する",
      containerTag,
      customId: "session-1",
      metadata: {
        sm_scope: "personal",
        sm_project_id: "project-alpha",
        topics: ["旧トピック"],
      },
    }),
  });
  assert.equal(first.status, 201);
  const firstBody = await first.json();
  assert.equal(firstBody.enrichmentStatus, "disabled");

  const update = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "記憶データはRemnifoldからCloudflare D1へ保存する",
      containerTag,
      customId: "session-1",
      metadata: {
        sm_scope: "personal",
        sm_project_id: "project-alpha",
        topics: [" Cloudflare D1 ", "データベース"],
      },
    }),
  });
  const updateBody = await update.json();
  assert.equal(updateBody.id, firstBody.id);
  assert.deepEqual(updateBody.topics, ["Cloudflare D1", "データベース"]);

  const secondContainerTag = "legacy_folder__fedcba9876543210";
  const second = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "別プロジェクトでもCloudflare D1を利用する",
      containerTag: secondContainerTag,
      customId: "session-2",
      metadata: { sm_project_id: "project-beta", topics: ["Cloudflare D1"] },
    }),
  });
  assert.equal(second.status, 201);

  const unclassifiedContainerTag = "legacy_folder__0011223344556677";
  const unclassified = await request("/v3/documents", {
    method: "POST",
    body: JSON.stringify({
      content: "まだ分類されていない既存メモリ",
      containerTag: unclassifiedContainerTag,
      customId: "session-3",
      metadata: { sm_project_id: "project-gamma" },
    }),
  });
  assert.equal(unclassified.status, 201);

  const profile = await request("/v4/profile", {
    method: "POST",
    body: JSON.stringify({ containerTag, q: "Cloudflare D1" }),
  });
  assert.equal(profile.status, 200);
  const profileBody = await profile.json();
  assert.equal(profileBody.searchResults.results.length, 1);
  assert.match(profileBody.searchResults.results[0].memory, /RemnifoldからCloudflare D1/);
  assert.deepEqual(profileBody.searchResults.results[0].topics, ["Cloudflare D1", "データベース"]);

  const list = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ containerTag, page: 1, limit: 10 }),
  });
  const listBody = await list.json();
  assert.equal(listBody.pagination.totalItems, 1);
  assert.equal(listBody.documents[0].enrichment.embeddingStatus, "disabled");
  assert.equal(listBody.documents[0].enrichment.vectorStatus, "disabled");
  assert.equal(listBody.documents[0].enrichment.topicStatus, "done");
  assert.deepEqual(listBody.documents[0].topics, ["Cloudflare D1", "データベース"]);
  assert.deepEqual(listBody.documents[0].provenance, {
    containerTag,
    projectId: "project-alpha",
  });

  const allCloudflare = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ topic: "cloudflare d1", page: 1, limit: 10 }),
  }).then((response) => response.json());
  assert.equal(allCloudflare.pagination.totalItems, 2);
  assert.deepEqual(
    new Set(allCloudflare.documents.map((document) => document.provenance.projectId)),
    new Set(["project-alpha", "project-beta"]),
  );

  const scopedCloudflare = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ containerTag, topic: "Cloudflare D1", page: 1, limit: 10 }),
  }).then((response) => response.json());
  assert.equal(scopedCloudflare.pagination.totalItems, 1);

  const staleTopic = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ topic: "旧トピック", page: 1, limit: 10 }),
  }).then((response) => response.json());
  assert.equal(staleTopic.pagination.totalItems, 0);

  const unclassifiedList = await request("/v3/documents/list", {
    method: "POST",
    body: JSON.stringify({ topic: "__unclassified__", page: 1, limit: 10 }),
  }).then((response) => response.json());
  assert.equal(unclassifiedList.pagination.totalItems, 1);
  assert.equal(unclassifiedList.documents[0].enrichment.topicStatus, "disabled");
  assert.deepEqual(unclassifiedList.documents[0].topics, []);

  const topicIndex = await request("/v4/topics?page=1&limit=1").then((response) => response.json());
  assert.equal(topicIndex.topics.length, 1);
  assert.deepEqual(topicIndex.topics[0], { topic: "Cloudflare D1", documentCount: 2 });
  assert.equal(topicIndex.unclassifiedCount, 1);
  assert.deepEqual(topicIndex.pagination, {
    currentPage: 1,
    limit: 1,
    totalItems: 2,
    totalPages: 2,
    hasMore: true,
  });
  const secondTopicPage = await request("/v4/topics?page=2&limit=1").then((response) => response.json());
  assert.deepEqual(secondTopicPage.topics, [{ topic: "データベース", documentCount: 1 }]);
  assert.equal(secondTopicPage.pagination.hasMore, false);

  const graph = await request("/v4/graph", {
    method: "POST",
    body: JSON.stringify({ containerTag }),
  });
  assert.deepEqual(await graph.json(), { facts: [], relations: [] });

  await callMcp();

  const responseSource = "# Transport reliability\n\n### User request\n接続を直して\n\n### Final assistant response\nHttpRecallNeedle は AbortSignal.timeout の設定で解決しました。";
  const responseRecord = await request("/v3/documents", {
    method: "POST", body: JSON.stringify({ containerTag: "http-recall", customId: "http-response-only", content: responseSource,
      metadata: { sm_project_id: "http-project", topics: ["HTTP Recall"], memoryIndex: { version: 1, title: "Transport reliability", description: "接続を直して", sections: [], recallable: true } } }),
  }).then((response) => response.json());
  const recallCalls = [];
  const localApi = (path, options = {}) => {
    recallCalls.push(path);
    return api(path, { ...options, config: { baseUrl, apiKey: token } });
  };
  const hook = await runHook("UserPromptSubmit", { prompt: "HttpRecallNeedle" }, {
    settings: { recallMode: "direct", maxMemories: 5, minimumSimilarity: 0.7 }, context: {}, request: localApi,
  });
  assert.match(hook.hookSpecificOutput.additionalContext, new RegExp(responseRecord.id));
  assert.match(hook.hookSpecificOutput.additionalContext, /原文の該当箇所.*HttpRecallNeedle/u);
  assert.deepEqual(recallCalls, ["/v4/search"]);
  const original = await readDocument(responseRecord.id, localApi);
  assert.match(original.text, /AbortSignal\.timeout/u);
  const artifact = resolve(root, "../dist/recall-http-validation.json");
  mkdirSync(resolve(artifact, ".."), { recursive: true });
  writeFileSync(artifact, `${JSON.stringify({ node: process.version, runtime: "Wrangler local/workerd + local D1 + authenticated HTTP + real hook/API/getDocument", passed: true, recallHttpRequests: 1, fullSourcePreserved: true }, null, 2)}\n`);

  const forgotten = await request("/v4/memories", {
    method: "DELETE",
    body: JSON.stringify({ containerTag, content: "記憶データはRemnifoldからCloudflare D1へ保存する" }),
  });
  assert.equal(forgotten.status, 200);
  const forgetSecond = await request("/v4/memories", {
    method: "DELETE",
    body: JSON.stringify({ containerTag: secondContainerTag, content: "別プロジェクトでもCloudflare D1を利用する" }),
  });
  assert.equal(forgetSecond.status, 200);
  const topicsAfterForget = await request("/v4/topics").then((response) => response.json());
  assert.equal(topicsAfterForget.topics.some((topic) => topic.topic === "Cloudflare D1"), false);
  assert.equal(topicsAfterForget.unclassifiedCount, 1);
  const afterForget = await request("/v4/search", {
    method: "POST",
    body: JSON.stringify({ containerTag, q: "Cloudflare D1" }),
  });
  assert.equal((await afterForget.json()).results.length, 0);

  // 最大長の旧・現行文書が同じ検索に集まっても、実workerdで索引応答を作れることを確認する。
  const capacitySuffix = "\nCapacityNeedle source ending";
  const longSource = `${"漢".repeat(200_000 - capacitySuffix.length)}${capacitySuffix}`;
  for (let index = 0; index < 50; index += 1) {
    const saved = await request("/v3/documents", { method: "POST", body: JSON.stringify({
      containerTag: "capacity-local", customId: `capacity-${index}`, content: longSource,
      metadata: { title: "CapacityNeedle", ...(index % 2 ? { memoryIndex: {
        version: 1, title: "CapacityNeedle", description: "CapacityNeedle retrieval", sections: [], recallable: true,
      } } : {}) },
    }) });
    assert.equal(saved.status, 201);
  }
  const capacityStarted = performance.now();
  const capacity = await request("/v4/search", { method: "POST", body: JSON.stringify({
    allContainers: true, indexOnly: true, q: "CapacityNeedle", limit: 50,
  }) });
  assert.equal(capacity.status, 200);
  const capacityText = await capacity.text();
  const capacityBody = JSON.parse(capacityText);
  assert.equal(capacityBody.results.length, 50);
  assert.ok(capacityBody.results.every((record) => !record.content && !record.memory && record.metadata.memoryIndex.version === 1));
  assert.ok(Buffer.byteLength(capacityText) < 100_000);
  writeFileSync(artifact, `${JSON.stringify({ node: process.version, runtime: "Wrangler local/workerd + local D1 + authenticated HTTP + real hook/API/getDocument", passed: true,
    recallHttpRequests: 1, fullSourcePreserved: true, capacity: { records: 50, sourceCharactersEach: longSource.length, legacyRecords: 25,
      responseBytes: Buffer.byteLength(capacityText), elapsedMs: performance.now() - capacityStarted, cloudflareProductionLatencyMeasured: false } }, null, 2)}\n`);

  console.log("Remnifold smoke tests passed: auth, save/upsert, search, topics, list, graph, MCP, forget");
} finally {
  await stopChild(worker);
  rmSync(state, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
