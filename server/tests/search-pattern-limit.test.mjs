// D1 は LIKE/GLOB のパターンを UTF-8 で 50 バイトまでに制限し、超えると検索全体が失敗する。
// 手元の SQLite はこの上限を同じように扱うとは限らないため、束縛の時点で D1 と同じ失敗を再現する。
import test, { assert } from './support/test.mjs';
import { fixture } from './worker-fixture.mjs';
import { buildMemoryIndex } from '../src/shared/memory-index.ts';

const LIMIT = 50;
const bytes = (value) => new TextEncoder().encode(value).length;

// GLOB を含む文の束縛値を集め、上限を超えるパターンがあれば D1 と同じエラーにする。
function enforceD1PatternLimit(f) {
  const statements = [];
  f.DB.hooks.all = (sql, values) => {
    if (!sql.includes(' GLOB ?')) return;
    const patterns = values.filter((value) => typeof value === 'string' && value.startsWith('*') && value.endsWith('*'));
    statements.push({ sql, patterns });
    if (patterns.some((pattern) => bytes(pattern) > LIMIT)) throw Error('D1_ERROR: LIKE or GLOB pattern too complex: SQLITE_ERROR');
  };
  return statements;
}

async function search(f, statements, q) {
  statements.length = 0;
  const response = await f.api('/v4/search', { allContainers: true, q, indexOnly: true });
  assert.equal(response.status, 200, JSON.stringify(response.body));
  assert.ok(statements.length > 0, `no GLOB statement for ${q}`);
  // 重みの項は `ELSE 0 END`。優先順位の式（`THEN 0 ELSE 1`）とは区別する。
  const weighted = statements.find(({ sql }) => /GLOB \? THEN \d+ ELSE 0 END/.test(sql));
  assert.ok(weighted, `no weighted GLOB statement for ${q}`);
  for (const { patterns } of statements) assert.deepEqual(patterns, weighted.patterns);
  const weights = [...weighted.sql.matchAll(/GLOB \? THEN (\d+) ELSE 0 END/g)].map((match) => Number(match[1]));
  assert.equal(weights.length, weighted.patterns.length);
  return { response, patterns: weighted.patterns, weights };
}

test('search index patterns stay within the D1 LIKE/GLOB byte limit', async () => {
  const f = fixture();
  try {
    const ascii = 'abcdefghij'.repeat(8);
    const saved = await f.api('/v3/documents', { containerTag: 'space-a', content: `# long\n${ascii}`,
      metadata: { title: ascii, type: 'conversation', memoryIndex: buildMemoryIndex({ title: ascii, request: ascii, sourceUpdatedAt: '2026-10-01T00:00:00Z' }) } });
    assert.equal(saved.status, 201, JSON.stringify(saved.body));
    await f.drain();
    const statements = enforceD1PatternLimit(f);

    // 上限以内の語は従来どおりの文面。ちょうど 50 バイトは切らない。
    const short = await search(f, statements, 'alpha привет');
    assert.deepEqual(short.patterns, ['*alpha*', '*[пП][рР][иИ][вВ][еЕ][тТ]*']);
    assert.deepEqual(short.weights, [5, 6]);
    assert.deepEqual((await search(f, statements, 'a'.repeat(48))).patterns, [`*${'a'.repeat(48)}*`]);
    assert.deepEqual((await search(f, statements, 'a'.repeat(49))).patterns, [`*${'a'.repeat(48)}*`]);

    // 長い ASCII の語は先頭部分の一致になり、保存した記録は引き続き見つかる（索引一致・全文・本文の各経路）。
    const long = await search(f, statements, ascii);
    assert.deepEqual(long.patterns, [`*${ascii.slice(0, 48)}*`]);
    assert.deepEqual(long.weights, [12]);
    assert.deepEqual(long.response.body.results.map((row) => row.id), [saved.body.id]);
    assert.ok(statements.some(({ sql }) => sql.includes('WITH fts AS')), 'full-text path');
    await search(f, statements, 'z'.repeat(80));
    assert.ok(statements.some(({ sql }) => sql.includes(') AS lexical')), 'substring path');

    // 長い日本語の複合語は、文字の途中で切らずに先頭部分を残す。
    // 隣り合う 11 文字と 9 文字の語が 20 文字（60 バイト）の複合語になり、3 バイト文字の 16 文字まで残る。
    const japanese = 'インフラストラクチャーコミュニケーション';
    const compound = await search(f, statements, japanese);
    assert.deepEqual(compound.patterns, [`*${japanese.slice(0, 16)}*`]);
    assert.deepEqual(compound.weights, [12]);
    assert.equal(bytes(compound.patterns[0]), LIMIT);

    // 非 ASCII の大小文字は文字クラスごと残すか落とすかで、`[..]` を分断しない。
    const cyrillic = await search(f, statements, 'программирование'.repeat(2));
    assert.deepEqual(cyrillic.patterns, ['*[пП][рР][оО][гГ][рР][аА][мМ][мМ]*']);
    assert.deepEqual(cyrillic.weights, [12]);
    // 次の文字クラスが入り切らなければ、上限に余りがあってもそこで止める（後ろの短い文字は詰めない）。
    const mixed = await search(f, statements, `${'a'.repeat(45)}яz`);
    assert.deepEqual(mixed.patterns, [`*${'a'.repeat(45)}*`]);
    // 3 通りの大小文字を持つ文字（θ）も同じ。
    const theta = await search(f, statements, 'θ'.repeat(30));
    assert.deepEqual(theta.patterns, [`*${'[θΘϴ]'.repeat(6)}*`]);
  } finally { await f.drain(); f.db.close(); }
});
