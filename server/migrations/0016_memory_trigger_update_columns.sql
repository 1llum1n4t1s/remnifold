-- memories の UPDATE トリガーを「列を限定した」形（AFTER UPDATE OF <列>）に作り直す。
--
-- 背景: UPDATE 文を準備（コンパイル）するとき、SQLite は発火し得るトリガーの本文を文ごとに展開する。
-- 列を限定しないトリガーは、vector_status など状態列だけの UPDATE でも毎回展開され、重いビュー
-- （memory_organization_capture_conflicts など）を参照する本文のコンパイル費が D1 の時間の大半を占めていた。
-- AFTER UPDATE OF <列> にすると、SET に列が含まれない UPDATE ではトリガーの展開自体が起きない。
--
-- 本文と WHEN は 0016 より前の全移行を適用した後の sqlite_master.sql と一字も変えず、見出しに OF を足しただけである。
-- OF の列は、WHEN が比較する列と一致する（本文が走る条件は WHEN が決めるため、WHEN に現れない列は不要）。
-- データ・外部キー・他のトリガーは変えない。
--
-- 制約: memory_search_projection_memory_update の WHEN にある `OLD.rowid IS NOT NEW.rowid` は OF に書けない。
-- memories の rowid は暗黙の rowid で、アプリは `SET rowid` を使わない。この移行以降も `UPDATE memories SET rowid = ...` を
-- 書いてはならない（書くと射影が更新されない）。rowid を動かす必要が出たら、射影を作り直す移行と一緒に扱う。
--
-- 発火順: SQLite は同じ種類のトリガーを「作成順の逆」（sqlite_master の行の逆順）に発火する。元の順は
-- projection → capture_conflict → source → memories_after_update なので、作成順を
-- memories_after_update → source → capture_conflict → projection として相対順を保つ。
-- 移行は1ファイルとして原子的に適用されるため、途中の状態は外から見えない。

DROP TRIGGER IF EXISTS memories_after_update;
DROP TRIGGER IF EXISTS memory_organization_source_update;
DROP TRIGGER IF EXISTS memory_organization_capture_conflict_update;
DROP TRIGGER IF EXISTS memory_search_projection_memory_update;

CREATE TRIGGER memories_after_update
AFTER UPDATE OF content, container_tag ON memories
WHEN old.content IS NOT new.content OR old.container_tag IS NOT new.container_tag
BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content, container_tag)
  VALUES ('delete', old.rowid, old.content, old.container_tag);
  INSERT INTO memories_fts(rowid, content, container_tag)
  VALUES (new.rowid, new.content, new.container_tag);
END;

CREATE TRIGGER memory_organization_source_update AFTER UPDATE OF content, metadata_json, container_tag, topic_revision, is_forgotten, status ON memories
WHEN OLD.content IS NOT NEW.content OR OLD.metadata_json IS NOT NEW.metadata_json
  OR OLD.container_tag IS NOT NEW.container_tag OR OLD.topic_revision IS NOT NEW.topic_revision
  OR OLD.is_forgotten IS NOT NEW.is_forgotten OR OLD.status IS NOT NEW.status
BEGIN
  DELETE FROM memory_organization_batches WHERE request_json IS NULL AND id IN (
    SELECT batch_id FROM memory_organization_leases WHERE memory_id=OLD.id
    UNION SELECT batch_id FROM memory_organization_batch_summaries WHERE memory_id=OLD.id);
  UPDATE memory_organization_coverage SET status='invalid'
  WHERE memory_id=OLD.id OR memory_id IN (
    SELECT l.memory_id FROM memory_organization_coverage_summaries l
    JOIN memory_organization_dependencies_all d ON d.summary_id=l.summary_id WHERE d.memory_id=OLD.id
  ) OR memory_id IN (
    SELECT l.memory_id FROM memory_organization_coverage_summaries l
    JOIN memory_organization_summaries s ON s.id=l.summary_id WHERE s.memory_id=OLD.id
  );
  DELETE FROM facts WHERE source_memory_id IN (
    SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.memory_id<>OLD.id AND s.id IN (SELECT summary_id FROM memory_organization_dependencies_all WHERE memory_id=OLD.id)
  );
  DELETE FROM memory_topics WHERE memory_id IN (
    SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.memory_id<>OLD.id AND s.id IN (SELECT summary_id FROM memory_organization_dependencies_all WHERE memory_id=OLD.id)
  );
  UPDATE memories SET is_forgotten=1,status='superseded',vector_status='pending',vector_mutation_id=NULL,vector_attempted_at=NULL
  WHERE id<>OLD.id AND id IN (
    SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.id IN (SELECT summary_id FROM memory_organization_dependencies_all WHERE memory_id=OLD.id)
  );
  UPDATE memory_organization_summaries SET status='invalid'
  WHERE memory_id=OLD.id OR id IN (SELECT summary_id FROM memory_organization_dependencies_all WHERE memory_id=OLD.id);
  DELETE FROM facts WHERE source_memory_id=NEW.id
    AND EXISTS(SELECT 1 FROM memory_organization_summaries WHERE memory_id=NEW.id AND status='invalid');
  DELETE FROM memory_topics WHERE memory_id=NEW.id
    AND EXISTS(SELECT 1 FROM memory_organization_summaries WHERE memory_id=NEW.id AND status='invalid');
  UPDATE memories SET is_forgotten=1,status='superseded',vector_status='pending',vector_mutation_id=NULL,vector_attempted_at=NULL
    WHERE id=NEW.id AND EXISTS(SELECT 1 FROM memory_organization_summaries WHERE memory_id=NEW.id AND status='invalid');
  DELETE FROM memory_organization_progress WHERE memory_id=OLD.id;
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
  WHERE (id IN (SELECT group_id FROM memory_organization_group_members WHERE memory_id=OLD.id)
    OR id IN (SELECT l.group_id FROM memory_organization_group_summaries l JOIN memory_organization_summaries s ON s.id=l.summary_id WHERE s.memory_id=OLD.id)
    OR id IN (SELECT l.group_id FROM memory_organization_group_summaries l JOIN memory_organization_dependencies_all d ON d.summary_id=l.summary_id WHERE d.memory_id=OLD.id)
    OR (session_id=json_extract(NEW.metadata_json,'$.sessionId') AND turn=json_extract(NEW.metadata_json,'$.turn')
      AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(prefix)+1)=prefix||':'))
    AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
END;

CREATE TRIGGER memory_organization_capture_conflict_update AFTER UPDATE OF content, metadata_json, container_tag, is_forgotten ON memories
WHEN NEW.is_forgotten=0 AND (OLD.content IS NOT NEW.content OR OLD.metadata_json IS NOT NEW.metadata_json OR OLD.container_tag IS NOT NEW.container_tag OR OLD.is_forgotten IS NOT NEW.is_forgotten)
  AND EXISTS(SELECT 1 FROM memory_organization_capture_conflicts WHERE id=NEW.id)
BEGIN
  DELETE FROM facts WHERE source_memory_id IN(SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id));
  DELETE FROM memory_topics WHERE memory_id IN(SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id));
  UPDATE memories SET is_forgotten=1,status='superseded',vector_status='pending',vector_mutation_id=NULL,vector_attempted_at=NULL
    WHERE id IN(SELECT s.memory_id FROM memory_organization_summaries s
      WHERE s.id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id))
      AND (is_forgotten<>1 OR status<>'superseded' OR vector_status<>'pending' OR vector_mutation_id IS NOT NULL OR vector_attempted_at IS NOT NULL);
  UPDATE memory_organization_summaries SET status='invalid' WHERE status<>'invalid'
    AND id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id);
  UPDATE memory_organization_coverage SET status='invalid'
    WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id);
  DELETE FROM memory_organization_progress WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id) AND COALESCE(defer_reason,'')<>'capture-hash-unverifiable';
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
    WHERE (id IN(SELECT group_id FROM memory_organization_group_members WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id))
      OR id IN(SELECT group_id FROM memory_organization_group_summaries WHERE summary_id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id)))
      AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
END;

CREATE TRIGGER memory_search_projection_memory_update AFTER UPDATE OF metadata_json, topic_revision, is_forgotten, container_tag, created_at, updated_at, id ON memories
WHEN OLD.metadata_json IS NOT NEW.metadata_json OR OLD.topic_revision IS NOT NEW.topic_revision
  OR OLD.is_forgotten IS NOT NEW.is_forgotten OR OLD.container_tag IS NOT NEW.container_tag
  OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at OR OLD.id IS NOT NEW.id
  OR OLD.rowid IS NOT NEW.rowid
BEGIN
  DELETE FROM memory_search_projection WHERE memory_rowid = OLD.rowid AND OLD.rowid IS NOT NEW.rowid;
  DELETE FROM memory_search_projection WHERE memory_rowid = NEW.rowid
    AND NOT EXISTS(SELECT 1 FROM memory_search_projection_source WHERE memory_rowid = NEW.rowid);
  INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text)
  SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text
  FROM memory_search_projection_source WHERE memory_rowid = NEW.rowid
  ON CONFLICT(memory_rowid) DO UPDATE SET memory_id = excluded.memory_id, container_tag = excluded.container_tag,
    created_at = excluded.created_at, updated_at = excluded.updated_at, topic_revision = excluded.topic_revision,
    recallable = excluded.recallable, manual_only_marker = excluded.manual_only_marker, sm_scope = excluded.sm_scope,
    index_v1 = excluded.index_v1, recent_eligible = excluded.recent_eligible, task_project = excluded.task_project,
    task_session = excluded.task_session, task_turn = excluded.task_turn, task_part = excluded.task_part,
    source_date = excluded.source_date, index_text = excluded.index_text;
END;
