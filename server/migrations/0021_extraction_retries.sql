-- 事実・トピックの抽出が未完了のまま残った行を、既存の15分 cron が1枠1件だけ拾い直すための状態。
-- - memory_extraction_retries: 行ごとの試行回数と次回時刻。source_revision が行の topic_revision と違えば別の版として扱い、1回目に戻る。
--   memories の列は増やさない。memories の行を消すと一緒に消える。0020 の表は主キーが memory_id だけなので共用しない。
-- - memory_extraction_retry_slot: 1行だけの枠表。同じ枠の cron が重複起動しても AI を1回だけ呼ぶため、枠番号を1文で確保する。
-- - idx_memories_extraction_backlog: 候補の SELECT 用の部分索引（updated_at 順に1件だけ読む）。
--   速さだけのための索引で、結果は変わらない。
CREATE TABLE IF NOT EXISTS memory_extraction_retries (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  attempts INTEGER NOT NULL,
  next_attempt_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_extraction_retry_slot (
  id INTEGER PRIMARY KEY CHECK (id=1),
  slot INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memories_extraction_backlog ON memories(updated_at)
  WHERE is_forgotten=0 AND embedding_status='done'
    AND (fact_status IN ('pending','processing','failed') OR topic_status IN ('pending','processing','failed'));
