let caseMetrics=[];
export function startMetrics(){caseMetrics=[];}
export function recordMetrics(name,values){caseMetrics.push({name,...structuredClone(values)});}
export function collectedMetrics(){return caseMetrics;}
const cases = [];
function test(name, options, run) {
  cases.push({ name, run: typeof options === "function" ? options : run, timeout: options?.timeout ?? 3e4 });
}
function deepEqual(a, b, seen = /* @__PURE__ */ new Map()) {
  if (Object.is(a, b)) return true;
  if (!a || !b || typeof a !== "object" || typeof b !== "object" || Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  if (seen.get(a) === b) return true;
  seen.set(a, b);
  if (a instanceof Date) return a.getTime() === b.getTime();
  if (a instanceof RegExp) return a.source === b.source && a.flags === b.flags;
  if (a instanceof Set) {
    if (a.size !== b.size) return false;
    const remaining = [...b];
    for (const x of a) {
      const i = remaining.findIndex((y) => deepEqual(x, y, new Map(seen)));
      if (i < 0) return false;
      remaining.splice(i, 1);
    }
    return true;
  }
  if (a instanceof Map) {
    if (a.size !== b.size) return false;
    const remaining = [...b];
    for (const [k, v] of a) {
      const i = remaining.findIndex(([bk2, bv]) => deepEqual(k, bk2, new Map(seen)) && deepEqual(v, bv, new Map(seen)));
      if (i < 0) return false;
      remaining.splice(i, 1);
    }
    return true;
  }
  const ak = Reflect.ownKeys(a), bk = Reflect.ownKeys(b);
  return ak.length === bk.length && ak.every((k) => Object.hasOwn(b, k) && deepEqual(a[k], b[k], seen));
}
function fail(message, a, b) {
  throw new Error(`${message || "Assertion failed"}${arguments.length > 1 ? `
actual: ${JSON.stringify(a)?.slice(0,4096)}
expected: ${JSON.stringify(b)?.slice(0,4096)}` : ""}`);
}
function matches(error, expected) {
  if (!expected) return true;
  if (expected instanceof RegExp) return expected.test(String(error));
  if (typeof expected === "function") return expected === Error || expected.prototype instanceof Error ? error instanceof expected : expected(error) === true;
  return Object.keys(expected).every((k) => expected[k] instanceof RegExp ? expected[k].test(error[k]) : deepEqual(error[k], expected[k]));
}
const assert = { ok(v, m) {
  if (!v) fail(m, v, true);
}, equal(a, b, m) {
  if (!Object.is(a, b)) fail(m, a, b);
}, notEqual(a, b, m) {
  if (Object.is(a, b)) fail(m, a, b);
}, deepEqual(a, b, m) {
  if (!deepEqual(a, b)) fail(m, a, b);
}, match(a, b, m) {
  if (typeof a !== "string" || !b.test(a)) fail(m, a, String(b));
}, doesNotMatch(a, b, m) {
  if (typeof a !== "string" || b.test(a)) fail(m, a, String(b));
}, throws(fn, expected, m) {
  let caught, thrown = false;
  try {
    fn();
  } catch (e) {
    caught = e;
    thrown = true;
  }
  if (!thrown || !matches(caught, expected)) fail(m, String(caught), String(expected));
  return caught;
}, async rejects(fn, expected, m) {
  let caught, thrown = false;
  try {
    await (typeof fn === "function" ? fn() : fn);
  } catch (e) {
    caught = e;
    thrown = true;
  }
  if (!thrown || !matches(caught, expected)) fail(m, String(caught), String(expected));
  return caught;
} };
export {
  assert,
  cases,
  deepEqual,
  test as default
};
