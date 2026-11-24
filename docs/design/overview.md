# Design overview

Remnifold は、ユーザー自身の Cloudflare アカウントに有用な作業履歴を保持し、クライアントがソースの出所を失わずにプロジェクトをまたいで想起できるようにします。リポジトリには、サーバーとスタンドアロンのローカルアプリという 2 つのデプロイ可能な部品があります。

```text
Local app (Claude Code / Codex)       User-owned Cloudflare account
  prompt/capture hooks  ───────────▶  Worker API
  MCP search/getDocument ◀─────────   D1 + Vectorize + Workers AI
Remote MCP client      ◀──────────▶  Worker /mcp + OAuth endpoints
```

`server/` が、認証、永続化、語彙検索と意味検索、エンリッチメント、ドキュメント取得を担います。各インストールは、ユーザーの Cloudflare アカウント内のリソースを使います。ローカルアプリの設定は、そのインストールのエンドポイントと API キー、および想起の設定を選びます。リモート MCP クライアントは、ローカルアプリを介さず Worker に直接接続します。その場合、想起と保存はローカルフックではなく、クライアントとそのモデルが制御します。

リポジトリの入口は次のとおりです。ソース用とパッケージ用の登録には `.codex-plugin/`、`.mcp.json`、`hooks/`、クライアント・保守コマンド・インストーラー・登録には `native/`、Node を使わないツールチェーン・パッケージング・契約テストには `dev/`、Worker とマイグレーションには `server/`、セットアップと整理のワークフローには `skills/` を使います。ソースの登録は Cargo を使い、インストール済みの登録は固定の実行ファイルを使います。開発時のチェックは [AGENTS.md](../../AGENTS.md) で管理し、インストールと運用は [README.md](../../README.md) にあります。
