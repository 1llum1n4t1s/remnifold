# MCP registration

`mcpRegistration` in `remnifold.json` controls whether Setup adds the direct `claude mcp add` / `codex mcp add` registration:

| Value | Meaning |
| --- | --- |
| `auto` | Default. On Windows, macOS and Linux, for Claude Code and Codex separately, Setup skips the direct registration when the client's settings contain the Toolport gateway (`mcpServers` in `~/.claude.json`, or under `CLAUDE_CONFIG_DIR`; `~/.codex/config.toml`, or under `CODEX_HOME`) and the Toolport registry (`%APPDATA%\Toolport\registry.json` on Windows, `~/Library/Application Support/Toolport/registry.json` on macOS, `$XDG_CONFIG_HOME/Toolport/registry.json` on Linux when absolute, otherwise `~/.config/Toolport/registry.json`; or `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`) has an enabled server that runs a command whose file name is `remnifold` (case-insensitive, ignoring a `.exe` suffix) with the `mcp` argument in every profile in use. When this cannot be determined, Setup registers directly; on other operating systems it does not check and always registers directly. |
| `direct` | Always registers directly. |
| `off` | Setup neither adds nor repairs the registration. Removal on uninstall continues. |

Values are matched exactly in lowercase; missing or `null` is `auto`, and other values are logged and treated as `auto`.

- Setup never removes an existing direct registration. When it rebuilds its state (first run, a moved executable or a missing state file) while Toolport serves Remnifold, a direct registration pointing to the old location stays; remove it with `claude mcp remove remnifold --scope user` if needed.
- A client Setup registered directly is not re-added if you remove it; only a skipped client can return to direct registration.
- If the gateway disappears, or the executable Toolport points to no longer exists, the next Remnifold start registers directly. Disabling or deleting the server in Toolport adds nothing. Clients left unfinished earlier are checked the same way later.
- Claude Code hooks are still merged when the direct registration is skipped, if the Claude Code CLI or `~/.claude` exists.

`connect` does not check and always registers directly. To remove only the direct registration, run `claude mcp remove remnifold --scope user` or `codex mcp remove remnifold`; `connect claude-code --remove --apply` also removes the hooks.

The `PreToolUse` hook matcher is `^mcp__remnifold__|^mcp__toolport__toolport_(call_tool|run_script)$` (status "Checking tool access..."). A `toolport_call_tool` call is allowed automatically, and `search_memory`'s `sessionId: "current"` is resolved, when its `name` is exactly the id of a Remnifold server in the Toolport registry converted to Toolport's tool-name prefix (every character other than `A-Z a-z 0-9 _` becomes `_`, so `my-server` becomes `my_server`), then `__`, then one of the read-only tools (`search_memory`, `listTopics`, `listSpaces`, `listMemories`, `listDocuments`, `getDocument`, `whoAmI`). A `toolport_run_script` call is never allowed automatically, because Remnifold cannot prove that a script calls only read-only tools. When the script calls Remnifold's `search_memory` with `sessionId: "current"`, the hook prepends one line of JavaScript to the script (other inputs such as `data` and `input` stay as they are); at run time it replaces `"current"` with the real session ID in the `<prefix>__search_memory` / `searchMemory` calls that reach `toolport.call` and `callAsync` (the `servers.<id>.*` stubs and `callAll` go through them). It does not apply in auto permission mode (a call changed without a decision is denied there) or to a script that starts with `"use strict"` (a prepended line would disable strict mode); in those cases `"current"` inside `run_script` is not resolved and returns the usual fixed error. It does not apply when the gateway's server name is not `toolport`, or to `toolport_fetch_result`; if the prefix does not take effect, `"current"` arrives unchanged and returns the usual fixed error.

