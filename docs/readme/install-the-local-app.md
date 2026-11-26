# Install the local app

## Setup installer (recommended)

Setup needs no GitHub account. It registers Claude Code and Codex when their command-line tools are found; a tool installed later is registered at the next Remnifold start. On Windows, a client that already reaches Remnifold through the Toolport gateway gets no second, direct registration; see [MCP registration](mcp-registration.md). See [updates and uninstalling](workflow.md).

- **Windows x64:** run [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe). It is publisher-signed, needs no administrator rights, installs under `%LOCALAPPDATA%\RemnifoldApp`, and opens a first-run guide.
- **macOS Apple Silicon:** open `RemnifoldApp-osx-Setup.pkg`, signed and notarized with the publisher's Developer ID. "Install for me only" uses `~/Applications`; an all-users install uses `/Applications` and updates may ask for an administrator password. The app starts once to register its clients. Connect with `"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>`, adjusting the path for `~/Applications`.
- **Linux x64/ARM64:** keep `RemnifoldApp-linux-x64.AppImage` or `RemnifoldApp-linux-arm64.AppImage` in a permanent location, make it executable, and run it once without arguments. If moved, it registers its new location at the next start. Connect with `<path-to-AppImage> setup --base-url <URL>`. AppImages have no code signature: trust relies on HTTPS and feed checksums.

The macOS and Linux packages are published from the first release after 1.0.32.

## Strict installation (advanced)

The strict route ends at 1.0.32. Installed versions and `--verify-only` continue to work; run Setup to move to newer versions. See [strict installation](release-process.md) and [update/register commands](commands.md).

## First installation

For a strict installation, verify the bootstrap before running it: follow the [secure first installation](security.md#first-strict-installation).

## Connect to your server

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

Enter the API key at the hidden prompt. Setup verifies the session before writing the connection; `--force` updates an existing connection while preserving unrelated settings. Connection locations, environment overrides and legacy migration are in [configuration](configuration.md#connection-and-state).

Run the `whoAmI` MCP tool to verify connectivity and space discovery. To import existing Codex sessions, preview with `remnifold import --limit 10` and add `--apply` to upload sanitized records. Repeats reuse deterministic identities; preview writes no remote memory.
