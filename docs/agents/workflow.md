# Workflow

## Setup の隔離検証

実際のアプリ、登録、設定に触れず、テスト用 packId（例: `RemnifoldVelopackTest`）を使う。`RemnifoldApp` は使わない。共通して、外部の作業フォルダー、プロセス単位の `REMNIFOLD_HOME`、`CLAUDE_CONFIG_DIR`、`CODEX_HOME`、`TEMP`/`TMP` を設定し、`PATH` の先頭に引数だけを記録するスタブの `claude`/`codex` を置く。テスト用実行ファイルは `--features update-feed-test` でビルドする。

- Windows: `--installto` で外部の作業フォルダーへ導入する。署名なしの版は `scripts/velopack/pack-windows.ps1 -PackId <test id> -PackVersion <x.y.z> -Binary <exe>` でパックする。終了後はテスト用の HKCU `Run`、Uninstall エントリー、導入フォルダーを片付ける。レジストリのユニットテストは `RegLoadAppKeyW` のプライベートハイブを使う。
- macOS（`ssh mac`）: `HOME` を含む全てを 1 つの作業フォルダーの下に置き、Portable zip から導入する。`scripts/velopack/pack-macos.sh <empty dir> --pack-id <test id> --version <x.y.z> --binary <exe>` を使い、`vpk` は `--skip-updates` 付き。実際の `launchctl bootstrap` はテスト用ラベルだけで行い、`bootout` で終える。署名は一時キーチェーンだけを使う。
- Linux: `velopack-linux.yml` が `$RUNNER_TEMP` 下で `e2e-linux.sh` を行う。Environment、シークレット、R2 は使わない。

既知の設計条件と未検証範囲は [DESIGN の testing](../design/testing.md) を参照。
