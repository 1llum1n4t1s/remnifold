# Memory tool reference

## Find and read

- `listTopics` lists content categories. Pass a `topic` to `listDocuments` or `listMemories`; `__unclassified__` selects records with no current topic, including failed, disabled or skipped classification. Both legacy spaces and shared records are included.
- `search_memory` searches all stored projects by default. `sessionId` limits it to one session. `"current"` works only in Claude Code, where its `PreToolUse` hook resolves it for the directly registered `mcp__remnifold__search_memory` and for `search_memory` called through the Toolport gateway's `toolport_call_tool` or `toolport_run_script` (see [MCP registration](mcp-registration.md)); Codex and other gateways must use an actual ID. `includeProfile: true` adds the searched container list.
- `getDocument` reads a full record. Organized summaries and checkpoints can include source previews and `sourceCursor` paging.
- `listSpaces` exposes physical spaces for compatibility and diagnostics.

Global search includes spaces beyond the old discovery limit. An older server uses a compatibility path capped at 100 spaces and reports incomplete discovery. Failed spaces or documents do not discard successful results.

## Save, forget and consolidate

- `add_memory` saves in `memories`. An explicit `containerTag` overrides the location. The local client can add provenance from an absolute `sourceFolder` or one workspace root; remote MCP instead accepts `projectId`. The app never uses its installation folder as your project.
- To forget a record, read it first, then call `add_memory` with `action: "forget"`, `documentId` and its source `containerTag`. Exact stored content remains supported for compatibility.
- `consolidate_memory` advances one project by at most 40 new records and needs at least one unconsolidated source. Remote calls need the stored `projectId`; the local client can infer one workspace root or use `sourceFolder`/`projectId`. Repeat for a larger backlog.

Checkpoint content and metadata cannot be updated directly (HTTP 409). Edit its sources and regenerate it. Re-enrichment and deletion remain supported.

Use `whoAmI` to check the connection and space discovery. Recall settings are in [configuration](recall-settings.md).
