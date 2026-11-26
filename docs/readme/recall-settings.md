# Recall settings

Set these in `remnifold.json`:

| Setting | Meaning |
| --- | --- |
| `recallMode` | Default `off` disables automatic recall. `direct`, or any other non-empty value, enables it. Capture and manual tools remain available when off. |
| `recallScope` | Default and invalid values mean `session`. `all` allows other sessions and recent tasks. |
| `maxMemories` | Automatic and manual limit, 1–5; default 3. |
| `maxRecentTasks` | 0–3 in `all` mode; default 2, and 0 disables recent-work context. No effect in `session`. |

With `recallMode: off`, registration omits `SessionStart` and `UserPromptSubmit` but retains `Stop` and `PreToolUse`. Setup re-syncs hooks at the next logon, launch without arguments or MCP start; Claude Code uses them from the following session. Strict registrations and plugin hooks stay fixed but return immediately when off. Run current `connect claude-code --apply` to update a manual registration.

Set `"autoMemoryEnabled": false` in Claude Code if you do not want two memory systems. Remnifold never reads its built-in memory files.
