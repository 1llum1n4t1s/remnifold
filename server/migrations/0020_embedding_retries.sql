-- 埋め込みが無いまま残った行（応答後の実行の打ち切りなどで滞留した行）を、15分 cron が拾い直すための状態。
-- - memory_embedding_retries: 行ごとの試行回数と次回時刻。source_revision が行の topic_revision と違えば別の版として扱い、1回目に戻る。
--   memories の列は増やさない。memories の行を消すと一緒に消える。
-- - idx_memories_embedding_backlog: 候補の SELECT（WHERE is_forgotten = 0 AND embedding_json IS NULL AND updated_at < ?）用の部分索引。
--   既存の部分索引（embedding_json IS NOT NULL 側）は条件が逆なので使えない。速さだけのための索引で、結果は変わらない。
CREATE TABLE IF NOT EXISTS memory_embedding_retries (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  attempts INTEGER NOT NULL,
  next_attempt_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memories_embedding_backlog ON memories(updated_at)
  WHERE is_forgotten = 0 AND embedding_json IS NULL;
