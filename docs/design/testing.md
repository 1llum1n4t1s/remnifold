# Testing

検証コマンドは [validation.md](../agents/validation.md)、保つ契約は [testing.md](../agents/testing.md) が正本。ここは CI と隔離検証の設計を述べる。

## CI

`ci.yml` は PR、手動実行、`dependabot/**` を除くブランチの push、版タグの push で動く（スケジュールなし）。全イベントで次の高速検査を行う。

- `server-contracts`（`ubuntu-24.04`）: Worker の型、実 workerd のテスト、`git diff --check`
- `windows-native-contracts`: ネイティブテスト
- `windows-client-contracts`: クライアント E2E

4 つの `native-package` は PR、手動、`release/*`、版タグでだけ動く。`windows-latest`、`macos-latest`、`ubuntu-24.04`、`ubuntu-24.04-arm` で、ホストトリプルの一致、梱包、commit モードの検証、updater-test を行う。`main` と他のブランチは高速検査だけである。公開ゲートは `main` の結果を見て、タグの実行は並行の事後検査とする。Unix のランタイム検証は梱包と updater-test に限られる。

`main`、`release/*`、タグ以外では新しい push が古い実行を取り消す。concurrency にはイベント名を含め、同じ SHA の別イベントがゲート待ちの `main` を置き換えないようにする。ステージングは信頼した `release/*` の push だけで、PR・手動・タグはシークレットのない `validation` を使う。

## Setup の隔離検証

`velopack-linux.yml` は x64 と ARM64 でライブラリテスト、clippy のゲート、`RemnifoldVelopackTest` 9.0.0/9.0.1 の梱包と E2E を行う。登録と autostart、2 回目の起動で CLI を呼ばないこと、3 つの同時 `mcp` でダウンロードが 1 回だけであること、使用中の `busy`、誰でも書けるフォルダーの `untrusted`、停止後の適用とバイト列の一致、版だけの登録更新、`uninstall-setup` を確かめる。FUSE がなければ `APPIMAGE_EXTRACT_AND_RUN=1` を使い、R2 には書かない。

隔離検証では、実際の Linux デスクトップの autostart、macOS の `.pkg` postinstall から登録への到達、全ユーザー向け macOS 導入の昇格は未検証である。

アップデーターテストは隔離した `--previous-archive` から移行を検証する。checkout モードのアーカイブは公開準備の証明にならない。
