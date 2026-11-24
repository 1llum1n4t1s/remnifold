# Validation

開発と CI には Rust 1.100.0 を使い、ネイティブクレートが宣言する最低限の Rust 1.97 を維持する。コマンドは、ルートのワークスペースのロックファイルを使い、リポジトリのルートから実行する。ネイティブのツールチェーンは、固定された esbuild、ネイティブの TypeScript、workerd、Cloudflare の型を、Node.js なしで `.devtools` にダウンロードする（`--tool-root <absolute-cache>` で上書きできる）。`--work-root <absolute-output>` または `REMNIFOLD_VALIDATION_DIR` が検証の出力先を選ぶ（既定は `<OS-temp>/remnifold-validation`）。アップデーターの作業ルートは、リポジトリ、プラグインのソース、Codex のホームの外に置き、リンクされた祖先がないようにする。特定のマシンのドライブやフォルダーに依存しない。

```sh
cargo xtask prepare
cargo xtask type-check
cargo xtask native-test
cargo xtask server-test
cargo xtask client-test
cargo xtask check
git diff --check
```

ソースの `.mcp.json` は、`cwd: "."` と `./native/Cargo.toml` で Cargo を起動する。ソースのフックは、作業ディレクトリがユーザーのタスクに属するため、`${PLUGIN_ROOT}` の下で同じマニフェストを解決する。ソースから起動する前に `cargo xtask prepare` を実行して、固定されたネイティブのバンドラーを使えるようにする。プラットフォームパッケージは、Cargo の登録をネイティブ実行ファイルのコマンドに置き換える。Windows のパッケージのフックは、加えて、[DESIGN.md](../design/local-app-integration.md) に記載された固定の Git Bash の `commandWindows` ランチャーを使う。`connect claude-code` は、インストール済みの固定実行ファイルに対して同じイベントを登録する。ただし、`recallMode` が off のときは `SessionStart` と `UserPromptSubmit` を省く（そして削除する）。Setup の登録は、モードが変わったときにそれらを再同期する（`setup-registration.json` の `recall`）。フックのマッチャー、タイムアウト、非同期の挙動、ステータスメッセージを維持する。インストール済みのユーザーに Cargo は不要である。Windows のクライアント E2E は、キャプチャと想起について、パッケージされた Git Bash の経路を実行しなければならない。これには、Unicode／メタ文字を含むパス、元の標準入力、伏せ字化された失敗／終了の挙動を含む。

`check` は、型チェック、ネイティブテスト、実際の workerd を使うサーバーテスト、ネイティブのクライアント E2E、プラットフォームのパッケージング／検証、ローカルのチェックアウト検証を伴う隔離されたアップデーターテストを組み合わせる（公開は行わない）。成功を報告する前に、検証のアーティファクトと失敗を確認する。`remnifold server-live-test`（リモートのエンドポイント。明示的な依頼があるときだけ）は、通常のチェックには含まれない。

プラットフォームパッケージは `cargo xtask package --target <Rust-target>` で作る（任意で `--binary <absolute-prebuilt-executable>`、`--output <absolute-tar.gz-path>`、`--source-commit <sha>`）。既定の `--source-mode commit` は、実際の Git コミット、未追跡のソースがないクリーンなソースファイル、一致する Git blob の識別を要求する。SHA を与えるだけでは、その識別の証明にならない。対応ターゲットは、`x86_64-pc-windows-msvc`、`aarch64-apple-darwin`、`x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu` である。パッケージには、`remnifold/bin/remnifold`（Windows: `remnifold.exe`）と、ターゲット、ソースコミット、バージョン、ファイルのダイジェストを結び付ける `native-build.json` が含まれる。パッケージングを行ったチェックアウトから、`cargo xtask verify --target <Rust-target> --binary <same-absolute-executable> --archive <absolute-archive>` で検証する。未コミットまたはエクスポートしたソースのローカルチェックでは、両方のコマンドに `--source-mode checkout` を渡す（Git が使えないときは `--source-commit <sha>` も）。これは公開の準備が整ったことを決して証明しない。公開は別の操作で、バージョンタグで始まる（[Setup installer (Velopack)](setup-installer-velopack.md) を参照）。

エージェントが実行する Windows の検証では、テストを起動する前に、プロセス単位の `REMNIFOLD_KEEP_TEST_STATE=1` を設定する。各実行は、新しい UUID のディレクトリを保持し、親エージェントがそれを確認し、Prunlet で片付ける。保持されたパスを報告し、その前にローカルの Worker プロセスとファイルハンドルが閉じるのを待つ。これはエージェントの実行ポリシーであり、製品の依存関係ではない。他の実行や共有キャッシュは片付けない。
