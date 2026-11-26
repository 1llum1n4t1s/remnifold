# Migration and trigger contracts

[data-model.md](data-model.md) の移行・トリガーの細部。戻す場合も新しい移行を追加し、適用済みを編集しない。

## 移行ごとの契約

- 0016 の UPDATE は `OF` 列に限定する。`WHEN` の列を足すなら `OF` にも足す。`rowid` を更新せず、再作成の発火順を保つ。`EXPLAIN` と同値結果を検査し、時間は測らない。
- 0017 の索引（速度だけの索引）を `INDEXED BY` に使わない。`INDEXED BY` を使うのは、0002 の facts の索引 `idx_facts_identity` と `idx_facts_source` だけ（`replaceFacts` の候補・逆向きの関連・自己置換と、版付き経路の復帰 `restoreUnsupportedFactsStatement`）。`fact-restore.test.mjs` の `EXPLAIN` が、両索引を使い、`idx_facts_container_status` と facts の全走査を使わず、計画行に `subject_key=?` と `predicate_key=?` を含むことを守る。
- 版付き経路の復帰は、消える自分の排他的事実の組だけを見直す文を、同じ batch の事実の DELETE の前に置く（忘却・ハード削除は batch の先頭側）。ハード削除の結果は、先頭へ足してもずれないよう末尾の `DELETE FROM memories` を `results.at(-1)` で見る。
- 0018 の本体は `memory_organization_capture_invalidate` の 1 か所。識別/競合ビューを変えたら R、S、P と同値性テストも直す。文ごとに再評価し、一時表を使わず `INSERT OR ABORT` を保つ。トリガー順を変えない。逆置換テストは後のビュー変更を検出しない。深さは 4（workerd 測定上限 10、本番未確認）。
- 0019/0020 の速度索引と、射影が生きた全行にある前提を保つ。0020 は Worker より先に適用する。再試行は topic_revision/updated_at を変えず、失敗を `persistMemoryEmbedding` に流さず、確保・版・生存・埋め込みなしの条件を保つ（`embedding-retry.test.mjs`、`embedding-first.test.mjs`）。
- 0021 は Worker より先に適用する。抽出の拾い直しは、埋め込み完了・20分の猶予（`updated_at` と `embedded_at`）・確保・版・生存・未完了の側だけを書く条件を保ち、`topic_revision`/`updated_at` を変えず、AI は1枠1回・最大3回・180秒とする。1枠1回は、1行だけの `memory_extraction_retry_slot` の枠番号を候補1件の選択後・行の確保前に1文で確保して守る。定数は `EXTRACTION_RETRY_*`（`server/src/index.ts`）。期限付きの一括モード（`EXTRACTION_BACKLOG_UNTIL`）も同じ確保・条件・枠表を使い、1枠の件数と同時数だけを変える。0020 の表とは共用しない（`extraction-retry.test.mjs`、`fact-order.test.mjs`）。

## 検証の細部

0016 と 0017 のテストは移行の有無で結果を比べ、状態列だけの UPDATE にトリガープログラムがないことを `EXPLAIN` で確かめる。SQLite は同種トリガーを作成と逆順に発火する。

0018 の識別と競合の正本は `identities`、`unsafe`、`single_part_v2`、`conflicts` ビュー。逆置換は `server/tests/capture-conflict-single-body.test.mjs` が確認する。射影トリガー2つは競合トリガーの後に再作成する。トリガーは空ビュー `memory_organization_capture_invalidations` へ識別行を1つ挿入するだけとする。

0020 の補助索引 `idx_memories_embedding_backlog` が未適用なら `retryMissingEmbeddings` だけが失敗し、照合後に再送出する。0019/0020 は追加型で、再試行表以外は速度だけに影響する。0021 の補助索引 `idx_memories_extraction_backlog` が未適用なら `retryMissingExtractions` だけが失敗し、前段の後に再送出する。
