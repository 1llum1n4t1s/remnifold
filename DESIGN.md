# Design

Remnifold は、作業の履歴を利用者自身の Cloudflare アカウントに残し、出典を保ったままプロジェクトをまたいで想起できるようにする。構成はサーバー（`server/`）とローカルアプリ（`native/`）の2つ。全体像と責務の境界は [docs/design/overview.md](docs/design/overview.md)。

```text
ローカルアプリ（hook / MCP） ─▶ Worker API ─ D1 + Vectorize + Workers AI
リモート MCP クライアント   ◀▶ Worker /mcp + OAuth
```

## Local runtime and distribution

Rust の単一実行ファイルが全コマンドを担い、Node.js を要しない。詳細は [docs/design/local-runtime-and-distribution.md](docs/design/local-runtime-and-distribution.md)。

### Setup installer (Velopack)

複数プロセスのアプリなので、更新は裏で取得し、ログオン時に他のプロセスがないときだけ適用する。詳細は[同文書の Setup の節](docs/design/local-runtime-and-distribution.md#setup-installer-velopack)。

## Storage and provenance

記録は共有の `memories` に置き、プロジェクトは保存先を分けず出典として持つ。詳細は [docs/design/storage-and-provenance.md](docs/design/storage-and-provenance.md)。

## Recall

自動想起の既定の範囲は現在のセッション（`recallScope: session`）で、ID がなければ何も注入しない。詳細は [docs/design/recall.md](docs/design/recall.md)。

## Localization

人が読む固定文言は9言語の辞書から引く。詳細は [docs/design/localization.md](docs/design/localization.md)。

## Project consolidation

プロジェクト単位の統合と checkpoint の公開条件。詳細は [docs/design/project-consolidation.md](docs/design/project-consolidation.md)。

## Memory organization

利用者が起動する整理で、原本は消さず要約で覆う。詳細は [docs/design/memory-organization.md](docs/design/memory-organization.md)。

## Capture

決定的なキーと ID で、再試行しても重複しない。詳細は [docs/design/capture.md](docs/design/capture.md)。

## Enrichment and topics

埋め込み・事実・トピックの補強と、revision の防護。詳細は [docs/design/enrichment-and-topics.md](docs/design/enrichment-and-topics.md)。

## Vector reconciliation

D1 と Vectorize のずれを直す。詳細は [docs/design/vector-reconciliation.md](docs/design/vector-reconciliation.md)。

## Provisioning

Cloudflare REST による作成と更新で、既定は読み取りだけ。詳細は [docs/design/provisioning.md](docs/design/provisioning.md)。

## Remote MCP

Worker の `/mcp` と OAuth。詳細は [docs/design/remote-mcp.md](docs/design/remote-mcp.md)。

## Local app integration

固定の実行ファイルを Claude Code・Codex に登録する。詳細は [docs/design/local-app-integration.md](docs/design/local-app-integration.md)。

### Release installation

strict 経路は 1.0.32 で凍結し、既存の公開物と検証契約を保つ。詳細は [docs/design/release-process.md](docs/design/release-process.md)。
