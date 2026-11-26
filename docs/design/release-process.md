# Release installation

strict 経路は 1.0.32 で終了した。以後の版には `release/*` ブランチと `releases/<version>/index.json` がなく、`install` は fail closed になる。既存の公開、レシート、`--verify-only` は使え、出荷済みのクライアントが頼るブランチ、オブジェクト、CI の経路は残す。Setup の公開は [workflow.md](workflow.md)、信頼の限界は [security.md](security.md)、CI は [testing.md](testing.md)。

## 公開の検証

最終アーカイブをクリーンなチェックアウトと実際の Git ツリーに照らし、許可リスト、生成登録、実行ファイルの一致を確かめる。Cargo、プラグイン、サーバー、ビルドマニフェストの版は一致させる。4 ターゲットが成功した後、パブリッシャーが不変のアーカイブとマニフェスト、追跡用ステータスを書き、条件付き書き込みで索引を切り替える。ステージは実行/試行ごとに create-only。受理済みの索引は不変で、再試行は新しいログと全バイトで証明する。失敗・取消・タイムアウトした暫定公開だけを、候補のログと以前のルートの全体を検証して CAS で修復できる。

非公開の `remnifold-builds` がステージ、公開の `remnifold-updates` が配信を担う。Unix アーカイブのローカル取り込みは廃止済み。`publish-r2` は `ubuntu-latest` で動き、実行/試行ごとの `RUNNER_TEMP` に状態を分ける。SHA 固定の Rust キャッシュ以外の Action は実行せず、キャッシュもリリースや R2 資格情報のあるジョブでは使わない。

## install

`install` は認証済み GitHub CLI で `release/<version>` のコミットとプラグイン版、CI のパスと ID、両リポジトリ ID、push、ブランチ、最新の成功した実行/試行を確かめる。`publish-r2` の生ログ（4 MiB 上限）から schema-2 の `REMNIFOLD_R2_MANIFEST_V2` 1 件と `REMNIFOLD_R2_ASSET_V2` 4 件を要求する。旧 schema-1 は V1 と 5 ターゲット。混在、欠落、重複、不一致は拒む。コミットステータスは証明に使わない。古いアップデーターは 4 ターゲット形式を認証できない。

R2 のオブジェクトはリダイレクトなしで取得し、ハッシュ、許可リスト、ターゲット、版、ソースを検証する。schema-2 レシートは公開と証跡の実行を別に持つ。`--verify-only` は保持した `receipts/verified-<commit>-<target>.json` を使い、ログと R2 は要らないが、GitHub の ref とソース情報は要る。ログの期限後は新規導入できない。R2 以前や無効な配信設定は拒む。

リンクや想定外のファイルを拒み、検証済みの一時兄弟フォルダーを版フォルダーへ改名してから固定実行ファイルを置き換える。完全一致は `already-current`。接続と状態、サーバーには触れず、導入後は新しいセッションを要する。

## 最初の実行ファイル

Windows の署名済みブートストラップは不変キー `velopack/remnifold-bootstrap-<version>-x86_64-pc-windows-msvc.exe` で、`release-velopack.yml` が Setup パッケージから取り出し、署名者と RFC 3161 を検証して公開する。Unix は信頼した `scripts/bootstrap-plugin.sh` が同じ証跡を検証する（既定は確認のみ、`--apply` は導入、`--download-only` は実行なし）。手順は [README](../readme/install-the-local-app.md#first-installation)、検証時の規則は [AGENTS](../agents/standalone-app-installation.md)。

Receipt schema 2 はリリーススキーマとは別形式である。既定のリポジトリは `1llum1n4t1s/remnifold` で、`--config` はフィクスチャのリポジトリと GitHub CLI を指定できる。GitHub CLI の生ログ用エスケープフラグは互換性のためヘルプから検出する。

## パッケージとフックのランチャー

`dev/` の Rust パッケージャーは、生成した MCP とフックの登録を含む許可リスト化されたアーカイブを作る。出力パスはリンクされた祖先を拒み、リポジトリ内なら `dist/` の下に置く。

Windows のパッケージは 4 フックに `commandWindows` を加え、固定の Git for Windows シェルで実行ファイルを起動する。定義は `native/src/hook_command.rs` をパッケージャーと検証で共有する。外側のコマンドは固定の 8 進数エンコードのスクリプトだけで、Git Bash が `PLUGIN_ROOT` を環境から読んで引用するため、Unicode やメタ文字を含むパスでも展開されず stdin も保たれる。アップデーターは上書きなしか完全な Windows 用一式だけを受け入れ、一部だけの一式や他ターゲットでの上書きを拒む。
