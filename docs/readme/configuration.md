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

Set `"autoMemoryEnabled": false` in Claude Code if you do not want two memory systems. Remnifold never reads its built-in memory files.

`REMNIFOLD_LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG` and the OS language select recall wording: Japanese environments receive Japanese and all others English. Stored text is not translated.
