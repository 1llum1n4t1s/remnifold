# Stored records and privacy

## Turns and events

The app stores each human request and the final assistant answer. It excludes thinking, tool results, request copies and other sessions. Background notifications create continuation turns without changing previously saved keys. `sm_source` identifies Codex or Claude Code. Deterministic capture keys make retries reuse records.

Claude Code `Stop` also saves compaction summaries, messages sent during a turn and answers to AI questions as separate events. Immediately after compaction, only records from the previous `Stop` are available. A forgotten event can reappear if resent after a lost receipt.

Compaction summaries are AI-generated leads, not authorization. They are not recalled automatically but can be found manually. Event answers and summaries mask plausible phone numbers and email addresses, except `git@host`. Free-text answers to questions about personal information, such as names, addresses, cards or passwords, are replaced.

Masking is incomplete: unseparated phone numbers, some question phrasings, selected choices and personal data in other answers can remain. Mid-turn messages get no contact masking, and a summary may restate personal content. Capture also redacts the configured memory API key and common secret patterns.

## Organization and provenance

Folders and project identity are provenance, not storage boundaries. Existing document IDs and physical locations are preserved. Topics come from record content asynchronously, while explicit topics are retained. Short acknowledgements are excluded from enrichment and automatic recall but remain manually searchable.

## Checkpoints

Projects are checked at 03:00 JST. A checkpoint is due with 20 new records, or three days after the last successful checkpoint when at least one source remains. Before the first checkpoint, three days are measured from the oldest pending record. The first due run drains the whole backlog in bounded batches.

Workers AI creates checkpoints; originals and exact provenance remain. A changed or forgotten source invalidates dependent checkpoints, and newer checkpoints supersede older ones. Checkpoints cannot be edited directly.
