-- 速度だけのための索引（0017 がなくても同じ条件の行を返す）。ただし vector_attempted_at が NULL の indexed 行が同時刻で並ぶときは、
-- reconcile が1回に選ぶ行（LIMIT の境界）の順だけが変わり得る。残りの行は後の回で照合されるため、最終状態は変わらない。
-- - idx_memories_custom_id: custom_id だけで引く reuse 検索（reuseExistingCapture）が memories の全走査になるのを避ける。
--   UNIQUE (container_tag, custom_id) は先頭が container_tag なので custom_id 単独の条件には使えない。
-- - idx_memories_vector_sweep: reconcileVectors の vector_status = 'indexed' の分岐
--   （WHERE is_forgotten = 0 AND embedding_json IS NOT NULL AND vector_status = 'indexed' ORDER BY COALESCE(vector_attempted_at, created_at)）を
--   索引の順で読んで打ち切る。部分索引の条件はその文の条件に含まれる。vector_status <> 'indexed' の分岐は範囲条件のため並びには使えない。
CREATE INDEX IF NOT EXISTS idx_memories_custom_id ON memories(custom_id);
CREATE INDEX IF NOT EXISTS idx_memories_vector_sweep ON memories(vector_status, COALESCE(vector_attempted_at, created_at))
  WHERE is_forgotten = 0 AND embedding_json IS NOT NULL;
