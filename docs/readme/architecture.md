# Memory architecture

## Recall

Automatic recall combines the current prompt with a concrete topic from bounded user history when needed, including before a large tool output. It searches content topics across spaces, not just the current folder. The latest captured revision of each turn and part is selected; manual search can return earlier revisions.

In `session` scope, client kind and exact session ID are checked before the result limit. A missing session ID produces no injection, and records without one are ignored. When the client is known, records without a stored client kind are ignored too. Claude Code resumes use at most eight copied IDs; records after a copied session's last row are excluded, so later work in a parent or parallel session is not mixed in. Old servers may return fewer matching candidates, but the client check still prevents other sessions from being injected.

In `all` scope, recent tasks are kept separate from topic matches. Automatic results include titles, dates, IDs and bounded excerpts; full source remains available with `getDocument`. Changed records can reappear, and compaction or clear resets repetition tracking. Local state contains no conversation text. If it is unavailable, recall continues without duplicate suppression.

Server migration `0015_memory_search_projection_session.sql` makes session filtering faster without changing results. See the [recall design](../design/recall.md) and [search architecture](../design/architecture.md).

## Organization visibility

No original is deleted. A fully represented source leaves normal search and topic recall only while its summary is valid; direct retrieval and recent-task eligibility remain. Unresolved contradictions are retained as uncertainty. Conflicting captures remain visible unless distinct version-two keys and body hashes prove independent evidence; that does not prove which is newer.

A changed or forgotten source invalidates its dependent summaries and exposes surviving originals. With AI enabled, originals already used for semantic search stay visible until replacement embeddings and vectors are ready; failures keep them visible. With AI disabled, lexical coverage can hide them. Re-enabling AI may reveal originals again. Vector recovery is asynchronous, and the fifteen-minute cron is not a completion deadline.

Older project checkpoints can still expose information until processed as whole records. The [organization design](../design/memory-organization.md) explains coverage, invalidation and limits.
