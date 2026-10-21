import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const baseUrl = process.env.REMNIFOLD_API_URL || process.env.KAGAYOI_MEMORY_API_URL;
const apiKeyFile = process.env.REMNIFOLD_API_KEY_FILE || process.env.KAGAYOI_MEMORY_API_KEY_FILE;
if (!baseUrl || !apiKeyFile) {
  throw new Error("REMNIFOLD_API_URL and REMNIFOLD_API_KEY_FILE are required");
}

const apiKey = readFileSync(apiKeyFile, "utf8").trim();
const headers = {
  Authorization: `Bearer ${apiKey}`,
  "Content-Type": "application/json",
};
const containerTag = `semantic_smoke__${Date.now()}`;
const ids = [];

async function request(path, options = {}) {
  const response = await fetch(`${baseUrl}${path}`, { ...options, headers: { ...headers, ...options.headers } });
  if (!response.ok) throw new Error(`${options.method ?? "GET"} ${path} returned ${response.status}: ${await response.text()}`);
  return response.status === 204 ? null : response.json();
}

async function post(path, body) {
  return request(path, { method: "POST", body: JSON.stringify(body) });
}

async function waitForEnrichment(id) {
  let document;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    document = await request(`/v3/documents/${id}`);
    const { embeddingStatus, factStatus } = document.enrichment;
    if (!["pending", "processing"].includes(embeddingStatus) && !["pending", "processing"].includes(factStatus)) {
      return document;
    }
    await new Promise((resolve) => setTimeout(resolve, 1_500));
  }
  return document;
}

async function waitForSemanticResult() {
  let result;
  for (let attempt = 0; attempt < 12; attempt += 1) {
    result = await post("/v4/search", {
      containerTag,
      q: "検証対象が利用しているデータベースは何か",
      limit: 5,
    });
    if (result.results.length > 0) return result;
    await new Promise((resolve) => setTimeout(resolve, 1_500));
  }
  return result;
}

try {
  const health = await request("/health");
  const first = await post("/v3/documents", {
    content: "CloudflareMemorySemanticSmokeTest の current_color は blue である。current_storage は Cloudflare D1 である。",
    containerTag,
    customId: "semantic-smoke-1",
    metadata: { sm_scope: "personal", test: true },
  });
  ids.push(first.id);
  const firstDocument = await waitForEnrichment(first.id);

  const second = await post("/v3/documents", {
    content: "CloudflareMemorySemanticSmokeTest の current_color は red に変更された。current_color の現在値は red である。",
    containerTag,
    customId: "semantic-smoke-2",
    metadata: { sm_scope: "personal", test: true },
  });
  ids.push(second.id);
  const secondDocument = await waitForEnrichment(second.id);
  const semantic = await waitForSemanticResult();
  const graph = await post("/v4/graph", { containerTag, limit: 100 });
  const profile = await post("/v4/profile", { containerTag });

  const summary = {
    health,
    first: { id: first.id, enrichment: firstDocument.enrichment },
    second: { id: second.id, enrichment: secondDocument.enrichment },
    semanticSearch: {
      resultCount: semantic.results.length,
      ids: semantic.results.map((result) => result.id),
      scores: semantic.results.map((result) => result.score),
    },
    graph: {
      factCount: graph.facts.length,
      relationCount: graph.relations.length,
      statuses: graph.facts.map((fact) => fact.status),
      relations: graph.relations.map((relation) => relation.relation),
    },
    profile: { dynamicCount: profile.profile.dynamic.length },
  };
  console.log(JSON.stringify(summary, null, 2));

  assert.equal(health.semanticSearch, "cloudflare-vectorize");
  assert.equal(health.factGraph, "cloudflare-d1-workers-ai");
  assert.equal(firstDocument.enrichment.embeddingStatus, "done");
  assert.equal(firstDocument.enrichment.factStatus, "done");
  assert.equal(secondDocument.enrichment.embeddingStatus, "done");
  assert.equal(secondDocument.enrichment.factStatus, "done");
  assert.ok(semantic.results.some((result) => result.id === first.id));
  assert.ok(graph.facts.length >= 2);
  assert.ok(graph.relations.length >= 1);
  assert.ok(profile.profile.dynamic.length >= 1);
} finally {
  if (process.env.KEEP_LIVE_SMOKE !== "1") {
    for (const id of ids) {
      try {
        await request(`/v3/documents/${id}`, { method: "DELETE" });
      } catch (error) {
        console.error(`Cleanup failed for ${id}: ${String(error)}`);
      }
    }
  } else {
    console.error(`Live smoke data retained for diagnosis: ${containerTag}`);
  }
}

process.exit(0);
