# Local runtime and distribution

`native/` は Rust の単一実行ファイルで、`mcp`、`hook`、`capture`、`import`、`setup`、`status`、`install`、`connect`、`server-setup`、`topics-setup`、`memory-organize` を提供する。導入後の運用とサーバー保守に Node.js、Cargo、Codex、プラグインシステムは要らない。stdio の MCP は公開ツールのスキーマと応答を保ち、上限付きの要求で単一のワークスペースルートを解決し、ルートが変わると共有コンテキストキャッシュを無効にする。廃止した `plugin-update` と `plugin-bootstrap` は `USE_REMNIFOLD_INSTALL` を返す。

プラットフォームパッケージは `remnifold-<Rust target>.tar.gz` で、`remnifold/` の下に `bin/remnifold`（Windows は `.exe`）、生成した登録、`native-build.json`（ターゲット、ソースコミット、版、SHA-256）を持つ。対象は Windows x64、macOS Apple Silicon、Linux x64/ARM64 で、macOS Intel は対象外。作成と検証の既定は `--source-mode commit` で、クリーンなソースと Git blob をコミットに結び付ける。`checkout` はローカルのバイト列だけを検証し、リリースの同一性は証明しない。`cargo xtask check` は後者を使う。

Worker は `server/src/` の TypeScript。`dev/` のタスクランナーは整合性を固定した esbuild、TypeScript、workerd と Cloudflare の型を取得し、Node.js なしで検査・梱包する。実行ファイルはサーバーセットアップ用に Worker バンドルと SQL 移行を埋め込む。本番の保存は D1、ローカルのサーバーテストは実 workerd とテスト専用の Durable Object SQLite アダプター、決定的な AI/ベクターのフィクスチャを使う。`package.json` は旧リーダー向けの版情報だけを持つ。旧接続の別名、状態、capture 識別子、出典、API の契約は保つ。

## Setup installer (Velopack)

一般利用者はユーザー単位の Velopack Setup（packId `RemnifoldApp`、Windows は `%LOCALAPPDATA%\RemnifoldApp\current\remnifold.exe`）を使う。strict な `install` は別フォルダー（`Programs\Remnifold`）の上級者向け経路で、1.0.32 で凍結した（オーナー決定、2026-11-18）。それより新しい版は Setup だけで公開する。`connect` は実行した側の実行ファイルで登録を置き換える。設定と状態は `native/src/paths.rs` のフォルダーに置き、更新で置き換わる導入フォルダーには入れない。

- 取得と適用の分離、公開の流れ: [workflow.md](workflow.md)
- 登録、ログオンのエントリ、プロセス検出、OS ごとの差: [configuration.md](configuration.md)
- 署名者の検証と信頼の限界: [security.md](security.md)
- Linux の隔離 E2E と未検証の範囲: [testing.md](testing.md)

引数なしの端末起動は MCP クライアントではないため、登録の後にインストール状態と自動更新を説明し、対話的で接続がなければサーバー URL を尋ねて `setup` を始める。stdin がパイプなら既定の `mcp` として動く。
