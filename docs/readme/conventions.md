# Input conventions for memory organization

Before `prepare`, arrange a safe stdin path and retain the complete response so you can page through it. On Windows, first run the execution skill's local synthetic PTY probe. A truncated display must be recovered from the same response, not by preparing another batch.

Validate the exact JSON in memory before commit. Every coverage entry needs its prepared ID, revision and an explicit `summaries` array, including `[]` for partial or deferred work. Follow the [execution skill](../../skills/organize-memory/SKILL.md#one-batch) for field limits and partition rules.

JSON stdin has a 30-second deadline and a 128,000-byte UTF-8 limit, including escaping. `--line` needs one compact JSON line and its newline; otherwise close stdin after the object. Use a non-echoing channel and keep generated text out of shell arguments and logs.

On Windows, launch the native executable directly in a dedicated PTY with `memory-organize lookup --line --private-stdin` or `memory-organize commit --line --private-stdin`. Wait for `REMNIFOLD_STDIN_READY` on stderr, then send exactly one compact JSON line and newline, without trailing bytes. Keep the payload plus newline at or below 127,000 UTF-8 bytes to allow terminal transformations.

Private mode disables echo and line editing and restores the original mode after the terminator. If readiness is absent or any step fails, stop sending and close that PTY. Do not reuse it or restore echo while input might remain. Do not use a shared console or Git Bash wrapper.

Redirected stdin and non-Windows hosts reject `--private-stdin`. On macOS and Linux, use a non-echoing pipe with line or EOF input. See [safe stdin and failures](../../skills/organize-memory/SKILL.md#safe-stdin-and-failures) for the full procedure.
