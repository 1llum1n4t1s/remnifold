import { assert } from "./support/test.mjs";
import test from "./support/test.mjs";
import { parseEnrichmentPayload } from "../src/index.ts";

test("parses and normalizes facts and content topics from one enrichment payload", () => {
  const result = parseEnrichmentPayload(JSON.stringify({
    facts: [{
      subject: " Cloudflare ",
      predicate: " uses ",
      object: " D1 ",
      confidence: 0.9,
      exclusive: true,
    }],
    topics: [
      " Cloudflare　D1 ",
      "cloudflare d1",
      "repo_test__0123456789abcdef",
      "TypeScript",
    ],
  }));

  assert.deepEqual(result.facts, [{
    subject: "Cloudflare",
    predicate: "uses",
    object: "D1",
    confidence: 0.9,
    exclusive: true,
  }]);
  assert.deepEqual(result.topics, ["Cloudflare D1", "TypeScript"]);
});

test("rejects enrichment payloads with invalid facts or topics structure", () => {
  for (const payload of [{ facts: {}, topics: [] }, { facts: [], topics: {} }, null]) {
    assert.throws(
      () => parseEnrichmentPayload(JSON.stringify(payload)),
      /invalid enrichment payload/,
    );
  }
});
