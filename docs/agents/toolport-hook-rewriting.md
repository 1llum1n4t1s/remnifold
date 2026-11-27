# Toolport 経由の PreToolUse フック

`PreToolUse` のマッチャーは `^mcp__remnifold__|^mcp__toolport__toolport_(call_tool|run_script)$`（ステータス "Checking tool access..."）。実装は `native/src/hooks.rs` と `native/src/toolport.rs`（後者は Edictora の正本の複製）。

- 直接登録の `mcp__remnifold__*`: `search_memory` の `sessionId: "current"` をペイロードの `session_id` にした `updatedInput`（入力全体の置き換え）を返し、他の引数は変えない。
- `toolport_call_tool`: `tool_input.name` が「registry 上の Remnifold サーバーの id（`[A-Za-z0-9_]` 以外を `_` に置換）`__` 読み取り専用の7ツール（`search_memory`、`listTopics`、`listSpaces`、`listMemories`、`listDocuments`、`getDocument`、`whoAmI`）」に完全一致するときだけ、同じ許可と置き換えを `arguments` だけに行う。registry はツール名が一致したときだけ環境変数と既定パスで解決し、有効・無効やプロファイルは見ない。別サーバーの同名ツール、書き込みツール、registry を読めない場合は `{}`。
- `toolport_run_script`: 許可（`permissionDecision`）は出さない（読み取り専用だけを呼ぶと証明できないため）。`permission_mode` が `"auto"`（判定なしの `updatedInput` は拒否される）か、元の `script` の先頭（空白とコメントを除く）が `"use strict"`（前置きで strict が外れる）なら `{}`。それ以外で `script` に `current` と `search_memory`（または `searchMemory`）が含まれ、registry に Remnifold があるときだけ、元の先頭行へ改行なしで連結した1行の JavaScript を前置きした `updatedInput` を返す（`data`・`input` は保つ。256 KiB を超えるなら何もしない）。前置きは実行時に `toolport.call` / `callAsync`（`servers.<id>.*` と `callAll` も経由）へ渡る `<前置き>__search_memory` / `searchMemory` の `"current"` を置き換える。
- ゲートウェイの登録名が `toolport` でない場合と `toolport_fetch_result` は対象外。前置きが効かなければ `"current"` は固定エラーになる。
