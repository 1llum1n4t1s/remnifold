-- 過去の自動統合へ混入したopt-out資料を、永続membershipから特定して除外する。
-- 原本とmembershipは保持し、eligibleな原本から初回backfillをやり直せる状態へ戻す。
UPDATE memory_consolidation_projects
SET active_consolidation_id = NULL, initial_backfill_completed = 0
WHERE project_key IN (
  SELECT c.project_key FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
);

UPDATE memories SET status = 'superseded', is_forgotten = 1, vector_status = 'pending',
  vector_mutation_id = NULL, vector_attempted_at = NULL
WHERE id IN (
  SELECT c.memory_id FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
);

-- 無効化したcheckpointだけが抑制していた原本factを既存の復旧規約で戻す。
UPDATE facts SET status = 'active'
WHERE status = 'superseded' AND container_tag IN (
  SELECT checkpoint.container_tag FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
) AND NOT EXISTS (
  SELECT 1 FROM fact_relations relation
  JOIN facts newer ON newer.id = relation.from_fact_id
  JOIN memories source ON source.id = newer.source_memory_id
  WHERE relation.to_fact_id = facts.id AND relation.relation = 'supersedes'
    AND newer.status = 'active' AND source.is_forgotten = 0
);

DELETE FROM facts WHERE source_memory_id IN (
  SELECT c.memory_id FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
);

DELETE FROM memory_topics WHERE memory_id IN (
  SELECT c.memory_id FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
);

UPDATE memory_consolidations SET status = 'invalid'
WHERE id IN (
  SELECT c.id FROM memory_consolidations c
  JOIN memories checkpoint ON checkpoint.id = c.memory_id
  WHERE c.status = 'active' AND (
    json_extract(checkpoint.metadata_json, '$.memoryIndex.recallable') IS 0 OR EXISTS (
      SELECT 1 FROM memory_consolidation_sources s JOIN memories m ON m.id = s.memory_id
      WHERE s.consolidation_id = c.id AND json_extract(m.metadata_json, '$.memoryIndex.recallable') IS 0
    )
  )
);
