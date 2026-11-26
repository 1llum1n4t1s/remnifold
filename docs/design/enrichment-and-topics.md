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

事実・トピックが未完了で埋め込み済みの行は、15分 cron の3段目 `retryMissingExtractions` が30分に1件、1行最大3回まで拾い直す。条件・確保・既知の未対応は [vector-reconciliation.md](vector-reconciliation.md#抽出の拾い直し)、消費と定数は [configuration.md](configuration.md#抽出の拾い直しの調整)。

## 事実の新旧と復帰

事実の新旧は元記録の `(updated_at, id)` で決め（`replaceFacts`）、遅れた古い記録は新しい事実を上書きしない。同じ組の active が4件以上あると順序独立にならない。

置き換えられた事実は、生きた元記録の active な新しい事実から `supersedes` の辺がなくなれば戻る（`restoreUnsupportedFactsStatement`）。版付きの経路は、消える自分の排他的な事実と同じ `(container, subject_key, predicate_key)` の組だけを、同じ batch の DELETE の前に見直す。ハード削除と `removeDerivedMemory` の後はコンテナ全件を見直す（`restoreUnsupportedFacts`）。`a←b←c` の連鎖で a が戻り得る経路は既知の限界として直さない。

## 失敗の診断

予期しない失敗はリビジョンを記録し、D1 が使えれば保留・処理中の段階だけを失敗にして完了分を残す。公開と同じリビジョン・忘却ガードを使う。状態の記録も失敗すれば別に記録し、元の拒否を Workers に見せる。部分失敗は `memory_enriched` に `ok: false` と段階別エラーを付け、スケジュール失敗は cron を付けた `scheduled_failed` の後に再送出する。診断は API キーを伏せ、エラーを 2,000 文字に制限する。

## D1 の更新コスト

トリガーの準備費用は [architecture.md](architecture.md#d1-の更新コスト)、統合対象の集計は [storage-and-provenance.md](storage-and-provenance.md) を参照。
