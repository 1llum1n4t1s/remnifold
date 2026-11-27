# Local app integration

アプリは管理者権限なしでユーザーごとに入る（Windows `%LOCALAPPDATA%\Programs\Remnifold`、macOS `~/Library/Application Support/Remnifold/app`、Linux `$XDG_DATA_HOME/remnifold`、または `<REMNIFOLD_HOME>/app`）。検証済みの版は一度だけ `versions/<version>` に展開し、内容の異なる既存フォルダーは置き換えない。全登録は固定の `bin/remnifold(.exe)` を指し、`receipts/` はオフライン検証用、`current.json` は導入済みの版を指す。置き換えは書き込んでから改名で入れ、Windows では実行中のファイルを先に脇へ移して後で消し、`restartRequired` を報告する。接続ファイルは `remnifold.json`、`kagayoi-memory.json`、`supermemory.json` の順に設定フォルダー、次に旧 Codex ホームで読み、Setup は現行のファイルだけを書く。上書きと移行手順は [README](../readme/upgrading-from-1-x-names.md)。

`connect claude-code` は MCP サーバーを `claude mcp add --scope user` で登録し（CLI がなければコマンドを表示）、`PreToolUse`、`SessionStart`、`UserPromptSubmit`、非同期の `Stop` をパッケージの設定のまま `settings.json` へマージする。`recallMode`（`read_settings` で読み、未設定は `off`）が off なら `SessionStart` と `UserPromptSubmit` は登録せず既存のエントリを消し、`Stop` と `PreToolUse` は常に残す。`PreToolUse` は読み取りツールを許可し、`search_memory` の `sessionId: "current"` を実際のセッション ID に置き換える。Toolport ゲートウェイ経由の判定と `run_script` への前置きは [toolport-hook-rewriting.md](../agents/toolport-hook-rewriting.md)。

マージは Remnifold を実行するエントリ（旧ランチャーを含む）だけを置き換え、他の設定を保ち、不正な形式は拒否し、一度だけバックアップを残す。Windows ではフックが Git Bash で動くため、パスはスラッシュ区切りで引用し、引用や展開に使う文字を含むパスは拒む。`connect <tool> --remove` は逆の操作で、Remnifold のエントリ（`--executable` ならそのファイルを指すものだけ）と空のイベントを除き、`claude mcp remove remnifold --scope user` か `codex mcp remove remnifold` を実行する。`connect codex` は公式のフック登録手段がないため `codex mcp add` で MCP だけを登録する。`.codex-plugin/`、`.mcp.json`、`hooks/` はソース資産で導入経路ではない。

アーカイブと Windows の `commandWindows` ランチャーは [release-process.md](release-process.md#パッケージとフックのランチャー)。
