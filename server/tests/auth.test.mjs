import { assert } from "./support/test.mjs";
import test from "./support/test.mjs";
import worker from "../src/index.ts";

for (const secret of [undefined, "", "   "]) {
  test(`missing or blank configured key denies access (${JSON.stringify(secret)})`, async () => {
    for (const authorization of [undefined, "Bearer ", "Bearer wrong"]) {
      const headers = authorization ? { authorization } : {};
      const response = await worker.fetch(new Request("https://memory.example/v3/session", { headers }),
        { MEMORY_API_KEY: secret, AI_ENRICHMENT_MODE: "off" }, {});
      assert.equal(response.status, 401);
    }
  });
}

test("configured key requires matching bearer authentication", async () => {
  for (const [authorization, status] of [[undefined, 401], ["Bearer wrong", 401], ["Bearer test-key", 200]]) {
    const response = await worker.fetch(new Request("https://memory.example/v3/session", {
      headers: authorization ? { authorization } : {},
    }), { MEMORY_API_KEY: "test-key", AI_ENRICHMENT_MODE: "off" }, {});
    assert.equal(response.status, status);
  }
});

test("REST JSON streaming preserves decoded UTF-8 size limits and cancels oversized bodies", { timeout: 10_000 }, async () => {
  const limit = 2 * 1024 * 1024;
  const encoder = new TextEncoder();
  const prefix = encoder.encode('{"projection":false,"padding":"');
  const suffix = encoder.encode('"}');
  const padded = (size, unit = "x") => {
    const fixed = prefix.length + suffix.length;
    const fill = unit.repeat(Math.floor((size - fixed) / encoder.encode(unit).length));
    return encoder.encode(`{"projection":false,"padding":"${fill}${"x".repeat(size - fixed - encoder.encode(fill).length)}"}`);
  };
  const cases = [
    { name: "empty", chunks: [], status: 200 },
    { name: "exact limit", chunks: [padded(limit)], status: 400 },
    { name: "large chunk", chunks: [padded(limit + 1)], status: 413, cancel: true },
    { name: "pending cancel acknowledgement", chunks: [padded(limit + 1)], status: 413, cancel: true, pendingCancel: true },
    { name: "UTF-8 split", chunks: [...encoder.encode('{"projection":false,"padding":"😀あ"}')].map((byte) => Uint8Array.of(byte)), status: 400 },
    { name: "multibyte exact limit", chunks: [padded(limit, "😀")], status: 400 },
    { name: "split BOM exact limit", chunks: [Uint8Array.of(0xef), Uint8Array.of(0xbb), Uint8Array.of(0xbf), padded(limit)], status: 400 },
    { name: "invalid UTF-8 replacement exact limit", chunks: [prefix, encoder.encode("x".repeat(limit - prefix.length - suffix.length - 3)), Uint8Array.of(0xff), suffix], status: 400 },
    { name: "invalid UTF-8 replacement over limit", chunks: [prefix, encoder.encode("x".repeat(limit - prefix.length - suffix.length - 2)), Uint8Array.of(0xff), suffix], status: 413, cancel: true },
    { name: "incomplete UTF-8 at EOF", chunks: [encoder.encode(" ".repeat(limit - 2)), Uint8Array.of(0xf0)], status: 413 },
  ];
  for (const scenario of cases) {
    let position = 0;
    let cancelled = false;
    const body = new ReadableStream({
      pull(controller) {
        if (position < scenario.chunks.length) controller.enqueue(scenario.chunks[position++]);
        else controller.close();
      },
      cancel() { cancelled = true; if (scenario.pendingCancel) return new Promise(() => {}); },
    }, { highWaterMark: 0 });
    const response = await worker.fetch(new Request("https://memory.example/v3/documents/list", {
      method: "POST", headers: { authorization: "Bearer test-key" }, body, duplex: "half",
    }), {
      MEMORY_API_KEY: "test-key", AI_ENRICHMENT_MODE: "off",
      DB: { prepare: () => ({ bind() { return this; } }), batch: async () => [{ results: [{ total: 0 }] }, { results: [] }] },
    }, {});
    assert.equal(response.status, scenario.status, scenario.name);
    if (scenario.status === 400) assert.match(await response.text(), /projection must be/iu, scenario.name);
    if (scenario.status === 413) assert.match(await response.text(), /Request body is too large/u, scenario.name);
    if (scenario.cancel) assert.equal(cancelled, true, `${scenario.name}: cancel`);
  }
  let pulls = 0;
  let cancelled = false;
  const response = await worker.fetch(new Request("https://memory.example/v3/documents/list", {
    method: "POST", headers: { authorization: "Bearer test-key" }, duplex: "half",
    body: new ReadableStream({
      pull(controller) { pulls++; controller.enqueue(new Uint8Array(64 * 1024).fill(0x20)); },
      cancel() { cancelled = true; },
    }, { highWaterMark: 0 }),
  }), { MEMORY_API_KEY: "test-key", AI_ENRICHMENT_MODE: "off" }, {});
  assert.equal(response.status, 413);
  assert.equal(pulls, 33, "stop reading immediately after decoded limit is exceeded");
  assert.equal(cancelled, true);
});
