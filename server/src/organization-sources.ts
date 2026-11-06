// Pages follow the evidence associations themselves; unrelated memories are never scanned.
type Root = { id: string; revision: string | number; status: string; forgotten: number };
type Kind = 'summary' | 'checkpoint';
type Phase = 'direct' | 'group' | 'checkpoint';
type Position = { phase: Phase; group: string; after: string };
type Member = { id: string; source_revision: string; part?: number; read_offset?: number; total?: number | null };
type Row = { id: string; containerTag: string; createdAt: string; updatedAt: string; revision: string; forgotten: number; generated: number; checkpoint: number; filepath: string | null; filepathTruncated: number };
type Source = { id: string; containerTag: string; createdAt: string; updatedAt: string; sourceKind: 'document' | 'checkpoint' | 'capturePart'; groupId?: string; part?: number; filepath?: string; filepathTruncated?: true };
const restart = () => Response.json({ error: "Organization sources changed; retrieve the summary again." }, { status: 409 });
const invalid = () => Response.json({ error: "Invalid organization source cursor." }, { status: 400 });
const summarySql = `SELECT s.id,s.revision,s.status,m.is_forgotten AS forgotten FROM memory_organization_summaries s JOIN memories m ON m.id=s.memory_id WHERE s.memory_id=?`;
const checkpointSql = `SELECT c.id,c.revision,c.status,m.is_forgotten AS forgotten FROM memory_consolidations c JOIN memories m ON m.id=c.memory_id WHERE c.memory_id=?`;

export async function organizationSourcePage(db: D1Database, documentId: string, cursor?: string, limit = 50): Promise<Response | null> {
  let root = await db.prepare(summarySql).bind(documentId).first<Root>();
  let kind: Kind = 'summary';
  if (!root) {
    if (await db.prepare('SELECT memory_id FROM memory_organization_versions WHERE memory_id=?').bind(documentId).first()) return restart();
    root = await db.prepare(checkpointSql).bind(documentId).first<Root>();
    kind = 'checkpoint';
  }
  if (!root) return null;
  if (root.status !== 'active' || root.forgotten) return restart();
  const revision = kind === 'summary' ? String(root.revision) : `checkpoint:${root.id}:${root.revision}`;
  let position: Position = { phase: kind === 'summary' ? 'direct' : 'checkpoint', group: '', after: '' };
  if (cursor !== undefined) {
    if (cursor.length > 2048) return invalid();
    try {
      const value: unknown = JSON.parse(decodeURIComponent(cursor));
      if (!Array.isArray(value) || value.length !== 8 || value[0] !== 2 || value.slice(1).some(item => typeof item !== 'string')) return invalid();
      if (value[1] !== documentId || value[2] !== kind || value[3] !== root.id || value[4] !== revision) return restart();
      const [, , , , , phase, group, after] = value as [number,string,string,string,string,string,string,string];
      if (!['direct','group','checkpoint'].includes(phase) || group.length > 64 || after.length > 64) return invalid();
      if (kind === 'checkpoint' ? phase !== 'checkpoint' || group !== '' : phase === 'checkpoint') return invalid();
      if (phase === 'group' ? !group || !/^\d{1,5}$/.test(after) || Number(after) > 10000 : group !== '' || !after) return invalid();
      position = { phase: phase as Phase, group, after };
    } catch { return invalid(); }
  }
  const count = Number.isFinite(limit) ? Math.max(1, Math.min(50, Math.floor(limit))) : 50;
  const sources: Source[] = [];
  const encoder = new TextEncoder();
  let bytes = 0;
  let done = false;
  let stopped = false;
  const nextGroup = async (after: string): Promise<string | null> => {
    const group = await db.prepare(`SELECT s.group_id AS id,g.status FROM memory_organization_group_summaries s
      JOIN memory_organization_groups g ON g.id=s.group_id WHERE s.summary_id=? AND s.group_id>? ORDER BY s.group_id LIMIT 1`)
      .bind(root!.id, after).first<{ id: string; status: string }>();
    if (group && group.status !== 'active') throw new Error('stale');
    return group?.id ?? null;
  };
  try {
    // Bound empty/filtered associations too. Each turn fetches at most 51 indexed keys.
    for (let turn = 0; turn < 8 && !done && !stopped && sources.length < count; turn++) {
      if (position.phase === 'group') {
        const group = await db.prepare(`SELECT g.status FROM memory_organization_group_summaries s
          JOIN memory_organization_groups g ON g.id=s.group_id WHERE s.summary_id=? AND s.group_id=?`)
          .bind(root.id, position.group).first<{ status: string }>();
        if (!group || group.status !== 'active') return restart();
      }
      const take = count - sources.length;
      const members = position.phase === 'direct'
        ? await db.prepare('SELECT memory_id AS id,source_revision FROM memory_organization_dependencies WHERE summary_id=? AND memory_id>? ORDER BY memory_id LIMIT ?')
          .bind(root.id, position.after, take + 1).all<Member>()
        : position.phase === 'checkpoint'
          ? await db.prepare('SELECT memory_id AS id,source_revision FROM memory_consolidation_sources WHERE consolidation_id=? AND memory_id>? ORDER BY memory_id LIMIT ?')
            .bind(root.id, position.after, take + 1).all<Member>()
          : await db.prepare('SELECT memory_id AS id,source_revision,part,read_offset,total FROM memory_organization_group_members WHERE group_id=? AND part>? ORDER BY part LIMIT ?')
            .bind(position.group, Number(position.after), take + 1).all<Member>();
      const candidates = members.results.slice(0, take);
      const documents = candidates.length ? await db.prepare(`SELECT m.id,m.container_tag AS containerTag,m.created_at AS createdAt,m.updated_at AS updatedAt,
        COALESCE(m.topic_revision,m.updated_at) AS revision,m.is_forgotten AS forgotten,
        EXISTS(SELECT 1 FROM memory_organization_versions v WHERE v.memory_id=m.id) AS generated,
        EXISTS(SELECT 1 FROM memory_consolidations c WHERE c.memory_id=m.id) AS checkpoint,
        CASE WHEN json_type(m.metadata_json,'$.filepath')='text' AND length(json_extract(m.metadata_json,'$.filepath'))<=4096
          AND length(CAST(json_extract(m.metadata_json,'$.filepath') AS BLOB))<=16384 THEN json_extract(m.metadata_json,'$.filepath') ELSE NULL END AS filepath,
        CASE WHEN json_type(m.metadata_json,'$.filepath')='text' AND (length(json_extract(m.metadata_json,'$.filepath'))>4096
          OR length(CAST(json_extract(m.metadata_json,'$.filepath') AS BLOB))>16384) THEN 1 ELSE 0 END AS filepathTruncated
        FROM memories m WHERE m.id IN (${candidates.map(() => '?').join(',')})`).bind(...candidates.map(member => member.id)).all<Row>() : { results: [] as Row[] };
      const rows = new Map(documents.results.map(row => [row.id, row]));
      for (const member of candidates) {
        const row = rows.get(member.id);
        if (!row || row.forgotten) return restart();
        if (!row.generated) {
          if (row.revision !== member.source_revision || position.phase === 'group' && (member.total === null || member.total === undefined || member.read_offset! < member.total)) return restart();
          const source: Source = { id: row.id, containerTag: row.containerTag, createdAt: row.createdAt, updatedAt: row.updatedAt,
            sourceKind: position.phase === 'group' ? 'capturePart' : row.checkpoint ? 'checkpoint' : 'document' };
          if (position.phase === 'group') { source.groupId = position.group; source.part = member.part!; }
          if (row.filepathTruncated || typeof row.filepath === 'string' && row.filepath.length > 4096) source.filepathTruncated = true;
          else if (typeof row.filepath === 'string' && row.filepath.trim()) source.filepath = row.filepath;
          const size = encoder.encode(JSON.stringify(source)).length + 1;
          if (bytes + size > 60 * 1024) { stopped = true; break; }
          sources.push(source); bytes += size;
        }
        position.after = position.phase === 'group' ? String(member.part) : member.id;
      }
      if (stopped) break;
      if (members.results.length > candidates.length) break;
      if (position.phase === 'checkpoint') { done = true; break; }
      const group = await nextGroup(position.phase === 'group' ? position.group : '');
      if (!group) { done = true; break; }
      position = { phase: 'group', group, after: '0' };
    }
  } catch (error) {
    if (error instanceof Error && error.message === 'stale') return restart();
    throw error;
  }
  const current = await db.prepare(kind === 'summary' ? summarySql : checkpointSql).bind(documentId).first<Root>();
  if (!current || current.id !== root.id || current.revision !== root.revision || current.status !== 'active' || current.forgotten) return restart();
  const hasMore = !done;
  const nextCursor = hasMore ? encodeURIComponent(JSON.stringify([2, documentId, kind, root.id, revision, position.phase, position.group, position.after])) : null;
  return Response.json({ protocol: 1, documentId, revision, sources, hasMore, nextCursor });
}
