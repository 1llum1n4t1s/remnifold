[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

A self-hosted Cloudflare memory server with remote MCP access and a standalone local app for recalling useful implementation history in Claude Code and Codex. Completed work is saved in one shared collection with its project folder as provenance. See the [overview](docs/readme/overview.md).

## Requirements

The local app is one native executable for Windows x64, macOS Apple Silicon, or Linux x64/ARM64. The server needs your own Cloudflare account. See [requirements](docs/readme/requirements.md).

## Connect a remote MCP client

Use `https://<your-worker>/mcp` with OAuth or a Bearer key; nothing is installed locally. See [remote MCP clients](docs/readme/connect-a-remote-mcp-client.md).

## Install the local app

### Setup installer (recommended)

On Windows, run [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe). Setup registers Claude Code and Codex and updates itself. For macOS, Linux, updates and uninstalling, see [Setup](docs/readme/install-the-local-app.md#setup-installer-recommended). Then connect your server:

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

### First installation

The advanced strict route is frozen at 1.0.32; verify its bootstrap before running it. See [first installation](docs/readme/install-the-local-app.md#first-installation).

## Create or update a server

`remnifold server-setup --account-id <id>` previews; `--apply` provisions and migrates. Update the server before clients. See [server setup](docs/readme/create-or-update-a-server.md).

## Memory behavior

What is saved, redacted, searched and recalled is described in [memory behavior](docs/readme/memory-behavior.md). Review what you store: redaction only reduces accidental secret capture.

## Memory organization

Organize memory on demand with the organize-memory skill; originals are never deleted. See [memory organization](docs/readme/memory-organization.md).

## Upgrading from 1.x names

Older configuration and environment names remain readable. See [upgrading](docs/readme/upgrading-from-1-x-names.md).

Remnifold implements its own API, independent of the hosted Supermemory service, and does not read or modify Codex's built-in memory files. Architecture: [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md). Contributing: [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md). Packaged copies omit `docs/`; read the [guides online](https://github.com/1llum1n4t1s/remnifold/tree/main/docs/readme).

Licensed under the [MIT License](LICENSE).
