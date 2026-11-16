-- 自動想起の検索で毎回すべての記憶から作り直していた派生値（索引テキスト・想起可否・直近作業キー）を、
-- 書込み時に1行ずつ保守する射影テーブルへ移す。追加のみのマイグレーションで、memories と既存の索引・トリガーは変更しない。
--
-- 背景: 本番（記憶 約9千行、分類 約3.7万行、embedding_json 平均19KB）では、1入力あたりの検索文が全行で
-- json_extract と分類の相関サブクエリを評価し、D1 時間が1秒を超えていた。D1 は1データベース1スレッドで処理するため、
-- 同時セッションが重なると 503 過負荷になる。射影後も対象行の走査は残るが、1行の読み取りが小さく、式の評価も無くなる。
--
-- 設計上の約束:
-- - 派生式の定義はビュー memory_search_projection_source だけに置く。テーブルへの埋め戻しとトリガーは必ずビューから読む。
--   式を変えるときは、このファイルを書き換えず、ビューを作り直して全行を再度埋め戻す新しいマイグレーションを追加する。
--   （server/tests/search-projection.test.mjs がビューの文面のハッシュと、旧式との値の一致を固定している）
-- - トリガーは射影テーブルにだけ書き、memories は更新しない。0009 の整理トリガーを再発火させず、再帰トリガーにも依存しない。
-- - 行は「現在の memories の状態」から作り直す（NEW の値を直接使わない）。入れ子のトリガーが同じ行を更新しても、
--   最後に走った作り直しが最新の状態を反映する。
-- - 非表示判定（memory_organization_hidden）と manual-only の登録確認は他テーブルの状態に依存するため射影に入れず、
--   検索時に評価する。
-- - memories の暗黙の rowid をキーにする。memories_fts（content_rowid='rowid'）と同じ前提で、rowid は保存中に変わらない。

-- (1) 派生式の唯一の定義。忘却済みの行は射影しない。
CREATE VIEW memory_search_projection_source AS
SELECT
  m.rowid AS memory_rowid,
  m.id AS memory_id,
  m.container_tag AS container_tag,
  m.created_at AS created_at,
  m.updated_at AS updated_at,
  m.topic_revision AS topic_revision,
  -- 自動想起の対象か（明示的な recallable=false/0 だけを除く。旧 searchConditions と同じ IS NOT 0）。
  json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0 AS recallable,
  -- manual-only の印。登録済みの生成版かどうかは検索時に memory_organization_versions で確かめる。
  json_extract(m.metadata_json, '$.sm_organization_manual_only') IS 1 AS manual_only_marker,
  -- 生の値（文字列以外もそのまま）。束縛値との比較は旧式の json_extract(...) = ? と同じ結果になる。
  json_extract(m.metadata_json, '$.sm_scope') AS sm_scope,
  -- 索引版の3値（NULL=版なし、0=整数1以外、1=整数1）。旧 memoryIndexV1Sql と同じ式。
  (json_type(m.metadata_json, '$.memoryIndex.version') = 'integer' AND
           json_extract(m.metadata_json, '$.memoryIndex.version') = 1) AS index_v1,
  -- 直近作業の対象か（旧 listDocuments の recentTasks 条件5つ）。NULL は WHERE と同じく対象外にする。
  CASE WHEN json_extract(m.metadata_json, '$.memoryIndex.recallable') IS NOT 0
    AND json_extract(m.metadata_json, '$.sm_consolidation') IS NOT 1
    AND json_extract(m.metadata_json, '$.type') = 'conversation'
    AND json_type(m.metadata_json, '$.sessionId') = 'text'
    AND length(trim(json_extract(m.metadata_json, '$.sessionId'))) > 0 THEN 1 ELSE 0 END AS recent_eligible,
  COALESCE(CASE WHEN json_type(m.metadata_json, '$.sm_project_id') = 'text'
        THEN NULLIF(trim(json_extract(m.metadata_json, '$.sm_project_id')), '') END, m.container_tag) AS task_project,
  json_extract(m.metadata_json, '$.sessionId') AS task_session,
  CASE WHEN json_type(m.metadata_json, '$.turn') = 'integer' THEN json_extract(m.metadata_json, '$.turn') ELSE 0 END AS task_turn,
  CASE WHEN json_type(m.metadata_json, '$.part') = 'integer' THEN json_extract(m.metadata_json, '$.part') ELSE 1 END AS task_part,
  -- 元の作業日時。不正な日付・非文字列は飛ばし、最後は作成日時へフォールバックする。
  COALESCE(CASE WHEN json_type(m.metadata_json, '$.memoryIndex.sourceUpdatedAt') = 'text'
      AND json_extract(m.metadata_json, '$.memoryIndex.sourceUpdatedAt') GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*'
      AND strftime('%Y-%m-%d', substr(json_extract(m.metadata_json, '$.memoryIndex.sourceUpdatedAt'), 1, 10), '+0 days') = substr(json_extract(m.metadata_json, '$.memoryIndex.sourceUpdatedAt'), 1, 10)
      THEN julianday(json_extract(m.metadata_json, '$.memoryIndex.sourceUpdatedAt')) END, CASE WHEN json_type(m.metadata_json, '$.sourceTimestamp') = 'text'
      AND json_extract(m.metadata_json, '$.sourceTimestamp') GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*'
      AND strftime('%Y-%m-%d', substr(json_extract(m.metadata_json, '$.sourceTimestamp'), 1, 10), '+0 days') = substr(json_extract(m.metadata_json, '$.sourceTimestamp'), 1, 10)
      THEN julianday(json_extract(m.metadata_json, '$.sourceTimestamp')) END, CASE WHEN json_type(m.metadata_json, '$.sourceUpdatedAt') = 'text'
      AND json_extract(m.metadata_json, '$.sourceUpdatedAt') GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*'
      AND strftime('%Y-%m-%d', substr(json_extract(m.metadata_json, '$.sourceUpdatedAt'), 1, 10), '+0 days') = substr(json_extract(m.metadata_json, '$.sourceUpdatedAt'), 1, 10)
      THEN julianday(json_extract(m.metadata_json, '$.sourceUpdatedAt')) END, CASE WHEN json_type(m.metadata_json, '$.completedAt') = 'text'
      AND json_extract(m.metadata_json, '$.completedAt') GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*'
      AND strftime('%Y-%m-%d', substr(json_extract(m.metadata_json, '$.completedAt'), 1, 10), '+0 days') = substr(json_extract(m.metadata_json, '$.completedAt'), 1, 10)
      THEN julianday(json_extract(m.metadata_json, '$.completedAt')) END,
      julianday(m.created_at)) AS source_date,
  -- 索引照合の対象（索引タイトル・保存タイトル・説明・節・現行改訂の分類）。旧 indexHaystackSql('m') と文字どおり同じ式。
  replace(lower(COALESCE(json_extract(m.metadata_json, '$.memoryIndex.title'), '') || ' ' ||
                      COALESCE(json_extract(m.metadata_json, '$.title'), '') || ' ' ||
                      COALESCE(json_extract(m.metadata_json, '$.memoryIndex.description'), '') || ' ' ||
                      COALESCE(json_extract(m.metadata_json, '$.memoryIndex.sections'), '') || char(10) ||
                      COALESCE((SELECT group_concat(topic.topic, char(10)) FROM memory_topics AS topic
                                WHERE topic.memory_id = m.id AND topic.source_revision = m.topic_revision), '')), 'İ', 'i̇') AS index_text
FROM memories AS m
WHERE m.is_forgotten = 0;

-- (2) 射影テーブル。memories への外部キーは付けない（削除はトリガーで追従し、外部キー連鎖の順序に依存しない）。
-- sm_scope・task_session・task_turn・task_part は型宣言なし（BLOB 親和性）にして、保存時の型変換を起こさない。
-- TEXT/NUMERIC 親和性だと数値と文字列が変換され、束縛値との比較結果が旧式と変わり得る。
-- index_text は最後の列に置く。前方の列だけを読む走査（直近作業など）で長い照合対象を読まないため。
CREATE TABLE memory_search_projection (
  memory_rowid INTEGER PRIMARY KEY,
  memory_id TEXT NOT NULL UNIQUE,
  container_tag TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  topic_revision TEXT,
  recallable INTEGER NOT NULL,
  manual_only_marker INTEGER NOT NULL,
  sm_scope,
  index_v1 INTEGER,
  recent_eligible INTEGER NOT NULL,
  task_project TEXT,
  task_session,
  task_turn,
  task_part,
  source_date REAL,
  index_text TEXT NOT NULL
);

-- (3) 既存行の埋め戻し。D1 のマイグレーションは全体が1つのトランザクションなので、分割・再開の仕組みは置かない。
INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
  recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
  source_date, index_text)
SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
  recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
  source_date, index_text
FROM memory_search_projection_source WHERE 1;

-- (4) 直近作業の部分索引。大量挿入の後に作る。窓関数の PARTITION BY / ORDER BY と同じ列順で、
-- 候補の列をすべて含むため、直近作業の候補は本体を読まずにこの索引だけで作れる。
CREATE INDEX idx_memory_search_projection_recent ON memory_search_projection(
  task_project, task_session, source_date DESC, task_turn DESC, task_part, created_at DESC, updated_at DESC, memory_id DESC)
  WHERE recent_eligible = 1;

-- コンテナを指定した索引一致（明示的な containerTag の検索）で、従来の idx_memories_container_updated と同じく
-- そのコンテナの行だけを読むための索引。
CREATE INDEX idx_memory_search_projection_container ON memory_search_projection(container_tag);

-- (5) 保守トリガー。どれも「キーに一致する射影行を、ビューの現在の値で作り直す」2文だけを実行する。
--   (i) ビューに無くなった（忘却・削除された）行を消す。
--   (ii) ビューの行で上書きする（WHERE を付けて upsert の構文上のあいまいさを避ける）。

-- 新しい記憶。
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

-- 射影に効く列が変わった更新だけで作り直す。ベクトル・埋め込み・enrichment 状態だけの更新では発火しない。
-- rowid が変わる更新（通常は起きない）では、古い rowid の行を先に消して memory_id の一意制約と衝突させない。
CREATE TRIGGER memory_search_projection_memory_update AFTER UPDATE ON memories
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

-- 削除された記憶（hard delete・コンテナ削除の外部キー連鎖を含む）。
CREATE TRIGGER memory_search_projection_memory_delete AFTER DELETE ON memories
BEGIN
  DELETE FROM memory_search_projection WHERE memory_rowid = OLD.rowid;
END;

-- 分類は索引照合の対象に入るため、射影行と同じ改訂の分類が変わったときだけ作り直す。
-- 古い改訂の分類（再分類前後の書込み）は照合対象に入らないので作り直さない。
CREATE TRIGGER memory_search_projection_topic_insert AFTER INSERT ON memory_topics
WHEN EXISTS(SELECT 1 FROM memory_search_projection WHERE memory_id = NEW.memory_id AND topic_revision IS NEW.source_revision)
BEGIN
  DELETE FROM memory_search_projection WHERE memory_id = NEW.memory_id
    AND NOT EXISTS(SELECT 1 FROM memory_search_projection_source WHERE memory_id = NEW.memory_id);
  INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text)
  SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text
  FROM memory_search_projection_source WHERE memory_id = NEW.memory_id
  ON CONFLICT(memory_rowid) DO UPDATE SET memory_id = excluded.memory_id, container_tag = excluded.container_tag,
    created_at = excluded.created_at, updated_at = excluded.updated_at, topic_revision = excluded.topic_revision,
    recallable = excluded.recallable, manual_only_marker = excluded.manual_only_marker, sm_scope = excluded.sm_scope,
    index_v1 = excluded.index_v1, recent_eligible = excluded.recent_eligible, task_project = excluded.task_project,
    task_session = excluded.task_session, task_turn = excluded.task_turn, task_part = excluded.task_part,
    source_date = excluded.source_date, index_text = excluded.index_text;
END;

CREATE TRIGGER memory_search_projection_topic_delete AFTER DELETE ON memory_topics
WHEN EXISTS(SELECT 1 FROM memory_search_projection WHERE memory_id = OLD.memory_id AND topic_revision IS OLD.source_revision)
BEGIN
  DELETE FROM memory_search_projection WHERE memory_id = OLD.memory_id
    AND NOT EXISTS(SELECT 1 FROM memory_search_projection_source WHERE memory_id = OLD.memory_id);
  INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text)
  SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text
  FROM memory_search_projection_source WHERE memory_id = OLD.memory_id
  ON CONFLICT(memory_rowid) DO UPDATE SET memory_id = excluded.memory_id, container_tag = excluded.container_tag,
    created_at = excluded.created_at, updated_at = excluded.updated_at, topic_revision = excluded.topic_revision,
    recallable = excluded.recallable, manual_only_marker = excluded.manual_only_marker, sm_scope = excluded.sm_scope,
    index_v1 = excluded.index_v1, recent_eligible = excluded.recent_eligible, task_project = excluded.task_project,
    task_session = excluded.task_session, task_turn = excluded.task_turn, task_part = excluded.task_part,
    source_date = excluded.source_date, index_text = excluded.index_text;
END;

-- 分類行の更新は、旧行・新行のどちらかが射影行の改訂と一致すれば、両方の記憶を作り直す
-- （memory_id や改訂を書き換える更新でも、移動元と移動先の照合対象を揃える）。
CREATE TRIGGER memory_search_projection_topic_update AFTER UPDATE ON memory_topics
WHEN EXISTS(SELECT 1 FROM memory_search_projection WHERE memory_id = OLD.memory_id AND topic_revision IS OLD.source_revision)
  OR EXISTS(SELECT 1 FROM memory_search_projection WHERE memory_id = NEW.memory_id AND topic_revision IS NEW.source_revision)
BEGIN
  DELETE FROM memory_search_projection WHERE memory_id = OLD.memory_id
    AND NOT EXISTS(SELECT 1 FROM memory_search_projection_source WHERE memory_id = OLD.memory_id);
  INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text)
  SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text
  FROM memory_search_projection_source WHERE memory_id = OLD.memory_id
  ON CONFLICT(memory_rowid) DO UPDATE SET memory_id = excluded.memory_id, container_tag = excluded.container_tag,
    created_at = excluded.created_at, updated_at = excluded.updated_at, topic_revision = excluded.topic_revision,
    recallable = excluded.recallable, manual_only_marker = excluded.manual_only_marker, sm_scope = excluded.sm_scope,
    index_v1 = excluded.index_v1, recent_eligible = excluded.recent_eligible, task_project = excluded.task_project,
    task_session = excluded.task_session, task_turn = excluded.task_turn, task_part = excluded.task_part,
    source_date = excluded.source_date, index_text = excluded.index_text;
  DELETE FROM memory_search_projection WHERE memory_id = NEW.memory_id
    AND NOT EXISTS(SELECT 1 FROM memory_search_projection_source WHERE memory_id = NEW.memory_id);
  INSERT INTO memory_search_projection(memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text)
  SELECT memory_rowid, memory_id, container_tag, created_at, updated_at, topic_revision,
    recallable, manual_only_marker, sm_scope, index_v1, recent_eligible, task_project, task_session, task_turn, task_part,
    source_date, index_text
  FROM memory_search_projection_source WHERE memory_id = NEW.memory_id
  ON CONFLICT(memory_rowid) DO UPDATE SET memory_id = excluded.memory_id, container_tag = excluded.container_tag,
    created_at = excluded.created_at, updated_at = excluded.updated_at, topic_revision = excluded.topic_revision,
    recallable = excluded.recallable, manual_only_marker = excluded.manual_only_marker, sm_scope = excluded.sm_scope,
    index_v1 = excluded.index_v1, recent_eligible = excluded.recent_eligible, task_project = excluded.task_project,
    task_session = excluded.task_session, task_turn = excluded.task_turn, task_part = excluded.task_part,
    source_date = excluded.source_date, index_text = excluded.index_text;
END;
