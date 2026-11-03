import test, {assert} from './support/test.mjs';
import {fixture as baseFixture} from './worker-fixture.mjs';
import {buildMemoryIndex} from '../src/shared/memory-index.ts';
const sourceDate='2025-03-04T12:00:00.000Z';
const index=(title,request=title)=>({memoryIndex:buildMemoryIndex({title,request,sourceUpdatedAt:sourceDate})});
function scenario(name,run){test(name,async()=>{
 const f=baseFixture();const metrics={embeddings:0,vectorQueries:[]};const raw=f.api;
 const originalAi=f.env.AI.run;f.env.AI.run=async(model,input)=>{if(input.text)metrics.embeddings++;return originalAi(model,input)};
 f.env.MEMORY_VECTORS.query=async(...args)=>{metrics.vectorQueries.push(args);return {matches:[],count:0}};
 const seed=async(tag,content,metadata)=>{const r=await raw('/v3/documents',{containerTag:tag,content,metadata});assert.equal(r.status,201);await f.drain();return r.body.id};
 try{await run({...f,raw,seed,metrics,reset(){metrics.embeddings=0;metrics.vectorQueries=[];f.env.AI_ENRICHMENT_MODE='on'}})}finally{await f.drain();f.db.close()}
});}
scenario("recent completed tasks cross projects without matches and paginate after task grouping", async (f) => {
  const capture = (sessionId, date, extra = {}) => ({ type: "conversation", sessionId, captureVersion: 2,
    turn: 1, part: 1, parts: 1, sm_project_id: "other-project", sourceTimestamp: date,
    memoryIndex: buildMemoryIndex({ title: "別作業の完了", request: "果樹の配置を変更", sourceUpdatedAt: date }), ...extra });
  const firstDate = "2026-10-22T10:00:00Z";
  const first = await f.seed("unrelated-space", "# 果樹\n配置を完了しました", capture("task-one", firstDate, { turn: 3 }));
  for (let i = 0; i < 55; i++) {
    const id = await f.seed(i % 2 ? "shared-space" : "unrelated-space", `# 分割 ${i}\n完了しました`, capture("task-one", firstDate, { turn: i % 2 ? 2 : 3, part: i + 2, parts: 56 }));
    f.db.prepare("UPDATE memories SET updated_at = '2099-01-01T00:00:00Z' WHERE id = ?").run(id);
  }
  const second = await f.seed("other-space", "# 料理\n手順の整理を完了", capture("task-two", "2026-10-22T09:00:00Z"));
  const legacy = await f.seed("legacy-space", "# 旧記録\n作業を完了しました", {
    type: "conversation", sessionId: "legacy-task", completedAt: "2026-10-21T12:00:00Z",
  });
  const old = await f.seed("reimport-space", "# 再取込\n過去の作業", capture("old-task", "2020-01-01T00:00:00Z"));
  f.db.prepare("UPDATE memories SET updated_at = '2099-01-01T00:00:00Z' WHERE id = ?").run(old);
  const excluded = [];
  excluded.push(await f.seed("current", "# 現在\n完了", capture("current-task", "2098-01-01T00:00:00Z")));
  excluded.push(await f.seed("optout", "# 除外\n完了", capture("optout", firstDate, { memoryIndex: { ...index("除外").memoryIndex, recallable: false } })));
  excluded.push(await f.seed("checkpoint", "# 統合\n完了", capture("checkpoint", firstDate, { sm_consolidation: true })));
  excluded.push(await f.seed("manual", "# 手動\n完了", { ...capture("manual", firstDate), type: "note" }));
  excluded.push(await f.seed("no-session", "# 無所属\n完了", { ...capture("", firstDate) }));
  const forgotten = await f.seed("forgotten", "# 忘却\n完了", capture("forgotten", firstDate));
  await f.raw("/v4/memories", { containerTag: "forgotten", documentId: forgotten }, "DELETE"); excluded.push(forgotten);
  const body = { projection: "index", recentTasks: true, excludeSessionId: "current-task", limit: 2, page: 1 };
  f.reset();
  const result = await f.raw("/v3/documents/list", body);
  assert.equal(result.status, 200); assert.equal(result.body.recentTasks, true);
  assert.deepEqual(result.body.documents.map((row) => row.id), [first, second]);
  assert.equal(result.body.pagination.totalItems, 4); assert.equal(result.body.pagination.hasMore, true);
  assert.equal(result.body.documents[0].metadata.sessionId, "task-one");
  assert.equal(result.body.documents[0].metadata.turn, 3); assert.equal(result.body.documents[0].metadata.part, 1);
  assert.equal(result.body.documents[0].provenance.projectId, "other-project");
  const page2 = await f.raw("/v3/documents/list", { ...body, page: 2 });
  assert.deepEqual(page2.body.documents.map((row) => row.id), [legacy, old]);
  assert.equal(page2.body.pagination.hasMore, false);
  assert.ok(![...result.body.documents, ...page2.body.documents].some((row) => excluded.includes(row.id)));
  assert.equal(f.metrics.embeddings, 0); assert.equal(f.metrics.vectorQueries.length, 0);
  const ordinary = await f.raw("/v3/documents/list", { projection: "index", containerTag: "manual" });
  assert.equal(ordinary.body.documents.length, 1); assert.equal(ordinary.body.recentTasks, undefined);
});
scenario("recent task chronology validates dates, respects timezones and selects revision representatives", async (f) => {
  const seed = async (tag, sessionId, metadata, createdAt = "2020-01-01T00:00:00Z") => {
    const id = await f.seed(tag, `# ${tag}\n作業を完了しました`, { type: "conversation", sessionId, ...metadata });
    f.db.prepare("UPDATE memories SET created_at = ?, updated_at = ? WHERE id = ?").run(createdAt, "2099-01-01T00:00:00Z", id);
    return id;
  };
  const earlyZone = await seed("zone-a", "same-session", { sourceTimestamp: "2026-10-23T01:00:00+09:00" });
  const lateZone = await seed("zone-b", "same-session", { sourceTimestamp: "2026-10-22T18:00:00Z" });
  const fallback = await seed("invalid-date", "invalid", { memoryIndex: { sourceUpdatedAt: "not-a-date" }, sourceTimestamp: "2026-02-30T00:00:00Z", sourceUpdatedAt: "2026-10-22T17:00:00Z" });
  const invalidFallback = await seed("invalid-all", "invalid-all", { sourceTimestamp: "not-a-date", completedAt: 9999999 });
  const dateOnly = await seed("date-only", "date-only", { memoryIndex: { sourceUpdatedAt: "2099-01-01" }, sourceTimestamp: "2099-01-01" }, "2019-01-01T00:00:00Z");
  const olderRevision = await seed("revisions", "revision-task", { sm_project_id: "revision-project", sourceTimestamp: "2026-10-22T19:00:00Z", turn: 4, part: 1 });
  const latestRevision = await seed("revisions", "revision-task", { sm_project_id: "revision-project", sourceTimestamp: "2026-10-22T19:00:00Z", turn: 4, part: 1 }, "2020-01-02T00:00:00Z");
  f.db.prepare("UPDATE memories SET updated_at = '2020-01-03T00:00:00Z' WHERE id = ?").run(latestRevision);
  // 旧版だけを後から更新しても、後に作成された新版を代表にする。
  const patched = await f.raw(`/v3/documents/${olderRevision}`, { content: "# 旧版の再更新\n元の完了記録を再送しました" }, "PATCH");
  assert.equal(patched.status, 200); await f.drain();
  const revisionDates = f.db.prepare("SELECT created_at, updated_at FROM memories WHERE id IN (?, ?) ORDER BY created_at ASC").all(olderRevision, latestRevision);
  assert.ok(revisionDates[0].created_at < revisionDates[1].created_at);
  assert.ok(revisionDates[0].updated_at > revisionDates[1].updated_at);
  const latestTurn = await seed("turns", "turn-task", { sourceTimestamp: "2026-10-22T20:00:00Z", turn: 9, part: 1 });
  await seed("turns", "turn-task", { sourceTimestamp: "2026-10-22T20:00:00Z", turn: 8, part: 1 });
  const priority = await seed("index-date", "index-task", { memoryIndex: { sourceUpdatedAt: "2026-10-22T21:00:00Z" }, sourceTimestamp: "2020-01-01T00:00:00Z" });
  const result = await f.raw("/v3/documents/list", { projection: "index", recentTasks: true, limit: 50 });
  assert.equal(result.status, 200);
  assert.deepEqual(result.body.documents.map((row) => row.id), [priority, latestTurn, latestRevision, lateZone, fallback, earlyZone, invalidFallback, dateOnly]);
  assert.equal(result.body.pagination.totalItems, 8);
});
scenario("recent task list rejects invalid mode and session inputs", async (f) => {
  for (const body of [
    { recentTasks: "true", projection: "index" }, { recentTasks: true },
    { recentTasks: true, projection: "full" }, { recentTasks: true, projection: "capture" },
    { recentTasks: true, projection: "index", containerTag: "scoped" },
    { recentTasks: true, projection: "index", topic: "scoped" },
    ...[null, 1, "", "  ", "a".repeat(161)].map((excludeSessionId) => ({ recentTasks: true, projection: "index", excludeSessionId })),
    { projection: "index", excludeSessionId: "current" }, { recentTasks: false, projection: "index", excludeSessionId: "current" },
  ]) assert.equal((await f.raw("/v3/documents/list", body)).status, 400, JSON.stringify(body));
  const result = await f.raw("/v3/documents/list", { recentTasks: false, projection: "index" });
  assert.equal(result.status, 200); assert.equal(result.body.recentTasks, undefined);
});
