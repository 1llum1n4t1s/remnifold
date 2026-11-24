# Enrichment and topics

補強は非同期で、埋め込み、ベクトル、事実、トピックの結果を別々に追跡する。移行の詳細は [storage-and-provenance.md](storage-and-provenance.md)、埋め込みの再試行は [vector-reconciliation.md](vector-reconciliation.md)。

## トピックと API

移行 0004 はリビジョン付き `memory_topics` と分類状態を追加する。Workers AI の抽出と、明示され検証済みの `metadata.topics` をラベルにする。トピックは内容、コンテナは物理的な保存先、プロジェクトは来歴である。事実の裏付けと置き換えは、コンテナと来歴が一致する範囲に限る。

`GET /v4/topics` は件数と未分類数を返す。`POST /v3/documents/list` は既定で全コンテナを対象とし、コンテナと正規化したトピックで絞れる。`__unclassified__` は現在のトピック行がない記録を選ぶ。`projection` は完全、`index`、`ids`、`capture` の形を選ぶ。セットアップは `enrichmentEligible` を指定し、opt-out が古い対象の選択を妨げない。

`POST /v4/profile` の `sm_scope` は、空クエリの事実集計とドキュメントのフォールバック、空でない検索のソースを制約する。省略時は旧来のコンテナ全体のプロファイルを保つ。

## リビジョンと公開

読み取りは現在のリビジョンに結び付け、忘却を除く。更新と再試行はリビジョンを替え、ガード付き D1 バッチが古い事実とトピックの公開を防ぐ。再補強は依存チェックポイントも原子的に無効にする。チェックポイント自身では事実抽出を無効に保ち、旧チェックポイント由来の事実を除き、対応するソース事実を同じバッチで戻す。記録の更新と忘却は派生行を除き、生きた記録に置き換えられていない事実を再び有効にする。

埋め込みは完了した時点で保存し、事実とトピックの抽出を待たない。抽出後にリビジョンを再確認し、古い結果は公開しない。埋め込み保存の例外は再送出し、その後の事実保存も行わない。AI 呼び出し回数は変わらない。

`recallable: false` の記録は保存と手動検索を許すが、自動想起と自動補強から除く。明示トピックは保存する。無効、失敗、意図的な省略は未分類として残る。`topics-setup --apply` の受理は完了ではなく、派生データ全体を更新する。サーバー移行をクライアントより先に適用する。

## 抽出の拾い直し

打ち切りなどで事実・トピックが未完了のまま残った行は、`retryMissingExtractions`（15分 cron の照合・埋め込みの拾い直しの後の3段目）が拾う。対象は、埋め込みが完了し（`embedding_status='done'`）、事実かトピックのどちらかが `pending`/`processing`/`failed` で、更新・埋め込みから20分以上たち、忘却・`recallable: false`・整理のまとめでなく、`topic_revision` がある行。`/v4/enrich` は `updated_at` を変えず埋め込みを作り直すため、`embedded_at` の20分の猶予で通常処理との並走を避ける。新しい順（`updated_at` 降順）に1件選ぶ。

頻度は15分枠の2回に1回（毎時15分と45分。30分に1件、1日最大48件）で、18:00 UTC の cron の枠とは重ならない。その日（UTC）の新規が300件以上の忙しい日は拾わない。同じ枠の cron が重複起動しても AI を2回呼ばないよう、候補を1件選んだ後に1行だけの枠表 `memory_extraction_retry_slot` へ枠番号（15分枠の開始時刻の分数。間隔の定数を変えても減らない）を1文の upsert（大きいときだけ更新）で確保し、確保できなければ AI を呼ばない。行の確保は AI の前に `memory_extraction_retries` へ条件付き upsert で行い、1行につき最大3回、次の試行は6時間後、24時間後。AI は `extractEnrichment` を1回だけ呼び、180秒で打ち切る。遅れた結果と、処理中に編集・忘却・`recallable: false`・`/v4/enrich` があった結果は捨てる。書くのは未完了の側だけで、done・disabled の側は変えない。明示トピックは AI なしで保存し、残りが無ければ AI を呼ばない。両側が終われば再試行行を消し、`enrichment_error` は抽出由来（`fact `・`topic classification: `）か空のときだけ書き換える。3回失敗した行は failed のまま止まり、編集か `/v4/enrich` で再開する。

移行 0021（再試行表・枠表と部分索引 `idx_memories_extraction_backlog`）は Worker より先に適用する。未適用だと3段目だけが失敗し、照合と埋め込みの拾い直しは動いて、最後に例外を送出する。既知の未対応として、`/v4/enrich` 後に埋め込みが完了しなかった行は、埋め込みの拾い直しにも抽出の拾い直しにも入らない。

Cloudflare の消費は、実測の平均が約20 Neurons/件で、上限の48件/日なら約960 Neurons/日。応答が長い行が多ければ約1,700〜2,750 Neurons/日になる。無料枠は10,000 Neurons/日。件数を減らすときは `server/src/index.ts` の `EXTRACTION_RETRY_INTERVAL_MINUTES`（30。60なら24件/日）を大きくする。忙しい日の閾値は `EXTRACTION_RETRY_BUSY_DAY_THRESHOLD`、試行回数は `EXTRACTION_RETRY_MAX_ATTEMPTS`、打ち切りは `EXTRACTION_RETRY_TIMEOUT_MS`。

滞留を一時的に早く片付けるときだけ、任意の環境値 `EXTRACTION_BACKLOG_UNTIL`（ISO 8601 UTC、例 `2026-12-01T00:00:00Z`）で期限付きの一括モードにできる。未設定・空・不正・過去、または期限の7日前より早い間は通常モードのままで、設定しない環境は挙動も消費も変わらない。一括モードの間は、15分枠ごとに1回（枠の確保は同じ表）、最大20件を同時4件で処理し、忙しい日の判定を飛ばす。cron の処理開始（前段を含む）から8分を過ぎたら新しい行を始めない（cron の15分の上限のため）。候補・順序・行の確保・最大3回・180秒・書き込み条件は通常と同じ関数（`retryMissingExtraction`）を使い、1件の失敗で残りを止めず、最初の例外を最後に再送出する。同じ組の行を並行に `replaceFacts` しても、各呼び出しは1回の `DB.batch` で完結し、通常の補強が並行に走るのと同じ性質である。消費は最大 80件/時（20件×4枠）で、約20 Neurons/件なら約1,600 Neurons/時になるため、期限を短く置く。値は `wrangler*.jsonc` に常設せず、期限を過ぎたら外す。

事実の新旧は、元記録の `(updated_at, id)` で決める（`replaceFacts`）。抽出が遅れた古い記録が新しい事実を上書きせず、拾い直しは `factUpdatedAt` に記録の `updated_at` を渡す。同じ組の active が4件以上あると順序独立にならない（既知の制約）。

## 失敗の診断

予期しない失敗はリビジョンを記録し、D1 が使えれば保留・処理中の段階だけを失敗にして完了分を残す。公開と同じリビジョン・忘却ガードを使う。状態の記録も失敗すれば別に記録し、元の拒否を Workers に見せる。部分失敗は `memory_enriched` に `ok: false` と段階別エラーを付け、スケジュール失敗は cron を付けた `scheduled_failed` の後に再送出する。診断は API キーを伏せ、エラーを 2,000 文字に制限する。

## D1 の更新コスト

移行 0016 が限定する4トリガーは `memories_after_update`、`memory_organization_source_update`、`memory_organization_capture_conflict_update`、`memory_search_projection_memory_update`。SQLite は UPDATE の準備時に発火しうる本体を展開するため、`vector_status` など状態だけの更新を対象から外す。射影の `OLD.rowid IS NOT NEW.rowid` は `OF` に書けない。0017 により、`vector_attempted_at` が NULL で並ぶ場合に上限内で選ばれる `indexed` 行だけは変わりうる。

移行 0018 以前は2つの衝突トリガーが同じ7文を持ち、重いビューを8回参照した。`memory_organization_capture_invalidations` への `INSERT OR ABORT` 1行に替え、本体の失敗を元の文へ伝える。テーブルと既存ビューは変えず、バックフィルも不要。準備命令数は例として upsert で 164,181 から 19,259 になった。`recursive_triggers` が ON なら、上限近くの依存連鎖は1段早く失敗する。

移行 0019 の `consolidationTargets` は、`projectKey` なしなら射影から候補を絞り、残った行だけ `is_forgotten = 0` と `sm_consolidation` を確かめる。射影の欠落は対象の黙った欠落になる。`projectKey` ありでは 0007 の索引を使う。整理の可視性判定は `memory_organization_hidden` の相関ではなく、バインドした ID を使う。
