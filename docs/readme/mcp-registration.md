# MCP registration

`mcpRegistration` in `remnifold.json` controls whether Setup adds the direct `claude mcp add` / `codex mcp add` registration:

| Value | Meaning |
| --- | --- |
| `auto` | Default. On Windows, for Claude Code and Codex separately, Setup skips the direct registration when the client's settings contain the Toolport gateway (`mcpServers` in `~/.claude.json`, or under `CLAUDE_CONFIG_DIR`; `~/.codex/config.toml`, or under `CODEX_HOME`) and the Toolport registry (`%APPDATA%\Toolport\registry.json`, or `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`) has an enabled server that runs `remnifold.exe` with the `mcp` argument in every profile in use. When this cannot be determined, Setup registers directly; on other platforms it does not check and always registers directly. |
| `direct` | Always registers directly. |
| `off` | Setup neither adds nor repairs the registration. Removal on uninstall continues. |

Values are matched exactly in lowercase; missing or `null` is `auto`, and other values are logged and treated as `auto`.

- Setup never removes an existing direct registration. When it rebuilds its state (first run, a moved executable or a missing state file) while Toolport serves Remnifold, a direct registration pointing to the old location stays; remove it with `claude mcp remove remnifold --scope user` if needed.
- A client Setup registered directly is not re-added if you remove it; only a skipped client can return to direct registration.
- If the gateway disappears, or the executable Toolport points to no longer exists, the next Remnifold start registers directly. Disabling or deleting the server in Toolport adds nothing. Clients left unfinished earlier are checked the same way later.
- Claude Code hooks are still merged when the direct registration is skipped, if the Claude Code CLI or `~/.claude` exists.

`connect` does not check and always registers directly. To remove only the direct registration, run `claude mcp remove remnifold --scope user` or `codex mcp remove remnifold`; `connect claude-code --remove --apply` also removes the hooks.

The `PreToolUse` hook matcher is `^mcp__remnifold__|^mcp__toolport__toolport_call_tool$` (status "Checking tool access..."). A `toolport_call_tool` call is allowed automatically, and `search_memory`'s `sessionId: "current"` is resolved, when its `name` is exactly the id of a Remnifold server in the Toolport registry converted to Toolport's tool-name prefix (every character other than `A-Z a-z 0-9 _` becomes `_`, so `my-server` becomes `my_server`), then `__`, then one of the read-only tools (`search_memory`, `listTopics`, `listSpaces`, `listMemories`, `listDocuments`, `getDocument`, `whoAmI`). It does not apply when the gateway's server name is not `toolport` or inside `toolport_run_script`.

