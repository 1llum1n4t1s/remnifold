# Recall

自動想起はプロンプトの話題で簡潔な索引を順位付けし、現在のフォルダーには限定しない。サーバー側の検索は [architecture.md](architecture.md)。

## スコープとセッション

`recallScope` の既定と不正値は `session`。ID がなければ注入せず、最近のタスクも要求しない。クライアントはリビジョン選択と上限より前に（クライアント種別、`sessionId`）の完全一致で絞り、古いサーバーがフィルターを無視しても他セッションを注入しない。種別は新しい capture の `sm_source` に持ち、capture キーには含めない。種別を特定できなければ ID だけで照合する。種別指定時に対応サーバーは不明な記録を拒み、種別を返さない古いサーバーでは ID だけで通す。

Codex は現在の ID だけを使う。Claude Code は再開に備えてトランスクリプト先頭（4 MiB、20000 行、1 行 256 KiB まで）の user/assistant 行から最大 8 個の ID を集め、サイドチェーンとメタ行を除く。コピー元の ID ごとに最後の行の時刻を `sessionUntil` とし、元ターンの時刻がその後か不明の記録を拒む。フォーク後の親の作業を混ぜないためで、余裕は設けない。現在の ID には上限がない。

手動 `search_memory` の `sessionId` は検索本文とクライアントの両方で確かめる。`"current"` は Claude Code の `PreToolUse` だけが解決する（直接登録した `mcp__remnifold__search_memory` と、Toolport の `toolport_call_tool` 経由の呼び出し。[local-app-integration](local-app-integration.md)）。未解決なら固定エラーになる。

`all` は他セッションの最近のタスクを最大 2 件（`maxRecentTasks` は 0〜3）加える。話題一致ではないと明示し、1,000 UTF-16 単位を確保する。話題検索とは並行し、片方の失敗でもう片方を捨てず、重複抑制のハッシュも分ける。同じ応答の話題索引は繰り返さない。

## クエリと履歴

継続だけのプロンプトは、先頭 64 KiB の完全な `session_meta` の ID が一致するときだけ履歴から話題を復旧する。128 KiB 単位で最大 8 MiB、ユーザーメッセージ 16 件を後ろ向きに読み、256 KiB 超の行を飛ばす。矛盾する ID を拒み、残り 250 ms 以下で止まる。一般語だけでは検索せず、`timeout` などの技術語は使う。パスは作業フォルダー直下のプロジェクト名か末尾要素に縮め、Unicode と識別子を正規化する。話題がなければ検索も Git 探索も行わない。

索引は添付スキル、ブラウザー状態、Page-state を省き、質問への返答は回答を先に置く。既存の本文と capture 識別は変えない。同じプロジェクト・セッション・ターン・パートは最新リビジョンだけを使い、手動検索は過去も残す。

## 注入と実行の上限

既定 3 件、設定で最大 5 件。全体 3,000 UTF-16 単位、1 エントリ 1,800、話題 3 件まで。収まらない候補は飛ばして後続を試す。状態には `remnifold/recall-state/` のハッシュ、パス、オフセットだけを保存する。compact/clear、圧縮行、パス変更、ファイルの縮小、不完全な行、1 MiB を超す新規データで抑制を解除し、状態が使えなければ想起を優先する。

フックの読み取りは 9 秒の期限と `recall-locks/` の 2 スロットを共有し、429/502/503/504 を 1.2 秒以上残るときだけ計 3 回まで試す。待機中はスロットを放す。`Retry-After` は収まる場合だけ従い、そうでなければジッター付き短期 backoff。ロック不能はフェイルオープン。`Stop` と手動 MCP はこのスロットを使わない。一部失敗は件数だけを報告し、注入した索引だけを既読にする。

## 実装上の契約

- `search_index` の `onlySessionId` はスコア、`latest()`、候補上限、取得より前に効く。旧サーバーの候補窓は全体 `limit: 40`、スペースごと 20 件で、その下位にある現在セッションの記録は返らないことがある。
- フックは `transcript_path` の先頭で種別を判定する（Codex は `session_meta`、Claude は文字列の `sessionId`）。capture は rollout の `id` を `sessionId` とし、`rootSessionId` と `sessionKind`（root/subagent）も保存する。
- 時刻は元ターンの `sourceTimestamp`（索引の `sourceUpdatedAt`）で、`--fork-session` などのフォーク前に始まったターンは後で終わっても含む。
- `all` の最近タスクは `projection: "index"`、`recentTasks: true`、分かれば `excludeSessionId` を送る。
- 固定文言は `session` で `recall.index_prefix_session`、`all` で `recall.index_prefix`。9言語は [localization.md](localization.md)。capture の除外が頼る `<remnifold-index>` タグは全言語で固定する。
- スロットは `recall-api-<n>.lock` の advisory `File::try_lock` で、異常終了した保持者の分は OS が解放する。backoff は約 0.6 秒、次に 1.2 秒。
- パスは `dev`、`projects`、`workspaces`、`repos`、`work`（単数形も）の下ならプロジェクト名を使う。ASCII の単語断片には一致させず、`.NET` は `.NET8` に一致する。
