# Remnifold

A self-hosted Cloudflare memory server with remote MCP access and a Codex plugin for recalling useful implementation history across project folders.

The plugin saves completed work in a shared collection and keeps its project folder as provenance. Browse records by content topics, or ask about a topic such as “Chrome extension”: recall searches across memory spaces in one server request, injects concise indexes, and lets Codex open an applicable source record through MCP before reusing the implementation.

## Requirements

Remote MCP clients connect to the deployed server without installing Node.js or Codex locally. The optional Codex integration requires Codex with plugin and hook support and the package matching your operating system and CPU. Its MCP server, hooks, connection setup, history import, and local plugin updater run as a native executable without Node.js. Local plugin updates also require authenticated GitHub CLI and native Codex CLI.

Windows packages using the Git Bash hook launcher require Git for Windows at the standard `C:\Program Files\Git` location, including `usr\bin\sh.exe`, and `.EXE` in the Codex process's `PATHEXT`. Nonstandard Git locations are unsupported. The fixed Git Bash launcher passes the original hook input to the native executable; it does not add Node.js or change the macOS/Linux launchers.

Before applying that Windows update, install trusted Git for Windows at the standard location and restart Codex so it inherits the normal Windows environment. New updaters reject `--apply` before changing the installation with `WINDOWS_GIT_BASH_REQUIRED` when the required shell is unavailable, `WINDOWS_PATHEXT_EXE_REQUIRED` when `.EXE` is missing from `PATHEXT`, or `WINDOWS_GIT_BASH_UNSAFE_RESOLUTION` when an extensionless file, `sh.ps1`, or another `PATHEXT` candidate could replace `sh.exe`. `WINDOWS_GIT_BASH_UNSUPPORTED_LOCATION` means Windows's actual Program Files location does not match the supported fixed location. Repair the corresponding prerequisite and retry the same update command; nonstandard installations remain unsupported. Do not substitute another executable through `PATH` or the workspace.

These are installation-time path and resolution checks, not continuous verification of Git's files or permissions. A successful `--apply` checks the updater process's `PATHEXT`, not an already running Codex environment snapshot. Restart Codex from a normal Windows environment, open a new task to load the updated plugin, and verify capture and recall there. Keep the trusted Git installation protected afterward. Publication checks and artifact-only `--verify-only` do not establish that hooks can run in Codex's environment. Older updaters, including 1.0.16, cannot perform these new prerequisite checks, so satisfy them before updating with an older executable.

Server setup and maintenance use the same native executable and your own Cloudflare account with Workers, D1, Vectorize, and Workers AI enabled. No Node.js, package manager, Wrangler, or Rust installation is required to use the platform package.

## Connect a remote MCP client

After the server has been updated and verified below, use `https://your-worker.example.workers.dev/mcp` as the Streamable HTTP MCP endpoint. A compatible client can authenticate with OAuth 2.1 discovery, public dynamic client registration, and PKCE S256. On the server's authorization page, enter the existing memory API key and approve the named client. If the completion page does not return to the client automatically, follow its link to continue. OAuth tokens are valid for MCP only; existing REST clients continue to use the API key. Clients supporting a configured Bearer credential can also use the memory API key directly. Keep keys and tokens private.

[Grok Bot's Connect apps](https://docs.x.ai/grok-bot/overview) and [Grok connectors](https://docs.x.ai/grok/connectors) provide app and custom MCP connection paths. Choose OAuth when the client supports it; availability of manual Bearer-key entry in Grok Bot has not been verified.

For ChatGPT, follow the [private plugin setup](https://developers.openai.com/plugins/build/plugins): choose **+ Add custom MCP server**, enter the `/mcp` URL and authentication, then choose **Create as a plugin**. This private connection does not require submission to the public directory. [ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) uses supported, installed and enabled plugins; confirm in your account that this plugin appears and works in dots. Direct entry of an arbitrary MCP URL in dots has not been verified.

The server asks the AI to search for relevant context, open full records when needed, and save durable outcomes. Tool use is chosen by the client and model, so automatic searches or saves are not guaranteed. Ask the AI to search or save explicitly when needed. This remote integration does not replace the existing Codex stdio server or hooks; avoid registering a duplicate remote server under the same Codex integration.

## Use with Claude Code

The installed native executable also works as a local Claude Code integration on the same connection (`~/.codex/remnifold.json`). Register its stdio MCP server at user scope and add command hooks for `SessionStart` and `UserPromptSubmit` (recall) and an asynchronous `Stop` hook (capture), each running `remnifold hook <event>`. Claude Code runs hook commands through Git Bash on Windows and passes the event JSON on stdin. Because the Codex plugin cache path contains the version, point both registrations at a launcher that selects the newest installed `remnifold/<version>/bin/remnifold.exe`. Inside this repository, the source `.mcp.json` (`cargo run`) takes precedence over the user registration; add `"disabledMcpjsonServers": ["remnifold"]` to the untracked `.claude/settings.local.json` to use the installed executable instead.

`Stop` capture reads Claude Code transcripts as well as Codex rollouts. It stores only each human request and the final assistant text, skipping thinking, tool calls and results, request copies and other sessions' rows. A reply after a background notification is stored as a separate continuation turn so that already saved turns keep their capture keys. Claude Code records carry `sm_source: "claude-code"`. To avoid two memory systems, disable Claude Code's built-in auto memory with `"autoMemoryEnabled": false`; Remnifold does not read or import those files.

## Connect an existing server

Starting with 1.0.16, published native packages are served from [Remnifold updates](https://remnifold-updates.kagayoi.com). Earlier releases retain their existing distribution path. The version index is `releases/<version>/index.json`; its manifest selects a hash-named archive for each target below. Authenticate publication through GitHub before running a downloaded executable; an R2 URL or a commit-status link alone does not establish its origin.

| Platform | Rust target |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Mac Intel is not supported.

Starting with 1.0.20, releases use a four-target schema-2 manifest and V2 publication witness. An updater from 1.0.19 or earlier cannot read this format: first use the authenticated Windows bootstrap or trusted-source Unix bootstrap below to obtain the new updater. Keep the same server connection and existing registration. The new updater still reads historical five-target schema-1 releases and their verification receipts; historical releases are not rewritten.

For an existing `remnifold@personal` installation, use the native executable's `plugin-update` command with authenticated native GitHub CLI and Codex CLI. The updater verifies the release commit, successful CI publisher logs, R2 manifest, archive hashes, and package source before using Codex's official plugin CLI. Publication does not update your installation automatically.

Run these commands from the extracted plugin folder, using the same published version and work root for all three steps:

```sh
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Claude/remnifold/plugin-update --check-published
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Claude/remnifold/plugin-update --apply
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Claude/remnifold/plugin-update --verify-only
```

On macOS and Linux, use `./bin/remnifold` and an absolute external work directory. On every platform, keep the work root outside the repository, plugin source, and Codex home, with no linked ancestors; Windows requires a child of `D:/Claude/remnifold`. Keep the `verified-<commit>-<target>.json` receipt in that work root for later verification. Connection and capture state are preserved; a new task is still required to load the updated plugin.

An older client cannot discover the R2 distribution itself. Windows uses the native `plugin-bootstrap` command. Choose a published release that includes both this command and the `remnifold-bootstrap-x86_64-pc-windows-msvc.exe` GitHub Release asset. Releases predating that asset can still be reached from a trusted Rust source build; an unverified executable downloaded from R2 is not a bootstrap trust root.

Obtain the first executable from an immutable GitHub Release using native GitHub CLI 2.93.0 or newer with `release verify` and `release verify-asset` support. Check `gh --version` before using these verification commands; older versions are affected by [CVE-2026-48501](https://github.com/advisories/GHSA-8xvp-7hj6-mcj9). Select an existing trusted private directory where another user cannot replace the downloaded file between verification and execution, and a new absolute output filename. Git Bash `chmod` alone does not establish Windows ACL safety. Replace the quoted version and path below, then run from Git Bash:

```sh
version='<published-version>'
tag="v$version"
repo=github.com/1llum1n4t1s/remnifold
bootstrap='/absolute/trusted/private/path/remnifold-bootstrap.exe'
gh release verify "$tag" --repo "$repo" &&
gh release download "$tag" --repo "$repo" --pattern remnifold-bootstrap-x86_64-pc-windows-msvc.exe --output "$bootstrap" &&
gh release verify-asset "$tag" "$bootstrap" --repo "$repo" &&
"$bootstrap" plugin-bootstrap --version "$version" --work-root D:/Claude/remnifold/bootstrap
```

The release and local asset must both pass GitHub's signature verification before execution. Do not substitute an unverified R2 helper or bypass a failed verification. The bootstrap then independently checks the exact GitHub CI publisher-log witness, R2 manifest, release commit, archive size and digest. It does not require an installed plugin for publication checking or download-only mode. The default checks publication; add `--apply` only to migrate an existing registered installation, followed by receipt verification. For an unregistered installation, use `--download-only` and the official registration procedure below. Legacy `-Version`, `-WorkRoot`, `-Apply`, and `-DownloadOnly` argument names remain readable by the native command.

On macOS and Linux, obtain and review the trusted-source [Bash bootstrap](scripts/bootstrap-plugin.sh), saving it outside the work directory. It requires native GitHub CLI, curl, tar, and `sha256sum` or `shasum`:

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --work-root /absolute/external/remnifold-bootstrap
```

The Unix default checks publication without invoking Codex or requiring an installed plugin. If the authenticated target release lacks native bootstrap support, use `--download-only`, or a trusted current native `plugin-bootstrap --version <older-published-version>`.

The external work-root requirements above apply to both bootstraps. They retain invocation state and never send GitHub credentials to R2. Explicit apply creates the updater verification receipt; a publication check or download-only operation creates no installation receipt. A later installation failure can leave a receipt for recovery. When witness logs expire, fresh downloads fail closed; an existing updater receipt still supports `--verify-only` without retained R2 objects or CI logs. GitHub access to release source metadata remains required.

For a first installation, add `--download-only`. This returns the verified formal archive without executing its contents, requiring an existing plugin, or invoking Codex. Windows copies and rehashes it into a new private folder under the OS user profile's `Remnifold/Downloads`; the D: work root holds processing state only. Unsafe delivery ancestors are rejected, and existing ACLs are not changed. Extract the archive into the OS user profile's `plugins` directory so the source is `plugins/remnifold`, matching the updater default. Open that folder in Codex and ask plugin-creator to register the source in the personal marketplace and install through `codex plugin add remnifold@personal`. Custom marketplace sources use download-only and the verified executable's `plugin-update --config <absolute-local-config>` procedure. Rust is not required for the published native bootstrap. A trusted source build remains an alternative; local checkout checks alone do not establish release-commit identity. Historical releases without R2 configuration retain their GitHub-artifact update compatibility.

```sh
./bin/remnifold.exe setup --base-url https://your-worker.example.workers.dev
```

On macOS and Linux, use `./bin/remnifold` in place of `./bin/remnifold.exe` in these commands. Run them from the extracted plugin folder. Enter the API key at the hidden prompt if it is not already supplied. Setup verifies an authenticated session before writing `remnifold.json` in the selected Codex home: `--codex-home`, then `CODEX_HOME`, then the default `~/.codex`. Run Codex with the same home so it reads that connection. To update an existing connection, use `--force`; unrelated settings are preserved. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` can supply process-level overrides. For noninteractive setup, `--api-key-stdin` reads the key from standard input.

After installing or updating the plugin, review any hook trust requested by Codex for its four hooks, then start a new task to load the updated plugin. Run the plugin's `whoAmI` MCP tool to verify connectivity and space discovery. Installing a plugin alone does not trust its hooks.

To import existing Codex sessions, preview a bounded batch with `./bin/remnifold.exe import --limit 10` (macOS/Linux: `./bin/remnifold import --limit 10`). Add `--apply` when you want to upload the sanitized records. The preview does not write remote memories; repeat imports reuse deterministic capture identities.

Bootstrap release publication is a maintainer procedure documented in [AGENTS.md](AGENTS.md#bootstrap-release-publication).

## Create or update a server

The package includes the Worker bundle and migrations. Provide a Cloudflare API token through the process environment `CLOUDFLARE_API_TOKEN` or `--token-stdin`, then preview the existing resources:

```sh
./bin/remnifold.exe server-setup --account-id <Cloudflare-account-id>
```

On macOS and Linux use `./bin/remnifold`. The default is authenticated, read-only discovery. The token needs account read, Workers scripts, D1, and Vectorize permissions; domain or route changes also require the corresponding Zone, Routes, or Custom Domain permissions. Setup does not import Wrangler OAuth credentials. Keep the token and memory API key private.

For an existing installation, pass `--config <absolute-existing-jsonc-path>` to read its configuration without overwriting it. Setup reuses resource names in the order `remnifold`, `kagayoi-memory`, then `cloudflare-supermemory`; explicit `--worker-name`, `--database-name`, and `--vector-index-name` take priority. Check the plan selects the existing Worker, D1 database, and Vectorize index. Configured IDs and vector dimensions/metric must match. Existing bindings, cron schedules, and routes are preserved; conflicts stop setup. Do not create a replacement server or copy memories for this upgrade.

When you intend to provision or deploy, provide the chosen memory key through `REMNIFOLD_API_KEY` and apply the plan:

```sh
./bin/remnifold.exe server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

Use the same configuration and names as the preview. `--apply` creates missing resources, installs a supplied key as the Worker secret, applies additive D1 migrations, deploys the embedded Worker, and checks the endpoint. For an existing server, supply its existing key to avoid replacing it unintentionally. Without a supplied key, setup retains the secret and reports authenticated verification as skipped; health alone does not verify migration and MCP access. A supplied key enables session verification, remote MCP initialization, and discovery of `search_memory`, `getDocument`, and `add_memory`.

Apply the server update and migrations before connecting or updating clients. Setup stores resumable configuration outside the repository and plugin in `<CODEX_HOME>/remnifold/server/config.json` (default `~/.codex/remnifold/server/config.json`); `--state <absolute-json-path>` selects another external location. Keep this state for updates, never commit generated IDs or credentials, and rerun the same command after correcting a failed step. Use `--base-url` for an existing custom endpoint and `--help` for all options. Cloudflare usage is billed to the account hosting the server.

For 1.0.22, update the server with that release's `server-setup --apply` before updating clients. Migration `0013_memory_created_index.sql` adds an index for day-ranged organization; it changes no stored records or IDs.

For 1.0.21, update the server with that release's `server-setup --apply` before updating clients. Migrations `0010_capture_variants.sql`, `0011_consolidation_recall_opt_out.sql`, and `0012_organization_retry_fairness.sql` add verified capture variants, invalidate historical checkpoints containing recall-excluded sources, and shorten future legacy deferrals to one hour. Original records and IDs are retained; affected checkpoints can regenerate from eligible sources.

## Memory behavior

- Completed user requests and final answers are saved in the shared `memories` collection with source-folder provenance. Attached skill definitions and browser/Page-state envelopes are omitted from saved request text; final answers are preserved. A deterministic capture identity makes retries reuse the same stored record instead of creating duplicates.
- Use `listTopics` to find content categories, then pass a `topic` to `listDocuments` or `listMemories`. Lists include records across both legacy folder spaces and the shared collection. Use `topic: "__unclassified__"` to see records without current topic labels, including records whose classification failed, is disabled, or was deliberately skipped.
- Manual `add_memory` saves to the shared collection. To forget a record after reading it, call `add_memory` with `action: "forget"`, its `documentId`, and source `containerTag`; exact stored content remains supported for compatibility. With the Codex plugin, an absolute `sourceFolder` or a single workspace root supplied by the MCP client adds provenance; the remote MCP tool instead accepts an explicit `projectId`. An explicit `containerTag` overrides storage. The plugin never uses its installation folder as the user's project. `listSpaces` exposes physical spaces for compatibility and diagnostics.
- Prompt recall searches across spaces by topic. Short follow-ups can recover a topic from earlier user messages in the same session, even before large tool outputs. History recovery is bounded; without a concrete topic, no topic search runs. Automatic recall selects the latest available revision of each captured turn and part, while manual search retains historical revisions. Global search includes spaces beyond the old discovery limit; older servers use a compatibility path capped at 100 spaces and report when discovery is incomplete. Partial searches report unavailable spaces and document indexes while retaining successful results. The initial session event only resets recall state. Each nonempty ordinary prompt also shows up to two recent completed tasks across projects, independently of topic matching, with the source project, date, and document ID. The current task and duplicate task records are excluded. These summaries are labeled separately so they can suggest prior work without implying relevance. Set `maxRecentTasks` in `remnifold.json` to an integer from 0 to 3; 0 disables this recent-work context. Older servers without this listing mode continue to provide topic recall.
- Manual searches also match the stored project name. Project-name-only searches prefer matching source projects and newer source records; combined project/topic searches still prioritize topic coverage.
- Automatic context contains a title, description, section names, timestamps, and document IDs. A short matching original passage can expose details absent from an index; it remains historical source text to verify before reuse. Codex opens full records with `getDocument` when needed.
- The fixed wording of the injected `<remnifold-index>` block, search results and recall notices follows the language of the environment that writes them: `REMNIFOLD_LANGUAGE`, then `LC_ALL`, `LC_MESSAGES` and `LANG`, then the OS display language (Windows user UI language, macOS `AppleLanguages`). Japanese environments get Japanese wording and all others get English. Stored titles, descriptions and excerpts are shown as saved, not translated.
- Topic recall defaults to at most three records. Set `maxMemories` in `remnifold.json` to an integer from 1 to 5 to change the limit for automatic recall and manual search; setup preserves an existing value when updating the connection. Automatic context is also limited to 3,000 UTF-16 code units, so fewer records may fit.
- Automatic context tracks recent tasks separately from topic indexes and omits unchanged entries on later prompts when session and transcript information are available. Updated records can appear again, and compaction or clearing the context resets duplicate suppression. Local recall state stays under `remnifold/recall-state/` in the selected Codex home without copying conversation text; unavailable state leaves normal recall enabled. Query matching and state details are described in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall).
- The server checks projects daily at 03:00 JST. It asks Workers AI to create a consolidated checkpoint when a project has at least 20 unconsolidated records, or when it has unconsolidated records and three days have elapsed since the last successful checkpoint (before the first checkpoint, since the oldest pending record). On a project's first due run, the server automatically drains the entire existing backlog in successive bounded AI batches, so old records do not require a manual backfill command. Original records are retained and linked from the checkpoint. Older checkpoints are superseded, and changing or forgetting a source invalidates the checkpoint that depended on it.
- Use the `consolidate_memory` MCP tool to advance a project checkpoint by one batch immediately without waiting for the daily check. Remote MCP requires `projectId`, the stored provenance ID. The Codex plugin can infer the current project from exactly one workspace root, or accept `sourceFolder` or `projectId` explicitly. Each call handles at most 40 new records; repeat it if a larger backlog remains. Manual execution still requires at least one unconsolidated source record.
- Automatic recall prefers a current checkpoint at equal topic relevance and avoids repeating originals covered by a selected checkpoint. Specific matching details absent from the checkpoint still expose their originals. Manual search continues to expose the original records.
- Checkpoint content and metadata cannot be edited directly: updates return HTTP 409. Edit the original records and regenerate the checkpoint instead. Re-enrichment and deletion remain available.
- Capture redacts the configured memory API key and common secret patterns, and retries unacknowledged records. Local capture state stays under `remnifold/capture-state/` in the selected Codex home.

Capture failures include fixed `stage`, `cause`, and `action` codes without printing exception text or credentials. An `action=repair-required` result, such as `stage=receipt; cause=invalid`, requires repairing the corresponding local capture state; retries alone will keep failing. `action=retry-or-repair` retains the normal retry behavior while allowing a persistent connection or response problem to be investigated. To check API reachability separately, run this from the extracted plugin folder:

```sh
./bin/remnifold.exe status
```

Topics are derived from record content during the existing Workers AI enrichment step. Classification is asynchronous. Brief acknowledgements without a substantial result, and similar low-information records, remain available to manual search but are excluded from automatic recall and enrichment; explicit topic labels are still retained. Folder names are retained as source metadata. Existing records keep their original document IDs and storage locations and appear in the same topic browser.

Update the server and apply its migrations before installing a client that uses topic browsing or consolidation. To classify old records, preview a bounded batch with the platform executable using the existing memory connection:

```sh
./bin/remnifold.exe topics-setup --limit 10
```

On macOS and Linux use `./bin/remnifold topics-setup --limit 10`. The default batch is 10; `--limit` accepts 1–50. Add `--apply` to queue that finite batch when remote enrichment is intended. This uses existing Workers AI billing, refreshes derived AI/vector/fact enrichment, and invalidates affected project checkpoints while preserving opt-outs and revision guards. The result reports accepted jobs, not completed classification. Wait for processing to finish before selecting the next batch; inspect `getDocument` and `listTopics` to verify the result.

The API key grants access to the records in one server installation. Use separate installations for separate trust boundaries. Redaction reduces accidental secret capture; review what you choose to store in your own backend.

## Codex memory organization

After installing a compatible plugin and applying the [current server migrations](#create-or-update-a-server), including the organization schema in `0009_memory_organization.sql` and the ranged-organization index in `0013_memory_created_index.sql`, organize memory on demand with the [organize-memory skill](skills/organize-memory/SKILL.md), for example `$organize-memory 7` or "organize the last 7 days of memory". The day count (1–3650) selects memories created in the last N days by server time; the run repeats batches until that range has no due work or an optional batch cap (default 3, at most 20) is reached. Without a day count, one batch is taken from the global queue. Organization is no longer registered as a Codex scheduled task; [setup-remnifold](skills/setup-remnifold/SKILL.md) only pauses tasks left by earlier versions.

Each run uses the currently installed executable. One batch processes at most twenty target records and 48,000 UTF-16 source units, including partial or deferred work. These are not completed-record counts or total reading/token guarantees: candidate summaries and targeted evidence reads are additional. Lookup is limited to two calls per batch. Unresolved work is deferred rather than expanding into bulk exploration; a ranged run stopped by its cap can simply be run again.

For direct CLI use, run the platform executable from the extracted plugin folder (`./bin/remnifold` on macOS/Linux):

```text
./bin/remnifold.exe memory-organize prepare [--days N]
./bin/remnifold.exe memory-organize lookup --line
./bin/remnifold.exe memory-organize commit --line
./bin/remnifold.exe memory-organize sources <summary-document-id> [cursor]
```

These commands use the existing owner API-key connection; remote MCP OAuth credentials cannot authorize organization. They neither schedule Codex nor deploy/migrate a server. `prepare` creates a thirty-minute lease; `--days N` limits it to memories created in the last N days without moving the global discovery queue; each ranged response returns `range.cursor`/`range.exhausted`, and the next call passes a non-null cursor as `--range-cursor <cursor>`, and an older server rejects it rather than falling back. It returns source IDs/revisions/chunks, continuation notes and candidate summaries; it is not a read-only connectivity check. `lookup` reads `{"query":"a concise topic"}` from stdin. `commit` reads a JSON object containing `batchId`, `summaries`, and `coverage`. Follow the execution skill's exact field limits, partition rules and multipart `groupProgress` procedure. Declare `complete` only when every chunk and important topic is represented, preserving conclusions, reasons, evidence and uncertainty; use partial continuation or deferral when that cannot be done. Codex makes this judgment. Check the commit receipt and distinguish published summaries from unfinished progress.

Before `prepare`, establish a safe stdin path and a way to retain and page its complete response; on Windows, follow the execution skill's local synthetic PTY probe before acquiring a lease. Read every source chunk, continuation memo and summary candidate from the same retained response. A truncated display is recovered from that invocation, not by preparing another batch. Before commit, validate the exact JSON in memory: every coverage entry needs its prepared ID/revision and an explicit `summaries` array, including `summaries:[]` for partial or deferred entries. Follow the [execution skill](skills/organize-memory/SKILL.md#one-batch) for paging, preflight and recovery after an interrupted run.

JSON stdin has a thirty-second deadline and a 128,000-byte UTF-8 limit including escaping. `--line` requires one compact JSON line and its terminating newline; without it, close stdin after the object. Use a non-echoing input channel and keep generated text out of shell arguments and command logs. Unsupported servers stop explicitly without a save/forget fallback.

For Windows PTY input, launch the native executable directly in a dedicated PTY with `memory-organize lookup --line --private-stdin` or `memory-organize commit --line --private-stdin`. Wait for `REMNIFOLD_STDIN_READY` on stderr before sending exactly one compact JSON line and its final newline, with no bytes afterward. Keep the payload plus newline at or below 127,000 UTF-8 bytes to leave room for terminal transformations. Private mode disables echo and line editing, restoring the original console mode after the terminator. If readiness is absent or any step fails, stop sending input and close that PTY; do not reuse it or restore echo while input might remain. Use neither a shared console nor a Git Bash wrapper. Redirected stdin and non-Windows hosts reject `--private-stdin`; on macOS/Linux, use a non-echoing pipe with line or EOF input. See the [execution skill](skills/organize-memory/SKILL.md#safe-stdin-and-failures) for the complete input procedure.

No original is deleted. Fully represented originals leave normal search and topic recall only while their summaries remain valid; original IDs/bodies stay directly retrievable, and recent-task awareness retains its existing eligibility. Conflicting captures remain visible and deferred unless the server verifies distinct single-part v2 capture keys and exact body hashes as independent evidence. That verification does not establish which version is newest; unresolved contradictions must survive in the summary. Ordinary deferrals become eligible after one hour, while unresolved capture conflicts continue to block automatic retries. Bounded queues share progress between older due work, continuations and untouched records; one run does not establish archive-wide completion. Changes or forgetting invalidate dependent summaries and expose surviving originals again. Older project checkpoints can still expose information until they are processed as whole records. With AI enabled, originals already supporting semantic search wait for verified summary embeddings/vectors before disappearing. With AI disabled, lexical coverage is sufficient; enabling AI can reveal originals again until semantic replacements are ready. Vector recovery is asynchronous, and the fifteen-minute cron is not a completion deadline. Summary embeddings use existing Workers AI billing; Codex generates the prose.

`sources` is read-only and pages up to fifty recorded source IDs, spaces and dates. Retrieve bodies with getDocument; follow a checkpoint's own source preview/cursor to its recorded members. Separate provenance paths may repeat an ID. Current native and remote MCP getDocument responses also include a source preview and optional `sourceCursor` paging; older native plugins can use the CLI when they omit these fields. Cursors stop with conflict if their parent changes. The [organization design](DESIGN.md#codex-organization) describes coverage, invalidation and vector recovery in detail.

## Upgrading from 1.x names

Replace an installed `kagayoi-memory` or 1.x `cloudflare-supermemory` plugin with `remnifold`, enabling only the new plugin. The client reads `remnifold.json` first, then `kagayoi-memory.json` and `supermemory.json` in the selected Codex home. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` are the normal environment variables; the former `KAGAYOI_MEMORY_*`, `SUPERMEMORY_API_URL`, `SUPERMEMORY_CODEX_API_KEY`, and `CLOUDFLARE_MEMORY_API_KEY` readers remain for compatibility. Run `bin/remnifold setup` (`bin/remnifold.exe setup` on Windows) to write the normal `remnifold.json` while keeping the previous files. Existing capture receipts and recall state remain readable from the former product folders. Server setup reuses resources named `remnifold`, then `kagayoi-memory`, then `cloudflare-supermemory`; explicit resource names always win. Renaming the plugin does not deploy a Worker or move a remote database. Existing document IDs, capture IDs, project provenance and persisted `sm_*` metadata remain unchanged. Storage compatibility details are in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance).

Remnifold implements its own self-hosted API. It is independent of the hosted Supermemory service and does not read or modify Codex's built-in local memory files.

For architecture and repository layout, see the repository's [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md). Contributor constraints, validation, and packaging commands are in [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md).

Licensed under the [MIT License](LICENSE).
