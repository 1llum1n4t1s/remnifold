-- セッション絞り込み（検索 API の sessionId / sessionIds / sessionUntil）のための索引。追加的な変更で、行・列・既存の索引は変えない。
--
-- 絞り込みは射影テーブル memory_search_projection（migration 0014）の task_session と source_date を使う:
--   p.task_session IN (...)  /  (task_session = ? AND source_date <= julianday(?))
--   id IN (SELECT memory_id FROM memory_search_projection WHERE <同じ条件>)
-- 索引が無いと、どの経路も射影の全行を走査する。task_session は会話の記録だけが持つ（それ以外は NULL）ため、
-- NULL を除く部分索引にして、書込みの保守費用と容量を会話の記録の分に抑える。
-- 部分索引は、問い合わせの条件が task_session への比較（= / IN）を含むときに使われる（それは IS NOT NULL を含意する）。
-- source_date は ID ごとの時刻の上限、memory_id は IN の副問い合わせが射影の行を読まずに済むための列。
-- 未適用のサーバーでも問い合わせは同じ結果を返す（索引が無く、射影を走査するぶん遅いだけ）。
-- IF NOT EXISTS により、再適用しても安全。
CREATE INDEX IF NOT EXISTS idx_memory_search_projection_session
  ON memory_search_projection(task_session, source_date, memory_id)
  WHERE task_session IS NOT NULL;
