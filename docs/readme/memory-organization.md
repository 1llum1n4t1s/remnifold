# Memory organization

After applying the [server migrations](create-or-update-a-server.md), including `0009_memory_organization.sql` and the range index in `0013_memory_created_index.sql`, use the [organize-memory skill](../../skills/organize-memory/SKILL.md), for example `$organize-memory 7` or "organize the last 7 days of memory". The optional day count is 1–3650 and uses server creation time. The run stops when no due work remains or its batch cap is reached (default 3, maximum 20). Without days, it takes one global batch. [setup-remnifold](../../skills/setup-remnifold/SKILL.md) pauses old scheduled tasks; it creates none.

Each batch handles at most 20 targets and 48,000 UTF-16 source units, with at most two lookups. Candidate summaries and targeted reads are additional. Partial work can continue in a later run; this does not mean the archive is complete.

## Commands

Run the installed executable:

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

`prepare` creates a 30-minute lease. Ranged calls return `range.cursor` and `range.exhausted`; pass a non-null cursor with `--range-cursor <cursor>` next time. Older servers reject unsupported ranges rather than falling back. Read all source IDs/revisions/chunks and candidates from the retained response. `lookup` accepts `{"query":"a concise topic"}`; `commit` accepts `batchId`, `summaries` and `coverage`.

Declare `complete` only when every chunk, conclusion, reason, evidence and uncertainty is represented. Otherwise use a partial continuation or deferral. Check the commit receipt, which distinguishes published summaries from unfinished progress. Before lookup or commit, follow the [stdin conventions](conventions.md); for the exact fields, paging and recovery, follow the [execution skill](../../skills/organize-memory/SKILL.md#one-batch).

These commands need the owner API key. Remote MCP OAuth cannot authorize them, and unsupported servers never fall back to save, forget or generation. Codex writes the prose; existing Workers AI embedding usage remains billed.

`sources` pages up to 50 record IDs, spaces and dates; IDs can repeat by provenance path. Use `getDocument` for bodies. A changed parent returns conflict. Originals are never deleted, and changes invalidate their summaries. For visibility and vector readiness, see [memory architecture](architecture.md#organization-visibility).
