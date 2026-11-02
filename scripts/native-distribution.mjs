import { createHash } from "node:crypto";

export const NATIVE_TARGETS = Object.freeze({
  "x86_64-pc-windows-msvc": { platform: "win32", arch: "x64", executable: "bin/remnifold.exe" },
  "aarch64-apple-darwin": { platform: "darwin", arch: "arm64", executable: "bin/remnifold" },
  "x86_64-apple-darwin": { platform: "darwin", arch: "x64", executable: "bin/remnifold" },
  "x86_64-unknown-linux-gnu": { platform: "linux", arch: "x64", executable: "bin/remnifold" },
  "aarch64-unknown-linux-gnu": { platform: "linux", arch: "arm64", executable: "bin/remnifold" },
});

export const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
export function nativeTarget(target) {
  const result = NATIVE_TARGETS[target];
  if (!result) throw new Error(`Unsupported native target: ${target}`);
  return result;
}

export function nativeMcp(target) {
  return { mcpServers: { remnifold: { command: `./${nativeTarget(target).executable}`, args: ["mcp"], cwd: "." } } };
}

export function nativeHooks(target, source) {
  const hooks = structuredClone(source);
  for (const [event, registrations] of Object.entries(hooks.hooks)) {
    for (const registration of registrations) {
      for (const hook of registration.hooks) {
        hook.command = `"\${PLUGIN_ROOT}/${nativeTarget(target).executable}" hook ${event}`;
      }
    }
  }
  return hooks;
}

export function isNativeSourcePath(name) {
  if (/(?:^|\/)(?:node_modules|tests?|__tests__)(?:\/|$)|\.test\./u.test(name)) return false;
  return name === ".codex-plugin/plugin.json" || /^(?:README\.md|CHANGELOG\.md|LICENSE(?:\.md)?)$/u.test(name)
    || /^scripts\/setup-server\.(?:mjs|ps1)$/u.test(name) || name === "client/unicode-text.mjs"
    || /^(?:skills\/.*\.(?:md|ya?ml)|server\/(?:src\/.*\.ts|migrations\/.*\.sql|package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|tsconfig\.json|wrangler\.example\.jsonc))$/u.test(name);
}

export function nativeJson(value) { return `${JSON.stringify(value, null, 2)}\n`; }

export function verifyNativeFiles(files, { target, sourceCommit, version } = {}) {
  const manifest = JSON.parse(files.get("native-build.json")?.toString("utf8") ?? "null");
  if (manifest?.schema !== 1 || !/^[a-f0-9]{40}$/u.test(manifest.sourceCommit ?? "") || !manifest.files || Array.isArray(manifest.files)) throw new Error("Invalid native build manifest");
  const info = nativeTarget(manifest.target);
  if ((target && target !== manifest.target) || (sourceCommit && sourceCommit !== manifest.sourceCommit) || (version && version !== manifest.version)) throw new Error("Native build identity mismatch");
  const expected = new Set([...Object.keys(manifest.files), "native-build.json"]);
  if (files.size !== expected.size || [...files.keys()].some(name => !expected.has(name))) throw new Error("Native package file set mismatch");
  for (const [name, hash] of Object.entries(manifest.files)) {
    if ((!isNativeSourcePath(name) && ![info.executable, ".mcp.json", "hooks/hooks.json"].includes(name)) || !/^[a-f0-9]{64}$/u.test(hash) || sha256(files.get(name) ?? Buffer.alloc(0)) !== hash) throw new Error(`Native package hash mismatch: ${name}`);
  }
  if (!files.get(info.executable)?.length) throw new Error("Native executable is missing");
  if (files.get(".mcp.json")?.toString("utf8") !== nativeJson(nativeMcp(manifest.target))) throw new Error("Native MCP launcher mismatch");
  const plugin = JSON.parse(files.get(".codex-plugin/plugin.json")?.toString("utf8") ?? "null");
  if (plugin?.version !== manifest.version) throw new Error("Native package version mismatch");
  if (files.has("server/package.json") && JSON.parse(files.get("server/package.json").toString("utf8")).version !== manifest.version) throw new Error("Native server template version mismatch");
  const hooks = JSON.parse(files.get("hooks/hooks.json")?.toString("utf8") ?? "null");
  const events = ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"];
  if (Object.keys(hooks?.hooks ?? {}).sort().join() !== events.sort().join()) throw new Error("Native hooks event mismatch");
  for (const event of events) {
    const commands = hooks.hooks[event];
    if (commands.length !== 1 || commands[0].hooks?.length !== 1 || commands[0].hooks[0].command !== `"\${PLUGIN_ROOT}/${info.executable}" hook ${event}` || commands[0].hooks[0].type !== "command" || !Number.isFinite(commands[0].hooks[0].timeout) || commands[0].hooks[0].timeout <= 0) throw new Error("Native hook launcher mismatch");
  }
  return manifest;
}
