# Setup installer (Velopack)

packId は `RemnifoldApp`（`Remnifold` は作業フォルダーと衝突）、Velopack CLI は 1.2.161、クレートは `=1.2.161`。フィード `https://remnifold-updates.kagayoi.com/velopack/` は strict の索引とは別である。実装は `native/src/app_update.rs` と `native/src/app_update/`、設計は [DESIGN.md](../design/local-runtime-and-distribution.md#setup-installer-velopack)。

マルチプロセスのアプリなので、他プロセスを強制終了する apply を日中に行わない。

- `main()` は引数解析より前に全サブコマンドで `VelopackApp` を実行し、自動 apply は**オフ**。フックは終了コード 0 で、導入・更新・削除を妨げない。フック内の CLI は `updater::process::run` で 3 秒、外では `connect::set_cli_timeout` の 30 秒に制限する。
- 登録は `setup-registration.json`（`native/src/app_update/registration.rs`）に記録する。未完了は次のログオン時の `update-apply`、引数なし起動、`mcp` 開始で再試行する。MCP エントリーがないと `mcp` は始まらないので、ログオンの再試行を保つ。
- `remnifold` の MCP エントリーと自身の Claude Code フックは、現在の実行ファイルなら変更せず、別の Remnifold なら置き換える。ログオンのエントリーは別の導入を指すものを変更しない。削除は自身を指すものだけ。
- `mcp` のスレッドだけが確認とダウンロードを行い、apply しない。stdout に書かず、診断は `app-update.log`。Velopack 以外のビルドは何もしない。
- ログオン時の `update-apply` は保留なしなら即終了、他プロセスがあれば適用しない。署名者（Windows の全 PE、macOS のチーム `SL228Y8UUR` と公証、Linux の HTTPS・フィードハッシュ・AppImage 1 個）を検証し、再起動なしで適用する。
- macOS/Linux は引数なし起動が登録し、`mcp` と `update-apply` も再確認する。Linux は `$APPIMAGE` のファイル自体を登録する。アンインストール準備は `uninstall-setup --apply`、macOS の LaunchAgent は `AbandonProcessGroup` が必要。
- `update-feed-test` は本番ビルドに含めない。`REMNIFOLD_UPDATE_FEED_DIR` と `REMNIFOLD_UPDATE_ALLOW_UNSIGNED=1` を有効にする。Windows の pack はフィーチャーなしでビルドし、テスト指定と `-Sign` の併用を拒む。

実環境に触れない検証手順は [workflow.md](workflow.md)、公開の手順と条件は [release-process.md](release-process.md) を読む。
