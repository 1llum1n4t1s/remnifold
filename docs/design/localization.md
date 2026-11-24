# Localization

ネイティブクライアントは、固定の人間向け文言を 9 つの言語で表示する。`ja`、`en`、`zh-CN`、`zh-TW`、`ko`、`es`、`fr`、`de`、`pt-BR` である（日本語と英語が原文で、それ以外は機械翻訳）。これはクライアントの表示上の選択であり、Worker、REST/MCP の契約、ツールのスキーマ、保存データは変わらない。

- **言語の選択**（`native/src/locale.rs`、テキストを書き出す環境から、プロセスごとに 1 回）: `REMNIFOLD_LANGUAGE`、次に `LC_ALL`、`LC_MESSAGES`、`LANG`（空、`C`、`POSIX` は読み飛ばす）、次に OS の表示言語（Windows は `GetUserDefaultUILanguage`、macOS は `AppleLanguages`）の順に見る。未対応または検出できない言語は英語を選ぶ。タグの対応は次のとおり。`zh-Hans`/`zh-CN`/`zh-SG` は `zh-CN`、`zh-Hant`/`zh-TW`/`zh-HK`/`zh-MO` は `zh-TW`（スクリプトサブタグは地域より優先し、単独の `zh` は `zh-CN`）、`pt` とすべての `pt-*` は `pt-BR`、それ以外は主言語で対応づける（`ja-JP` は `ja`）。
- **辞書**: 言語ごとに、`native/src/locales/<tag>.json` に平らな JSON オブジェクトを 1 つ置き、`include_str!` で埋め込む。キーは安定した識別子（`hook.notice.partial_spaces`）で、プレースホルダーは `{name}` であり、言語間で同一である。コードは `locale::tr(key)` または `locale::tr_with(key, &[("name", value)])` を呼ぶ。辞書にないキーは英語の文言にフォールバックし、英語にもないキーはキーそのものが表示される。
- **翻訳の対象**: CLI のヘルプと状態出力、Setup のウェルカムガイド、フックが利用者に表示する通知（`systemMessage`）、および想起の見出しと索引項目のラベル。
- **翻訳の対象外**: AI 向けのテキスト（MCP のツール名、説明、入力スキーマ、挿入される索引の指示とそれに付随する警告、`getDocument` の次の手順のヒント、`SKILL.md`）。日本語はこれらの指示の原文をそのまま保ち、他のすべての言語は同じキーの下に英語のテキストを持つ。また、保存データ（タイトル、説明、抜粋、トピック）、識別子、コマンド、パス、設定キー、固定の失敗コード（`capture failed ... [stage=...]`、`INSTALL_FAILED`）、`memory-organize` プロトコルのヘルプ、server-setup の進捗行、OS や外部 API からの技術的なエラー詳細にも手を加えない。
- **テスト**（`locale.rs`）: タグの対応、Windows の言語 ID、9 つの辞書すべてでキーとプレースホルダーの集合が同一であること、`native/src` から参照されるすべてのキーが辞書に存在すること、未使用のキーがないこと。テストのプロセスは `REMNIFOLD_LANGUAGE=en` を設定し、アサーションがホストの言語に依存しないようにする。
- **README**: `README.md`（英語）が正本で、他の 8 言語には `README.<tag>.md` があり、それぞれの 1 行目に同じ言語切り替えが付く。開発者向け文書（`AGENTS.md`、`DESIGN.md`、`CHANGELOG.md`）は翻訳しない。
