# Configuration

## Setup の登録

`setup-registration.json` は実行ファイル、版、登録済み CLI、`recall` の on/off を記録し、`setup-registration.lock` の try-lock が二重登録を防ぐ。

- 新しいパスではログオンのエントリと見つかった全 CLI を登録する。版だけの更新ではログオンのエントリと自身の Claude Code フックだけを書き直し、MCP エントリには触れない。未登録・失敗の CLI は見つかった時点で登録する。
- 登録前に `mcp get remnifold` を問い合わせ、この実行ファイルを指すものは残し、別の場所のものだけを置き換える。Windows の導入フックは以前の登録状態を破棄し、ログオンのエントリを先に書く。
- `recallMode` が off（未設定も off）なら `SessionStart` と `UserPromptSubmit` を外し、`Stop` と `PreToolUse` は残す。モードが変わるか記録がなければ、自身のフックを持つ設定だけを CLI なしで書き直し、次のセッションから効く。静的なプラグインと strict の登録は 4 フックを保ち、実行時に off を確かめる。
- macOS と Linux は引数なしの起動で同期的に登録し、`mcp` の更新スレッドが再確認する。最小限の `PATH` に備えて `~/.local/bin`、`~/.claude/local`、`~/.npm-global/bin`、`/usr/local/bin`、Homebrew の場所も探し、CLI のフォルダーを子の `PATH` の先頭に足す。

ログオンのエントリ、プロセス検出、アンインストールの OS ごとの差は [local-runtime-and-distribution.md](local-runtime-and-distribution.md#os-ごとの登録)、想起の設定は [recall.md](recall.md)。

## Toolport 判定と `mcpRegistration`

判定は `native/src/toolport.rs`。Edictora の `crates/edictora/src/install/toolport.rs` を正本とするバイト同一の複製で、ここでは編集しない。クライアント設定（Claude Code の `.claude.json` の `mcpServers`、Codex の `config.toml` の `[mcp_servers.*]`）に実在する stdio の Toolport ゲートウェイがあり、registry（ゲートウェイの env、`TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`、既定の場所の順。既定は Windows が `%APPDATA%\Toolport\registry.json`、macOS が `~/Library/Application Support/Toolport/registry.json`、Linux が `$XDG_CONFIG_HOME/Toolport/registry.json`（絶対パスのとき。無ければ `~/.config/Toolport/registry.json`）、`version` 3）で `remnifold mcp` を起動する有効なサーバーが使用中の全プロファイルの `enabledServerIds` にあれば、その `command` が自分（直接登録に使うのと同じ実行ファイル）と同じ実体（大文字小文字・別名を `canonicalize` で正規化して比べ、失敗した側は字面のまま比べる）なら `Served`、別の実行ファイルなら `ServedElsewhere`（自分と他者が両方有効なら `Served`）。`command` が実在する絶対パスでなければ `NotConnected`（他者の実行ファイルだけが無い場合も）、Toolport 側の無効化・削除は `NotServed`、Windows・macOS・Linux 以外の OS や registry を読めないときは `Unknown`（直接登録）。サーバーの `env` と `quarantine.json` は見ない。

`mcpRegistration`（`auto` 既定 / `direct` / `off`、小文字の完全一致。不正値はログを残して auto、null・未設定は auto）。状態ファイルの `skipped` はクライアントごとに `toolport`・`toolport-foreign`・`off` のどれかを持ち、`done` の意味は変えない。`toolport-foreign` は、Toolport が別の実行ファイルで提供しているため直接登録を省いた印で、旧版に戻っても知らない理由は読み捨てられるだけで害はない。

- 新規・状態の作り直し: `off` は `skipped=off`、auto で `Served` は CLI を呼ばず `skipped=toolport`、`ServedElsewhere` は CLI を呼ばず `skipped=toolport-foreign`（`app-update.log` に警告を1行。Toolport 側の command は書かず、自分の実行ファイルのパスだけを書く）、他は直接登録。作り直し時は移動前の exe を指す既存の直接登録が残る。
- 前回未完了のクライアントも後で同じ判定をする（auto は CLI があるときだけ）。
- 版更新・通常起動（`mcp`、引数なし、ログオン時の適用）: `skipped` が `toolport` か `toolport-foreign` で auto のときは、CLI の有無に関係なく毎回判定し直す。`Served` は `toolport`、`ServedElsewhere` は `toolport-foreign`（変わるときだけ記録し、foreign にしたときだけ警告）へ記録し直し、`NotConnected` で CLI があれば直接登録して `done` に移し `skipped` から外す。`NotServed`・`Unknown` と、同じ理由のままの判定は何もしない。
- `done` は利用者が外しても足し直さない。モード変更後は `direct` が `skipped` を直接登録し、`auto` が `skipped=off` を新規扱いで判定する。
- 既存の直接登録は外さない。アンインストールは自分を指す直接登録だけを外し、Toolport の設定には書かない。
- Claude Code のフックは `Served`・`ServedElsewhere`・`off` でも、CLI か `~/.claude` があれば統合する。どちらも無ければ `skipped` も記録しない。フックの統合に失敗してやり直すときも元の理由（`toolport-foreign` を含む）を保つ。
- 初回の案内（`welcome`）は、各クライアントで `ServedElsewhere` なら `welcome.toolport_foreign`（9言語）で、Toolport 側の command をこの実行ファイルへ向け直すよう促す。
- 手動 `connect` は判定せず直接登録し、`Served`・`ServedElsewhere` なら結果に `toolport: {serverId, command, duplicate: true, sameExecutable}` を付ける（`sameExecutable` は `Served` が true、`ServedElsewhere` が false）。
