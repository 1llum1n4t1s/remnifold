# Enrichment and topics

補強は非同期で、結果を種類別に追跡する。移行の詳細は [storage-and-provenance.md](storage-and-provenance.md)、埋め込みの再試行は [vector-reconciliation.md](vector-reconciliation.md)。

## トピックと API

移行 0004 はリビジョン付き `memory_topics` と分類状態を追加する。Workers AI の抽出と、明示され検証済みの `metadata.topics` をラベルにする。トピックは内容、コンテナは保存先、プロジェクトは来歴。事実の裏付けと置き換えは、コンテナと来歴が一致する範囲に限る。

`GET /v4/topics` は件数と未分類数を返す。`POST /v3/documents/list` は既定で全コンテナを対象とし、コンテナと正規化したトピックで絞れる。`__unclassified__` は現在のトピック行がない記録を選ぶ。`projection` は完全、`index`、`ids`、`capture` の形を選ぶ。セットアップは `enrichmentEligible` を指定し、opt-out が古い対象の選択を妨げない。

`POST /v4/profile` の `sm_scope` は、空クエリの事実集計とドキュメントのフォールバック、空でない検索のソースを制約する。省略時は旧来のコンテナ全体のプロファイルを保つ。

## リビジョンと公開

読み取りは現在のリビジョンに結び付け、忘却を除く。更新と再試行はリビジョンを替え、ガード付き D1 バッチが古い事実とトピックの公開を防ぐ。再補強は依存チェックポイントも原子的に無効にする。チェックポイント自身では事実抽出を無効に保ち、旧チェックポイント由来の事実を除き、対応するソース事実を同じバッチで戻す。記録の更新と忘却は派生行を除き、生きた記録に置き換えられていない事実を再び有効にする。

埋め込みは完了した時点で保存し、事実とトピックの抽出を待たない。抽出後にリビジョンを再確認し、古い結果は公開しない。埋め込み保存の例外は再送出し、事実も保存しない。

`recallable: false` の記録は保存と手動検索を許すが、自動想起と自動補強から除く。明示トピックは保存する。無効、失敗、意図的な省略は未分類として残る。`topics-setup --apply` の受理は完了ではなく、派生データ全体を更新する。サーバー移行をクライアントより先に適用する。

## 抽出の拾い直し

事実・トピックが未完了の行は、`retryMissingExtractions`（15分 cron の照合・埋め込みの拾い直しの後の3段目）が拾う。対象は、埋め込みが完了し（`embedding_status='done'`）、事実かトピックのどちらかが `pending`/`processing`/`failed` で、更新・埋め込みから20分以上たち、忘却・`recallable: false`・整理のまとめでなく、`topic_revision` がある行。`/v4/enrich` は `updated_at` を変えず埋め込みを作り直すため、`embedded_at` の20分の猶予で通常処理との並走を避ける。新しい順（`updated_at` 降順）に1件選ぶ。

頻度は30分に1件で（毎時15分と45分）、18:00 UTC の cron の枠とは重ならない。その日（UTC）の新規が300件以上なら拾わない。重複起動で AI を2回呼ばないよう、候補の選択後に枠表 `memory_extraction_retry_slot` へ枠番号（15分枠の開始時刻の分数）を、大きいときだけ更新する1文の upsert で確保し、できなければ AI を呼ばない。

行も AI の前に `memory_extraction_retries` への条件付き upsert で確保する。1行最大3回、次の試行は6時間後、24時間後。`extractEnrichment` は1回だけ呼び、180秒で打ち切る。遅れた結果と、途中で編集・忘却・`recallable: false`・`/v4/enrich` があった結果は捨てる。書くのは未完了の側だけ。明示トピックは AI なしで保存し、残りが無ければ AI を呼ばない。両側が終われば再試行行を消し、`enrichment_error` は抽出由来（`fact `・`topic classification: `）か空のときだけ書き換える。3回失敗した行は failed のまま止まり、編集か `/v4/enrich` で再開する。

移行 0021 は Worker より先に適用する。未適用なら3段目だけが失敗する。既知の未対応: `/v4/enrich` 後に埋め込みが完了しなかった行は、埋め込みの拾い直しにも抽出の拾い直しにも入らない。

消費、定数、一括モードは [configuration.md](configuration.md#抽出の拾い直しの調整) を参照。一括モードも同じ `retryMissingExtraction` を使う。

事実の新旧は、元記録の `(updated_at, id)` で決める（`replaceFacts`）。遅れた古い記録は新しい事実を上書きせず、拾い直しは `factUpdatedAt` に記録の `updated_at` を渡す。同じ組の active が4件以上あると順序独立にならない（既知の制約）。

置き換えられた事実の復帰（`restoreUnsupportedFactsStatement`）の判定は、生きた元記録の active な新しい事実から `supersedes` の辺が1本もないこと（R1）。版付きの経路（`replaceFacts`・`addMemory`・PATCH・`retryEnrichment` の事実なし・忘却・ハード削除）は、消える自分の排他的な事実と同じ `(container, subject_key, predicate_key)` の組だけを見直し、同じ batch の DELETE の**前**に実行する（自分の事実は置き換え元に数えない。版が合わなければ0件。ハード削除は版ガードを持たず、行の存在だけで判定し、存在しない ID では事実が無く何も変えない）。忘却・ハード削除は、batch の直後に例外が出ても、直接置き換えられていた事実が戻る。コンテナ全件の見直し（`restoreUnsupportedFacts`）は、ハード削除の後（無効化された派生記憶の別コンテナを含む）と、派生記憶の削除（`removeDerivedMemory`）の後に残す。既知の限界: `a←b←c` のように、置き換え元 b 自身が置き換えられた連鎖で a が戻り得る経路は直さない（組を絞った版では無関係な保存・抽出で戻らなくなったが、全件版・同じ組の変更では今も戻り得る）。

## 失敗の診断

予期しない失敗はリビジョンを記録し、D1 が使えれば保留・処理中の段階だけを失敗にして完了分を残す。公開と同じリビジョン・忘却ガードを使う。状態の記録も失敗すれば別に記録し、元の拒否を Workers に見せる。部分失敗は `memory_enriched` に `ok: false` と段階別エラーを付け、スケジュール失敗は cron を付けた `scheduled_failed` の後に再送出する。診断は API キーを伏せ、エラーを 2,000 文字に制限する。

## D1 の更新コスト

トリガーの準備費用は [architecture.md](architecture.md#d1-の更新コスト)、統合対象の集計は [storage-and-provenance.md](storage-and-provenance.md) を参照。
