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

## 失敗の診断

予期しない失敗はリビジョンを記録し、D1 が使えれば保留・処理中の段階だけを失敗にして完了分を残す。公開と同じリビジョン・忘却ガードを使う。状態の記録も失敗すれば別に記録し、元の拒否を Workers に見せる。部分失敗は `memory_enriched` に `ok: false` と段階別エラーを付け、スケジュール失敗は cron を付けた `scheduled_failed` の後に再送出する。診断は API キーを伏せ、エラーを 2,000 文字に制限する。

## D1 の更新コスト

移行 0016 が限定する4トリガーは `memories_after_update`、`memory_organization_source_update`、`memory_organization_capture_conflict_update`、`memory_search_projection_memory_update`。SQLite は UPDATE の準備時に発火しうる本体を展開するため、`vector_status` など状態だけの更新を対象から外す。射影の `OLD.rowid IS NOT NEW.rowid` は `OF` に書けない。0017 により、`vector_attempted_at` が NULL で並ぶ場合に上限内で選ばれる `indexed` 行だけは変わりうる。

移行 0018 以前は2つの衝突トリガーが同じ7文を持ち、重いビューを8回参照した。`memory_organization_capture_invalidations` への `INSERT OR ABORT` 1行に替え、本体の失敗を元の文へ伝える。テーブルと既存ビューは変えず、バックフィルも不要。準備命令数は例として upsert で 164,181 から 19,259 になった。`recursive_triggers` が ON なら、上限近くの依存連鎖は1段早く失敗する。

移行 0019 の `consolidationTargets` は、`projectKey` なしなら射影から候補を絞り、残った行だけ `is_forgotten = 0` と `sm_consolidation` を確かめる。射影の欠落は対象の黙った欠落になる。`projectKey` ありでは 0007 の索引を使う。整理の可視性判定は `memory_organization_hidden` の相関ではなく、バインドした ID を使う。
