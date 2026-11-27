# Configuration

## Setup の登録

`setup-registration.json` は実行ファイル、版、登録済み CLI、`recall` の on/off を記録し、`setup-registration.lock` の try-lock が二重登録を防ぐ。

- 新しいパスではログオンのエントリと見つかった全 CLI を登録する。版だけの更新ではログオンのエントリと自身の Claude Code フックだけを書き直し、MCP エントリには触れない。未登録・失敗の CLI は見つかった時点で登録する。
- 登録前に `mcp get remnifold` を問い合わせ、この実行ファイルを指すものは残し、別の場所のものだけを置き換える。Windows の導入フックは以前の登録状態を破棄し、ログオンのエントリを先に書く。
- `recallMode` が off（未設定も off）なら `SessionStart` と `UserPromptSubmit` を外し、`Stop` と `PreToolUse` は残す。モードが変わるか記録がなければ、自身のフックを持つ設定だけを CLI なしで書き直し、次のセッションから効く。静的なプラグインと strict の登録は 4 フックを保ち、実行時に off を確かめる。
- macOS と Linux は引数なしの起動で同期的に登録し、`mcp` の更新スレッドが再確認する。最小限の `PATH` に備えて `~/.local/bin`、`~/.claude/local`、`~/.npm-global/bin`、`/usr/local/bin`、Homebrew の場所も探し、CLI のフォルダーを子の `PATH` の先頭に足す。

ログオンのエントリ、プロセス検出、アンインストールの OS ごとの差は [local-runtime-and-distribution.md](local-runtime-and-distribution.md#os-ごとの登録)、想起の設定は [recall.md](recall.md)。

## Toolport 判定と `mcpRegistration`

`native/src/toolport.rs`（Edictora の複製）が Toolport ゲートウェイの提供状況を判定し、`mcpRegistration`（`auto` / `direct` / `off`）に従って直接登録を省くか決める。判定の詳細と状態遷移は [toolport-mcp-registration.md](../agents/toolport-mcp-registration.md)。
