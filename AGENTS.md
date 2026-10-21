# Repository guidance

This repository packages the Remnifold self-hosted Cloudflare server and its Codex plugin.

- Keep `client/` compatible with Node.js 24 and avoid runtime dependencies unless they materially improve reliability.
- Keep `server/` deployable with Wrangler. Never commit API keys, generated resource IDs, `.dev.vars`, or `.wrangler/` state.
- Preserve the storage, provenance, recall, and enrichment contracts in [DESIGN.md](DESIGN.md), including the 1.x connection/environment readers, legacy capture receipts and spaces, existing document IDs, and persisted `sm_*` metadata.
- Keep the official Codex local memory store outside this integration.
- Changes to capture must preserve deterministic capture keys/custom IDs, retry-safe document reuse, and redaction of the configured memory API key and supported secret patterns.
- Run client tests, server tests, plugin validation, and `git diff --check` for affected changes.
- Do not deploy, migrate a remote database, publish a package, create a remote repository, push, run `scripts/setup-topics.mjs --apply` or `pnpm -C server test:live`, or invoke remote consolidation without an explicit request.

## Local plugin update after a vava release

`vava.config.json.localPlugin` configures the personal release installation described in [DESIGN.md](DESIGN.md#release-installation). Run its updater on Windows with authenticated GitHub CLI access and the native Codex CLI available. Its external work root must be under `D:/Codex/remnifold`, outside the repository and selected Codex home, with no linked ancestors.

- During vava preflight, read `localPlugin` and run its `verify` commands for the current version before deciding new versus resume. Verification compares the installed files with the release commit and does not depend on CI artifact retention. An already published version whose local installation is pending must resume that version; do not bump or publish again to repair the installation.
- After the exact release commit's configured CI workflow succeeds and publishes its plugin artifact, run `ready`, then `update`, then every `verify` command from the repository root, substituting the fixed `NEW_VERSION`. This is a required release checkpoint, including resumed releases; do not require another installation request. Dry-run reports these commands without running the update.
- Use the downloaded published artifact, validate its files against that release commit, and install through the official Codex plugin CLI. Preserve the connection, capture receipts, and hook/MCP registration. Never edit Codex's plugin cache directly, add a duplicate registration, or commit/push the non-Git marketplace source.
- Record the local result as `complete` or `already-current` only after the official installed version and both marketplace/cache contents match. A failed update or verification remains `pending`/`blocked` and must be reported even when publication succeeded. Re-run this checkpoint idempotently on resume.
- Installation verification does not prove that an already running chat has reloaded its MCP server or hooks. Report any required new chat/restart or hook trust separately, based on the actual CLI result and runtime checks.
- The updater returns its external `workRoot`/`stagingPath`; clean this invocation's downloaded/extracted temporary files under the applicable cleanup rules after validation, retaining only evidence needed for a failed checkpoint. If obsolete managed files require Prunlet cleanup, keep the checkpoint blocked until that cleanup and re-verification finish.

For updater changes, run `node scripts/local-plugin-update-e2e.mjs --archive <absolute-path-to-published-remnifold.tar.gz> --work-root <absolute-external-work-directory>` on Windows using the same work-root constraints. The E2E launches the actual updater against isolated GitHub/Codex CLI fixtures; it does not change the real installation or remote memory. Inspect `local-plugin-update-validation.json` in that work directory for successful installation, idempotent retry, recovery after CLI failure, verification without retained CI artifacts, and rejection before installation changes on invalid publication inputs or local edits. This check is separate from the normal CI checks below.

## Dependency maintenance

- Update `server/package.json` and `server/pnpm-lock.yaml` together, then verify the frozen-lockfile install and server checks below.
- Review `server/pnpm-workspace.yaml` with dependency updates. It holds native build approvals for `esbuild` and `workerd` and exact-version `minimumReleaseAgeExclude` entries; replace obsolete exceptions when upgrading rather than broadening them to all versions.
- Keep `.github/dependabot.yml` covering GitHub Actions and npm manifests in both `/` and `/server` on a weekly schedule, with minor and patch updates grouped separately from major updates.

## Validation

Run these checks from the repository root for affected changes (Node.js 24.18.0 or later in the 24 series, pnpm 11.4.0):

```powershell
node --test client/*.test.mjs scripts/*.test.mjs
node scripts/capture-redaction-e2e.mjs
node scripts/client-repair-e2e.mjs
node scripts/capture-diagnostics-e2e.mjs
node scripts/opop-client-e2e.mjs
node scripts/recall-hook-e2e.mjs
node scripts/memory-quality-e2e.mjs
node scripts/remnifold-rename-e2e.mjs
node scripts/loopback-connection-e2e.mjs
node scripts/client-ranking-e2e.mjs
pnpm -C server install --frozen-lockfile
pnpm -C server check
node scripts/validate-plugin.mjs
pnpm run package:plugin
git diff --check
```

Server tests use local D1 and deterministic AI/vector fixtures. Provisioning tests simulate Wrangler calls without creating remote resources. In `server/tests/smoke.mjs`, preserve bounded waits for local Worker startup and MCP responses; on Windows, stop the Wrangler process tree, including `workerd`, before removing temporary D1 state so database locks are released.

`scripts/capture-redaction-e2e.mjs` exercises the real Stop hook against an authenticated loopback API and writes `dist/capture-redaction-after-validation.json`. It verifies secret assignments in captured content and titles, preserved discussion text, deterministic IDs, acknowledged retry suppression, and concurrent Stop receipt integrity. Keep its rendezvous and child-process waits bounded, including the missing-participant canary. It runs with the script checks in CI.

`scripts/client-repair-e2e.mjs` verifies Unicode-safe output with unchanged v2 capture identities and bounded history discovery. `scripts/capture-diagnostics-e2e.mjs` verifies fixed capture failure codes through the hook and standalone CLI without exposing exception text. Both use isolated fixtures, write reproducible results under `dist/`, and run with the script checks. `server/tests/review-fixes-e2e.mjs` verifies atomic source/checkpoint mutations, restore/forget cleanup races, FTS updates, and scoped consolidation queries through the authenticated Worker and migration-backed SQLite. It runs as part of `pnpm -C server check` and writes `dist/server-review-fixes-validation.json`.

`scripts/opop-client-e2e.mjs` checks invalid configuration and stdin roots, legacy settings fallback, partial document-fetch notifications, and event-specific hook imports through actual launcher processes and an authenticated loopback API. Inspect the checks and module-load counts in `dist/opop-client-validation.json`.

`scripts/recall-hook-e2e.mjs` verifies current [recall contracts](DESIGN.md#recall) through actual hook subprocesses without depending on Git HEAD. It checks duplicate suppression, revisions, compaction, context-budget skipping and later recall, default and explicit recall counts through client setup, browser-state and structured-reply projection, legacy index repair without extra document fetches, and Git discovery counts. It writes `dist/recall-hook-validation.json` and runs with the script checks. Use `--runtime-root dist/remnifold` to check the packaged launcher; both subprocess waits and fixture cleanup must remain bounded and portable.

For request projection, continuation topics, or capture revision selection, preserve the real hook/MCP coverage in `scripts/memory-quality-e2e.mjs`: multipart v2 identities, skill-only completed work, ordinary variables and paths, user-only bounded topic recovery, automatic latest revisions versus manual history, and legacy document projection. Topic recovery checks must retain coverage across large tool outputs and UTF-8 chunk boundaries, reject mismatched sessions and incomplete or oversized rows, and verify the byte, user-message, and deadline bounds in [DESIGN.md](DESIGN.md#recall). Inspect `dist/memory-quality-validation.json`. For naming or compatibility-reader changes, run `scripts/remnifold-rename-e2e.mjs` against both source and `--runtime-root dist/remnifold` after packaging; inspect `dist/remnifold-rename-validation.json` and `dist/remnifold-rename-installed-validation.json`. Both checks run against source with the script checks in CI.

When changing asynchronous enrichment, preserve revision guards and project-scoped fact relationships; validate stale topic/vector work and update/forget races in `server/tests/races.test.mjs`, plus the API coverage in `server/tests/smoke.mjs`. Vector changes must also cover bounded Vectorize lookups, forgotten-vector cleanup even when AI enrichment is disabled, and repair after late upserts or deletion races. Topic API changes must retain unclassified browsing and exact-topic filtering across legacy and shared spaces.

For recall changes, validate the [recall contracts](DESIGN.md#recall) through `server/tests/recall-efficiency.mjs`, which connects the real client and authenticated Worker API to migration-backed SQLite with deterministic AI/vector fixtures. Retain coverage for request/embedding/vector counts across many spaces, scoped searches, failure fallback, legacy indexes, source excerpts, and checkpoint selection. Inspect its reproducible metrics and results in `dist/recall-efficiency-validation.json`.

`server/tests/opop-performance.mjs` compares current and former display projections and ranking through authenticated Worker APIs on migration-backed SQLite. It verifies identical public JSON, stable ties, checkpoint selection, scopes, and retained semantic embeddings, and measures actual D1 row bytes and bounded ranking computation. It runs with server checks and writes `dist/opop-performance-validation.json`; its synthetic CPU timings do not measure deployed API latency.

`server/tests/opop-projection-e2e.mjs` compares consolidation, profile fallback, enrichment retry, and PATCH reads against their former full-row queries through authenticated Worker APIs on migration-backed SQLite. Preserve Unicode and embedded-NUL behavior, excerpt suffixes, scope and order, consolidation and enrichment revision guards, and PATCH conflicts. It writes `dist/opop-projection-validation.json` and measures returned row bytes rather than deployed latency.

`scripts/loopback-connection-e2e.mjs` verifies setup, MCP, Stop capture, and history import against an authenticated loopback fixture, including setup/runtime endpoint agreement and deterministic capture IDs. It writes `dist/loopback-connection-validation.json`; `REMNIFOLD_VALIDATION_DIR` selects an absolute external output directory for this check and the projection E2E.

`scripts/client-ranking-e2e.mjs` verifies ranking, hydration, capture revisions, and checkpoint selection through the real client and an authenticated loopback API. For manual project search changes, retain project provenance and recency ordering before the hydration cap, combined project/topic coverage, and isolation from automatic recall. It writes `dist/client-ranking-validation.json` and measures matching and normalization counts. `server/tests/structure-performance-e2e.mjs` verifies document projections, search, vector reconciliation, and enrichment through authenticated Worker APIs backed by migration-based SQLite. It writes `dist/structure-performance-validation.json` and measures repeated string, row lookup, and digest work. Both support `--comparison-root <repository>` for an exact pre-change source comparison and `REMNIFOLD_VALIDATION_DIR` for an absolute external artifact directory. Their operation counts do not measure deployed latency.

Consolidation changes must preserve the [project consolidation contracts](DESIGN.md#project-consolidation). Cover them in `client/mcp-server.test.mjs`, `server/tests/races.test.mjs`, and the authenticated API path in `server/tests/smoke.mjs`. Validate truncated-output retry, publication races, stale checkpoint recovery, and enrichment/scheduled failure diagnostics in `server/tests/log-regressions.mjs`; inspect `dist/server-log-validation.json`. Both artifact-producing checks run as part of `pnpm -C server check`. Preserve both cron definitions in `server/wrangler.example.jsonc`, and apply migration `0005_memory_consolidations.sql` before installing a client that depends on consolidation.

`pnpm run package:plugin` builds and validates `dist/remnifold`. CI runs the client, script, server, and plugin checks and uploads the package as an artifact; `git diff --check` remains a local check. Release tagging and publication are separate operations.
