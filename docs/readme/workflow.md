# Update and uninstall workflow

## Automatic updates

During an MCP session the app checks at most once a day, including after failure, and downloads without interruption. At logon it applies the update only if no other Remnifold process is running:

| Platform | Logon entry |
| --- | --- |
| Windows | HKCU `Run` |
| macOS | `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist` |
| Linux | `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop` |

Before applying, Windows checks the publisher signature of every executable and macOS checks its Developer ID. Linux requires `/var/tmp/velopack/RemnifoldApp` to belong to you and not be writable by other users; it has no code signature.

Set `"auto_update": false` in `app-update.json` to disable updates. The file and `app-update.log` are in the [state folder](configuration.md#connection-and-state).

Registrations that did not finish are retried at logon. Setup takes over the registrations of an existing strict installation but keeps its files; delete `%LOCALAPPDATA%\Programs\Remnifold` yourself when no longer needed.

## Uninstall

On Windows, uninstall from Settings. This removes Remnifold's registrations and logon entry, while keeping connection and memory state. To return to strict installation, run its own `connect claude-code --apply` and `connect codex --apply` afterward.

macOS and Linux have no uninstaller. Before deleting the app, run its `uninstall-setup --apply`; without `--apply` it only previews the changes.

macOS:

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

Linux (adjust the path):

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

Configuration and memory state are kept. You can delete the update cache: `~/Library/Caches/velopack/RemnifoldApp` on macOS or `/var/tmp/velopack/RemnifoldApp` on Linux.

To only remove registrations, use:

```sh
remnifold connect claude-code --remove --apply
remnifold connect codex --remove --apply
```
