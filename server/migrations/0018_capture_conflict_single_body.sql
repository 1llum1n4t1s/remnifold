-- 競合トリガー2本（memory_organization_capture_conflict_insert / _update）の本文を、1つの INSTEAD OF トリガーへ集約し、
-- 本文が参照するビューの入れ子を平らに書き直す。結果は 0017 までと同じである（例外は、下の「トリガーの入れ子の深さ」の境界だけ）。
--
-- 理由: SQLite は INSERT / UPDATE を準備（コンパイル）するとき、発火し得るトリガーの本文を文ごとに展開する。
-- 本文7文は memory_organization_capture_siblings と memory_organization_capture_conflict_summaries を計8回参照し、
-- どちらもビューの入れ子（conflicts → siblings → conflict_summaries）として展開される。upsert は本文を約3回展開するため、
-- 競合が無い通常の保存でも、準備の命令数の約93〜95%がこの2本の本文の展開だった。
-- 本文を memory_organization_capture_invalidations の INSTEAD OF トリガー1つへ移すと、2本のトリガーは「識別を1行渡す」
-- INSERT 1文だけになり、本文の展開は INSERT OR ABORT の1か所に集まる。さらに、本文内の siblings・conflict_summaries を
-- 展開済みの文（下の R・S）で置き換えることで、ビューの入れ子の展開を避ける。
--
-- 本文7文は、0016 適用後の memory_organization_capture_conflict_update の本文に、次の2つの文字列置換だけを行った（生成物）。
--   SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id          → R
--   SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id → S
--   P = 同じ識別（session_id・turn・part・project_key）の peer。本文の途中で自分が忘却された場合と、peer が消えた場合の
--       2条件で、旧ビューの「conflicts に属する」を再現する。
--   R = P に、旧 siblings の組の規則（v2 の組・coverage の再評価）と unsafe の自分自身を足したもの。
--   S = 旧 conflict_summaries（依存する要約と、checkpoint の要約）の同等の文。R と P を使う。
-- R と S は旧ビューと同じく文ごとに評価し直す（一時表に固めない）。途中の source_update が coverage を無効化し、後の文の
-- 対象が減る挙動も同じになる。識別の判定は既存のビュー（identities・unsafe・single_part_v2・conflicts）だけに置き、
-- ここでは変えない。a 側の値は発火時の現在行から1回だけ読む。
--
-- 発火順: SQLite は同じ種類のトリガーを作成順の逆に発火する。INSERT は …→conflict_insert→projection_insert、
-- UPDATE は …→source_update→conflict_update→projection_update の順に作られているので、競合トリガー2本と
-- 射影トリガー2本（文面は 0014・0016 と一字も変えない）を同じ順で作り直して相対順を保つ。
-- WHEN と OF は 0010・0016 のまま。INSERT OR ABORT は旧本文の文の失敗と同じく、本文の失敗で元の文を失敗させる。
-- OR ABORT が旧本文と同じ結果になる前提: 本文が触る表・列に ON CONFLICT 句が無く、トリガー本文に OR IGNORE / OR REPLACE が無い
-- （元の文の競合解決は本文の文へ引き継がれるため。足すときは見直す）。
--
-- ビューを変えず、表・列・索引・データも変えない。埋め戻しは不要で、1ファイルの適用で原子的に入る。
-- 最大の1文（INSTEAD OF トリガー）は約15,100字で、D1 の1文の上限（100KB）に収まる。
-- トリガーの入れ子の深さ: 元の文 → 競合トリガー → INSTEAD OF の本文 → 本文内の UPDATE のトリガーと続くため、
-- 要約の依存鎖がある競合に必要な深さは 3 から 4 になる。recursive_triggers が OFF なら必要な深さは一定で影響しない。
-- ON で、鎖の長さが実行環境の上限に近い場合は、通る依存鎖の境界が1段短くなる（これが「結果は同じ」の唯一の例外）。
-- 上限は実行環境の設定で、このリポジトリの検証用 workerd で実測した値は、10段のトリガーの入れ子は通り、11段で
-- "triggers nested too deep" になる。D1 の本番の値は確認していない（SQLite の既定は 1000）。
--
-- 戻し方: 次の移行で4本のトリガーを落とし、DROP VIEW memory_organization_capture_invalidations（INSTEAD OF も消える）を
-- 実行し、同じ順（conflict → projection）に再作成する。文面の出典は、insert の2本が 0010・0014、update の2本が 0016。
-- 適用済みのこの移行は消さず、戻しも追加の移行にする。
-- 制約: memories の rowid は動かさない（0016 の制約と同じ。射影トリガーの文面は変えていない）。

DROP TRIGGER IF EXISTS memory_organization_capture_conflict_insert;
DROP TRIGGER IF EXISTS memory_organization_capture_conflict_update;
DROP TRIGGER IF EXISTS memory_search_projection_memory_insert;
DROP TRIGGER IF EXISTS memory_search_projection_memory_update;

CREATE VIEW memory_organization_capture_invalidations AS
SELECT NULL AS memory_id,NULL AS session_id,NULL AS turn,NULL AS part,NULL AS project_key,
  NULL AS capture_key,NULL AS single_part_v2,NULL AS unsafe WHERE 0;

CREATE TRIGGER memory_organization_capture_invalidate
INSTEAD OF INSERT ON memory_organization_capture_invalidations
BEGIN
  DELETE FROM facts WHERE source_memory_id IN(SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.id IN(SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0)
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key)))));
  DELETE FROM memory_topics WHERE memory_id IN(SELECT s.memory_id FROM memory_organization_summaries s
    WHERE s.id IN(SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0)
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key)))));
  UPDATE memories SET is_forgotten=1,status='superseded',vector_status='pending',vector_mutation_id=NULL,vector_attempted_at=NULL
    WHERE id IN(SELECT s.memory_id FROM memory_organization_summaries s
      WHERE s.id IN(SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0)
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key)))))
      AND (is_forgotten<>1 OR status<>'superseded' OR vector_status<>'pending' OR vector_mutation_id IS NOT NULL OR vector_attempted_at IS NOT NULL);
  UPDATE memory_organization_summaries SET status='invalid' WHERE status<>'invalid'
    AND id IN(SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0)
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))));
  UPDATE memory_organization_coverage SET status='invalid'
    WHERE memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0);
  DELETE FROM memory_organization_progress WHERE memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0) AND COALESCE(defer_reason,'')<>'capture-hash-unverifiable';
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
    WHERE (id IN(SELECT group_id FROM memory_organization_group_members WHERE memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0))
      OR id IN(SELECT group_id FROM memory_organization_group_summaries WHERE summary_id IN(SELECT d.summary_id FROM memory_organization_dependencies_all d WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))
      AND (b.id=NEW.memory_id OR NEW.single_part_v2=0
        OR NOT(NEW.capture_key<>b.capture_key
          AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
        OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage
          JOIN memories original ON original.id=coverage.memory_id
          WHERE coverage.memory_id=b.id AND coverage.status='active'
            AND (coverage.capture_verified_revision IS NOT coverage.source_revision
              OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at))))
    UNION ALL SELECT NEW.memory_id WHERE NEW.unsafe=1
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0)
    UNION SELECT p.summary_id FROM memory_consolidation_sources d
    JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id
    WHERE d.memory_id IN(SELECT b.id FROM memory_organization_capture_identities b
    WHERE b.session_id=NEW.session_id AND b.turn=NEW.turn AND b.part=NEW.part AND b.project_key=NEW.project_key
      AND (SELECT self.is_forgotten FROM memories self WHERE self.id=NEW.memory_id)=0
      AND (b.id<>NEW.memory_id OR EXISTS(SELECT 1 FROM memory_organization_capture_identities p
        WHERE p.id<>NEW.memory_id AND p.session_id=NEW.session_id AND p.turn=NEW.turn
          AND p.part=NEW.part AND p.project_key=NEW.project_key))))))
      AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
END;

CREATE TRIGGER memory_organization_capture_conflict_insert AFTER INSERT ON memories
WHEN NEW.is_forgotten=0 AND EXISTS(SELECT 1 FROM memory_organization_capture_conflicts WHERE id=NEW.id)
BEGIN
  INSERT OR ABORT INTO memory_organization_capture_invalidations(
    memory_id,session_id,turn,part,project_key,capture_key,single_part_v2,unsafe)
  SELECT a.id,a.session_id,a.turn,a.part,a.project_key,a.capture_key,
    EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=a.id),0
  FROM memory_organization_capture_identities a WHERE a.id=NEW.id
  UNION ALL SELECT id,NULL,NULL,NULL,NULL,NULL,0,1
  FROM memory_organization_capture_unsafe WHERE id=NEW.id;
END;

CREATE TRIGGER memory_organization_capture_conflict_update AFTER UPDATE OF content, metadata_json, container_tag, is_forgotten ON memories
WHEN NEW.is_forgotten=0 AND (OLD.content IS NOT NEW.content OR OLD.metadata_json IS NOT NEW.metadata_json OR OLD.container_tag IS NOT NEW.container_tag OR OLD.is_forgotten IS NOT NEW.is_forgotten)
  AND EXISTS(SELECT 1 FROM memory_organization_capture_conflicts WHERE id=NEW.id)
BEGIN
  INSERT OR ABORT INTO memory_organization_capture_invalidations(
    memory_id,session_id,turn,part,project_key,capture_key,single_part_v2,unsafe)
  SELECT a.id,a.session_id,a.turn,a.part,a.project_key,a.capture_key,
    EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=a.id),0
  FROM memory_organization_capture_identities a WHERE a.id=NEW.id
  UNION ALL SELECT id,NULL,NULL,NULL,NULL,NULL,0,1
  FROM memory_organization_capture_unsafe WHERE id=NEW.id;
END;

CREATE TRIGGER memory_search_projection_memory_insert AFTER INSERT ON memories
BEGIN
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
