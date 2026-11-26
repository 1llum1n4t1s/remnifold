[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

リモート MCP アクセスを備えたセルフホスト型の Cloudflare メモリサーバーと、スタンドアロンのローカルアプリです。Claude Code と Codex で、プロジェクトフォルダーをまたいで有用な実装履歴を呼び出せます。

アプリは完了した作業を共有コレクションに保存し、プロジェクトフォルダーを出所情報として保持します。レコードは内容に基づくトピックで閲覧できます。「Chrome extension」のようなトピックについて尋ねると、リコールが 1 回のサーバーリクエストで複数のメモリスペースを横断して検索し、簡潔なインデックスを注入します。アシスタントは、該当するソースレコードを MCP 経由で開いてから実装を再利用できます。

## 要件

リモート MCP クライアントは、ローカルに何もインストールせずにデプロイ済みのサーバーへ接続できます。ローカルアプリは、お使いの OS と CPU 向けの単一のネイティブ実行ファイルです。その MCP サーバー、フック、接続設定、履歴のインポート、インストーラーに、Node.js、Codex、Claude Code のプラグイン機構は不要です。一般のユーザーは Setup インストーラーでインストールします。コマンドラインは不要で、自動的に最新の状態に保たれます。上級者向けの厳格な `remnifold install` 経路には、認証済みのネイティブ GitHub CLI が必要です。Claude Code への登録には、利用可能であれば Claude Code CLI を使います。利用できない場合は、実行すべきコマンドがそのまま表示されます。

サーバーのセットアップと保守にも同じネイティブ実行ファイルを使います。Workers、D1、Vectorize、Workers AI を有効にした、ご自身の Cloudflare アカウントが必要です。プラットフォーム向けパッケージの利用に、Node.js、パッケージマネージャー、Wrangler、Rust のインストールは不要です。

## リモート MCP クライアントを接続する

下記の手順でサーバーを更新して検証したあと、`https://your-worker.example.workers.dev/mcp` を Streamable HTTP の MCP エンドポイントとして使います。対応クライアントは、OAuth 2.1 のディスカバリー、公開の動的クライアント登録、PKCE S256 で認証できます。サーバーの認可ページで既存のメモリ API キーを入力し、名前付きのクライアントを承認します。完了ページが自動的にクライアントへ戻らない場合は、ページ内のリンクから続行してください。OAuth トークンは MCP でのみ有効で、既存の REST クライアントはこれまでどおり API キーを使います。設定済みの Bearer 認証情報に対応するクライアントは、メモリ API キーを直接使うこともできます。キーとトークンは非公開にしてください。

[Grok Bot の Connect apps](https://docs.x.ai/grok-bot/overview) と [Grok コネクター](https://docs.x.ai/grok/connectors) には、アプリ接続とカスタム MCP 接続の経路があります。クライアントが対応していれば OAuth を選んでください。Grok Bot で Bearer キーを手動入力できるかどうかは確認していません。

ChatGPT では、[プライベートプラグインのセットアップ](https://developers.openai.com/plugins/build/plugins)に従い、**+ Add custom MCP server** を選んで `/mcp` の URL と認証を入力し、**Create as a plugin** を選びます。このプライベート接続に、公開ディレクトリへの申請は不要です。[ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) は、対応していてインストール・有効化済みのプラグインを使います。このプラグインがご自身のアカウントの dots に表示され、動作することを確認してください。dots で任意の MCP URL を直接入力できるかどうかは確認していません。

サーバーは AI に対し、関連するコンテキストの検索、必要に応じた完全なレコードの参照、永続的な成果の保存を促します。ツールを使うかどうかはクライアントとモデルが選ぶため、自動的な検索や保存は保証されません。必要なときは、AI に検索または保存を明示的に依頼してください。このリモート連携は、ローカルの stdio サーバーやフックの代わりにはなりません。同じクライアントに、重複するリモートサーバーを登録しないでください。

## ローカルアプリをインストールする

### Setup インストーラー（推奨）

Windows x64 では、[`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe)（発行元による署名付き、管理者権限は不要）をダウンロードして実行します。後述の macOS と Linux 向け Setup パッケージは、1.0.32 の次の版から Windows 版と同時に公開されます。Setup はアプリをユーザー単位で `%LOCALAPPDATA%\RemnifoldApp` にインストールし、Claude Code と Codex に登録します（コマンドラインツールが見つかったものだけ）。そのうえで、ターミナルに初回ガイドを開き、次の手順であるサーバーへの接続（`remnifold setup`）を案内します。Windows では、Toolport ゲートウェイ経由で既に Remnifold につながっているクライアントには、直接登録を重ねて行いません（後述の「更新・検証・登録」を参照）。これらのチェックに GitHub アカウントは不要です。macOS（Apple Silicon）では、`RemnifoldApp-osx-Setup.pkg`（発行元の Developer ID で署名・公証済み）を開きます。これは `Remnifold.app` をインストールし（「自分用にインストール」を選ぶと `~/Applications` に、全ユーザー向けのインストールは `/Applications` に入ります。後者ではアップデートの適用時に管理者パスワードを求められることがあります）、アプリを一度起動して Claude Code と Codex に登録します。クライアントのコマンドラインツールを後からインストールした場合は、次回の Remnifold 起動時に登録されます。サーバーに接続するには、`"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>` を実行します（`~/Applications` にある場合はパスを読み替えてください）。Linux（x64 または ARM64）の Setup は単一の AppImage（`RemnifoldApp-linux-x64.AppImage` または `RemnifoldApp-linux-arm64.AppImage`）です。保管したい場所（たとえば `~/Applications`）に置き、実行権限を付けて引数なしで一度実行すると、その AppImage が Claude Code と Codex に登録されます。後でファイルを移動した場合は、次の起動時に新しい場所が登録されます。サーバーに接続するには、`<AppImage へのパス> setup --base-url <URL>` を実行します。AppImage にはコード署名がないため、Linux では、アップデートサイトへの HTTPS と、アップデートフィード内のチェックサムを信頼します。

アップデートは自動です。MCP セッションの実行中、アプリはアップデートフィードを 1 日に最大 1 回確認し（確認に失敗した場合も 1 日待ちます）、新しいバージョンがあればバックグラウンドでダウンロードします。作業を中断することはありません。新しいバージョンは次回のログオン時に適用されます（Windows: `Run` エントリ、macOS: LaunchAgent の `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist`、Linux: 自動起動エントリの `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop`）。ほかの Remnifold プロセスが動いていないときだけ適用されるため、開いているセッションが閉じられることはありません。適用前に、ダウンロードしたパッケージ内のすべての実行ファイルが発行元の署名であることを確認します（macOS: 発行元の Developer ID、Linux: 署名はありませんが、ダウンロード先の `/var/tmp/velopack/RemnifoldApp` が自分の所有で、ほかのユーザーが書き込めないこと）。無効にするには、アプリの状態フォルダー（`%APPDATA%\Remnifold`、`~/Library/Application Support/Remnifold`、Linux では `$XDG_STATE_HOME/remnifold`（既定は `~/.local/state/remnifold`）、または `<REMNIFOLD_HOME>/remnifold`）内の `app-update.json` に `"auto_update": false` を設定します。診断情報はその隣の `app-update.log` に書き込まれます。すでに後述の厳格インストールを使っている場合、Setup は Claude Code と Codex の登録を自分自身に切り替え、同じ接続とメモリの状態を使います。厳格インストールのファイルは残るため、不要になったら `%LOCALAPPDATA%\Programs\Remnifold` を自分で削除してください。Setup 中に登録が完了しなかった場合（たとえばクライアント CLI の応答が遅いとき）は、次回のログオン時に自動で再試行されます。アンインストールは Windows の設定から行います。Claude Code と Codex の登録およびログオンエントリは一緒に削除され、接続とメモリの状態は残ります。その後に厳格インストールへ戻す場合は、それ自身の `connect claude-code --apply` と `connect codex --apply` で登録し直します。アンインストールせずに登録だけ削除するには、`remnifold connect claude-code --remove --apply` と `remnifold connect codex --remove --apply` を実行します。

macOS にはアンインストーラーがありません。`Remnifold.app` をゴミ箱へ移す前に、登録とログオンエントリを削除してください（`--apply` を付けない場合、コマンドは削除対象を表示するだけです）。

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

`~/Library/Application Support/Remnifold` にある接続、設定、メモリの状態は残ります。ダウンロードしたアップデートのキャッシュは `~/Library/Caches/velopack/RemnifoldApp` です。

Linux にもアンインストーラーはありません。AppImage を削除する前に、同じ方法で実行してください（パスは保管場所に合わせて読み替えてください）。

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

`~/.config/remnifold` と `~/.local/state/remnifold` にある接続、設定、メモリの状態は残ります。ダウンロードしたアップデートのキャッシュ `/var/tmp/velopack/RemnifoldApp` は削除して構いません。

### 厳格インストール（上級者向け）

厳格インストールは最終版の 1.0.32 で凍結されています。新しいバージョンは上記の Setup インストーラー（Windows、macOS、Linux x64 と ARM64）だけで公開されるため、`remnifold install` でインストールできるのは 1.0.32 までです。インストール済みのバージョンと `--verify-only` は引き続き動作します。新しいバージョンへ移るには Setup インストーラーを実行してください。Claude Code と Codex への登録は Setup が引き継ぎます。

公開済みのネイティブパッケージは [Remnifold updates](https://remnifold-updates.kagayoi.com) から配布されます。バージョンインデックスは `releases/<version>/index.json` で、そのマニフェストが、下記の各ターゲットに対してハッシュ名のアーカイブを指定します。ダウンロードした実行ファイルを実行する前に、GitHub を通じて公開元を認証してください。R2 の URL やコミットステータスのリンクだけでは、出所を証明できません。

| プラットフォーム | Rust ターゲット |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Mac Intel には対応していません。

アプリは管理者権限なしで、ユーザー単位にインストールされます。

| プラットフォーム | アプリフォルダー | 固定の実行ファイル |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold`（既定は `~/.local/share/remnifold`） | `bin/remnifold` |

`remnifold install` は、何かを書き込む前に、リリースコミット、成功した CI の公開元ログ、R2 マニフェスト、アーカイブのサイズとダイジェスト、パッケージの出所を検証します。各バージョンは `versions/<version>` に 1 度だけ展開され（インストール済みのバージョンを異なるバイト列で上書きすることはありません）、後でオフライン検証できるよう `receipts/` に受領記録を残し、固定の実行ファイルを置き換えます。登録は常に固定の実行ファイルを指すため、アップデートで再登録は不要です。Windows では、実行中の MCP サーバーは再起動するまで古い実行ファイルを使い続けます。その場合、結果に `"restartRequired": true` が報告されます。

### 初回インストール

Windows では、署名済みの bootstrap 実行ファイルを、リリース用ワークフローが R2 の版付きの不変キーへ置きます。バージョン 1.0.30 以降にのみ存在し、それより前のバージョンでは、信頼できるソースから最初の実行ファイルをビルドしてください。`install` は厳格インストールのバージョンだけを受け付けるため、バージョンには 1.0.30 から 1.0.32 のいずれかを指定してください。検証から実行までの間に、ほかのユーザーがダウンロードしたファイルを差し替えられない、既存の信頼できるプライベートディレクトリを選んでください。引用符で囲まれたバージョンとパスを置き換え、PowerShell から実行します。

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

ダウンロードはリダイレクトさせず、実行前にファイルが Authenticode の検査（状態 `Valid`、署名者 `CN=Open Source Developer Yuichiro Shinozaki`、RFC 3161 タイムスタンプあり）に合格する必要があります。失敗した場合は実行しないでください。この署名が示すのは署名者だけで、製品・バージョン・公開の受理までは保証しません。`install` には引き続き認証済みのネイティブ GitHub CLI が必要で、公開の検証は `install` 自身が行います（[DESIGN.md](DESIGN.md#release-installation) を参照）。検証されていない R2 のヘルパーで代用したり、失敗した検証を回避したりしないでください。

macOS と Linux では、信頼できるソースの [Bash ブートストラップ](scripts/bootstrap-plugin.sh)を、作業ディレクトリの外に保存して取得し、内容を確認してください。ネイティブ GitHub CLI、curl、tar、`sha256sum` または `shasum` が必要です。

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

`--apply` を付けない場合は、公開内容の検証だけを行います。`--download-only` は、検証済みのアーカイブを実行せずに返します。

### 更新・検証・登録

```sh
remnifold install --version <published-version>                 # 公開内容の検証のみ
remnifold install --version <published-version> --apply         # インストールまたは更新
remnifold install --version <published-version> --verify-only   # インストール済みのバージョンをオフラインで再確認
remnifold connect claude-code                                    # 登録の予定内容を表示
remnifold connect claude-code --apply                            # MCP サーバーとフックを登録
remnifold connect claude-code --remove --apply                   # Remnifold の MCP エントリとフックだけを削除
```

アプリフォルダーにある固定の実行ファイルを使うか、その `bin` フォルダーを `PATH` に追加してください。`install` は、Windows では `%LOCALAPPDATA%\remnifold`、それ以外では `~/.cache/remnifold` の下の作業フォルダーを使います。`--work-root <absolute-path>` で、アプリ・設定・リポジトリの各フォルダーの外にある別のフォルダーを指定できます。証跡ログ（witness log）が期限切れになると、新規インストールは安全側に倒れて失敗します。保持している受領記録は、引き続き `--verify-only` に使えます。リリースのソースメタデータへの GitHub アクセスは、引き続き必要です。

`connect claude-code --apply` は、`claude mcp add --scope user remnifold -- <fixed executable> mcp` で MCP サーバーを登録し（Claude Code CLI が `PATH` にない場合はそのコマンドを表示します。`--cli <path>` で CLI を指定できます）、`SessionStart`、`UserPromptSubmit`、非同期の `Stop`、`PreToolUse` の各フックを Claude Code の `settings.json`（`$CLAUDE_CONFIG_DIR` または `~/.claude`）にマージします。`recallMode` が `off` のときは `SessionStart` と `UserPromptSubmit` を登録せず、既にある Remnifold の項目も外します。置き換えるのは既存の Remnifold のフックエントリだけで、最初の変更時に `settings.json.remnifold-backup` を残します。形式が不正な設定ファイルは変更されません。`connect <tool> --remove`（まずプレビューし、`--apply` で書き込み）は、Remnifold 自身の MCP エントリとフックだけを削除し、ほかの設定はすべて保持します。`--executable <path>` を付けると、その実行ファイルを指すエントリだけが対象になります。`connect codex --apply` は `codex mcp add` で MCP サーバーを登録します。Codex にはプラグイン以外でフックを登録する正式な方法がないため、自動のリコールとキャプチャは Claude Code を通じて動作します。`connect` は Toolport を確認せず、常に直接登録します。Toolport が既に Remnifold を提供している場合は、結果の JSON に通知として `toolport: {serverId, command, duplicate: true}` が加わります。Windows の Setup は、Claude Code または Codex の設定に Toolport ゲートウェイがあり、Toolport の registry（`%APPDATA%\Toolport\registry.json`、または `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`）に、`remnifold.exe` を `mcp` 引数で起動する有効なサーバーが、使われる全プロファイルで入っているときは、直接登録を省きます。判定できない場合は直接登録します（Windows 以外では判定せず、常に直接登録します）。`remnifold.json` の `mcpRegistration` で切り替えられ、`auto`（既定）、`direct`（常に直接登録）、`off`（自動の追加・修復をしない。アンインストール時の解除は続けます）のいずれかです。不正な値は `auto` として扱います。Setup は既存の直接登録を外さず、Setup が一度直接登録したクライアントは、利用者が外しても自動では足し直しません（戻すのは Setup が省いたクライアントだけ）。省いたあと、クライアントから Toolport ゲートウェイが消えた場合や、Toolport が指す実行ファイルが無くなった場合は、次の起動時（`mcp` の起動・引数なしの起動・ログオン時の適用）に直接登録へ戻し、Toolport 側でサーバーを無効化・削除しても何も足しません。前回未完了のクライアントも同じ判定にかけます。Claude Code のフックはどちらの場合も統合しますが、Claude Code の CLI も `~/.claude` フォルダーも無いときは作りません。直接登録だけを外すには `claude mcp remove remnifold --scope user` または `codex mcp remove remnifold` を実行します（`connect claude-code --remove --apply` はフックも外します）。`PreToolUse` のマッチャーは `^mcp__remnifold__|^mcp__toolport__toolport_call_tool$` で、`toolport_call_tool` 経由では、`name` が Toolport の registry にある Remnifold サーバーの id を Toolport のツール名の前置きに変換したもの（`[A-Za-z0-9_]` 以外は `_` に置き換え）に `__` と読み取り専用ツールを続けたものであるときだけ許可します。登録後は、新しいセッションを開始してください。

`Stop` によるキャプチャは、Codex のロールアウトに加えて Claude Code のトランスクリプトも読み取ります。保存するのは、人間のリクエストと最終的なアシスタントのテキストだけで、思考、ツール呼び出しとその結果、リクエストのコピー、ほかのセッションの行は除外します。バックグラウンド通知の後の返信は、すでに保存されたターンのキャプチャキーが変わらないよう、別の継続ターンとして保存されます。Claude Code のレコードには `sm_source: "claude-code"` が付きます。2 つのメモリシステムが併存しないよう、Claude Code 組み込みの自動メモリは `"autoMemoryEnabled": false` で無効にしてください。Remnifold はそれらのファイルを読み取ったりインポートしたりしません。このリポジトリの内部では、ソースの `.mcp.json`（`cargo run`）がユーザー登録より優先されます。インストール済みのアプリを使うには、追跡されていない `.claude/settings.local.json` に `"disabledMcpjsonServers": ["remnifold"]` を追加してください。保存済みのターンは変えずに、3種類のセッションイベントも別の記録として保存します。AI が書いた圧縮の要約、AI の作業中に送った発言、AI の質問（AskUserQuestion）への回答です。保存は `Stop` のときだけなので、圧縮の直後に検索できるのは前回の `Stop` までの記録です。

かつての Codex プラグインコマンド `plugin-update` と `plugin-bootstrap` は廃止され、`USE_REMNIFOLD_INSTALL` を返します。代わりにスタンドアロンアプリをインストールしてください。その後、古いプラグインは Codex から削除できます。

### サーバーに接続する

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

API キーがまだ渡されていない場合は、非表示のプロンプトで入力します。Setup は、接続を書き込む前に、認証済みセッションを検証します。既存の接続を更新するには `--force` を使います。関係のない設定は保持されます。`REMNIFOLD_API_URL` と `REMNIFOLD_API_KEY` で、プロセス単位の上書きができます。非対話のセットアップでは、`--api-key-stdin` が標準入力からキーを読み取ります。

接続とローカルの状態は、Codex や Claude Code とは独立した、アプリ自身の設定フォルダーに置かれます。

| プラットフォーム | 接続（`remnifold.json`）とサーバーの状態 | キャプチャとリコールの状態 |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | 同じフォルダー |
| macOS | `~/Library/Application Support/Remnifold` | 同じフォルダー |
| Linux | `$XDG_CONFIG_HOME/remnifold`（既定は `~/.config/remnifold`） | `$XDG_STATE_HOME/remnifold`（既定は `~/.local/state/remnifold`） |

`REMNIFOLD_HOME` は両方を上書きします（接続は `<REMNIFOLD_HOME>/remnifold.json`、状態は `<REMNIFOLD_HOME>/remnifold/` の下、アプリは `<REMNIFOLD_HOME>/app`）。`setup --config-dir <directory>` は、接続を別の場所に書き込みます。以前のバージョンは、これらのファイルを Codex ホーム（`CODEX_HOME`、既定は `~/.codex`）に置いていました。新しいフォルダーに接続がない場合、初回の実行時に、有効な旧来の接続をそこへコピーし、以前のキャプチャとリコールの状態を読み取ります。新しい状態は新しいフォルダーにだけ書き込みます。旧来のファイルが削除されることはありません。`server-setup` が旧来の状態を移動するのは、`--apply` を付けた場合だけです。

接続とスペースの検出を確認するには、`whoAmI` MCP ツールを実行します。

既存の Codex セッションをインポートするには、`remnifold import --limit 10` で、上限付きのバッチをプレビューします。サニタイズ済みのレコードをアップロードする場合は、`--apply` を追加します。プレビューではリモートのメモリを書き込みません。インポートを繰り返しても、決定的なキャプチャ ID が再利用されます。`--codex-home` で、Codex セッションの読み取り元を指定します。

## サーバーを作成・更新する

パッケージには Worker のバンドルとマイグレーションが含まれます。Cloudflare API トークンを、プロセス環境変数 `CLOUDFLARE_API_TOKEN` または `--token-stdin` で渡し、既存のリソースをプレビューします。

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

既定は、認証付きの読み取り専用ディスカバリーです。トークンには、アカウントの読み取り、Workers スクリプト、D1、Vectorize の権限が必要です。ドメインやルートを変更する場合は、対応する Zone、Routes、Custom Domain の権限も必要です。Setup は Wrangler の OAuth 認証情報をインポートしません。トークンとメモリ API キーは非公開にしてください。

既存のインストールでは、`--config <absolute-existing-jsonc-path>` を渡すと、設定を上書きせずに読み取ります。Setup は、リソース名を `remnifold`、`kagayoi-memory`、`cloudflare-supermemory` の順に再利用します。`--worker-name`、`--database-name`、`--vector-index-name` を明示した場合はそちらが優先されます。プランが、既存の Worker、D1 データベース、Vectorize インデックスを選択していることを確認してください。設定された ID と、ベクトルの次元数・距離指標は一致している必要があります。既存のバインディング、cron スケジュール、ルートは保持され、競合があるとセットアップは停止します。このアップグレードのために、置き換え用のサーバーを作成したり、メモリをコピーしたりしないでください。

プロビジョニングまたはデプロイを行う場合は、選んだメモリキーを `REMNIFOLD_API_KEY` で渡し、プランを適用します。

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

プレビューと同じ設定と名前を使ってください。`--apply` は、不足しているリソースを作成し、渡されたキーを Worker のシークレットとしてインストールし、追加型（additive）の D1 マイグレーションを適用し、組み込みの Worker をデプロイして、エンドポイントを確認します。既存のサーバーでは、意図せずキーを置き換えてしまわないよう、既存のキーを渡してください。キーを渡さない場合、セットアップはシークレットを保持し、認証付きの検証をスキップしたと報告します。ヘルスチェックだけでは、マイグレーションと MCP へのアクセスを検証したことにはなりません。キーを渡すと、セッションの検証、リモート MCP の初期化、`search_memory`、`getDocument`、`add_memory` の検出が有効になります。

クライアントを接続または更新する前に、サーバーの更新とマイグレーションを適用してください。Setup は再開可能な設定を、リポジトリの外、つまりアプリの設定フォルダー内の `server/config.json` に保存します（[サーバーに接続する](#サーバーに接続する)を参照）。`--state <absolute-json-path>` で、別の外部の場所を指定できます。更新のためにこの状態を保持し、生成された ID や認証情報は決してコミットせず、失敗したステップを修正したら同じコマンドを再実行してください。既存のカスタムエンドポイントには `--base-url` を、すべてのオプションには `--help` を使います。Cloudflare の利用料金は、サーバーをホストするアカウントに請求されます。

1.0.22 では、クライアントを更新する前に、そのリリースの `server-setup --apply` でサーバーを更新してください。マイグレーション `0013_memory_created_index.sql` は、日付範囲での整理のためのインデックスを追加します。保存済みのレコードや ID は変更しません。

1.0.21 では、クライアントを更新する前に、そのリリースの `server-setup --apply` でサーバーを更新してください。マイグレーション `0010_capture_variants.sql`、`0011_consolidation_recall_opt_out.sql`、`0012_organization_retry_fairness.sql` は、検証済みのキャプチャバリアントを追加し、リコール対象外のソースを含む過去のチェックポイントを無効化し、今後の旧来の先送りを 1 時間に短縮します。元のレコードと ID は保持され、影響を受けたチェックポイントは、対象となるソースから再生成できます。

## メモリの動作

- 完了したユーザーのリクエストと最終回答は、共有の `memories` コレクションに、ソースフォルダーの出所情報とともに保存されます。添付されたスキル定義とブラウザー/ページ状態のエンベロープは、保存されるリクエストのテキストから除外され、最終回答はそのまま保持されます。決定的なキャプチャ ID により、再試行時も重複を作らず、同じ保存済みレコードが再利用されます。AI の質問に入力した回答と、作業中に送った発言も保存されます。メールアドレスと電話番号は伏せ字になり、個人情報（氏名・住所・電話・メール・カード・パスワードなど）を尋ねる質問への自由入力の回答は差し替えられます。それ以外の質問への回答や途中の発言（連絡先の伏せ字も掛かりません）に個人情報を入力すると保存され、圧縮の要約も発言を再掲することがあります。圧縮の要約は AI が書いた手掛かりで、許可の根拠にはならず、自動では想起されません（`recallable: false`）が、手動の検索では見つかります。伏せ字にするのは桁構成の合う電話番号とメール（`git@host` は除く）なので、区切りなしの国内番号などは残ります。質問の語の判定は言い回しで漏れることがあり、選択肢の回答は残ります。忘却した記録は、レシートの喪失などで再送されると復活し得ます。
- `listTopics` で内容のカテゴリーを調べ、その `topic` を `listDocuments` または `listMemories` に渡します。一覧には、旧来のフォルダースペースと共有コレクションの両方にまたがるレコードが含まれます。現在のトピックラベルがないレコード（分類に失敗した、分類が無効、または意図的にスキップされたレコードを含む）を見るには、`topic: "__unclassified__"` を使います。
- 手動の `add_memory` は共有コレクションに保存します。読んだレコードを忘れさせるには、`add_memory` を `action: "forget"`、その `documentId`、ソースの `containerTag` で呼び出します。互換性のため、保存済みの内容そのものを指定する方法も引き続き使えます。Codex プラグインでは、絶対パスの `sourceFolder`、または MCP クライアントが渡す単一のワークスペースルートが出所情報を追加します。リモート MCP ツールは、代わりに明示的な `projectId` を受け付けます。明示的な `containerTag` は保存先を上書きします。プラグインが、自身のインストールフォルダーをユーザーのプロジェクトとして使うことはありません。`listSpaces` は、互換性と診断のために物理スペースを公開します。
- プロンプトのリコールは、トピックごとにスペースを横断して検索します。短いフォローアップでは、同じセッション内の以前のユーザーメッセージ（大きなツール出力より前のものを含む）からトピックを復元できます。履歴の復元には上限があり、具体的なトピックがなければ、トピック検索は実行されません。自動リコールは、キャプチャされた各ターンと各パートの利用可能な最新リビジョンを選択しますが、手動検索は過去のリビジョンも保持します。グローバル検索は従来の検出上限を超えたスペースも含みます。古いサーバーは 100 スペースを上限とする互換パスを使い、検出が不完全な場合はそれを報告します。部分的な検索では、利用できないスペースとドキュメントインデックスを報告しつつ、成功した結果は保持します。最初のセッションイベントは、リコール状態をリセットするだけです。既定（`recallScope` が `session`）では、自動リコールは今のセッションの記録だけを注入します。トピックで検索し、結果の件数上限を適用する前に、保存された `sessionId` がフックの `session_id` と一致する項目だけを残します。セッション ID のない記録や、セッション ID のないプロンプトでは何も注入しません。他のセッションの最近のタスクは取得しないため、`maxRecentTasks` は効果を持ちません。他のセッションを探すときは、手動の `search_memory` ツールを使います。`sessionId` を渡すとそのセッションだけに絞れます。`"current"` は今のセッションを指し、Claude Code でのみ使えます（`PreToolUse` フックが、直接登録した `mcp__remnifold__search_memory` と、Toolport ゲートウェイの `toolport_call_tool` 経由の `search_memory` に対して解決します。他のゲートウェイや `toolport_run_script` 内の呼び出しではセッション ID を明示してください。Codex は対象外です）。`includeProfile` が true でなければ、結果にコンテナー一覧は含まれません。`remnifold.json` の `recallScope` を `all` にすると従来の挙動に戻り、すべてのセッションを対象にしたトピックリコールに加えて、他のセッションで最近完了したタスクが最大 2 件、出所のプロジェクト、日付、ドキュメント ID とともに、関連性を示唆せず過去の作業を示唆するものとして別枠で表示されます。`all` のときは `maxRecentTasks` に 0 から 3 の整数を設定でき、0 にすると最近の作業のコンテキストは無効になります。この一覧モードがない古いサーバーでも、トピックリコールは引き続き提供されます。`recallScope` に他の値を設定した場合は `session` として扱います。`remnifold.json` の `recallMode` を `off` にすると、自動リコールを完全に止められます。`recallScope` に関係なく、索引の注入もリコール用の検索も行いませんが、各ターンの保存と、`search_memory` などの手動ツールは引き続き使えます。`off` のときは、`connect claude-code` と Setup 版の登録も、Claude Code の `settings.json` に Remnifold の `SessionStart`・`UserPromptSubmit` フックを登録せず、既にあるものは外します。保存の `Stop` フックと読み取り許可の `PreToolUse` フックは残ります。`recallMode` を変えた後は、Setup 版なら次の `mcp` の起動・引数なしの起動・ログオンのときにフックを張り直し、Claude Code ではその次のセッションから反映されます。既存の導入も同じ仕組みで移行し、`recallMode` が `off` なら、この版への最初の更新または `mcp` の起動のときに想起用のフックを外します。strict の `install` 経路（1.0.32 で凍結）で登録したものは4つのフックのまま残り、`off` では想起用のフックがすぐに戻ります。現在の版の実行ファイルで `connect claude-code --apply` を実行すると、同じ規則で登録し直せます。プラグインパッケージのフックは固定のため、何もせずに戻ります。既定は `off` で、`recallMode` を `direct` にするまで自動想起は動きません。空でないそれ以外の値も `direct` として扱います。 記録は、セッション ID に加えてクライアント種別（記録の `sm_source` に保存される Codex または Claude Code）でも照合します。そのため、別のクライアントの同じ ID の記録は注入されません。現在のクライアントを判別できるとき、種別が保存されていない記録も注入されません。 Claude Code では、再開によって transcript にコピーされた旧 session ID（transcript ファイルの先頭から読み、今の ID を含めて最大 8 件）も同じセッションとして扱い、再開前の記録も注入します。ただし、その ID でコピーされた最後の行の時刻以前の元ターンだけで、フォーク後や並行して元のセッションが保存した記録は注入されません。時刻のない記録も除外します。Codex は今の ID だけで照合します。 サーバーの migration `0015_memory_search_projection_session.sql` は、このセッション絞り込みを速く保つ索引を追加します。未適用でも同じ記録が返り、遅くなるだけです。
- 手動検索は、保存されたプロジェクト名にも一致します。プロジェクト名だけの検索は、一致するソースプロジェクトと新しいソースレコードを優先します。プロジェクトとトピックを組み合わせた検索では、引き続きトピックの網羅性が優先されます。
- 自動コンテキストには、タイトル、説明、セクション名、タイムスタンプ、ドキュメント ID が含まれます。一致する短い元の文章は、インデックスにない詳細を示せますが、これはあくまで過去のソーステキストであり、再利用する前に確認が必要です。Codex は、必要に応じて `getDocument` で完全なレコードを開きます。
- 注入される `<remnifold-index>` ブロック、検索結果、リコール通知の固定文言は、それらを書き出す環境の言語に従います。`REMNIFOLD_LANGUAGE`、次に `LC_ALL`、`LC_MESSAGES`、`LANG`、その次に OS の表示言語（Windows のユーザー UI 言語、macOS の `AppleLanguages`）の順です。日本語環境では日本語の文言に、それ以外では英語の文言になります。保存されたタイトル、説明、抜粋は、保存されたままの言語で表示され、翻訳されません。
- トピックリコールの既定は、最大 3 件のレコードです。`remnifold.json` の `maxMemories` に 1 から 5 の整数を設定すると、自動リコールと手動検索の上限を変更できます。接続を更新する際、setup は既存の値を保持します。自動コンテキストも 3,000 UTF-16 コード単位に制限されるため、収まるレコードがそれより少ないこともあります。
- 自動コンテキストは、最近のタスクをトピックインデックスとは別に追跡し、セッションとトランスクリプトの情報が利用できる場合、以降のプロンプトでは変更のないエントリを省きます。更新されたレコードは再び表示されることがあり、コンパクション（圧縮）やコンテキストのクリアで、重複の抑制はリセットされます。ローカルのリコール状態は、会話テキストをコピーせずに、選択した Codex ホームの `remnifold/recall-state/` の下に保存されます。状態を利用できない場合も、通常のリコールは有効なままです。クエリの照合と状態の詳細は [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall) に記載されています。
- サーバーは毎日 03:00 JST にプロジェクトを確認します。プロジェクトに未統合のレコードが 20 件以上ある場合、または未統合のレコードがあり、前回の成功したチェックポイントから 3 日が経過した場合（最初のチェックポイントの前は、最も古い保留中のレコードから起算）に、Workers AI に統合チェックポイントの作成を依頼します。プロジェクトで最初に実行時期が来たときは、サーバーが、既存のバックログ全体を、上限付きの AI バッチに分けて自動的に消化するため、古いレコードのために手動のバックフィルコマンドは不要です。元のレコードは保持され、チェックポイントからリンクされます。古いチェックポイントは置き換えられ、ソースの変更や忘却は、それに依存していたチェックポイントを無効にします。
- `consolidate_memory` MCP ツールを使うと、毎日のチェックを待たずに、プロジェクトのチェックポイントを 1 バッチ分すぐに進められます。リモート MCP では、保存された出所 ID である `projectId` が必要です。Codex プラグインは、ワークスペースルートがちょうど 1 つの場合は現在のプロジェクトを推測でき、`sourceFolder` または `projectId` を明示することもできます。1 回の呼び出しで処理するのは、新しいレコード最大 40 件です。バックログがそれより多い場合は、繰り返し実行してください。手動実行でも、未統合のソースレコードが少なくとも 1 件は必要です。
- 自動リコールは、トピックの関連度が同じなら現在のチェックポイントを優先し、選択されたチェックポイントがすでにカバーしている元のレコードの繰り返しを避けます。チェックポイントにない具体的な一致の詳細は、引き続き元のレコードを表示します。手動検索は、引き続き元のレコードを表示します。
- チェックポイントの内容とメタデータは直接編集できず、更新は HTTP 409 を返します。代わりに元のレコードを編集して、チェックポイントを再生成してください。再エンリッチメントと削除は引き続き利用できます。
- キャプチャは、設定されたメモリ API キーと一般的なシークレットのパターンを伏せ字にし、確認応答のないレコードを再試行します。ローカルのキャプチャ状態は、アプリの状態フォルダー内の `capture-state/` の下に保存されます。Codex ホームにある以前のバージョンの受領記録も、引き続き読み取れます。

キャプチャの失敗には、例外のテキストや認証情報を出力せず、固定の `stage`、`cause`、`action` コードが含まれます。`stage=receipt; cause=invalid` のような `action=repair-required` の結果は、対応するローカルのキャプチャ状態を修復する必要があることを示し、再試行だけでは失敗し続けます。`action=retry-or-repair` は通常の再試行動作を維持しつつ、接続や応答の問題が続く場合の調査を可能にします。API への到達性を別途確認するには、展開したプラグインフォルダーから次を実行します。

```sh
remnifold status
```

トピックは、既存の Workers AI によるエンリッチメントの工程で、レコードの内容から導出されます。分類は非同期です。実質的な結果を伴わない簡単な了承のメッセージや、それに類する情報量の少ないレコードは、手動検索では引き続き利用できますが、自動リコールとエンリッチメントからは除外されます。明示的なトピックラベルは引き続き保持されます。フォルダー名は、ソースのメタデータとして保持されます。既存のレコードは、元のドキュメント ID と保存場所を保ち、同じトピックブラウザーに表示されます。

トピックの閲覧や統合を使うクライアントをインストールする前に、サーバーを更新し、そのマイグレーションを適用してください。古いレコードを分類するには、既存のメモリ接続を使い、プラットフォームの実行ファイルで、上限付きのバッチをプレビューします。

```sh
remnifold topics-setup --limit 10
```

既定のバッチは 10 件で、`--limit` は 1 から 50 を受け付けます。リモートのエンリッチメントを意図する場合は、`--apply` を追加して、その有限のバッチをキューに入れます。これは既存の Workers AI の課金を使い、導出された AI/ベクトル/ファクトのエンリッチメントを更新し、影響を受けるプロジェクトのチェックポイントを無効にしますが、オプトアウトとリビジョンガードは保持されます。結果が報告するのは受理されたジョブであり、分類の完了ではありません。処理が終わるのを待ってから次のバッチを選び、`getDocument` と `listTopics` で結果を確認してください。

API キーは、1 つのサーバーインストール内のレコードへのアクセスを許可します。信頼境界が異なる場合は、別々のインストールを使ってください。伏せ字処理は、意図しないシークレットのキャプチャを減らしますが、ご自身のバックエンドに保存するものは、ご自身で確認してください。

## メモリの整理

アプリをインストールし、[現在のサーバーマイグレーション](#サーバーを作成更新する)を適用したあと（整理用スキーマの `0009_memory_organization.sql` と、範囲指定の整理用インデックスの `0013_memory_created_index.sql` を含みます）、[organize-memory スキル](skills/organize-memory/SKILL.md)で、必要に応じてメモリを整理できます。たとえば `$organize-memory 7` や「直近 7 日間のメモリを整理して」のように使います。日数（1 から 3650）は、サーバー時刻で直近 N 日間に作成されたメモリを選択します。実行は、その範囲に実行すべき作業がなくなるか、任意のバッチ上限（既定 3、最大 20）に達するまで、バッチを繰り返します。日数を指定しない場合は、グローバルキューから 1 バッチを取ります。整理は、もう Codex のスケジュールタスクとして登録されません。[setup-remnifold](skills/setup-remnifold/SKILL.md) は、以前のバージョンが残したタスクを一時停止するだけです。

各実行は、現在インストールされている実行ファイルを使います。1 バッチで処理するのは、対象レコード最大 20 件と、48,000 UTF-16 のソース単位までで、部分的な処理や先送りした作業も含みます。これらは、完了したレコード数や、読み取り量・トークンの総量の保証ではありません。候補の要約と、的を絞った根拠の読み取りは追加で行われます。検索はバッチあたり 2 回までです。未解決の作業は、大規模な探索へ広げず、先送りされます。上限で止まった範囲指定の実行は、もう一度実行するだけで続けられます。

CLI を直接使う場合は、インストール済みの実行ファイルを実行します。

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

これらのコマンドは、既存のオーナー API キーによる接続を使います。リモート MCP の OAuth 認証情報では、整理を承認できません。Codex のスケジュール登録も、サーバーのデプロイやマイグレーションも行いません。`prepare` は 30 分間のリースを作成します。`--days N` は、グローバルの検出キューを動かさずに、直近 N 日間に作成されたメモリへ範囲を絞ります。範囲指定の各レスポンスは `range.cursor` と `range.exhausted` を返し、次の呼び出しでは、null でないカーソルを `--range-cursor <cursor>` として渡します。古いサーバーは、これをフォールバックせずに拒否します。`prepare` は、ソースの ID、リビジョン、チャンク、継続メモ、要約の候補を返します。これは読み取り専用の接続確認ではありません。`lookup` は、標準入力から `{"query":"a concise topic"}` を読み取ります。`commit` は、`batchId`、`summaries`、`coverage` を含む JSON オブジェクトを読み取ります。実行スキルが定める正確なフィールド制限、分割ルール、複数パートの `groupProgress` の手順に従ってください。`complete` を宣言するのは、すべてのチャンクと重要なトピックが表現され、結論、理由、根拠、不確実性が保たれている場合だけです。それができない場合は、部分的な継続または先送りを使います。この判断は Codex が行います。コミットの受領記録を確認し、公開された要約と未完了の進捗を区別してください。

`prepare` の前に、安全な標準入力の経路と、その完全なレスポンスを保持してページ送りする方法を用意してください。Windows では、リースを取得する前に、実行スキルのローカルな疑似 PTY プローブに従います。すべてのソースチャンク、継続メモ、要約候補を、同じ保持済みレスポンスから読み取ります。表示が切り捨てられた場合は、別のバッチを準備するのではなく、その呼び出しから復元します。コミットの前に、正確な JSON をメモリ上で検証してください。すべての coverage エントリには、準備された ID とリビジョン、および明示的な `summaries` 配列が必要で、部分的または先送りのエントリには `summaries:[]` を指定します。ページ送り、事前チェック、中断された実行からの復旧については、[実行スキル](skills/organize-memory/SKILL.md#one-batch)に従ってください。

JSON の標準入力には、30 秒の期限と、エスケープを含めて 128,000 バイトの UTF-8 という上限があります。`--line` は、1 行の簡潔な JSON と、末尾の改行を必要とします。付けない場合は、オブジェクトの後で標準入力を閉じます。エコーしない入力経路を使い、生成されたテキストをシェルの引数やコマンドログに残さないでください。未対応のサーバーは、保存・忘却のフォールバックなしで、明示的に停止します。

Windows の PTY 入力では、専用の PTY でネイティブ実行ファイルを直接起動し、`memory-organize lookup --line --private-stdin` または `memory-organize commit --line --private-stdin` を指定します。stderr に `REMNIFOLD_STDIN_READY` が出るのを待ってから、1 行の簡潔な JSON と最後の改行をちょうど 1 回だけ送り、それ以降はバイトを送らないでください。端末による変換の余裕を残すため、ペイロードと改行の合計は 127,000 UTF-8 バイト以下にしてください。プライベートモードは、エコーと行編集を無効にし、終端文字の後で元のコンソールモードに戻します。準備完了の合図がない場合や、いずれかのステップが失敗した場合は、入力の送信を止め、その PTY を閉じてください。再利用したり、入力が残っている可能性があるうちにエコーを復元したりしないでください。共有コンソールも Git Bash のラッパーも使わないでください。リダイレクトされた標準入力と Windows 以外のホストは `--private-stdin` を拒否します。macOS/Linux では、エコーしないパイプで、行または EOF による入力を使います。入力手順の全体は、[実行スキル](skills/organize-memory/SKILL.md#safe-stdin-and-failures)を参照してください。

元のレコードが削除されることはありません。完全に表現された元のレコードが通常の検索とトピックリコールから外れるのは、その要約が有効な間だけです。元の ID と本文は直接取得でき、最近のタスクの把握に使う対象条件は従来のままです。競合するキャプチャは、サーバーが、別個の単一パート v2 キャプチャキーと正確な本文ハッシュを独立した根拠として検証しない限り、可視のまま先送りされます。この検証は、どのバージョンが最新かを確定するものではありません。解消されていない矛盾は、要約に残す必要があります。通常の先送りは 1 時間後に対象に戻り、解消されていないキャプチャの競合は、自動の再試行を引き続きブロックします。上限付きのキューは、古い実行待ちの作業、継続、手つかずのレコードの間で進捗を分け合うため、1 回の実行でアーカイブ全体の完了が確定するわけではありません。変更や忘却は、依存する要約を無効にし、残っている元のレコードを再び表示します。古いプロジェクトチェックポイントは、レコード全体として処理されるまで、情報を表示し続けることがあります。AI が有効な場合、すでにセマンティック検索を支えている元のレコードは、検証済みの要約の埋め込み/ベクトルが揃うまで待ってから非表示になります。AI が無効な場合は、字句検索のカバレッジで足ります。AI を有効にすると、セマンティックな代替が整うまで、元のレコードが再び表示されることがあります。ベクトルの復旧は非同期で、15 分間隔の cron は完了期限ではありません。要約の埋め込みは既存の Workers AI の課金を使い、文章は Codex が生成します。

`sources` は読み取り専用で、記録されたソース ID、スペース、日付を最大 50 件ずつページ送りします。本文は getDocument で取得し、チェックポイント自身のソースプレビュー/カーソルをたどって、記録されたメンバーに到達します。別々の出所の経路が、同じ ID を繰り返すことがあります。現在のネイティブおよびリモート MCP の getDocument レスポンスには、ソースプレビューと、任意の `sourceCursor` によるページ送りも含まれます。古いネイティブプラグインでそれらのフィールドがない場合は、CLI を使えます。親が変更されると、カーソルは競合で停止します。[整理の設計](DESIGN.md#memory-organization)に、カバレッジ、無効化、ベクトルの復旧が詳しく記載されています。

## 1.x の名称からのアップグレード

インストール済みの `kagayoi-memory`、1.x の `cloudflare-supermemory`、または `remnifold` の Codex プラグインを削除し、スタンドアロンアプリをインストールしてください。クライアントは、設定フォルダー、次に以前の Codex ホームで、`remnifold.json`、次に `kagayoi-memory.json`、`supermemory.json` の順に読み取ります。通常の環境変数は `REMNIFOLD_API_URL` と `REMNIFOLD_API_KEY` です。以前の `KAGAYOI_MEMORY_*`、`SUPERMEMORY_API_URL`、`SUPERMEMORY_CODEX_API_KEY`、`CLOUDFLARE_MEMORY_API_KEY` の読み取りは、互換性のために残っています。`remnifold setup` を実行すると、以前のファイルを保持したまま、通常の `remnifold.json` を書き込みます。既存のキャプチャ受領記録とリコール状態は、以前の製品フォルダーから引き続き読み取れます。サーバーのセットアップは、`remnifold`、次に `kagayoi-memory`、`cloudflare-supermemory` という名前のリソースを再利用します。明示したリソース名が常に優先されます。ローカルアプリを変更しても、Worker はデプロイされず、リモートデータベースも移動しません。既存のドキュメント ID、キャプチャ ID、プロジェクトの出所情報、永続化された `sm_*` メタデータは変わりません。ストレージ互換性の詳細は [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance) にあります。

Remnifold は独自のセルフホスト API を実装しています。ホスト型の Supermemory サービスとは独立しており、Codex に組み込まれたローカルのメモリファイルを読み取ったり変更したりしません。

アーキテクチャとリポジトリ構成は、リポジトリの [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md) を参照してください。コントリビューター向けの制約、検証、パッケージングのコマンドは [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md) にあります。

[MIT License](LICENSE) の下でライセンスされています。
