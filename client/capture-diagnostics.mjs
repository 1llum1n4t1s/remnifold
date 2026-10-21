// 診断は固定コードだけを表示し、例外本文・設定・HTTP応答を参照しない。
const failures = new WeakMap();
const diagnostics = new Map([
  ["configuration", ["configuration", "invalid", "repair-required"]],
  ["transcript", ["transcript", "unavailable", "repair-required"]],
  ["receipt-read", ["receipt", "read-failed", "repair-required"]],
  ["receipt-invalid", ["receipt", "invalid", "repair-required"]],
  ["receipt-write", ["receipt", "write-failed", "repair-required"]],
  ["transport", ["transport", "request-failed", "retry-or-repair"]],
  ["acknowledgement", ["transport", "invalid-acknowledgement", "retry-or-repair"]],
  ["payload", ["payload", "invalid", "repair-required"]],
]);

export function markCaptureFailure(error, code) {
  if (error && (typeof error === "object" || typeof error === "function") && diagnostics.has(code) && !failures.has(error)) {
    failures.set(error, code);
  }
  return error;
}

export async function captureStage(code, operation) {
  try { return await operation(); }
  catch (error) { throw markCaptureFailure(error, code); }
}

export function captureFailureMessage(error) {
  const [stage, cause, action] = diagnostics.get(error && (typeof error === "object" || typeof error === "function") ? failures.get(error) : undefined)
    || ["capture", "unknown", "retry-or-repair"];
  return `Remnifold capture failed; unacknowledged records will be retried. [stage=${stage}; cause=${cause}; action=${action}]\n`;
}
