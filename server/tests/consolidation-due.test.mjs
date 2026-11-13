// 手動 consolidate（force なし）の実行時期判定を確認する。
// 判定は集計結果の行に対する純関数で、プロジェクトの集計を取り直さずに行う。
// 判定条件（DESIGN.md の統合スケジュール）:
// - 未完了の初回バックフィル
// - 未統合20件以上
// - 前回成功（なければ最古の未統合）から3日経過
// これまで not_due を返す経路のテストが無かったため、project: と container: の両方のキーで、
// 実行されない場合と実行される各条件を固定する。条件ごとに独立した fixture（テストケース）を使い、
// 前の実行結果（統合済み状態）を次の判定へ持ち込まない。
import test, {assert} from './support/test.mjs';
import {fixture} from './worker-fixture.mjs';

const old = new Date(Date.now() - 4 * 24 * 60 * 60 * 1000).toISOString();
const recent = () => new Date().toISOString();

async function saveMemories(f, count, { containerTag, projectId }) {
  f.env.AI_ENRICHMENT_MODE = 'off';
  for (let i = 0; i < count; i++) {
    const saved = await f.api('/v3/documents', { containerTag, customId: `${containerTag}-${projectId ?? 'none'}-${i}`, content: `Verified due source ${i}`,
      metadata: projectId ? { sm_project_id: projectId } : {} });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
  }
  await f.drain();
  f.env.AI_ENRICHMENT_MODE = 'on';
}

// プロジェクト行（統合の進捗）を用意する。
function projectRow(f, { projectKey, containerTag }, { backfill, lastSuccessAt = null }) {
  f.db.prepare(`INSERT INTO memory_consolidation_projects (project_key, source_container_tag, initial_backfill_completed, last_success_at, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?)`).run(projectKey, containerTag, backfill, lastSuccessAt, old, old);
}

const scenarios = [
  // 少数・最近の未統合で、初回バックフィル済みかつ前回成功も最近 → not_due。
  { name: 'recent backfilled project is not due', due: false, count: 2, project: { backfill: 1, lastSuccessAt: recent } },
  // プロジェクト行が無い（バックフィル状態が null）場合も、件数・経過時間を満たさなければ not_due。
  { name: 'missing project row without count or age is not due', due: false, count: 2 },
  { name: 'unfinished initial backfill is due', due: true, count: 2, project: { backfill: 0 } },
  { name: 'last success older than three days is due', due: true, count: 2, project: { backfill: 1, lastSuccessAt: () => old } },
  { name: 'twenty unconsolidated memories are due', due: true, count: 20, project: { backfill: 1, lastSuccessAt: recent } },
  // 前回成功が無い場合は最古の未統合を基準にする。3日を超えて古ければ due。
  { name: 'oldest unconsolidated memory older than three days is due without a previous success', due: true, count: 2, project: { backfill: 1 }, aged: true },
];

for (const legacy of [false, true]) for (const scenario of scenarios) {
  test(`manual consolidation due check: ${scenario.name} (${legacy ? 'container' : 'project'} key)`, async () => {
    const f = fixture();
    try {
      const containerTag = legacy ? 'due-space' : 'due-shared';
      const projectId = legacy ? undefined : 'due-project';
      const scope = { containerTag, projectId, projectKey: legacy ? `container:${containerTag}` : `project:${projectId}` };
      await saveMemories(f, scenario.count, scope);
      if (scenario.aged) f.db.prepare('UPDATE memories SET created_at = ?').run(old);
      if (scenario.project) projectRow(f, scope, { backfill: scenario.project.backfill, lastSuccessAt: scenario.project.lastSuccessAt?.() ?? null });
      const response = await f.api('/v4/consolidate', { projectId: legacy ? containerTag : projectId });
      assert.ok(response.status < 300, JSON.stringify(response.body));
      if (scenario.due) assert.notEqual(response.body.status, 'not_due', JSON.stringify(response.body));
      else assert.equal(response.body.status, 'not_due', JSON.stringify(response.body));
    } finally { await f.drain(); }
  });
}
