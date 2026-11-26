# Configuration

## Connection and state

The app uses its own folders, independent of Codex and Claude Code:

| Platform | Connection and server state | Capture and recall state |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | same |
| macOS | `~/Library/Application Support/Remnifold` | same |
| Linux | `$XDG_CONFIG_HOME/remnifold` (default `~/.config/remnifold`) | `$XDG_STATE_HOME/remnifold` (default `~/.local/state/remnifold`) |

The connection is `remnifold.json`. `REMNIFOLD_HOME` overrides the locations: connection at its root, state under `remnifold/`, app under `app/`. `setup --config-dir <directory>` selects another connection folder. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` override the connection; `--api-key-stdin` supports noninteractive setup.

Without a new connection, the first run copies a valid legacy one from Codex home. Earlier state is read but only the new folder is written. Legacy files are never deleted; `server-setup` moves its state only with `--apply`.

## Recall

Set these in `remnifold.json`:

| Setting | Meaning |
| --- | --- |
| `recallMode` | Default `off` disables automatic recall. `direct`, or any other non-empty value, enables it. Capture and manual tools remain available when off. |
| `recallScope` | Default and invalid values mean `session`. `all` allows other sessions and recent tasks. |
| `maxMemories` | Automatic and manual limit, 1–5; default 3. |
| `maxRecentTasks` | 0–3 in `all` mode; default 2, and 0 disables recent-work context. No effect in `session`. |

With `recallMode: off`, registration omits `SessionStart` and `UserPromptSubmit` but retains `Stop` and `PreToolUse`. Setup re-syncs hooks at the next logon, launch without arguments or MCP start; Claude Code uses them from the following session. Strict registrations and plugin hooks stay fixed but return immediately when off. Run current `connect claude-code --apply` to update a manual registration.

## MCP registration

`mcpRegistration` in `remnifold.json` controls whether Setup adds the direct `claude mcp add` / `codex mcp add` registration:

| Value | Meaning |
| --- | --- |
| `auto` | Default. On Windows, for Claude Code and Codex separately, Setup skips the direct registration when the client's settings contain the Toolport gateway (`mcpServers` in `~/.claude.json`, or under `CLAUDE_CONFIG_DIR`; `~/.codex/config.toml`, or under `CODEX_HOME`) and the Toolport registry (`%APPDATA%\Toolport\registry.json`, or `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`) has an enabled server that runs `remnifold.exe` with the `mcp` argument in every profile in use. When this cannot be determined, Setup registers directly; on other platforms it does not check and always registers directly. |
| `direct` | Always registers directly. |
| `off` | Setup neither adds nor repairs the registration. Removal on uninstall continues. |

Values are matched exactly in lowercase; a missing or `null` value is `auto`, and any other value (such as `"Off"` or one with surrounding spaces) is treated as `auto` and logged. When Setup rebuilds its state (first run, a moved executable, or a missing state file) while Toolport serves Remnifold, it does not call the CLI, so an existing direct registration that points to the executable's previous location is not re-pointed and stays; remove it with `claude mcp remove remnifold --scope user` if needed. Setup never removes an existing direct registration, and a client that Setup registered directly once is not registered again automatically if you remove that registration (only a client Setup skipped can go back to direct registration). If the gateway later disappears from the client, or the executable Toolport points to no longer exists, the next Remnifold start (`mcp`, a start without arguments, or the sign-in apply) registers directly; disabling or deleting the server in Toolport adds nothing. A client left unfinished earlier (its command-line tool was missing, or registration failed) is checked the same way when it is handled later. Toolport's quarantine state is not consulted. Claude Code's hooks in `settings.json` are merged even when the direct registration is skipped, but only when the Claude Code command-line tool or the `~/.claude` folder exists.

`connect` does not check and always registers directly. To remove only the direct registration, run `claude mcp remove remnifold --scope user` or `codex mcp remove remnifold`; `connect claude-code --remove --apply` also removes the hooks.

The `PreToolUse` hook matcher is `^mcp__remnifold__|^mcp__toolport__toolport_call_tool$` (status "Checking tool access..."). A `toolport_call_tool` call is allowed automatically, and `search_memory`'s `sessionId: "current"` is resolved, when its `name` is exactly the id of a Remnifold server in the Toolport registry converted to Toolport's tool-name prefix (every character other than `A-Z a-z 0-9 _` becomes `_`, so `my-server` becomes `my_server`), then `__`, then one of the read-only tools (`search_memory`, `listTopics`, `listSpaces`, `listMemories`, `listDocuments`, `getDocument`, `whoAmI`). It does not apply when the gateway's server name is not `toolport` or inside `toolport_run_script`.

Set `"autoMemoryEnabled": false` in Claude Code if you do not want two memory systems. Remnifold never reads its built-in memory files.

`REMNIFOLD_LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG` and the OS language select recall wording: Japanese environments receive Japanese and all others English. Stored text is not translated.
