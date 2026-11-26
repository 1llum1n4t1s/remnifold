# Memory behavior

Completed requests and final answers are stored in the shared `memories` collection with project provenance. Attached skill definitions and browser state are excluded from the saved request. See [records and privacy](data-model.md) for events, redaction and checkpoints.

## Search

`search_memory` searches across sessions. Pass `sessionId` to limit it. Read full results with `getDocument`; browse with `listTopics`, `listDocuments` or `listMemories`. Manual and compatibility options are in the [tool reference](api-reference.md).

Project names are searchable. Project-only queries prefer that project and newer sources; mixed queries prioritize topic coverage. Manual search retains historical revisions and original records. Partial failures keep successful results and report unavailable indexes.

## Automatic recall

Automatic recall is off by default. With it enabled, `recallScope: session` searches by the current topic but injects only records from this session and client kind. It needs a session ID and never asks for recent tasks elsewhere. Claude Code also recognizes session IDs copied on resume, with a time bound that excludes later parallel work; Codex uses only the current ID.

Set `recallScope: all` to search all sessions and include labeled recent work. These entries suggest prior work without implying relevance. Automatic context is an index of titles, descriptions, sections, dates and IDs, sometimes with a short original passage to verify. It prefers relevant checkpoints without repeating covered originals.

The default is three records, at most 3,000 UTF-16 units. Unchanged entries are not repeated until the context is compacted or cleared. Short follow-ups can recover a concrete topic from bounded session history. Without one, no search runs. See [recall architecture](architecture.md#recall) and [configuration](recall-settings.md).

## Capture and checkpoints

Each completed turn is captured, with safe retries that reuse the same record. Messages sent while the AI works, question answers and compaction summaries have separate retention rules. Capture redacts the API key and common secrets, but do not rely on redaction for sensitive content. See [records and privacy](data-model.md).

Projects are consolidated daily, or immediately with `consolidate_memory`. Originals stay available and changes invalidate dependent checkpoints. Low-information records remain searchable manually but do not receive automatic enrichment or recall.

For `stage`, `cause` and `action` codes, see [troubleshooting](troubleshooting.md). To classify old records, see [commands](commands.md#status-and-old-records).
