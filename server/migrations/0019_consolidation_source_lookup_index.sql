-- 統合対象の集計（consolidationTargets）の anti-join 用に、統合元の照合索引を貼り換える。
-- 新しい索引は旧索引 (memory_id, source_revision) の末尾に consolidation_id を足した被覆索引で、
-- 照合（memory_id と source_revision で引き、consolidation_id で active を確かめる）を索引だけで終える。
-- 旧索引は新索引の先頭2列と同じなので不要になり、索引の本数は変えない。結果は変わらず、速さだけが変わる。
-- 旧索引名の参照は 0005 だけ（利用箇所は EXISTS・UNION・明示 ORDER BY で、索引の選択に結果が依存しない）。
CREATE INDEX IF NOT EXISTS idx_memory_consolidation_sources_lookup
  ON memory_consolidation_sources(memory_id, source_revision, consolidation_id);
DROP INDEX IF EXISTS idx_memory_consolidation_sources_memory;
