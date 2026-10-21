# Remnifold

A Codex plugin and self-hosted Cloudflare backend for recalling useful implementation history across project folders.

The plugin saves completed work in a shared collection and keeps its project folder as provenance. Browse records by content topics, or ask about a topic such as “Chrome extension”: recall searches across memory spaces in one server request, injects concise indexes, and lets Codex open an applicable source record through MCP before reusing the implementation.

## Requirements

Use Node.js 24.18.0 or later in the 24 series, pnpm 11.4.0, a Codex installation with plugin and hook support, and PowerShell 7 for the Windows setup helper. The MCP server and capture hooks have no npm runtime dependencies. The backend needs your own Cloudflare account with Workers, D1, Vectorize, and Workers AI enabled.

## Connect an existing server

Clone this repository or download and extract the CI package. In Codex, open the repository or extracted plugin folder and ask: “Use plugin-creator to install this Remnifold plugin in my personal marketplace.” The installer should register the source, validate it, and run `codex plugin add remnifold@personal`. For updates, replace the local source with the newer checkout or extracted CI package, then ask Codex to refresh that source and reinstall it. Building a distributable from a source checkout is covered in [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md#validation); the CI package is already built.

```powershell
pwsh -NoProfile -File scripts/setup-client.ps1 -BaseUrl https://your-worker.example.workers.dev
```

Enter the API key at the secure prompt if it is not already supplied. The script verifies an authenticated session before writing `remnifold.json` in the selected Codex home: `-CodexHome`, then `CODEX_HOME`, then the default `~/.codex`. Run Codex with the same home so it reads that connection. To update an existing connection, use `-Force`; unrelated settings are preserved. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` can supply process-level overrides.

After installing the plugin, review and trust its four hooks in Codex, then start a new task. Run the plugin's `whoAmI` MCP tool to verify connectivity and space discovery. Installing a plugin alone does not trust its hooks.

## Create or update a server

```powershell
pnpm -C server install --frozen-lockfile
pnpm -C server exec wrangler login
node scripts/setup-server.mjs
```

The default command performs read-only discovery and prints the proposed resources. To apply it, supply your chosen API key through the process environment `REMNIFOLD_API_KEY`, then run:

```powershell
node scripts/setup-server.mjs --apply --location apac
```

The setup reuses resources by name, checks the Vectorize dimensions and metric, creates missing resources, writes the ignored `server/wrangler.jsonc`, installs the Worker secret, applies D1 migrations, deploys, and verifies the endpoint. Re-run the same command to update the installation. An existing secret is retained unless a replacement key is supplied. Without its local value, verification can check health but cannot authenticate the session. Use `--base-url` for a custom domain when Wrangler does not report a workers.dev URL. `--help` lists resource-name, profile, location, and endpoint options.

Keep the local Wrangler configuration for future updates. Never commit generated resource IDs, credentials, or `.wrangler/` state. Cloudflare usage is billed to the account that hosts the server.

## Memory behavior

- Completed user requests and final answers are saved in the shared `memories` collection with source-folder provenance. Attached skill definitions and browser/Page-state envelopes are omitted from saved request text; final answers are preserved. A deterministic capture identity makes retries reuse the same stored record instead of creating duplicates.
- Use `listTopics` to find content categories, then pass a `topic` to `listDocuments` or `listMemories`. Lists include records across both legacy folder spaces and the shared collection. Use `topic: "__unclassified__"` to see records without current topic labels, including records whose classification failed, is disabled, or was deliberately skipped.
- Manual `add_memory` saves to the shared collection. To forget a record after reading it, call `add_memory` with `action: "forget"`, its `documentId`, and source `containerTag`; exact stored content remains supported for compatibility. An absolute `sourceFolder` or a single workspace root supplied by the MCP client adds provenance; an explicit `containerTag` overrides storage. The plugin never uses its installation folder as the user's project. `listSpaces` exposes physical spaces for compatibility and diagnostics.
- Prompt recall searches across spaces by topic. Short follow-ups can recover a topic from earlier user messages in the same session, even before large tool outputs. History recovery is bounded; without a concrete topic, no search runs. Automatic recall selects the latest available revision of each captured turn and part, while manual search retains historical revisions. Global search includes spaces beyond the old discovery limit; older servers use a compatibility path capped at 100 spaces and report when discovery is incomplete. Partial searches report unavailable spaces and document indexes while retaining successful results. The initial session event does not inject unrelated recent records.
- Manual searches also match the stored project name. Project-name-only searches prefer matching source projects and newer source records; combined project/topic searches still prioritize topic coverage.
- Automatic context contains a title, description, section names, timestamps, and document IDs. A short matching original passage can expose details absent from an index; it remains historical source text to verify before reuse. Codex opens full records with `getDocument` when needed.
- Recall defaults to at most three records. Set `maxMemories` in `remnifold.json` to an integer from 1 to 5 to change the limit for automatic recall and manual search; setup preserves an existing value when updating the connection. Automatic context is also limited to 3,000 UTF-16 code units, so fewer records may fit.
- Automatic context omits unchanged indexes on later prompts when session and transcript information are available. Updated records can appear again, and compaction or clearing the context resets duplicate suppression. Local recall state stays under `remnifold/recall-state/` in the selected Codex home without copying conversation text; unavailable state leaves normal recall enabled. Query matching and state details are described in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall).
- The server checks projects daily at 03:00 JST. It asks Workers AI to create a consolidated checkpoint when a project has at least 20 unconsolidated records, or when it has unconsolidated records and three days have elapsed since the last successful checkpoint (before the first checkpoint, since the oldest pending record). On a project's first due run, the server automatically drains the entire existing backlog in successive bounded AI batches, so old records do not require a manual backfill command. Original records are retained and linked from the checkpoint. Older checkpoints are superseded, and changing or forgetting a source invalidates the checkpoint that depended on it.
- Use the `consolidate_memory` MCP tool to advance the current workspace checkpoint by one batch immediately without waiting for the daily check. Pass `projectId` to target a known provenance ID, or `sourceFolder` when the MCP client does not provide exactly one workspace root. Each call handles at most 40 new records; repeat it if a larger backlog remains. Manual execution still requires at least one unconsolidated source record.
- Automatic recall prefers a current checkpoint at equal topic relevance and avoids repeating originals covered by a selected checkpoint. Specific matching details absent from the checkpoint still expose their originals. Manual search continues to expose the original records.
- Checkpoint content and metadata cannot be edited directly: updates return HTTP 409. Edit the original records and regenerate the checkpoint instead. Re-enrichment and deletion remain available.
- Capture redacts the configured memory API key and common secret patterns, and retries unacknowledged records. Local capture state stays under `remnifold/capture-state/` in the selected Codex home.

Capture failures include fixed `stage`, `cause`, and `action` codes without printing exception text or credentials. An `action=repair-required` result, such as `stage=receipt; cause=invalid`, requires repairing the corresponding local capture state; retries alone will keep failing. `action=retry-or-repair` retains the normal retry behavior while allowing a persistent connection or response problem to be investigated. To check API reachability separately, run this from the repository or extracted plugin folder:

```powershell
node --input-type=module -e "import { showStatus } from './client/status.mjs'; await showStatus();"
```

Topics are derived from record content during the existing Workers AI enrichment step. Classification is asynchronous. Brief acknowledgements without a substantial result, and similar low-information records, remain available to manual search but are excluded from automatic recall and enrichment; explicit topic labels are still retained. Folder names are retained as source metadata. Existing records keep their original document IDs and storage locations and appear in the same topic browser.

Update the server and apply its migrations before installing a client that uses topic browsing or consolidation. To classify old records, preview a bounded batch using the existing memory connection:

```powershell
node scripts/setup-topics.mjs --limit 10
```

Add `--apply` to queue the selected records for enrichment. This uses the backend's existing Workers AI billing and refreshes the records' derived enrichment. The result reports accepted jobs, not completed classification. Wait for processing to finish before selecting the next batch; inspect `getDocument` and `listTopics` to verify the result.

The API key grants access to the records in one server installation. Use separate installations for separate trust boundaries. Redaction reduces accidental secret capture; review what you choose to store in your own backend.

## Upgrading from 1.x names

Replace an installed `kagayoi-memory` or 1.x `cloudflare-supermemory` plugin with `remnifold`, enabling only the new plugin. The client reads `remnifold.json` first, then `kagayoi-memory.json` and `supermemory.json` in the selected Codex home. `REMNIFOLD_API_URL` and `REMNIFOLD_API_KEY` are the normal environment variables; the former `KAGAYOI_MEMORY_*`, `SUPERMEMORY_API_URL`, `SUPERMEMORY_CODEX_API_KEY`, and `CLOUDFLARE_MEMORY_API_KEY` readers remain for compatibility. Run `scripts/setup-client.ps1` to write the normal `remnifold.json` while keeping the previous files. Existing capture receipts and recall state remain readable from the former product folders. Server setup reuses resources named `remnifold`, then `kagayoi-memory`, then `cloudflare-supermemory`; explicit resource names always win. Renaming the plugin does not deploy a Worker or move a remote database. Existing document IDs, capture IDs, project provenance and persisted `sm_*` metadata remain unchanged. Storage compatibility details are in [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance).

Remnifold implements its own self-hosted API. It is independent of the hosted Supermemory service and does not read or modify Codex's built-in local memory files.

For architecture and repository layout, see the repository's [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md). Contributor constraints, validation, and packaging commands are in [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md).

Licensed under the [MIT License](LICENSE).
