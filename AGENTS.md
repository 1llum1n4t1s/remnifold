# リポジトリ規約

Remnifold は、自分で運用する Cloudflare サーバーと、Claude Code・Codex 用の単体ローカルアプリを収める。作業の前に下の「先に読む文書」の該当分を読む。詳細の原文は各文書が正本。

## 常に守る規則

- クライアントと保守コマンドは `native/`、開発・ビルド・梱包・テストは `dev/` に置く。Windows・macOS・Linux のどれも Node.js・npm・pnpm・Wrangler なしで動かす。Worker の TypeScript は `server/`。本番の保存は D1 のままとし、新しい依存を避ける。
- 公開済みのツール・HTTP・状態・capture 識別子の契約と、[DESIGN.md](DESIGN.md) の保存・出典・想起の契約を保つ。
- 設定と状態は `native/src/paths.rs` が決めるアプリのフォルダーに置く。旧 Codex home の接続と状態は移行のためだけに読み、消さない。テストは必ず `REMNIFOLD_HOME` で隔離する。
- API キー、生成されたリソース ID、`.dev.vars`、`.wrangler/`、プロビジョニング状態をコミットしない。
- 明示の依頼なしに、デプロイ、リモート DB の移行、公開、R2 への書き込み、リモート作成、push、`topics-setup --apply`、`server-live-test`、リモート統合、`memory-organize prepare`/`commit` を行わない。`v<x.y.z>` タグの push は R2 公開を始める。`release/*` ブランチは新しく作らない（1.0.32 で凍結）。
- 検証中に、本人の実環境(アプリのフォルダー、設定、`settings.json`、登録、HKCU `Run`)へ Setup・`install`・`connect`・`setup` を実行しない。
- 人が読む文言を変えたら9言語の辞書をすべて更新する。AI 向けの文言は英語のまま。`README.md` か `docs/readme/` を変えたら全 `README.<tag>.md` も同じ変更で直す。
- 影響する検証(`cargo xtask check` など)と `git diff --check` を行う。

## 先に読む文書

- コード・設定・文言を変える前: [docs/agents/conventions.md](docs/agents/conventions.md)(上の規則の詳細、対応対象)。
- 検証・梱包の前: [docs/agents/validation.md](docs/agents/validation.md)(コマンド、Windows での一時状態の扱い)。
- 挙動・スキーマ・移行・テストを変える前: [docs/agents/testing.md](docs/agents/testing.md)(テストに残す契約)。
- 導入や strict 経路を変える前: [docs/agents/standalone-app-installation.md](docs/agents/standalone-app-installation.md)。
- Setup・自動更新・登録・リリースを変える前: [docs/agents/setup-installer-velopack.md](docs/agents/setup-installer-velopack.md)。
- CI・R2 公開・信頼境界を変える前: [docs/agents/r2-publication-and-trust.md](docs/agents/r2-publication-and-trust.md)。
- 依存・ツール版を変える前: [docs/agents/dependency-maintenance.md](docs/agents/dependency-maintenance.md)。
