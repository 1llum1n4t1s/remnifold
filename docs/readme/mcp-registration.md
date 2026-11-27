# MCP registration

`mcpRegistration` in `remnifold.json` controls whether Setup adds the direct `claude mcp add` / `codex mcp add` registration:

| Value | Meaning |
| --- | --- |
| `auto` | Default. On Windows, macOS and Linux, per client, Setup skips direct registration when the client's settings contain the Toolport gateway and the Toolport registry has an enabled server running `remnifold mcp` in every profile in use. If its command points to the same executable Setup registers (paths are canonicalized before comparing; a case-only difference counts as the same), Setup records `toolport`; if it points to a different executable, Setup records `toolport-foreign` (and warns). Either way it skips direct registration. If this cannot be determined, or on other systems, it registers directly. |
| `direct` | Always registers directly. |
| `off` | Setup neither adds nor repairs the registration; uninstall still removes it. |

Values are matched exactly in lowercase; missing, `null` or unknown values mean `auto`. The client settings checked are `~/.claude.json` (or `CLAUDE_CONFIG_DIR`) and `~/.codex/config.toml` (or `CODEX_HOME`); the registry is Toolport's `registry.json` (or `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`).

- Setup never removes an existing direct registration; remove a stale one with `claude mcp remove remnifold --scope user`.
- A directly registered client you removed is not re-added. If the gateway or the executable it points to disappears, the next start registers directly.
- Claude Code hooks are still merged when direct registration is skipped.
- If Toolport serves a different executable (for example a copy), Setup still skips direct registration but warns, records the reason `toolport-foreign`, and re-evaluates at every start. Point the Toolport server's command to `%LOCALAPPDATA%\RemnifoldApp\current\remnifold.exe` (on macOS and Linux, the Setup executable).

`connect` always registers directly; its JSON `toolport` notice carries `sameExecutable` (`true` for the same executable, `false` for a different one, both with `duplicate: true`). `connect claude-code --remove --apply` also removes the hooks.

Through Toolport, the hook allows Remnifold's read-only tools in `toolport_call_tool` and resolves `search_memory`'s `sessionId: "current"`. `toolport_run_script` is never allowed automatically; `"current"` there is resolved except in auto permission mode or `"use strict"` scripts, where it returns the usual error. Details: [Toolport hook rewriting](../agents/toolport-hook-rewriting.md).
