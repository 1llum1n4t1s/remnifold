-- Shape only: organization additionally proves the exact captured body hash.
ALTER TABLE memory_organization_progress ADD COLUMN defer_reason TEXT;
ALTER TABLE memory_organization_coverage ADD COLUMN capture_verified_revision TEXT;
CREATE VIEW memory_organization_single_part_v2 AS
SELECT a.id FROM memory_organization_capture_identities a WHERE EXISTS(SELECT 1 FROM memories m WHERE m.id=a.id AND json_type(m.metadata_json,'$.captureVersion')='integer' AND json_extract(m.metadata_json,'$.captureVersion')=2 AND json_type(m.metadata_json,'$.parts')='integer' AND json_extract(m.metadata_json,'$.parts')=1 AND a.part=1 AND a.turn BETWEEN 1 AND 9007199254740991 AND length(CAST(a.capture_key AS BLOB))=length(CAST(a.session_id||':'||a.turn||':' AS BLOB))+83 AND substr(CAST(a.capture_key AS BLOB),1,length(CAST(a.session_id||':'||a.turn||':' AS BLOB)))=CAST(a.session_id||':'||a.turn||':' AS BLOB) AND length(CAST(substr(CAST(a.capture_key AS BLOB),-83) AS TEXT))=83 AND CAST(substr(CAST(a.capture_key AS BLOB),-83,64) AS TEXT) NOT GLOB '*[^0-9a-f]*' AND substr(CAST(a.capture_key AS BLOB),-19,3)=x'3a313a' AND substr(CAST(a.capture_key AS BLOB),-16)=substr(CAST(a.capture_key AS BLOB),-83,16));
CREATE VIEW memory_organization_capture_blocking_conflicts AS
SELECT a.id FROM memory_organization_capture_identities a WHERE EXISTS(
 SELECT 1 FROM memory_organization_capture_identities b WHERE b.id<>a.id
 AND b.session_id=a.session_id AND b.turn=a.turn AND b.part=a.part AND b.project_key=a.project_key
 AND NOT(a.capture_key<>b.capture_key
 AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=a.id)
 AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id)))
UNION ALL SELECT id FROM memory_organization_capture_unsafe;

-- Invalidation uses the same pair rule, while the old public conflict view
-- remains strict for a Worker deployed before this migration.
DROP VIEW memory_organization_capture_conflict_summaries;
DROP VIEW memory_organization_capture_siblings;
CREATE VIEW memory_organization_capture_siblings AS
SELECT a.id AS conflict_id,b.id AS memory_id
FROM memory_organization_capture_conflicts c
JOIN memory_organization_capture_identities a ON a.id=c.id
JOIN memory_organization_capture_identities b
 ON b.session_id=a.session_id AND b.turn=a.turn AND b.part=a.part AND b.project_key=a.project_key
WHERE a.id=b.id OR NOT(a.capture_key<>b.capture_key
 AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=a.id)
 AND EXISTS(SELECT 1 FROM memory_organization_single_part_v2 v WHERE v.id=b.id))
 OR EXISTS(SELECT 1 FROM memory_organization_coverage coverage JOIN memories original ON original.id=coverage.memory_id
   WHERE coverage.memory_id=b.id AND coverage.status='active'
   AND (coverage.capture_verified_revision IS NOT coverage.source_revision
     OR coverage.source_revision IS NOT COALESCE(original.topic_revision,original.updated_at)))
UNION ALL SELECT id,id FROM memory_organization_capture_unsafe;
CREATE VIEW memory_organization_capture_conflict_summaries AS
SELECT c.conflict_id,d.summary_id FROM memory_organization_capture_siblings c
JOIN memory_organization_dependencies_all d ON d.memory_id=c.memory_id
UNION
-- A checkpoint's original capture bodies were not individually hash-verified.
SELECT a.id,p.summary_id FROM memory_organization_capture_conflicts c
JOIN memory_organization_capture_identities a ON a.id=c.id
JOIN memory_organization_capture_identities b
 ON b.session_id=a.session_id AND b.turn=a.turn AND b.part=a.part AND b.project_key=a.project_key
JOIN memory_consolidation_sources d ON d.memory_id=b.id
JOIN memory_organization_checkpoints p ON p.consolidation_id=d.consolidation_id;

DROP TRIGGER memory_organization_capture_conflict_insert;
CREATE TRIGGER memory_organization_capture_conflict_insert AFTER INSERT ON memories
WHEN NEW.is_forgotten=0 AND EXISTS(SELECT 1 FROM memory_organization_capture_conflicts WHERE id=NEW.id)
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

DROP TRIGGER memory_organization_capture_conflict_update;
CREATE TRIGGER memory_organization_capture_conflict_update AFTER UPDATE ON memories
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
