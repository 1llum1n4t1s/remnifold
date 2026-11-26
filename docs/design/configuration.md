# Configuration

## Setup の登録

`setup-registration.json` は実行ファイル、版、登録済み CLI、`recall` の on/off を記録し、`setup-registration.lock` の try-lock が二重登録を防ぐ。フックは登録しない。

- 新しいパスではログオンのエントリと、見つかった全 CLI を登録する。版だけの更新では、ログオンのエントリと Remnifold 自身の Claude Code フックだけを書き直し、動作中の MCP エントリには触れない。なかった CLI や失敗した CLI は見つかった時点で登録する。
- 登録前に `mcp get remnifold` を問い合わせ、この実行ファイルを指すものは残し、別の場所のものだけを置き換える。Windows の導入フックは以前の登録状態を破棄し、ログオンのエントリを先に書く。
- `recallMode` が off（未設定も off）なら `SessionStart` と `UserPromptSubmit` を外し、`Stop` と `PreToolUse` は残す。モードが変わるか記録がなければ、この実行ファイルを指す自身のフックを持つ設定だけを CLI なしで書き直す。反映は次のセッションから。静的なプラグインと strict の登録は 4 フックを保ち、実行時の off 確認に頼る。
- macOS と Linux には Velopack のフックもアンインストーラーもない。引数なしの起動で同期的に登録し、`mcp` の更新スレッドが再確認する。最小限の `PATH` に備えて、`~/.local/bin`、`~/.claude/local`、`~/.npm-global/bin`、`/usr/local/bin` と Homebrew の場所（macOS `/opt/homebrew/bin`、Linux `/home/linuxbrew/.linuxbrew/bin`）も探し、CLI のフォルダーを子の `PATH` の先頭に足す。

## Toolport 判定と `mcpRegistration`

判定は `native/src/toolport.rs`。これは Edictora の `crates/edictora/src/install/toolport.rs` を正本とする、バイト単位で同一の複製（Plumivox も同じ複製）で、ここでは編集しない。直すときは正本を直して複製し直す。複製先でビルド・lint が通らなければ止めて正本を見直す。製品ごとの違い（`binary_stem`・`mode_arg`）は呼び出し側が渡す。クライアントごとに、(a) クライアント設定（Claude Code は `<CLAUDE_CONFIG_DIR>/.claude.json` またはホームの `.claude.json` の最上位 `mcpServers`、Codex は `<CODEX_HOME>/config.toml` または `~/.codex/config.toml` の `[mcp_servers.*]`）に実在する Toolport ゲートウェイ（stdio、`toolport-gateway` 系のファイル名か registry の `clientManagedEntries` と一致）があり、(b) Toolport registry（クライアント側ゲートウェイの env、プロセス環境の `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`、Windows 既定 `%APPDATA%\Toolport\registry.json` の順。`version` は 3）に、`remnifold` を `mcp` 引数で起動する有効な stdio サーバーがあり、使われる全プロファイルの `enabledServerIds` に入っているとき `Served` とする。結果は `Served` / `NotConnected` / `NotServed` / `Unknown`。Windows 以外では何も読まずに `Unknown` とし、判定せず直接登録する。registry が無い・読めない・形式が想定外のときも `Unknown`（直接登録）。条件を満たすサーバーがあっても `command` の実行ファイルが実在しない（または絶対パスでない）ときは、Toolport が提供できない状態なので `NotConnected` として扱い、直接登録へ戻す。Toolport 側での無効化・削除（`NotServed`）とは区別する。サーバーの `env` は読まず、`quarantine.json` は見ない。

`mcpRegistration`（`remnifold.json`、`auto` 既定 / `direct` / `off`。小文字の完全一致のみで、`"Off"`・前後に空白があるものなどの不正値は auto 扱いでログ。null・未設定は警告なしで auto）。状態ファイルに `skipped`（`claude`/`codex` ごとに `toolport` か `off`）を持ち、`done`（直接登録済み）の意味は変えない。

- 新しいパス・状態の作り直し: `off` は `skipped=off`、auto で `Served` は CLI を呼ばず `skipped=toolport`、それ以外は直接登録する。制約: 状態を作り直すとき（初回・exe の場所の変更・状態ファイルなし）に Toolport が提供中なら CLI を呼ばないため、移動前の exe を指す既存の直接登録は付け替えられずに残る（既存の直接登録は外さない方針）。必要なら `claude mcp remove remnifold --scope user` で外す。
- 前回未完了（`done` にも `skipped` にも無い。CLI が無かった・失敗した）のクライアント: 後で処理するときも判定する。auto は CLI があるときだけ判定し、`Served` なら `skipped=toolport`、それ以外は直接登録する。`direct` は判定せず直接登録、`off` は `skipped=off`。
- 版だけの更新・通常起動: `done` には触れず、`skipped=toolport` かつ auto のときだけ判定し直し、`NotConnected`（ゲートウェイが消えた、または Toolport が指す実行ファイルが無い）なら直接登録する。契機は `mcp` の起動・引数なしの起動・ログオン時の適用（`native/src/app_update/registration.rs`）で、「次の更新」ではない。`NotServed`（Toolport 側で無効化・削除）と `Unknown` は何もしない。
- Setup が一度直接登録したクライアント（`done`）は、利用者が外しても自動では足し直さない。戻すのは Setup が省いたクライアント（`skipped`）だけ。
- モード変更後: `direct` は `skipped` を直接登録し、`auto` は `skipped=off` を新規扱いで判定し、`off` は何もしない。
- 既存の直接登録は外さない。アンインストールは従来どおり自分を指す直接登録だけを外し、Toolport の設定には書かない。
- Claude Code のフックは、`Served`・`off` でも統合する。ただし Claude Code の CLI があるか `~/.claude` フォルダーがあるときだけで、どちらも無ければフックも作らず `skipped` も記録しない（次回また判断する）。版更新でのフックの張り直し条件は `done` と `skipped` の和集合に `claude` が含まれるときで、`state.recall` もフックだけ統合した場合に記録する。
- 手動 `connect` は判定せず直接登録し、`Served` なら結果 JSON に `toolport: {serverId, command, duplicate: true}` を付ける（状態ファイルは変えない）。
- 直接登録だけを外すには `claude mcp remove remnifold --scope user`（`connect claude-code --remove --apply` はフックも外す）か `codex mcp remove remnifold` を使う。

## OS ごとの設定

| OS | ログオンのエントリ | プロセス検出 |
| --- | --- | --- |
| Windows | HKCU `Run` の `<packId> Update`、`conhost.exe --headless` | 導入フォルダーの実行ファイル |
| macOS | LaunchAgent `com.kagayoi.<packId>.update.plist` | バンドルに対する `proc_listallpids`/`proc_pidpath` |
| Linux | XDG autostart の `com.kagayoi.<packId>.update.desktop` | `/proc/<pid>/environ` の `APPIMAGE` |

別の導入を指す同名のエントリやリンクされたファイルは変更しない。設定済みの `REMNIFOLD_HOME` は引き継ぐ。

- macOS の plist は `RunAtLoad`、`KeepAlive` なし。`AbandonProcessGroup` がないと launchd が入れ替え前に `UpdateMac` を終了させる。bootstrap しないので初回は次のログイン。`.pkg` の postinstall はアプリを一度開くが `VELOPACK_FIRSTRUN` は渡らないことがある。更新の置き場は `~/Library/Caches/velopack/<packId>/packages`。
- Linux は起動ごとのマウントではなく AppImage 自体を登録し、移動は新しいパスとして扱う。`Exec` は Desktop Entry の規則で引用し、改行を拒む。所有は `X-Remnifold-Executable=<path>` の完全一致。検出では呼び出し元、祖先、AppImage ランタイムを除く。継承した子 CLI も使用中に数え、他ユーザーは読めないので飛ばす。
- `uninstall-setup --apply` は自身を指す登録、ログオンのエントリ、状態を除く（既定は計画の表示）。Windows のアンインストールフックも自身の登録だけを除く。CLI の失敗は、エントリがなければ不在とみなす。`connect <tool> --remove` は他の設定を保つ。

想起の設定は [recall.md](recall.md) を参照。

## Linux の詳細

autostart は `Terminal=false`、`NoDisplay=true`、`X-GNOME-Autostart-enabled=true`。`XDG_CONFIG_HOME` が未設定か相対なら `~/.config` を使い、上書き時は `/usr/bin/env "REMNIFOLD_HOME=…"` を前置する。引数は引用し、`"`、バッククォート、`$`、`\` をエスケープして、文字列の `\` と `%` は二重にする。`PPid` で祖先を、`/proc/<pid>/exe` が AppImage 自体かでランタイムを除外する。`UpdateNix` は展開後に `mv -f`（`pkexec` へのフォールバックあり）で置き換える。

## 抽出の拾い直しの調整

動作は [vector-reconciliation.md](vector-reconciliation.md#抽出の拾い直し)。平均約20 Neurons/件で、上限48件/日なら約960/日、長い応答が多ければ約1,700〜2,750/日（無料枠10,000/日）。`server/src/index.ts` の定数は `EXTRACTION_RETRY_INTERVAL_MINUTES`（30。60なら24件/日）、`EXTRACTION_RETRY_BUSY_DAY_THRESHOLD`、`EXTRACTION_RETRY_MAX_ATTEMPTS`、`EXTRACTION_RETRY_TIMEOUT_MS`。

任意の `EXTRACTION_BACKLOG_UNTIL`（ISO 8601 UTC）は期限付きの一括モード。未設定・空・不正・過去、期限の7日前より早い間は通常モード。15分枠ごとに最大20件を同時4件で処理し、忙しい日の判定を飛ばす。cron 開始から8分で新しい行を始めず、失敗しても残りを続け、最初の例外を最後に再送出する。最大80件/時（約1,600 Neurons/時）なので期限を短くし、`wrangler*.jsonc` に常設せず、過ぎたら外す。
