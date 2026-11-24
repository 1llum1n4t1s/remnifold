# Data model contracts

## Worker と補強

実 workerd、移行済み SQLite、決定的 AI/ベクターで、認証済みネイティブクライアントを検証する。原子的変更、ソース/チェックポイント整合、FTS、scope、同順位、Unicode/NUL、復元/忘却の競合を保つ。待機に上限を設け、状態の片付け前に Worker を止める。

補強はリビジョンガード、プロジェクト別事実、古いトピック/ベクター拒否、更新/忘却競合、Vectorize 上限、AI 無効時の削除、遅い upsert/削除の修復を保つ。トピックは未分類と旧/共有スペースの正確な絞り込みを含む。再補強は opt-out を保ち、影響するチェックポイントを無効にする。

統合は切断の再試行、公開競合、古いチェックポイント回復、伏せ字診断を保つ。opt-out は対象数、新入力、以前の引き継ぎから除き、公開ガードと移行 0011 で汚染を防ぐ。空 profile も事実とフォールバックを scope に絞る。`server/wrangler.example.jsonc` の cron 2 件と移行 0005 を保つ。

## 整理

移行 0009 をクライアントより先に適用する。実 workerd と認証済みクライアントで prepare/lookup/sources、オーナー認可、リース/リビジョン、冪等 commit、パーティション、非公開マルチパート、重複保留、編集/忘却無効化、意味的可視性、ベクター回復を検証する。原本の ID/本文を取得でき、保存テキストは信頼しない。

生成と予算は `skills/organize-memory/` だけが持つ。日数指定は公平性カーソルを動かさず、0013 の索引を使う。0010/0012 の検証では独立キーと正確な本文ハッシュ、それ以外の保守的保留、1 時間後の公平な再試行を確かめる。capture のカウンターは新しさを証明しない。両側の短い作成済み要約の想起を保つ。

セッション検索の `sessionId(s)`、`sessionUntil`、`sessionSource` は 0015 の有無で同じ行を返す。`server/tests/search-session.test.mjs` のプラン検査を保つ。

## 移行とトリガー

- 0016 の UPDATE は `OF` 列に限定する。`WHEN` の列を足すなら `OF` にも足す。`rowid` を更新せず、再作成の発火順を保つ。`EXPLAIN` と同値結果を検査し、時間は測らない。
- 0017 の索引を `INDEXED BY` に使わない。例外は `replaceFacts` の `idx_facts_identity` だけ。
- 0018 の本体は `memory_organization_capture_invalidate` の 1 か所。識別/競合ビューを変えたら R、S、P と同値性テストも直す。文ごとに再評価し、一時表を使わず `INSERT OR ABORT` を保つ。トリガー順を変えない。逆置換テストは後のビュー変更を検出しない。深さは 4（workerd 測定上限 10、本番未確認）。
- 0019/0020 の速度索引と、射影が生きた全行にある前提を保つ。0020 は Worker より先に適用する。再試行は topic_revision/updated_at を変えず、失敗を `persistMemoryEmbedding` に流さず、確保・版・生存・埋め込みなしの条件を保つ（`embedding-retry.test.mjs`、`embedding-first.test.mjs`）。
- 戻す場合も新しい移行を追加し、適用済みを編集しない。

## トリガー検証の細部

0016（`0016_memory_trigger_update_columns.sql`）と 0017 のテストは移行の有無で結果を比べ、状態列だけの UPDATE にトリガープログラムがないことを `EXPLAIN` で確かめる。SQLite は同種トリガーを作成と逆順に発火する。

0018（`0018_capture_conflict_single_body.sql`）の識別と競合の正本は `identities`、`unsafe`、`single_part_v2`、`conflicts` ビュー。逆置換は `server/tests/capture-conflict-single-body.test.mjs` が確認する。射影トリガー2つは競合トリガーの後に再作成する。トリガーは空ビュー `memory_organization_capture_invalidations` へ識別行を1つ挿入するだけとする。

0020 の補助索引は `idx_memories_embedding_backlog`。未適用なら `retryMissingEmbeddings` だけが失敗し、照合後に再送出する。0019/0020 は追加型で、再試行表以外は速度だけに影響する。
