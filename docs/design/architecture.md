# Architecture

クライアントの想起規則は [recall.md](recall.md)、記録の形は [storage-and-provenance.md](storage-and-provenance.md)。ここは検索 API と D1 の構成を述べる。

## 検索 API

自動・手動の想起は `POST /v4/search` に `allContainers: true` を送る。サーバーは 1 回の埋め込みと名前空間なしの 1 回の Vectorize 検索で全コンテナを調べ、`searchScope: "all-containers"`、`searchedContainers`、`spaceDiscoveryComplete: true` を返す。これは全一致の列挙を意味せず、候補と結果には上限がある。クライアントは形を検証し、対応を主張する不正な応答では再試行せず失敗する。明示的に非対応の旧サーバーだけ、100 スペースまでの互換経路を使う。認証・サーバー・通信エラーではファンアウトしない。空のクライアントクエリは送らない。

`sessionId`（160 文字以下）か `sessionIds`（1〜8 個）は候補ウィンドウの前に適用し、併用と不正値は HTTP 400。`sessionSource`（64 文字以下）は `sm_source`、`sessionUntil`（8 件以下、時刻 64 文字以下）は `source_date` を制限する。セッション付きのセマンティック検索は D1 の保存埋め込み（新しい順、200 行）も調べ、Vectorize のヒットにも同じ条件を使う。チェックポイントは除く。旧サーバーが無視してもクライアントの検証で正しさは保たれるが、候補は減りうる。AI/Vectorize の失敗時も字句検索は残り、D1 のセマンティックフォールバックは 200 行までとする。

MCP は `includeProfile` が true でなければ `searchedContainers` を省く。手動検索は来歴名にも一致し、プロジェクト名だけなら該当プロジェクト、新しい出典を優先し、複合クエリは話題の一致を優先する。

## 射影と想起候補

自動想起の `indexOnly` は直接の索引/トピックと字句・意味候補を合わせ、最大 200 候補から 50 行ずつ本文を読む。`updated_at` と `topic_revision` を再確認し、変更・忘却を除く。原文だけにある一致語は、新しい事実ではないと示す 240 文字以内の抜粋にする。意味スコアだけで無関係な索引は注入しない。手動検索は意味候補と全文の抜粋を保つ。

移行 0014 の `memory_search_projection` は、検索文字列、opt-out、scope、最近タスクの適格性、セッション、日付などを生きている行ごとに持つ。`memory_search_projection_source` が唯一の定義で、移行のバックフィルと `memories`/現在の `memory_topics` のトリガーが使う。式の変更には新移行とバックフィルが要る。整理の可視性は一致行にだけクエリ時に判定する。射影は JSON とトピックの反復処理を除くもので、行走査自体はなくならない。

移行 0015 の部分索引はセッション検索を速めるだけで、有無により結果は変わらない。セッション付きの FTS と部分文字列検索は射影を先に絞る。セッションなしでは従来の文と順序を保つ。

## 最近のタスクと過負荷

D1 は単一スレッドなので、最近タスクはプロジェクトとセッションごとに 1 回のウィンドウクエリで選び、合計も同時に得る（範囲外ページだけ別集計）。忘却・opt-out・会話以外・統合・現在セッションを除く。元の時刻を優先し、再インポートで新しく見せない。`recentTasks: true` を宣言しない応答は使わない。

D1 の過負荷は `request_overloaded` と記録し、503 と `Retry-After: 10` を返す。一般の 500 は再試行させない。サーバー検索が軽くならない限り、10 件の同時の重い検索は 9 秒内に終わらない。

## クエリの契約

- FTS・部分文字列・索引検索は射影列 `task_session` を使う。0015 の `idx_memory_search_projection_session` は `task_session IS NOT NULL` の部分索引で、セッション付きでは `CROSS JOIN` を逆にする。`sessionUntil` は `source_date <= julianday(bound)`。
- 索引照合は索引/保存タイトル、説明、セクション、現在の話題をつないだ1文字列を `GLOB` で調べ、用語ごとの `json_each` を使わない。Unicode の大小文字変種を含む。`NOT v1 OR match` の3値優先、重み、rowid の同順位は射影前と同一で、サーバーテストが行、`rowsRead`、プランを比べる。
- 射影のトリガーは `memories` に書き戻さない。想起候補の本文読み取りは旧文書の互換索引を導出するが、保存メタデータは更新しない。
- MCP で `searchedContainers` を省いても `discoveredContainerCount`、`searchScope`、`spaceDiscoveryComplete` は残る。

## D1 の更新コスト

SQLite は UPDATE の準備時に発火しうるトリガー本体を展開するため、移行 0016 は `memories_after_update`、`memory_organization_source_update`、`memory_organization_capture_conflict_update`、`memory_search_projection_memory_update` を状態だけの更新から外す。射影の `OLD.rowid IS NOT NEW.rowid` は `OF` に書けない。0017 では、`vector_attempted_at` が NULL で並ぶとき上限内で選ばれる `indexed` 行だけは変わりうる。

0018 は衝突処理を `memory_organization_capture_invalidations` への `INSERT OR ABORT` に集め、失敗を元の文へ伝える。準備命令数は upsert の例で 164,181 から 19,259。`recursive_triggers` が ON なら上限近くの依存連鎖は1段早く失敗する。
