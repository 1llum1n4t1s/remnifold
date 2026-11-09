import test, { assert, recordMetrics } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';
import { organizationSourcePage } from '../src/organization-sources.ts';
import { adapter } from './d1-adapter.mjs';

for (const mutation of ['update', 'forget']) test(`organization sources: 55-part group tracks all members and rejects pages after ${mutation}`, async () => {
  const f = fixture();
  try {
    const saved = await f.api('/v3/documents', { containerTag: 'legacy-group-space', content: 'Multipart group final conclusion' });
    assert.equal(saved.status, 201);
    const summary = saved.body.id;
    f.db.prepare("INSERT INTO memory_organization_summaries(id,memory_id,revision,status,updated_at) VALUES ('group-summary',?,'group-r1','active','now')").run(summary);
    f.db.prepare("INSERT INTO memory_organization_versions(memory_id,summary_id) VALUES (?,'group-summary')").run(summary);
    f.db.prepare("INSERT INTO memory_organization_groups(id,prefix,session_id,turn,expected_parts,generation,status) VALUES ('group','session:1:body','session',1,55,1,'discovering')").run();
    const expected = [];
    for (let part = 1; part <= 55; part++) {
      const id = `part-${String(part).padStart(3, '0')}`;
      expected.push(id);
      const metadata = { captureVersion: 2, sessionId: 'session', turn: 1, part, parts: 55,
        captureKey: `session:1:body:${part}:part-hash`, filepath: `D:/記憶/part-${part}.jsonl` };
      f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES (?,?,'legacy-group-space','Original multipart evidence',?,'now','now')").run(id, id, JSON.stringify(metadata));
      f.db.prepare("INSERT INTO memory_organization_group_members(group_id,part,memory_id,source_revision,read_offset,total) VALUES ('group',?,?,'now',27,27)").run(part, id);
    }
    f.db.prepare("INSERT INTO memory_organization_group_summaries(group_id,summary_id) VALUES ('group','group-summary')").run();
    f.db.prepare("INSERT INTO memory_organization_dependencies(summary_id,memory_id,source_revision) VALUES ('group-summary','part-001','now')").run();
    f.db.prepare("UPDATE memory_organization_groups SET status='active' WHERE id='group'").run();
    const db = adapter(f.db);
    const ranges = [];
    db.hooks.all = (sql, args) => {
      if (!sql.includes('FROM memory_organization_group_members')) return;
      assert.ok(sql.includes('WHERE group_id=? AND part>? ORDER BY part LIMIT ?'));
      assert.ok(args[2] <= 51);
      ranges.push(args[1]);
      const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...args).map(row => row.detail);
      assert.ok(plan.some(detail => /SEARCH .* USING .*INDEX .*\(group_id=\? AND part>\?\)/u.test(detail)), JSON.stringify(plan));
      assert.ok(!plan.some(detail => /SCAN |TEMP B-TREE/u.test(detail)), JSON.stringify(plan));
    };
    const first = await (await organizationSourcePage(db, summary, undefined, 5)).json();
    assert.equal(first.sources.length, 5); assert.equal(first.hasMore, true);
    const found = [...first.sources];
    let cursor = first.nextCursor;
    for (let pages = 0; ; pages++) {
      assert.ok(pages < 5);
      const response = await organizationSourcePage(db, summary, cursor);
      assert.equal(response.status, 200);
      const page = await response.json();
      found.push(...page.sources);
      if (!page.hasMore) break;
      cursor = page.nextCursor;
    }
    assert.deepEqual(found.map(source => source.id), ['part-001', ...expected]);
    assert.equal(found[0].sourceKind, 'document');
    assert.ok(found.slice(1).every((source, index) => source.sourceKind === 'capturePart' && source.groupId === 'group' && source.part === index + 1));
    assert.ok(found.every(source => source.containerTag === 'legacy-group-space' && source.filepath === `D:/記憶/part-${Number(source.id.slice(-3))}.jsonl`));
    assert.deepEqual(ranges, [0,4,54]);
    const forged = JSON.parse(decodeURIComponent(first.nextCursor));
    forged[6] = 'foreign-group';
    assert.equal((await organizationSourcePage(db, summary, encodeURIComponent(JSON.stringify(forged)))).status, 409);
    for (const id of ['part-001', 'part-021', 'part-055']) assert.equal((await f.api(`/v3/documents/${id}`, undefined, 'GET')).status, 200);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id LIKE ?').get('part-%').n, 55);
    if (mutation === 'update') f.db.prepare("UPDATE memories SET content='Changed part evidence',updated_at='later' WHERE id='part-021'").run();
    else f.db.prepare("UPDATE memories SET is_forgotten=1 WHERE id='part-021'").run();
    assert.equal(f.db.prepare("SELECT status FROM memory_organization_summaries WHERE id='group-summary'").get().status, 'invalid');
    assert.equal((await organizationSourcePage(db, summary, first.nextCursor)).status, 409);
    assert.equal((await organizationSourcePage(db, summary)).status, 409);
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id LIKE ?').get('part-%').n, 0);
    assert.equal((await f.api('/v3/documents/part-055', undefined, 'GET')).status, 200);
    assert.equal((await f.api('/v3/documents/part-021', undefined, 'GET')).status, mutation === 'forget' ? 404 : 200);
  } finally { await f.drain(); f.db.close(); }
});

test('organization sources: summary to checkpoint hierarchy preserves exact identities and revision guards', async () => {
  const f = fixture();
  try {
    const save = async (content, containerTag = 'memories') => {
      const response = await f.api('/v3/documents', { containerTag, content });
      assert.equal(response.status, 201); await f.drain(); return response.body.id;
    };
    const summary = await save('Summary final conclusion');
    const checkpoint = await save('Old checkpoint');
    const legacy = await save('Exact original evidence', 'legacy/source-space');
    const generated = await save('Old generated summary version');
    f.db.prepare('UPDATE memories SET metadata_json=? WHERE id=?').run(JSON.stringify({ sm_organization: true, sm_organization_manual_only: true, filepath: 'D:/記憶/shared.md' }), legacy);
    const revision = id => f.db.prepare('SELECT COALESCE(topic_revision,updated_at) revision FROM memories WHERE id=?').get(id).revision;
    f.db.prepare("INSERT INTO memory_organization_summaries VALUES ('summary',?,'r1','active','now')").run(summary);
    f.db.prepare("INSERT INTO memory_organization_versions VALUES (?,'summary')").run(summary);
    f.db.prepare("INSERT INTO memory_organization_versions VALUES (?,'summary')").run(generated);
    for (const id of [legacy, checkpoint, generated]) f.db.prepare("INSERT INTO memory_organization_dependencies VALUES ('summary',?,?)").run(id, revision(id));
    f.db.prepare("INSERT INTO memory_organization_coverage(memory_id,source_revision,status) VALUES (?,?,'active')").run(legacy, revision(legacy));
    f.db.prepare("INSERT INTO memory_organization_coverage_summaries VALUES (?,'summary')").run(legacy);
    f.db.prepare("INSERT INTO memory_consolidation_projects(project_key,source_container_tag,created_at,updated_at) VALUES ('project','memories','now','now')").run();
    f.db.prepare("INSERT INTO memory_consolidations VALUES ('checkpoint','project',?,1,'active','now','now')").run(checkpoint);
    f.db.prepare("INSERT INTO memory_organization_checkpoints VALUES ('summary','checkpoint')").run();
    for (const id of [legacy, summary]) f.db.prepare("INSERT INTO memory_consolidation_sources VALUES ('checkpoint',?,?)").run(id, revision(id));
    const db = adapter(f.db);
    const first = await (await organizationSourcePage(db, summary, undefined, 1)).json();
    assert.equal(first.hasMore, true);
    const complete = await (await organizationSourcePage(db, summary)).json();
    assert.deepEqual(complete.sources.map(source => source.id).sort(), [legacy, checkpoint].sort());
    assert.equal(complete.hasMore, false);
    assert.equal(complete.sources.find(source => source.id === checkpoint).sourceKind, 'checkpoint');
    assert.equal(complete.sources.find(source => source.id === legacy).sourceKind, 'document');
    assert.equal(complete.sources.find(source => source.id === legacy).filepath, 'D:/記憶/shared.md');
    const child = await (await organizationSourcePage(db, checkpoint, undefined, 1)).json();
    assert.equal(child.hasMore, true);
    assert.ok(child.sources.every(source => source.sourceKind === 'document'));
    assert.ok(!complete.sources.some(source => source.id === generated));
    assert.equal(f.db.prepare('SELECT COUNT(*) n FROM memory_organization_hidden WHERE memory_id=?').get(legacy).n, 1);
    assert.deepEqual((await f.api(`/v3/documents/${legacy}`, undefined, 'GET')).body.containerTags, ['legacy/source-space']);
    assert.equal(await organizationSourcePage(db, legacy), null);
    assert.equal((await organizationSourcePage(db, generated)).status, 409);
    assert.equal(await organizationSourcePage(db, 'unknown-document'), null);
    assert.equal((await organizationSourcePage(db, summary, '%not-json')).status, 400);
    const foreign = JSON.parse(decodeURIComponent(first.nextCursor)); foreign[1] = 'foreign';
    assert.equal((await organizationSourcePage(db, summary, encodeURIComponent(JSON.stringify(foreign)))).status, 409);
    f.db.prepare("UPDATE memory_consolidations SET revision=2 WHERE id='checkpoint'").run();
    assert.equal((await organizationSourcePage(db, checkpoint, child.nextCursor)).status, 409);
    f.db.prepare("UPDATE memory_consolidations SET status='superseded' WHERE id='checkpoint'").run();
    assert.equal((await organizationSourcePage(db, checkpoint)).status, 409);
    f.db.prepare("UPDATE memory_organization_summaries SET revision='r2' WHERE id='summary'").run();
    assert.equal((await organizationSourcePage(db, summary, first.nextCursor)).status, 409);
    f.db.prepare("UPDATE memory_organization_summaries SET status='invalid' WHERE id='summary'").run();
    assert.equal((await organizationSourcePage(db, summary)).status, 409);
    assert.ok(complete.sources.every(source => !('content' in source) && !('metadata' in source)));
    f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES ('impostor','impostor','memories','Not a checkpoint','{\"sm_consolidation\":true}','now','now')").run();
    assert.equal(await organizationSourcePage(db, 'impostor'), null);
  } finally { await f.drain(); f.db.close(); }
});

test('organization sources: exact filepath provenance and escaped JSON byte budget preserve pagination', async () => {
  const f = fixture();
  try {
    const saved = await f.api('/v3/documents', { containerTag: 'memories', content: 'File provenance summary' });
    assert.equal(saved.status, 201);
    const id = saved.body.id;
    f.db.prepare("INSERT INTO memory_organization_summaries VALUES ('path-summary',?,'r1','active','now')").run(id);
    const controlPath = '\u0001'.repeat(4090) + '😀';
    const paths = [controlPath, controlPath, 'D:/別の記憶/😀.md', ...Array(8).fill(controlPath),
      'x'.repeat(4097), '\u0000' + 'x'.repeat(20000), null, false, '', '  ', undefined];
    for (let n = 0; n < paths.length; n++) {
      const sourceId = `path-${String(n).padStart(3, '0')}`;
      const metadata = { filepath: paths[n], sm_organization: true, sm_organization_manual_only: true };
      f.db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at) VALUES (?,?,'memories','Path evidence',?,'now','now')").run(sourceId, sourceId, JSON.stringify(metadata));
      f.db.prepare("INSERT INTO memory_organization_dependencies VALUES ('path-summary',?,'now')").run(sourceId);
    }
    const db = adapter(f.db);
    const sources = [];
    let cursor;
    let pages = 0;
    for (;;) {
      const response = await organizationSourcePage(db, id, cursor);
      assert.equal(response.status, 200);
      const body = await response.text();
      assert.ok(new TextEncoder().encode(body).length <= 64 * 1024);
      const page = JSON.parse(body);
      sources.push(...page.sources);
      pages++;
      assert.ok(pages < 20);
      if (!page.hasMore) break;
      assert.ok(page.nextCursor !== cursor);
      cursor = page.nextCursor;
    }
    assert.ok(pages > 1, 'the byte cap must split a page below its item limit');
    assert.deepEqual(sources.map(source => source.id), paths.map((_, n) => `path-${String(n).padStart(3, '0')}`));
    for (let n = 0; n < paths.length; n++) {
      const path = paths[n];
      if (typeof path === 'string' && path.length > 4096) {
        assert.equal(sources[n].filepathTruncated, true);
        assert.equal(sources[n].filepath, undefined);
      } else if (typeof path === 'string' && path.trim()) assert.equal(sources[n].filepath, path);
      else { assert.equal(sources[n].filepath, undefined); assert.equal(sources[n].filepathTruncated, undefined); }
    }
  } finally { await f.drain(); f.db.close(); }
});

test('organization sources: 50000 unrelated documents do not affect direct or 12005-member checkpoint keysets', { timeout: 120000 }, async () => {
  const f = fixture();
  try {
    const summaryResponse = await f.api('/v3/documents', { containerTag: 'memories', content: 'Large source summary' });
    const checkpointResponse = await f.api('/v3/documents', { containerTag: 'memories', content: 'Large checkpoint' });
    assert.equal(summaryResponse.status, 201); assert.equal(checkpointResponse.status, 201);
    const summary = summaryResponse.body.id;
    const checkpoint = checkpointResponse.body.id;
    f.db.prepare("INSERT INTO memory_organization_summaries VALUES ('large-summary',?,'r1','active','now')").run(summary);
    f.db.exec(`WITH RECURSIVE n(v) AS (VALUES(0) UNION ALL SELECT v+1 FROM n WHERE v<12004)
      INSERT INTO memories(id,custom_id,container_tag,content,metadata_json,created_at,updated_at)
      SELECT printf('z-%05d',v),printf('z-%05d',v),'memories','Original evidence','{"filepath":"D:/large/source.md","sm_organization":true,"sm_organization_manual_only":true}','now','now' FROM n;
      WITH RECURSIVE n(v) AS (VALUES(0) UNION ALL SELECT v+1 FROM n WHERE v<49999)
      INSERT INTO memories(id,custom_id,container_tag,content,created_at,updated_at)
      SELECT printf('unrelated-%05d',v),printf('unrelated-%05d',v),'memories','Unrelated evidence','now','now' FROM n;
      INSERT INTO memory_consolidation_projects(project_key,source_container_tag,created_at,updated_at) VALUES ('large-project','memories','now','now');`);
    f.db.prepare("INSERT INTO memory_consolidations VALUES ('large-cp','large-project',?,1,'active','now','now')").run(checkpoint);
    const checkpointRevision = f.db.prepare('SELECT COALESCE(topic_revision,updated_at) revision FROM memories WHERE id=?').get(checkpoint).revision;
    f.db.prepare("INSERT INTO memory_organization_dependencies VALUES ('large-summary',?,?)").run(checkpoint, checkpointRevision);
    f.db.exec("INSERT INTO memory_consolidation_sources SELECT 'large-cp',id,'now' FROM memories WHERE id>='z-00000';");
    const db = adapter(f.db);
    const queries = [];
    const prepare = db.prepare.bind(db);
    db.prepare = sql => {
      const statement = prepare(sql);
      return { ...statement, bind: (...args) => {
        const bound = statement.bind(...args);
        return { ...bound, all: async () => {
          const result = await bound.all();
          queries.push({ sql, args, rows: result.results.length });
          return result;
        } };
      } };
    };
    const direct = await (await organizationSourcePage(db, summary)).json();
    assert.equal(direct.hasMore, false);
    assert.deepEqual(direct.sources.map(source => [source.id,source.sourceKind]), [[checkpoint,'checkpoint']]);
    assert.ok(queries.reduce((sum, query) => sum + query.rows,0) <= 2);
    const ids = [];
    let cursor;
    for (let pages = 0; ; pages++) {
      assert.ok(pages < 250);
      const response = await organizationSourcePage(db, checkpoint, cursor);
      assert.equal(response.status, 200);
      const page = await response.json();
      assert.ok(page.sources.length <= 50);
      assert.ok(page.sources.every(source => source.filepath === 'D:/large/source.md' && source.sourceKind === 'document'));
      ids.push(...page.sources.map(source => source.id));
      if (!page.hasMore) break;
      assert.ok(page.nextCursor !== cursor); cursor = page.nextCursor;
    }
    assert.deepEqual(ids, Array.from({ length: 12005 },(_,n) => `z-${String(n).padStart(5,'0')}`));
    assert.ok(queries.every(query => query.rows <= 51));
    const membership = queries.filter(query => query.sql.includes('FROM memory_consolidation_sources'));
    assert.equal(membership.length,241);
    assert.equal(membership.reduce((sum,query) => sum + query.rows,0),12245);
    for (const query of [queries.find(query => query.sql.includes('FROM memory_organization_dependencies')),membership[0]]) {
      const plan = f.db.prepare(`EXPLAIN QUERY PLAN ${query.sql}`).all(...query.args).map(row => row.detail);
      assert.ok(plan.some(detail => /SEARCH .* USING .*INDEX .*\((?:summary_id|consolidation_id)=\? AND memory_id>\?\)/u.test(detail)),JSON.stringify(plan));
      assert.ok(!plan.some(detail => /SCAN |TEMP B-TREE/u.test(detail)),JSON.stringify(plan));
    }
    recordMetrics('organization-source-association-keysets',{unrelatedDocuments:50000,directPages:1,checkpointPages:membership.length,memberRows:membership.reduce((sum,q)=>sum+q.rows,0),maxRows:Math.max(...queries.map(q=>q.rows))});
    const first = await (await organizationSourcePage(db, checkpoint, undefined,5)).json();
    f.db.prepare("UPDATE memories SET content='Changed evidence',updated_at='later' WHERE id='z-00021'").run();
    assert.equal((await organizationSourcePage(db, checkpoint,first.nextCursor)).status,409);
    f.db.prepare("UPDATE memories SET is_forgotten=1 WHERE id='z-00000'").run();
    assert.equal((await organizationSourcePage(db, checkpoint)).status,409);
  } finally { await f.drain(); f.db.close(); }
});
