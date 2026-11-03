import test,{assert} from './support/test.mjs';
import worker from '../src/index.ts';
import {fixture} from './worker-fixture.mjs';
const utf8Bytes=value=>new TextEncoder().encode(value).length;
test('authenticated Worker API smoke, source capacity and remote MCP', {timeout:90000}, async()=>{
 const f=fixture();f.env.AI=undefined;f.env.MEMORY_VECTORS=undefined;
 const token=f.env.MEMORY_API_KEY;const baseUrl='https://smoke.example';
 const fetch=(url,options)=>worker.fetch(new Request(url,options),f.env,f.ctx);
 const request=(path,options={})=>fetch(`${baseUrl}${path}`,{...options,headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json',...options.headers}});
 try {
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



  const responseSource = "# Transport reliability\n\n### User request\n接続を直して\n\n### Final assistant response\nHttpRecallNeedle は AbortSignal.timeout の設定で解決しました。";
  const responseRecord = await request("/v3/documents", {
    method: "POST", body: JSON.stringify({ containerTag: "http-recall", customId: "http-response-only", content: responseSource,
      metadata: { sm_project_id: "http-project", topics: ["HTTP Recall"], memoryIndex: { version: 1, title: "Transport reliability", description: "接続を直して", sections: [], recallable: true } } }),
  }).then((response) => response.json());
  const sourceRead=await request(`/v3/documents/${responseRecord.id}`);assert.equal(sourceRead.status,200);assert.match((await sourceRead.json()).content,/AbortSignal\.timeout/u);  const forgotten = await request("/v4/memories", {
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
  assert.ok(utf8Bytes(capacityText) < 100_000);
  // リモートMCPも実workerdと認証付きHTTPを通し、旧記憶と同じD1へ接続する。
  let rpcId = 0;
  const rpc = async (method, params = {}) => {
    const id = ++rpcId;
    const response = await request("/mcp", { method: "POST", headers: { Accept: "application/json, text/event-stream", "MCP-Protocol-Version": "2025-06-18" }, body: JSON.stringify({ jsonrpc: "2.0", id, method, params }) });
    assert.equal(response.status, 200);
    const result = await response.json();
    assert.equal(result.id, id);
    assert.equal(result.error, undefined);
    assert.equal(result.result.isError, undefined, JSON.stringify(result));
    return result.result;
  };
  const initialized = await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "remote-http-smoke", version: "1" } });
  assert.equal(initialized.protocolVersion, "2025-06-18");
  assert.match(initialized.instructions, /search memory/);
  assert.ok((await rpc("tools/list")).tools.some(tool => tool.name === "add_memory"));
  const remoteCall = async (name, args) => (await rpc("tools/call", { name, arguments: args })).structuredContent;
  assert.equal((await remoteCall("getDocument", { documentId: responseRecord.id })).id, responseRecord.id);
  const remoteArgs = { content: `RemoteHttpNeedle ${token} password="remote-secret"`, containerTag: "remote-mcp-smoke", projectId: "remote-project", idempotencyKey: "http-save" };
  const remoteSaved = await remoteCall("add_memory", remoteArgs);
  assert.equal((await remoteCall("add_memory", remoteArgs)).id, remoteSaved.id);
  assert.equal((await remoteCall("getDocument", { documentId: remoteSaved.id })).content, 'RemoteHttpNeedle [REDACTED] password="[REDACTED]"');
  const remoteSearch = await remoteCall("search_memory", { query: "RemoteHttpNeedle", containerTag: "remote-mcp-smoke" });
  assert.equal(remoteSearch.results[0].id, remoteSaved.id);
  assert.equal(remoteSearch.results[0].content, undefined);
  await remoteCall("add_memory", { action: "forget", documentId: remoteSaved.id, containerTag: "remote-mcp-smoke" });
  assert.equal((await fetch(`${baseUrl}/mcp`)).status, 401);

} finally {await f.drain();f.db.close()} });
