import { readFileSync } from "node:fs";

const event = process.argv[2];
let payload;
try {
  payload = JSON.parse(readFileSync(0, "utf8"));
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) process.exit(0);
} catch {
  process.exit(0);
}

if (event === "SessionStart" || event === "UserPromptSubmit") {
  const { runHook } = await import("../client/memory-hooks.mjs");
  const result = await runHook(event, payload);
  if (Object.keys(result).length) process.stdout.write(JSON.stringify(result));
} else if (event === "PreToolUse") {
  const readOnly = new Set(["search_memory", "listTopics", "listSpaces", "listMemories", "listDocuments", "getDocument", "whoAmI"]);
  const name = /^mcp__remnifold__(.+)$/.exec(payload.tool_name || "")?.[1];
  if (readOnly.has(name)) process.stdout.write(JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "allow",
      permissionDecisionReason: "Memory index and document access is read-only.",
      updatedInput: payload.tool_input || {},
    },
  }));
} else if (event === "Stop") {
  const { captureFailureMessage } = await import("../client/capture-diagnostics.mjs");
  try {
    const { capture } = await import("../client/capture.mjs");
    const result = await capture(payload);
    if (result.saved || result.pending) process.stdout.write(JSON.stringify({
      systemMessage: `Remnifold: saved ${result.saved} documents; pending ${result.pending}.`,
    }));
  } catch (error) {
    process.stderr.write(captureFailureMessage(error));
    process.exitCode = 1;
  }
}
