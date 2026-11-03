import test, { assert } from "./test.mjs";
test("test assertions distinguish missing values, prototypes, collections and rejected errors", async () => {
  assert.throws(() => assert.deepEqual({ a: void 0 }, {}));
  assert.throws(() => assert.deepEqual([1, 2], [2, 1]));
  assert.throws(() => assert.deepEqual(/* @__PURE__ */ Object.create(null), {}));
  assert.deepEqual(/* @__PURE__ */ new Set([{ x: 1 }, { x: 2 }]), /* @__PURE__ */ new Set([{ x: 2 }, { x: 1 }]));
  assert.throws(() => assert.deepEqual(/* @__PURE__ */ new Set([{ x: 1 }, { x: 1 }]), /* @__PURE__ */ new Set([{ x: 1 }, { x: 2 }])));
  assert.deepEqual(/* @__PURE__ */ new Map([[{ id: 1 }, { v: 2 }]]), /* @__PURE__ */ new Map([[{ id: 1 }, { v: 2 }]]));
  const a = {};
  a.self = a;
  const b = {};
  b.self = b;
  assert.deepEqual(a, b);
  assert.equal(NaN, NaN);
  assert.throws(() => assert.equal(-0, 0));
  assert.throws(() => {
    throw Error("expected");
  }, Error);
  assert.throws(() => {
    throw "";
  });
  await assert.rejects(Promise.reject(Error("boundary")), /boundary/);
  await assert.rejects(() => Promise.reject(new TypeError("typed")), TypeError);
  await assert.rejects(() => assert.rejects(Promise.resolve(1)), /Assertion failed/);
});
