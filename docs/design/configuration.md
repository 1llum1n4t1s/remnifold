# Configuration

## Setup の登録

`setup-registration.json` は実行ファイル、版、登録済み CLI、`recall` の on/off を記録し、`setup-registration.lock` の try-lock が二重登録を防ぐ。

- 新しいパスではログオンのエントリと見つかった全 CLI を登録する。版だけの更新ではログオンのエントリと自身の Claude Code フックだけを書き直し、MCP エントリには触れない。未登録・失敗の CLI は見つかった時点で登録する。
- 登録前に `mcp get remnifold` を問い合わせ、この実行ファイルを指すものは残し、別の場所のものだけを置き換える。Windows の導入フックは以前の登録状態を破棄し、ログオンのエントリを先に書く。
- `recallMode` が off（未設定も off）なら `SessionStart` と `UserPromptSubmit` を外し、`Stop` と `PreToolUse` は残す。モードが変わるか記録がなければ、自身のフックを持つ設定だけを CLI なしで書き直し、次のセッションから効く。静的なプラグインと strict の登録は 4 フックを保ち、実行時に off を確かめる。
- macOS と Linux は引数なしの起動で同期的に登録し、`mcp` の更新スレッドが再確認する。最小限の `PATH` に備えて `~/.local/bin`、`~/.claude/local`、`~/.npm-global/bin`、`/usr/local/bin`、Homebrew の場所も探し、CLI のフォルダーを子の `PATH` の先頭に足す。

ログオンのエントリ、プロセス検出、アンインストールの OS ごとの差は [local-runtime-and-distribution.md](local-runtime-and-distribution.md#os-ごとの登録)、想起の設定は [recall.md](recall.md)。

## Toolport 判定と `mcpRegistration`

判定は `native/src/toolport.rs`。Edictora の `crates/edictora/src/install/toolport.rs` を正本とするバイト同一の複製で、ここでは編集しない。クライアント設定（Claude Code の `.claude.json` の `mcpServers`、Codex の `config.toml` の `[mcp_servers.*]`）に実在する stdio の Toolport ゲートウェイがあり、registry（ゲートウェイの env、`TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`、`%APPDATA%\Toolport\registry.json` の順、`version` 3）で `remnifold mcp` を起動する有効なサーバーが使用中の全プロファイルの `enabledServerIds` にあれば `Served`。`command` が実在する絶対パスでなければ `NotConnected`、Toolport 側の無効化・削除は `NotServed`、Windows 以外や registry を読めないときは `Unknown`（直接登録）。サーバーの `env` と `quarantine.json` は見ない。

`mcpRegistration`（`auto` 既定 / `direct` / `off`、小文字の完全一致。不正値はログを残して auto、null・未設定は auto）。状態ファイルの `skipped` はクライアントごとに `toolport` か `off` を持ち、`done` の意味は変えない。

- 新規・状態の作り直し: `off` は `skipped=off`、auto で `Served` は CLI を呼ばず `skipped=toolport`、他は直接登録。作り直し時は移動前の exe を指す既存の直接登録が残る。
- 前回未完了のクライアントも後で同じ判定をする（auto は CLI があるときだけ）。
- 版更新・通常起動（`mcp`、引数なし、ログオン時の適用）: `skipped=toolport` かつ auto だけを再判定し、`NotConnected` なら直接登録する。`NotServed`・`Unknown` は何もしない。
- `done` は利用者が外しても足し直さない。モード変更後は `direct` が `skipped` を直接登録し、`auto` が `skipped=off` を新規扱いで判定する。
- 既存の直接登録は外さない。アンインストールは自分を指す直接登録だけを外し、Toolport の設定には書かない。
- Claude Code のフックは `Served`・`off` でも、CLI か `~/.claude` があれば統合する。どちらも無ければ `skipped` も記録しない。
- 手動 `connect` は判定せず直接登録し、`Served` なら結果に `toolport: {serverId, command, duplicate: true}` を付ける。
