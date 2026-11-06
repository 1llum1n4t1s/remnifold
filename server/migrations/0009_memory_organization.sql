-- Codex supplies the conclusions; these tables retain revision-guarded evidence.
CREATE TABLE memory_organization_summaries (
  id TEXT PRIMARY KEY,
  memory_id TEXT NOT NULL UNIQUE REFERENCES memories(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','invalid')),
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_organization_summaries_recent ON memory_organization_summaries(status,updated_at DESC,id);
CREATE TABLE memory_organization_dependencies (
  summary_id TEXT NOT NULL REFERENCES memory_organization_summaries(id) ON DELETE CASCADE,
  memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  PRIMARY KEY(summary_id,memory_id)
);
CREATE TABLE memory_organization_versions (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  summary_id TEXT NOT NULL
);
-- Match native latest(): a nonempty string session, integer turn/part and
-- trimmed project ID (otherwise the original container). Capture keys are
-- optional legacy evidence and never manufacture a logical identity.
CREATE INDEX idx_organization_capture_logical ON memories(
  json_extract(metadata_json,'$.sessionId'),CAST(json_extract(metadata_json,'$.turn') AS INTEGER),CAST(json_extract(metadata_json,'$.part') AS INTEGER),
  CASE WHEN json_type(metadata_json,'$.sm_project_id')='text' AND
    trim(json_extract(metadata_json,'$.sm_project_id'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))<>''
    THEN trim(json_extract(metadata_json,'$.sm_project_id'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)) ELSE container_tag END,id
) WHERE is_forgotten=0 AND json_type(metadata_json,'$.sessionId')='text';
CREATE VIEW memory_organization_capture_identities AS
SELECT m.id,m.content,
  json_extract(m.metadata_json,'$.sessionId') AS session_id,
  CAST(json_extract(m.metadata_json,'$.turn') AS INTEGER) AS turn,CAST(json_extract(m.metadata_json,'$.part') AS INTEGER) AS part,
  CASE WHEN json_type(m.metadata_json,'$.sm_project_id')='text' AND
    trim(json_extract(m.metadata_json,'$.sm_project_id'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))<>''
    THEN trim(json_extract(m.metadata_json,'$.sm_project_id'),char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)) ELSE m.container_tag END AS project_key,
  CASE WHEN json_type(m.metadata_json,'$.captureKey')='text' AND json_extract(m.metadata_json,'$.captureKey')<>''
    THEN json_extract(m.metadata_json,'$.captureKey') END AS capture_key
FROM memories m WHERE m.is_forgotten=0
  AND json_type(m.metadata_json,'$.sm_consolidation') IS NOT 'true'
  AND json_type(m.metadata_json,'$.sessionId')='text' AND json_extract(m.metadata_json,'$.sessionId')<>''
  AND json_type(m.metadata_json,'$.turn') IN ('integer','real') AND json_type(m.metadata_json,'$.part') IN ('integer','real')
  AND CAST(json_extract(m.metadata_json,'$.turn') AS INTEGER) BETWEEN -9007199254740991 AND 9007199254740991
  AND CAST(json_extract(m.metadata_json,'$.part') AS INTEGER) BETWEEN -9007199254740991 AND 9007199254740991
  AND json_extract(m.metadata_json,'$.turn')=CAST(json_extract(m.metadata_json,'$.turn') AS INTEGER)
  AND json_extract(m.metadata_json,'$.part')=CAST(json_extract(m.metadata_json,'$.part') AS INTEGER)
  AND NOT EXISTS(SELECT 1 FROM memory_organization_versions v WHERE v.memory_id=m.id);
CREATE VIEW memory_organization_capture_unsafe AS
SELECT m.id FROM memories m WHERE m.is_forgotten=0 AND json_type(m.metadata_json,'$.sm_consolidation') IS NOT 'true'
  AND json_type(m.metadata_json,'$.sessionId')='text' AND json_extract(m.metadata_json,'$.sessionId')<>''
  AND json_type(m.metadata_json,'$.turn') IN ('integer','real') AND json_type(m.metadata_json,'$.part') IN ('integer','real')
  AND abs(CAST(json_extract(m.metadata_json,'$.turn') AS REAL))<=1.7976931348623157e308 AND abs(CAST(json_extract(m.metadata_json,'$.part') AS REAL))<=1.7976931348623157e308
  AND (json_type(m.metadata_json,'$.turn')='integer' OR CAST(json_extract(m.metadata_json,'$.turn') AS REAL)=round(CAST(json_extract(m.metadata_json,'$.turn') AS REAL),0))
  AND (json_type(m.metadata_json,'$.part')='integer' OR CAST(json_extract(m.metadata_json,'$.part') AS REAL)=round(CAST(json_extract(m.metadata_json,'$.part') AS REAL),0))
  AND (abs(CAST(json_extract(m.metadata_json,'$.turn') AS REAL))>9007199254740991 OR abs(CAST(json_extract(m.metadata_json,'$.part') AS REAL))>9007199254740991)
  AND NOT EXISTS(SELECT 1 FROM memory_organization_versions v WHERE v.memory_id=m.id);
CREATE VIEW memory_organization_capture_conflicts AS
SELECT a.id FROM memory_organization_capture_identities a WHERE EXISTS(
  SELECT 1 FROM memory_organization_capture_identities b WHERE b.id<>a.id AND b.session_id=a.session_id
    AND b.turn=a.turn AND b.part=a.part AND b.project_key=a.project_key)
UNION ALL SELECT id FROM memory_organization_capture_unsafe;
CREATE INDEX idx_organization_dependencies_memory ON memory_organization_dependencies(memory_id);
CREATE TABLE memory_organization_embedding_attempts (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  token TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE TABLE memory_organization_checkpoints (
  summary_id TEXT NOT NULL REFERENCES memory_organization_summaries(id) ON DELETE CASCADE,
  consolidation_id TEXT NOT NULL REFERENCES memory_consolidations(id) ON DELETE CASCADE,
  PRIMARY KEY(summary_id,consolidation_id)
);
CREATE INDEX idx_organization_checkpoints_consolidation ON memory_organization_checkpoints(consolidation_id);
CREATE TABLE memory_organization_groups (
  id TEXT PRIMARY KEY,
  prefix TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  anchor_memory_id TEXT REFERENCES memories(id) ON DELETE SET NULL,
  turn INTEGER NOT NULL,
  expected_parts INTEGER NOT NULL CHECK(expected_parts BETWEEN 2 AND 10000),
  generation INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'discovering' CHECK(status IN ('discovering','reading','ready','active','deferred','invalid')),
  partition_key TEXT,
  discovery_cursor TEXT NOT NULL DEFAULT '',
  memo TEXT NOT NULL DEFAULT '',
  reason TEXT,
  next_attempt TEXT NOT NULL DEFAULT '',
  batch_id TEXT,
  lease_until TEXT
);
CREATE TABLE memory_organization_group_members (
  group_id TEXT NOT NULL REFERENCES memory_organization_groups(id) ON DELETE CASCADE,
  part INTEGER NOT NULL,
  memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  read_offset INTEGER NOT NULL DEFAULT 0,
  total INTEGER,
  PRIMARY KEY(group_id,part),
  UNIQUE(group_id,memory_id)
);
CREATE INDEX idx_organization_group_members_memory ON memory_organization_group_members(memory_id,group_id);
CREATE TABLE memory_organization_group_summaries (
  group_id TEXT NOT NULL REFERENCES memory_organization_groups(id) ON DELETE CASCADE,
  summary_id TEXT NOT NULL REFERENCES memory_organization_summaries(id) ON DELETE CASCADE,
  PRIMARY KEY(group_id,summary_id)
);
CREATE INDEX idx_organization_group_summaries_summary ON memory_organization_group_summaries(summary_id,group_id);
CREATE INDEX idx_organization_capture_session ON memories(json_extract(metadata_json,'$.sessionId'),json_extract(metadata_json,'$.turn'),id);
CREATE INDEX idx_organization_group_due_ids ON memory_organization_groups(id) WHERE status IN ('discovering','reading','ready','deferred','invalid');
CREATE VIEW memory_organization_dependencies_all AS
SELECT summary_id,memory_id,source_revision FROM memory_organization_dependencies
UNION ALL
SELECT c.summary_id,s.memory_id,s.source_revision FROM memory_organization_checkpoints c
JOIN memory_consolidation_sources s ON s.consolidation_id=c.consolidation_id
UNION ALL
SELECT s.summary_id,m.memory_id,m.source_revision FROM memory_organization_group_summaries s
JOIN memory_organization_group_members m ON m.group_id=s.group_id;
CREATE VIEW memory_organization_capture_siblings AS
SELECT a.id AS conflict_id,b.id AS memory_id FROM memory_organization_capture_conflicts c
JOIN memory_organization_capture_identities a ON a.id=c.id JOIN memory_organization_capture_identities b
  ON b.session_id=a.session_id AND b.turn=a.turn AND b.part=a.part AND b.project_key=a.project_key
UNION ALL SELECT id,id FROM memory_organization_capture_unsafe;
CREATE VIEW memory_organization_capture_conflict_summaries AS
SELECT c.conflict_id,d.summary_id FROM memory_organization_capture_siblings c
JOIN memory_organization_dependencies_all d ON d.memory_id=c.memory_id;
CREATE TABLE memory_organization_coverage (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','invalid'))
);
CREATE TABLE memory_organization_coverage_summaries (
  memory_id TEXT NOT NULL REFERENCES memory_organization_coverage(memory_id) ON DELETE CASCADE,
  summary_id TEXT NOT NULL REFERENCES memory_organization_summaries(id) ON DELETE CASCADE,
  PRIMARY KEY(memory_id,summary_id)
);
CREATE TABLE memory_organization_progress (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  source_revision TEXT NOT NULL,
  next_offset INTEGER NOT NULL DEFAULT 0 CHECK(next_offset >= 0),
  memo TEXT NOT NULL DEFAULT '',
  next_attempt TEXT NOT NULL
);
CREATE INDEX idx_organization_progress_attempt ON memory_organization_progress(next_attempt,memory_id);
CREATE INDEX idx_organization_partial_ids ON memory_organization_progress(memory_id) WHERE next_offset>0;
CREATE INDEX idx_organization_deferred_ids ON memory_organization_progress(memory_id) WHERE next_offset=0;
CREATE TABLE memory_organization_batches (
  id TEXT PRIMARY KEY,
  expires_at TEXT NOT NULL,
  snapshot_json TEXT NOT NULL,
  request_json TEXT,
  response_json TEXT
);
CREATE INDEX idx_organization_pending_expiry ON memory_organization_batches(expires_at,id) WHERE response_json IS NULL;
CREATE TABLE memory_organization_leases (
  memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  batch_id TEXT NOT NULL REFERENCES memory_organization_batches(id) ON DELETE CASCADE,
  expires_at TEXT NOT NULL
);
CREATE INDEX idx_organization_leases_batch ON memory_organization_leases(batch_id);
CREATE TABLE memory_organization_batch_summaries (
  memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  batch_id TEXT NOT NULL REFERENCES memory_organization_batches(id) ON DELETE CASCADE,
  PRIMARY KEY(memory_id,batch_id)
);
CREATE INDEX idx_organization_batch_summaries_batch ON memory_organization_batch_summaries(batch_id);
CREATE TABLE memory_organization_cursor (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  last_id TEXT NOT NULL,
  next_lane INTEGER NOT NULL DEFAULT 0 CHECK(next_lane BETWEEN 0 AND 3)
);
INSERT INTO memory_organization_cursor(singleton,last_id) VALUES(1,'');
CREATE TABLE memory_organization_lane_cursors (
  kind TEXT PRIMARY KEY CHECK(kind IN ('group','partial','deferred')),
  last_id TEXT NOT NULL DEFAULT ''
);
INSERT INTO memory_organization_lane_cursors(kind) VALUES('group'),('partial'),('deferred');

-- Coverage is valid only while every required conclusion remains valid.
CREATE VIEW memory_organization_coverage_links AS
SELECT memory_id,summary_id FROM memory_organization_coverage_summaries
UNION ALL SELECT m.memory_id,s.summary_id FROM memory_organization_group_members m
JOIN memory_organization_groups g ON g.id=m.group_id AND g.status='active'
JOIN memory_organization_group_summaries s ON s.group_id=g.id;
CREATE VIEW memory_organization_complete AS
SELECT c.memory_id FROM memory_organization_coverage c
JOIN memories m ON m.id=c.memory_id AND m.is_forgotten=0
WHERE c.status='active' AND c.source_revision=COALESCE(m.topic_revision,m.updated_at)
  AND EXISTS(SELECT 1 FROM memory_organization_coverage_summaries l WHERE l.memory_id=c.memory_id)
  AND NOT EXISTS(
    SELECT 1 FROM memory_organization_coverage_summaries l
    LEFT JOIN memory_organization_summaries s ON s.id=l.summary_id
    LEFT JOIN memories sm ON sm.id=s.memory_id
    WHERE l.memory_id=c.memory_id AND (s.status IS NOT 'active' OR sm.is_forgotten IS NOT 0)
  )
UNION
SELECT member.memory_id FROM memory_organization_group_members member
JOIN memory_organization_groups g ON g.id=member.group_id AND g.status='active'
JOIN memories m ON m.id=member.memory_id AND m.is_forgotten=0
WHERE member.source_revision=COALESCE(m.topic_revision,m.updated_at)
AND EXISTS(SELECT 1 FROM memory_organization_group_summaries s WHERE s.group_id=g.id)
AND NOT EXISTS(SELECT 1 FROM memory_organization_group_summaries l
 LEFT JOIN memory_organization_summaries s ON s.id=l.summary_id LEFT JOIN memories sm ON sm.id=s.memory_id
 WHERE l.group_id=g.id AND (s.status IS NOT 'active' OR sm.is_forgotten IS NOT 0));
CREATE VIEW memory_organization_hidden AS
SELECT c.memory_id FROM memory_organization_complete c JOIN memories m ON m.id=c.memory_id
WHERE m.embedding_json IS NULL OR NOT EXISTS(
  SELECT 1 FROM memory_organization_coverage_links l
  JOIN memory_organization_summaries s ON s.id=l.summary_id JOIN memories sm ON sm.id=s.memory_id
  WHERE l.memory_id=c.memory_id AND (sm.embedding_json IS NULL OR sm.embedding_status<>'done' OR sm.vector_status<>'indexed')
);

-- A late or duplicate part invalidates a previously discovered capture set.
CREATE TRIGGER memory_organization_capture_insert AFTER INSERT ON memories
WHEN json_type(NEW.metadata_json,'$.captureKey')='text'
BEGIN
  DELETE FROM facts WHERE source_memory_id IN (
    SELECT s.memory_id FROM memory_organization_summaries s JOIN memory_organization_group_summaries l ON l.summary_id=s.id
    JOIN memory_organization_groups g ON g.id=l.group_id
    WHERE g.session_id=json_extract(NEW.metadata_json,'$.sessionId') AND g.turn=json_extract(NEW.metadata_json,'$.turn')
      AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(g.prefix)+1)=g.prefix||':'
  );
  DELETE FROM memory_topics WHERE memory_id IN (
    SELECT s.memory_id FROM memory_organization_summaries s JOIN memory_organization_group_summaries l ON l.summary_id=s.id
    JOIN memory_organization_groups g ON g.id=l.group_id
    WHERE g.session_id=json_extract(NEW.metadata_json,'$.sessionId') AND g.turn=json_extract(NEW.metadata_json,'$.turn')
      AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(g.prefix)+1)=g.prefix||':'
  );
  UPDATE memories SET is_forgotten=1,status='superseded',vector_status='pending',vector_mutation_id=NULL,vector_attempted_at=NULL
  WHERE id IN (SELECT s.memory_id FROM memory_organization_summaries s JOIN memory_organization_group_summaries l ON l.summary_id=s.id
    JOIN memory_organization_groups g ON g.id=l.group_id
    WHERE g.session_id=json_extract(NEW.metadata_json,'$.sessionId') AND g.turn=json_extract(NEW.metadata_json,'$.turn')
      AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(g.prefix)+1)=g.prefix||':');
  UPDATE memory_organization_summaries SET status='invalid' WHERE id IN (
    SELECT l.summary_id FROM memory_organization_group_summaries l JOIN memory_organization_groups g ON g.id=l.group_id
    WHERE g.session_id=json_extract(NEW.metadata_json,'$.sessionId') AND g.turn=json_extract(NEW.metadata_json,'$.turn')
      AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(g.prefix)+1)=g.prefix||':');
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
  WHERE session_id=json_extract(NEW.metadata_json,'$.sessionId') AND turn=json_extract(NEW.metadata_json,'$.turn')
    AND substr(json_extract(NEW.metadata_json,'$.captureKey'),1,length(prefix)+1)=prefix||':';
END;
-- Do not cascade from registry/group changes back into derived memories.
-- Workerd bounds trigger depth. Original mutations invalidate their direct
-- dependencies below; captureGroup repairs adjacent stale groups lazily, and
-- the reset batch invalidates every remaining summary before dropping links.

-- Do not rely on recursive_triggers or cascading-FK trigger order. Invalidate
-- dependencies before deleting a source, and ignore vector/enrichment bookkeeping.
CREATE TRIGGER memory_organization_source_update AFTER UPDATE ON memories
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
CREATE TRIGGER memory_organization_source_delete BEFORE DELETE ON memories
BEGIN
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
  WHERE (id IN (SELECT group_id FROM memory_organization_group_members WHERE memory_id=OLD.id)
    OR id IN (SELECT l.group_id FROM memory_organization_group_summaries l JOIN memory_organization_summaries s ON s.id=l.summary_id WHERE s.memory_id=OLD.id)
    OR id IN (SELECT l.group_id FROM memory_organization_group_summaries l JOIN memory_organization_dependencies_all d ON d.summary_id=l.summary_id WHERE d.memory_id=OLD.id))
    AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
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
END;

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
  DELETE FROM memory_organization_progress WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id);
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
    WHERE (id IN(SELECT group_id FROM memory_organization_group_members WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id))
      OR id IN(SELECT group_id FROM memory_organization_group_summaries WHERE summary_id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id)))
      AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
END;
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
  DELETE FROM memory_organization_progress WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id);
  UPDATE memory_organization_groups SET status='invalid',generation=generation+1,memo='',reason=NULL,batch_id=NULL,lease_until=NULL
    WHERE (id IN(SELECT group_id FROM memory_organization_group_members WHERE memory_id IN(SELECT memory_id FROM memory_organization_capture_siblings WHERE conflict_id=NEW.id))
      OR id IN(SELECT group_id FROM memory_organization_group_summaries WHERE summary_id IN(SELECT summary_id FROM memory_organization_capture_conflict_summaries WHERE conflict_id=NEW.id)))
      AND substr(COALESCE(batch_id,''),1,6)<>'reset:';
END;
