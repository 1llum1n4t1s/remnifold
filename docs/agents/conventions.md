# Conventions

このリポジトリは、セルフホストの Cloudflare サーバー Remnifold と、Claude Code・Codex 向けのスタンドアロンのローカルアプリをパッケージ化する。

- ローカルのクライアントコマンドと保守コマンドはすべて `native/` に、開発・ビルド・パッケージング・テストは Rust ワークスペースの `dev/` タスクランナーに置く。Windows・macOS・Linux のいずれの経路も、Node.js、npm、pnpm、Wrangler なしで動かなければならない。既存の公開ツール、HTTP、状態、キャプチャ識別の契約を維持する。
- Worker の TypeScript ソースは `server/` に置き、ネイティブの esbuild でバンドルし、ネイティブの `server-setup` コマンドが Cloudflare REST 経由でデプロイする。本番の永続化は D1 のままとし、Durable Object SQLite アダプターはテスト専用とする。新しい依存関係は避ける。
- Remnifold 自身の設定と状態は、`native/src/paths.rs` が解決するアプリフォルダー（`REMNIFOLD_HOME` による上書きが先、次に OS 標準の場所）に置き、Codex や Claude Code のホームには決して置かない。従来の Codex ホームにある接続と状態は、移行のためにだけ読み、削除せず、すべてのテストプロセスを `REMNIFOLD_HOME` で隔離する。
- [DESIGN.md](../../DESIGN.md) にある保存、来歴、想起、エンリッチメント、整理の契約を維持する。これには、1.x の接続・環境リーダー、従来のキャプチャレシートとスペース、既存のドキュメント ID、オーナー識別、永続化済みの `sm_*` メタデータを含む。
- 公式の Codex ローカルメモリストアは、この統合の外に置く。キャプチャは、決定的なキー／カスタム ID、再試行しても安全なドキュメントの再利用、設定されたメモリ API キーと対応するシークレットパターンの伏せ字化を保たなければならない。
- API キー、生成されたリソース ID、`.dev.vars`、`.wrangler/`、生成されたプロビジョニング状態は決してコミットしない。メタデータのみのルート／サーバーの `package.json` は古い版のリーダーを支えるためのもので、JavaScript パッケージマネージャーのツールチェーンの復活を認めるものではない。
- 現在のビルド・配布対象は、Windows x64、macOS Apple Silicon、Linux x64/ARM64 である。`ci.yml` のすべてのターゲットを、それぞれのネイティブな GitHub ホストランナー（`windows-latest`、`macos-latest`、`ubuntu-24.04`、`ubuntu-24.04-arm`）でビルドする。macOS Intel は現在のビルドとチェックから除外するが、過去の schema-1 の Intel メタデータは読める状態を保つ。
- 影響する [Validation](validation.md) のチェックと `git diff --check` を実行する。
- ローカライズ: 人間が読むテキスト（CLI の出力とヘルプ、Setup のウェルカム、フックの通知、想起の見出し）は、`native/src/locales/` にある 9 つの辞書から `locale::tr`/`tr_with` で引く（[DESIGN.md](../design/localization.md) を参照）。人間が読む文言を追加・変更するときは、9 つの辞書をすべて更新し、整合性テストを通し続ける。AI 向けのテキスト（MCP ツールの説明とスキーマ、モデルに返す指示）は英語のままとする。`README.md` または `docs/readme/` を変更するときは、同じ変更ですべての `README.<tag>.md` も変更する（翻訳は全文を 1 つのファイルに保つ）。
- 明示的な依頼なしに、次を行わない。デプロイ、リモートデータベースの移行、公開、R2 へのステージングやライブスモーク、リモートリポジトリの作成、push、`topics-setup --apply` の実行、`server-live-test` の実行、リモートでのコンソリデーションの呼び出し、`memory-organize prepare`/`commit` の実行。整理の prepare はリースを作るので、読み取り専用の探りではない。バージョンタグ `v<x.y.z>` を push すると R2 へ書き込む `release-velopack.yml` が始まり、`release/*` を push すると `ci.yml` の R2 へ書き込む strict 公開が始まる（1.0.32 で凍結済み。新しい `release/*` ブランチは作らない）ので、どちらも同じ明示的な依頼が必要である。ローカルでの検証は、リモートへの変更を認めるものではない。検証中に、Setup インストーラー、`install`、`connect`、`setup` を、ユーザーの実際のアプリフォルダー、設定、`settings.json`、Claude Code/Codex の登録、HKCU の `Run` キーに対して決して実行しない。[Setup installer (Velopack)](setup-installer-velopack.md) にある隔離の方法を使う。
