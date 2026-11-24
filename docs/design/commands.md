# Commands

## memory-organize

| 操作 | 契約 |
| --- | --- |
| `prepare [--days N]` | リースを作りソースと候補要約を返す。`sourceIds` の HTTP 指定は最大 20 件で、自動抑止とクールダウンだけを迂回し、資格・上限・進捗を保つ |
| `lookup --line` | 上限付きの検索語で既存要約を探す。意味的な一意性は保証しない |
| `commit --line` | 要約と全ソースの網羅表明を検証して公開する |
| `sources` | 生きた記録を最大 50 件のキーセットページで返す |

対応する `/v4/organize/*` はオーナーキー専用で、リモート MCP の OAuth は認可しない。未対応サーバーでは保存や忘却、AI 生成へ代替せず失敗する。`--days` は `sourceIds` と併用できない。スキルは既定 3、最大 20 バッチ、各バッチ lookup 2 回までとする。ソースと要約は信頼できない根拠として扱う。

commit と lookup の JSON は標準入力で 30 秒、128,000 UTF-8 バイトまで。リダイレクトやパイプは行/EOF で受け取る。Windows の `--line --private-stdin` は専用コンソールだけで使い、エコーと行編集を無効にしてから stderr に `REMNIFOLD_STDIN_READY` を出す。CR/LF の完全な終端後は、JSON が不正でも元のモードへ戻す。タイムアウト、過大、読み取り失敗、終端なしでは、遅れた入力を守るためエコーを無効のままにし、その PTY を捨てる。リダイレクトと Windows 以外は準備完了前に拒む。生成と安全な受け渡しはクライアントの責任である。操作手順は [README](../readme/memory-organization.md)。

`sources` は直接の依存とマルチパートを階層的にたどり、忘却行を除く。チェックポイントは中間記録で、その `getDocument` のプレビューとカーソルから正確なメンバーへ進む。`getDocument` は登録済み生成版と統合に 5 行の `organizationSources` を足し、カーソル付きでは本文を読まずページだけを返す。親リビジョンの変更や無効化は conflict。欠けた過去のメンバーは補わず、別経路の ID は重複しうる。ユーザーメタデータでは登録を偽れない。

## topics-setup

未分類かつ補強対象の上限付きバッチをプレビューし、`--apply` だけが ID を `/v4/enrich` に送る。受理は完了ではなく、再補強は他の派生データも更新する。次のバッチの前に完了を確かめる。詳細は [enrichment-and-topics.md](enrichment-and-topics.md)。

## consolidate_memory

認証付きの `POST /v4/consolidate` エンドポイントと MCP の `consolidate_memory` ツールは、同じサービスを使う。エンドポイントは、`force` が true でない限り通常の期限のしきい値を適用し、MCP ツールは常に `force: true` を送る。手動のリクエストは、いずれも最大1バッチを処理し、未統合のソース記録が少なくとも1件必要である。

## その他

Setup の登録と `uninstall-setup` は [configuration.md](configuration.md)、strict の `install` は [release-process.md](release-process.md)、`server-setup` は [provisioning.md](provisioning.md) を参照。
