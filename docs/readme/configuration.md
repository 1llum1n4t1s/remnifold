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

`recallMode`, `recallScope`, `maxMemories` and `maxRecentTasks` in `remnifold.json` control automatic recall, which is off by default. See [recall settings](recall-settings.md).

## MCP registration

`mcpRegistration` (`auto`, `direct` or `off`) controls whether Setup registers Remnifold directly or relies on the Toolport gateway on Windows, macOS and Linux. See [MCP registration](mcp-registration.md).

`REMNIFOLD_LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG` and the OS language select recall wording: Japanese environments receive Japanese and all others English. Stored text is not translated.
