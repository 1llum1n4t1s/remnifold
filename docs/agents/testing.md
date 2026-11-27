# Contracts to retain in tests

保存と整理は [data-model.md](data-model.md)、公開 API とリモート MCP は [api-reference.md](api-reference.md) の契約も保つ。

## クライアントと capture

- ネイティブ CLI/MCP/フックの子プロセステストは、認証済みフィクスチャで保存、検索、忘却、capture、想起、setup、import、旧リーダー移行を行う。ツールの形、Unicode、伏せ字化した失敗、上限付き待機、受領記録の並行性、v2 マルチパート、確認済み再試行の抑止を保つ。
- Codex import は Codex だけを読む。Claude の行は `type` で分類し、thinking、ツール、要求コピー、サイドチェーン、メタ、他セッションを除く。通知の継続は以前のキーを変えない。
- 圧縮要約、途中発言、質問回答は別記録で、turn/part/captureKey/captureVersion を持たず、uuid（回答は tool use ID）ごとに 1 件とする。圧縮は `recallable: false`。連絡先の伏せ字と個人情報の回答の置き換えを検証する。
- 手動検索の `sessionId: "current"` は Claude `PreToolUse` だけが解決する（直接登録と、Toolport の `toolport_call_tool` / `toolport_run_script` 経由。他のゲートウェイでは解決しない）。`call_tool` 経由は、`name` が「registry 上の Remnifold サーバーの id を前置きに変換したもの（`[A-Za-z0-9_]` 以外を `_` に）+ `__` + 読み取り7ツール」と完全一致するときだけで、別名・書き込みツール・registry が読めない場合は許可しない。`run_script` は許可を出さず、`script` だけを前置き付きに差し替える（`data`・`input` は保つ。セッション ID と名前は JSON としてエスケープする。`current` / `search_memory` を含まない、セッション ID が無い、サーバーが registry に無い、256 KiB を超える、`permission_mode` が `auto`、元の `script` の先頭（空白とコメントを除く）が `"use strict"` の場合は何もせず、`"current"` は従来の固定エラーになる。事前判定に外れたら registry を読まないことは注入した closure で確かめる）。
- `native/src/toolport.rs` は Edictora `crates/edictora/src/install/toolport.rs` が正本のバイト同一の複製で、ここでは編集しない。判定ルールのテストは正本のファイル内にあり、Remnifold では `app_update::registration::plan` に判定を注入して（Windows・macOS・Linux 以外は判定せず直接登録、実行ファイルが無ければ直接登録へ戻す、前回未完了のクライアントも判定する、Claude Code の CLI も `~/.claude` も無ければフックを作らない、別の実行ファイルでの提供 `ServedElsewhere` は `skipped=toolport-foreign` に記録して毎回判定し直す遷移表と、foreign の Claude がフックの再試行で理由を保つこと）確かめる。手動 `connect` の `toolport` 通知は `sameExecutable` を含めて確かめる。

## 想起

- 話題と最近のタスクの独立した抑止、圧縮回復、リビジョン、共有予算、`maxRecentTasks`、`recallScope` を保つ。既定 `session` は ID なしで注入せず、リビジョンと上限の前に絞る。`all` は旧来の横断を保つ。
- 構造化/ブラウザ要求の射影、大出力と UTF-8 境界をまたぐユーザー話題、不一致・不完全・過大な履歴の拒否を検証する。復旧は上限内の完全な一致ヘッダーを要求し、後方の矛盾する ID を拒む。手動のプロジェクト順位は自動想起と分け、最近のタスクはページングの前にまとめる。
- 期限付き読み取りはプロセス横断で 2 スロット、429/502/503/504 の試行は計 3 回まで、期限を超えず通知付きで続行する。Stop はスロットを使わない（`native/tests/hook_concurrency.rs`）。

## 配布と検証の範囲

- 梱包は不安全パス、識別・ターゲット・版・資産・ツール・実行ファイルの不一致を拒む。strict は公開検証、オフラインレシート、不変版、開いた実行ファイルの置き換え、旧接続/状態の読み取りを実環境に触れず検証する。
- Windows フックは上書きのない過去パッケージを受け入れ、部分的・改変・他 OS の上書きを拒む。[標準入力の契約](../readme/conventions.md)を保つ。非公開入力の変更では、専用 PTY のエコー/行編集、終端後の復元、失敗を実際に検証する。リダイレクト拒否だけでは足りない。
- プロビジョニングは Cloudflare REST フィクスチャだけを使い、読み取り専用の既定、`--apply`、既存 ID/バインディング/cron/ルート、競合拒否、外部の再開状態、伏せ字、キーなしの検証省略、置き換えなしの回復を調べる。

上限付き作業は操作数と返却サイズで検証する。明示許可なしに新旧の時間比較を行わない。
