# Remnifold

A self-hosted Cloudflare memory server with remote MCP access and a Codex plugin for recalling useful implementation history across project folders.

The plugin saves completed work in a shared collection and keeps its project folder as provenance. Browse records by content topics, or ask about a topic such as “Chrome extension”: recall searches across memory spaces in one server request, injects concise indexes, and lets Codex open an applicable source record through MCP before reusing the implementation.

## Requirements

Remote MCP clients connect to the deployed server without installing Node.js or Codex locally. The optional Codex integration requires Codex with plugin and hook support and the package matching your operating system and CPU. Its MCP server, hooks, connection setup, history import, and local plugin updater run as a native executable without Node.js. Local plugin updates also require authenticated GitHub CLI and native Codex CLI.

Server setup and maintenance use the same native executable and your own Cloudflare account with Workers, D1, Vectorize, and Workers AI enabled. No Node.js, package manager, Wrangler, or Rust installation is required to use the platform package.

## Connect a remote MCP client

After the server has been updated and verified below, use `https://your-worker.example.workers.dev/mcp` as the Streamable HTTP MCP endpoint. A compatible client can authenticate with OAuth 2.1 discovery, public dynamic client registration, and PKCE S256. On the server's authorization page, enter the existing memory API key and approve the named client. If the completion page does not return to the client automatically, follow its link to continue. OAuth tokens are valid for MCP only; existing REST clients continue to use the API key. Clients supporting a configured Bearer credential can also use the memory API key directly. Keep keys and tokens private.

[Grok Bot's Connect apps](https://docs.x.ai/grok-bot/overview) and [Grok connectors](https://docs.x.ai/grok/connectors) provide app and custom MCP connection paths. Choose OAuth when the client supports it; availability of manual Bearer-key entry in Grok Bot has not been verified.

For ChatGPT, follow the [private plugin setup](https://developers.openai.com/plugins/build/plugins): choose **+ Add custom MCP server**, enter the `/mcp` URL and authentication, then choose **Create as a plugin**. This private connection does not require submission to the public directory. [ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) uses supported, installed and enabled plugins; confirm in your account that this plugin appears and works in dots. Direct entry of an arbitrary MCP URL in dots has not been verified.

The server asks the AI to search for relevant context, open full records when needed, and save durable outcomes. Tool use is chosen by the client and model, so automatic searches or saves are not guaranteed. Ask the AI to search or save explicitly when needed. This remote integration does not replace the existing Codex stdio server or hooks; avoid registering a duplicate remote server under the same Codex integration.

## Connect an existing server

Starting with 1.0.16, published native packages are served from [Remnifold updates](https://remnifold-updates.kagayoi.com). Earlier releases retain their existing distribution path. The version index is `releases/<version>/index.json`; its manifest selects a hash-named archive for each target below. Authenticate publication through GitHub before running a downloaded executable; an R2 URL or a commit-status link alone does not establish its origin.

| Platform | Rust target |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Mac Intel | `x86_64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

For an existing `remnifold@personal` installation, use the native executable's `plugin-update` command with authenticated native GitHub CLI and Codex CLI. The updater verifies the release commit, successful CI publisher logs, R2 manifest, archive hashes, and package source before using Codex's official plugin CLI. Publication does not update your installation automatically.

Run these commands from the extracted plugin folder, using the same published version and work root for all three steps:

```powershell
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Codex/remnifold/plugin-update --check-published
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Codex/remnifold/plugin-update --apply
./bin/remnifold.exe plugin-update --version <published-version> --work-root D:/Codex/remnifold/plugin-update --verify-only
```

On macOS and Linux, use `./bin/remnifold` and an absolute external work directory. On every platform, keep the work root outside the repository, plugin source, and Codex home, with no linked ancestors; Windows requires a child of `D:/Codex/remnifold`. Keep the `verified-<commit>-<target>.json` receipt in that work root for later verification. Connection and capture state are preserved; a new task is still required to load the updated plugin.

An older 1.0.15 executable cannot discover the new R2 distribution itself. Obtain and review the appropriate bootstrap script from the trusted [GitHub source](https://github.com/1llum1n4t1s/remnifold/tree/main/scripts), saving it outside the work directory. The bootstrap requires native `gh`, the OS shell and tar, plus curl and `sha256sum` or `shasum` on macOS/Linux. It verifies GitHub's publisher-log witness and the archive's size and SHA-256 before extracting a new executable. It calls that executable's `--check-published` by default; installation requires explicit `-Apply` or `--apply`.

```powershell
.\bootstrap-plugin.ps1 -Version <published-version> -WorkRoot D:/Codex/remnifold/bootstrap
```

```sh
bash ./bootstrap-plugin.sh --version <published-version> --work-root /absolute/external/remnifold-bootstrap
```

Repeat with `-Apply` (Windows) or `--apply` (macOS/Linux) to install and verify. The external work-root requirements above also apply to bootstrap. The scripts retain their invocation directories and never send GitHub credentials to R2. Apply creates a verification receipt; a default publication check creates none. A failed later installation step can leave the receipt for recovery. Do not pipe a network download directly into a shell or bypass your system's script policy. When logs have expired, a fresh download fails closed; an existing verification receipt still supports `--verify-only` without retained R2 objects or CI logs. GitHub access to the release ref and source metadata remains required.

For a first installation, add `-DownloadOnly` (Windows) or `--download-only` (macOS/Linux) to the bootstrap command. This returns the verified archive path without running its executable, requiring an existing plugin, or invoking Codex. On Windows the formal download is copied and rehashed into a new private folder under the OS user profile's `Remnifold/Downloads`; the `D:/Codex/remnifold` work root holds processing state only. Unsafe delivery ancestors are rejected, and existing ACLs are not changed. Extract that archive under `$env:USERPROFILE/plugins` on Windows or `~/plugins` on macOS/Linux, so its source is `$env:USERPROFILE/plugins/remnifold` or `~/plugins/remnifold`, matching the updater default. Open that `remnifold` folder in Codex and ask plugin-creator to register this source in the personal marketplace and install through `codex plugin add remnifold@personal`. For a custom marketplace source, use download-only and the verified executable's `plugin-update --config <absolute-local-config>` procedure instead of the bootstrap's default update mode. Rust is not required. Alternatively, build and verify a platform package from trusted GitHub source using the [Rust development procedure](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md#validation); local checkout checks do not establish release-commit identity. The old JavaScript and native releases without an R2 distribution configuration retain their historical GitHub-artifact update path.

```powershell
./bin/remnifold.exe setup --base-url https://your-worker.example.workers.dev
```

On macOS and Linux, use `./bin/remnifold` in place of `./bin/remnifold.exe` in these commands. Run them from the extracted plugin folder. Enter the API key at the hidden prompt if it is not already supplied. Setup verifies an authenticated session before writing `remnifold.json` in the selected Codex home: `--codex-home`, then `CODEX_HOME`, then the default `~/.codex`. Run Codex with the same home so it reads that connection. To update an existing connection, use `--force`; unrelated settings are preserved. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` can supply process-level overrides. For noninteractive setup, `--api-key-stdin` reads the key from standard input.

After installing or updating the plugin, review any hook trust requested by Codex for its four hooks, then start a new task to load the updated plugin. Run the plugin's `whoAmI` MCP tool to verify connectivity and space discovery. Installing a plugin alone does not trust its hooks.

To import existing Codex sessions, preview a bounded batch with `./bin/remnifold.exe import --limit 10` (macOS/Linux: `./bin/remnifold import --limit 10`). Add `--apply` when you want to upload the sanitized records. The preview does not write remote memories; repeat imports reuse deterministic capture identities.

## Create or update a server

The package includes the Worker bundle and migrations. Provide a Cloudflare API token through the process environment `CLOUDFLARE_API_TOKEN` or `--token-stdin`, then preview the existing resources:

```powershell
./bin/remnifold.exe server-setup --account-id <Cloudflare-account-id>
```

On macOS and Linux use `./bin/remnifold`. The default is authenticated, read-only discovery. The token needs account read, Workers scripts, D1, and Vectorize permissions; domain or route changes also require the corresponding Zone, Routes, or Custom Domain permissions. Setup does not import Wrangler OAuth credentials. Keep the token and memory API key private.

For an existing installation, pass `--config <absolute-existing-jsonc-path>` to read its configuration without overwriting it. Setup reuses resource names in the order `remnifold`, `kagayoi-memory`, then `cloudflare-supermemory`; explicit `--worker-name`, `--database-name`, and `--vector-index-name` take priority. Check the plan selects the existing Worker, D1 database, and Vectorize index. Configured IDs and vector dimensions/metric must match. Existing bindings, cron schedules, and routes are preserved; conflicts stop setup. Do not create a replacement server or copy memories for this upgrade.

When you intend to provision or deploy, provide the chosen memory key through `REMNIFOLD_API_KEY` and apply the plan:

```powershell
./bin/remnifold.exe server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

Use the same configuration and names as the preview. `--apply` creates missing resources, installs a supplied key as the Worker secret, applies additive D1 migrations, deploys the embedded Worker, and checks the endpoint. For an existing server, supply its existing key to avoid replacing it unintentionally. Without a supplied key, setup retains the secret and reports authenticated verification as skipped; health alone does not verify migration and MCP access. A supplied key enables session verification, remote MCP initialization, and discovery of `search_memory`, `getDocument`, and `add_memory`.

Apply the server update and migrations before connecting or updating clients. Setup stores resumable configuration outside the repository and plugin in `<CODEX_HOME>/remnifold/server/config.json` (default `~/.codex/remnifold/server/config.json`); `--state <absolute-json-path>` selects another external location. Keep this state for updates, never commit generated IDs or credentials, and rerun the same command after correcting a failed step. Use `--base-url` for an existing custom endpoint and `--help` for all options. Cloudflare usage is billed to the account hosting the server.

## Memory behavior

- Completed user requests and final answers are saved in the shared `memories` collection with source-folder provenance. Attached skill definitions and browser/Page-state envelopes are omitted from saved request text; final answers are preserved. A deterministic capture identity makes retries reuse the same stored record instead of creating duplicates.
- Use `listTopics` to find content categories, then pass a `topic` to `listDocuments` or `listMemories`. Lists include records across both legacy folder spaces and the shared collection. Use `topic: "__unclassified__"` to see records without current topic labels, including records whose classification failed, is disabled, or was deliberately skipped.
- Manual `add_memory` saves to the shared collection. To forget a record after reading it, call `add_memory` with `action: "forget"`, its `documentId`, and source `containerTag`; exact stored content remains supported for compatibility. With the Codex plugin, an absolute `sourceFolder` or a single workspace root supplied by the MCP client adds provenance; the remote MCP tool instead accepts an explicit `projectId`. An explicit `containerTag` overrides storage. The plugin never uses its installation folder as the user's project. `listSpaces` exposes physical spaces for compatibility and diagnostics.
- Prompt recall searches across spaces by topic. Short follow-ups can recover a topic from earlier user messages in the same session, even before large tool outputs. History recovery is bounded; without a concrete topic, no topic search runs. Automatic recall selects the latest available revision of each captured turn and part, while manual search retains historical revisions. Global search includes spaces beyond the old discovery limit; older servers use a compatibility path capped at 100 spaces and report when discovery is incomplete. Partial searches report unavailable spaces and document indexes while retaining successful results. The initial session event only resets recall state. Each nonempty ordinary prompt also shows up to two recent completed tasks across projects, independently of topic matching, with the source project, date, and document ID. The current task and duplicate task records are excluded. These summaries are labeled separately so they can suggest prior work without implying relevance. Set `maxRecentTasks` in `remnifold.json` to an integer from 0 to 3; 0 disables this recent-work context. Older servers without this listing mode continue to provide topic recall.
- Manual searches also match the stored project name. Project-name-only searches prefer matching source projects and newer source records; combined project/topic searches still prioritize topic coverage.
- Automatic context contains a title, description, section names, timestamps, and document IDs. A short matching original passage can expose details absent from an index; it remains historical source text to verify before reuse. Codex opens full records with `getDocument` when needed.
- Topic recall defaults to at most three records. Set `maxMemories` in `remnifold.json` to an integer from 1 to 5 to change the limit for automatic recall and manual search; setup preserves an existing value when updating the connection. Automatic context is also limited to 3,000 UTF-16 code units, so fewer records may fit.
- Automatic context tracks recent tasks separately from topic indexes and omits unchanged entries on later prompts when session and transcript information are available. Updated records can appear again, and compaction or clearing the context resets duplicate suppression. Local recall state stays under `remnifold/recall-state/` in the selected Codex home without copying conversation text; unavailable state leaves normal recall enabled. Query matching and state details are described in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall).
- The server checks projects daily at 03:00 JST. It asks Workers AI to create a consolidated checkpoint when a project has at least 20 unconsolidated records, or when it has unconsolidated records and three days have elapsed since the last successful checkpoint (before the first checkpoint, since the oldest pending record). On a project's first due run, the server automatically drains the entire existing backlog in successive bounded AI batches, so old records do not require a manual backfill command. Original records are retained and linked from the checkpoint. Older checkpoints are superseded, and changing or forgetting a source invalidates the checkpoint that depended on it.
- Use the `consolidate_memory` MCP tool to advance a project checkpoint by one batch immediately without waiting for the daily check. Remote MCP requires `projectId`, the stored provenance ID. The Codex plugin can infer the current project from exactly one workspace root, or accept `sourceFolder` or `projectId` explicitly. Each call handles at most 40 new records; repeat it if a larger backlog remains. Manual execution still requires at least one unconsolidated source record.
- Automatic recall prefers a current checkpoint at equal topic relevance and avoids repeating originals covered by a selected checkpoint. Specific matching details absent from the checkpoint still expose their originals. Manual search continues to expose the original records.
- Checkpoint content and metadata cannot be edited directly: updates return HTTP 409. Edit the original records and regenerate the checkpoint instead. Re-enrichment and deletion remain available.
- Capture redacts the configured memory API key and common secret patterns, and retries unacknowledged records. Local capture state stays under `remnifold/capture-state/` in the selected Codex home.

Capture failures include fixed `stage`, `cause`, and `action` codes without printing exception text or credentials. An `action=repair-required` result, such as `stage=receipt; cause=invalid`, requires repairing the corresponding local capture state; retries alone will keep failing. `action=retry-or-repair` retains the normal retry behavior while allowing a persistent connection or response problem to be investigated. To check API reachability separately, run this from the extracted plugin folder:

```powershell
./bin/remnifold.exe status
```

Topics are derived from record content during the existing Workers AI enrichment step. Classification is asynchronous. Brief acknowledgements without a substantial result, and similar low-information records, remain available to manual search but are excluded from automatic recall and enrichment; explicit topic labels are still retained. Folder names are retained as source metadata. Existing records keep their original document IDs and storage locations and appear in the same topic browser.

Update the server and apply its migrations before installing a client that uses topic browsing or consolidation. To classify old records, preview a bounded batch with the platform executable using the existing memory connection:

```powershell
./bin/remnifold.exe topics-setup --limit 10
```

On macOS and Linux use `./bin/remnifold topics-setup --limit 10`. The default batch is 10; `--limit` accepts 1–50. Add `--apply` to queue that finite batch when remote enrichment is intended. This uses existing Workers AI billing, refreshes derived AI/vector/fact enrichment, and invalidates affected project checkpoints while preserving opt-outs and revision guards. The result reports accepted jobs, not completed classification. Wait for processing to finish before selecting the next batch; inspect `getDocument` and `listTopics` to verify the result.

The API key grants access to the records in one server installation. Use separate installations for separate trust boundaries. Redaction reduces accidental secret capture; review what you choose to store in your own backend.

## Upgrading from 1.x names

Replace an installed `kagayoi-memory` or 1.x `cloudflare-supermemory` plugin with `remnifold`, enabling only the new plugin. The client reads `remnifold.json` first, then `kagayoi-memory.json` and `supermemory.json` in the selected Codex home. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` are the normal environment variables; the former `KAGAYOI_MEMORY_*`, `SUPERMEMORY_API_URL`, `SUPERMEMORY_CODEX_API_KEY`, and `CLOUDFLARE_MEMORY_API_KEY` readers remain for compatibility. Run `bin/remnifold setup` (`bin/remnifold.exe setup` on Windows) to write the normal `remnifold.json` while keeping the previous files. Existing capture receipts and recall state remain readable from the former product folders. Server setup reuses resources named `remnifold`, then `kagayoi-memory`, then `cloudflare-supermemory`; explicit resource names always win. Renaming the plugin does not deploy a Worker or move a remote database. Existing document IDs, capture IDs, project provenance and persisted `sm_*` metadata remain unchanged. Storage compatibility details are in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance).

Remnifold implements its own self-hosted API. It is independent of the hosted Supermemory service and does not read or modify Codex's built-in local memory files.

For architecture and repository layout, see the repository's [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md). Contributor constraints, validation, and packaging commands are in [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md).

Licensed under the [MIT License](LICENSE).
