# Release process

Setup のパッケージングは `scripts/velopack/` の PowerShell/Bash と外部の `vpk`、Windows 署名の Sigilune を使い、Node.js は使わない。本番は packId `RemnifoldApp` で `pack-windows.ps1 -Sign`、`pack-macos.sh --sign`、`pack-linux.sh` を実行する。`release-velopack.yml` が `publish-r2.ps1` で不変の版付きオブジェクトを書き、`releases.<channel>.json` を最後に書く。

公開は `main` のコミット上の軽量タグ `v<x.y.z>` の push で始まり、GitHub Releases は作らない。`gh workflow run release-velopack.yml --ref v<x.y.z>` で再実行できる。

1. シークレットなしの `gate` が `scripts/velopack/wait-main-ci.sh` を実行する。タグは既定ブランチの祖先上の `v<.codex-plugin/plugin.json version>` と完全一致し、そのコミットの既定ブランチの `ci.yml` push が成功しなければならない。待機後にタグを再確認する。
2. `r2-release` で 4 つの独立ジョブを動かす。Windows は Certum/Sigilune、macOS は一時キーチェーンの Developer ID と公証、Linux x64/ARM64 は glibc 2.39 以下を要求する。
3. 各ジョブは `cargo xtask ci-release-tag` の後、公開済みチャンネルを飛ばし、より新しい版が載っていれば失敗する。

Windows は署名済み `lib/app/remnifold.exe` をブートストラップとして取り出し、Authenticode の状態、署名者 CN、タイムスタンプを検証する。`publish-r2.ps1` は公開最大版より低い版を拒む（意図したロールバックだけ `-AllowOlderVersion`）。署名を `ci.yml` に移さない。

スクリプトの手動公開は外部への書き込みなので明示的な依頼が必要。R2 の信頼境界は [r2-publication-and-trust.md](r2-publication-and-trust.md) を読む。

ジョブ名は `windows-installer`、`macos-installer`、`linux-installer`。スクリプトは全て `scripts/velopack/`、ワークフローは `.github/workflows/release-velopack.yml`。
