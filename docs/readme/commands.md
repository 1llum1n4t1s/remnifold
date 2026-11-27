# Local app commands

## Update, verify and register

```sh
remnifold install --version <published-version>                 # verify publication
remnifold install --version <published-version> --apply         # install
remnifold install --version <published-version> --verify-only   # verify installed version
remnifold connect claude-code                                    # preview registration
remnifold connect claude-code --apply                            # register
remnifold connect claude-code --remove --apply                   # remove registration
```

Use the fixed executable, or add its `bin` folder to `PATH`. Strict `install` uses `%LOCALAPPDATA%\remnifold` on Windows or `~/.cache/remnifold` elsewhere; `--work-root <absolute-path>` selects a work folder outside app, configuration and repository folders.

`connect claude-code --apply` uses `claude mcp add --scope user remnifold -- <executable> mcp`; if the CLI is absent it prints the command. `--cli <path>` selects a CLI. It merges `SessionStart`, `UserPromptSubmit`, asynchronous `Stop` and `PreToolUse` into `settings.json` under `$CLAUDE_CONFIG_DIR` or `~/.claude`. With recall off, it omits the first two. The first change keeps `settings.json.remnifold-backup`; malformed settings stay unchanged.

`--remove` previews unless `--apply` is present and removes only Remnifold's MCP entry and hooks. `--executable <path>` limits it to that path. `connect codex --apply` registers MCP only: Codex has no supported hook registration outside plugins. `connect` never checks for Toolport and always registers directly; when Toolport already serves Remnifold, its JSON result adds `toolport: {serverId, command, duplicate: true, sameExecutable}` as a notice; `sameExecutable` is `true` when Toolport's command points to the same executable and `false` when it points to a different one. See [MCP registration](mcp-registration.md). Start a new session after registering.

Inside this repository `.mcp.json` takes precedence. To use the installed app, add `"disabledMcpjsonServers": ["remnifold"]` to the untracked `.claude/settings.local.json`.

## Status and old records

Run `remnifold status` to check the API. `remnifold import --limit 10` previews a bounded Codex import; `--apply` uploads and `--codex-home` selects the session location.

After updating the server, preview old records for topic classification:

```sh
remnifold topics-setup --limit 10
```

`--limit` accepts 1–50. `--apply` queues Workers AI enrichment, which is billed, refreshes derived data and invalidates affected checkpoints. Opt-outs and revisions are preserved. Accepted jobs are not completed classifications: wait, then inspect `getDocument` and `listTopics` before the next batch.

Memory organization commands are in [memory organization](memory-organization.md).
