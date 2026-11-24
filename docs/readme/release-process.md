# Strict release installation process

The strict route is frozen at its final version, 1.0.32. Later releases use Setup only. `remnifold install` installs versions up to 1.0.32; installed versions and retained receipts remain verifiable.

Published packages are at [Remnifold updates](https://remnifold-updates.kagayoi.com). `releases/<version>/index.json` points to a manifest with hash-named archives. Authenticate publication through GitHub; an R2 URL or commit status does not prove its origin.

| Platform | Rust target |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Mac Intel is not supported. Installation is per user, without administrator rights:

| Platform | App folder | Fixed executable |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold` (default `~/.local/share/remnifold`) | `bin/remnifold` |

`install` verifies the release commit, successful CI publisher log, R2 manifest, archive digest/size and source before writing. Each version is kept once in `versions/<version>`; different bytes never overwrite it. Receipts in `receipts/` support offline verification. Registrations use the fixed executable and survive updates. On Windows, `"restartRequired": true` means a running MCP server must restart.

When witness logs expire, fresh installs fail closed. `--verify-only` can use the receipt but still needs GitHub release source metadata. See [commands](commands.md) and [secure first installation](security.md#first-strict-installation). The former `plugin-update` and `plugin-bootstrap` return `USE_REMNIFOLD_INSTALL`.
