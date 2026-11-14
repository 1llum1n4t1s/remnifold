#!/usr/bin/env bash
# Linux（x86_64 / aarch64）の Setup（Velopack の AppImage）と自動更新の一式を梱包する。
#
# 出力（<work>/packages/）は R2 へ置く一式:
#   <id>-<channel>.AppImage（固定名。最新版を指す）、<id>-<版>-<channel>.AppImage（版付きの不変名）、
#   <id>-<版>-<channel>-full.nupkg、releases.<channel>.json、assets.<channel>.json、RELEASES-<channel>
# channel は x86_64 が linux-x64、aarch64 が linux-arm64。配信は scripts/velopack/publish-r2.ps1 -Channel <channel>
# が行う（実行は本人の明示の依頼があるときだけ）。共通仕様は D:\Dev\APP-DISTRIBUTION.md。
#
# 使い方（梱包するのと同じ CPU の Linux で実行する。GitHub の ubuntu-latest / ubuntu-24.04-arm を想定）:
#   pack-linux.sh <空の絶対パスの作業フォルダー> [--pack-id <id>] [--version <x.y.z>] [--binary <path>]
#
#   検証用（本番では使わない）:
#   --pack-id  別の packId（例 RemnifoldVelopackTest）。--version  別の版。
#   --binary   ビルド済みの remnifold（update-feed-test 付きなど）。指定すると cargo build を行わない。
#
# 信頼モデル: AppImage にはコード署名の標準が無いため、Linux は R2 の HTTPS と Velopack のフィードの
# ハッシュ照合で信頼する（署名はしない。native/src/app_update/trust.rs）。アプリは適用前に、nupkg の中に
# AppImage がちょうど1つあることと、置き場が自分だけのものであることを確かめる。ここでも同じ形を確かめる。
#
# vpk 1.2.161 は AppImage のランタイム（AppImage/type2-runtime、静的リンク）を同梱しているが、Linux 上での squashfs の
# 作成には外部の `mksquashfs`（Ubuntu では squashfs-tools）を呼ぶ（velopack 1.2.161 の AppImageTool.cs。Windows からの
# クロス梱包だけは同梱の mksquashfs.exe を使う）。無ければ梱包の前に止める。
set -euo pipefail
umask 022

usage() {
  echo 'usage: pack-linux.sh <absolute-empty-work-directory> [--pack-id <id>] [--version <x.y.z>] [--binary <path>]' >&2
  exit 2
}
[[ $# -ge 1 ]] || usage
work="$1"; shift
pack_id=RemnifoldApp; version=''; binary=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pack-id) [[ $# -ge 2 ]] || usage; pack_id="$2"; shift ;;
    --version) [[ $# -ge 2 ]] || usage; version="$2"; shift ;;
    --binary) [[ $# -ge 2 ]] || usage; binary="$2"; shift ;;
    *) usage ;;
  esac
  shift
done

[[ "$work" = /* ]] || { echo 'Specify an absolute path.' >&2; exit 1; }
[[ -d "$work" && ! -L "$work" && -z "$(ls -A "$work")" ]] || { echo 'The work directory must be an empty, ordinary directory.' >&2; exit 1; }
[[ "$(uname -s)" = Linux ]] || { echo 'Pack on Linux.' >&2; exit 1; }
[[ "$pack_id" =~ ^[A-Za-z][A-Za-z0-9]*$ ]] || { echo 'The packId must be alphanumeric.' >&2; exit 1; }
[[ -z "$version" || "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'The version must be x.y.z.' >&2; exit 1; }
if [[ -n "$binary" ]]; then
  [[ "$binary" = /* && -f "$binary" ]] || { echo 'The binary must be an absolute path to an existing file.' >&2; exit 1; }
fi
# vpk の起動時の新版確認は切る（CI・SSH の非対話セッションで戻らないことがあるため）。版は固定の 1.2.161 を要求する。
vpk_help="$(vpk --skip-updates --help 2>&1 | sed $'s/\x1b\\[[0-9;]*m//g')"
grep -q 'Velopack CLI 1\.2\.161' <<<"$vpk_help" || { echo 'Velopack CLI 1.2.161 is required.' >&2; exit 1; }
command -v mksquashfs >/dev/null || { echo 'mksquashfs is required (sudo apt install squashfs-tools).' >&2; exit 1; }
command -v file >/dev/null || { echo 'file is required.' >&2; exit 1; }

case "$(uname -m)" in
  x86_64) target=x86_64-unknown-linux-gnu; channel=linux-x64; machine='x86-64' ;;
  aarch64) target=aarch64-unknown-linux-gnu; channel=linux-arm64; machine='aarch64' ;;
  *) echo 'Unsupported CPU.' >&2; exit 1 ;;
esac

repo="$(cd "$(dirname "$0")/../.." && pwd -P)"
case "$work/" in
  "$repo/"*) echo 'The work directory must be outside the repository.' >&2; exit 1 ;;
esac
mkdir "$work/payload" "$work/packages" "$work/tmp"
# Rust と vpk の一時ファイルを、梱包専用のディレクトリに閉じる。
export TMPDIR="$work/tmp"
cd "$repo"
if [[ -z "$version" ]]; then
  version="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print([p for p in json.load(sys.stdin)["packages"] if p["name"]=="remnifold"][0]["version"])')"
fi
if [[ -n "$binary" ]]; then
  cp "$binary" "$work/payload/remnifold"
else
  # build.rs が要る固定版の esbuild などを、先に用意する（既にあれば照合だけ）。
  cargo xtask prepare
  # 本番の梱包は既定の機能だけでビルドする（update-feed-test などのテスト用の機能を含めない）。
  cargo build --locked --release --target "$target" --target-dir "$work/target" --bin remnifold
  cp "$work/target/$target/release/remnifold" "$work/payload/remnifold"
fi
chmod 755 "$work/payload/remnifold"
# 梱包する実行ファイルが、この CPU 向けの ELF であることを確かめる（別の CPU 向けを取り違えない）。
description="$(file -b "$work/payload/remnifold")"
grep -q "ELF 64-bit.*$machine" <<<"$description" || { echo "The executable is not a $machine ELF: $description" >&2; exit 1; }

# packDir には実行ファイルだけを置く。vpk が AppDir（AppRun・.desktop・アイコン・UpdateNix・sq.version）を組み立てる。
# AppRun は vpk が生成するシェルスクリプトで、引数をそのまま主実行ファイルへ `exec` で渡す。
vpk --skip-updates pack \
  --packId "$pack_id" --packTitle Remnifold --packAuthors Kagayoi \
  --packVersion "$version" --channel "$channel" --runtime "$channel" \
  --mainExe remnifold --packDir "$work/payload" --outputDir "$work/packages" \
  --delta None

packages="$work/packages"
nupkg="$packages/$pack_id-$version-$channel-full.nupkg"
appimage="$packages/$pack_id-$channel.AppImage"
for required in "releases.$channel.json" "$(basename "$nupkg")" "$(basename "$appimage")"; do
  [[ -f "$packages/$required" ]] || { echo "Expected output was not generated: $required" >&2; ls -la "$packages" >&2; exit 1; }
done
chmod 755 "$appimage"
# 版付きの不変名の AppImage も置く（固定名は最新版を指す URL として毎回置き換える）。
cp "$appimage" "$packages/$pack_id-$version-$channel.AppImage"

# nupkg を、アプリが適用前に行うのと同じ規則で確かめる: 曖昧な名前が無く、lib/app/ 直下の AppImage がちょうど1つ。
python3 - "$nupkg" <<'PY'
import sys, zipfile
names = zipfile.ZipFile(sys.argv[1]).namelist()
for name in names:
    if "\\" in name or ":" in name or name.startswith("/") or ".." in name.split("/"):
        sys.exit(f"Entry name not allowed: {name}")
appimages = [n for n in names if n.startswith("lib/app/") and "/" not in n[len("lib/app/"):] and n.lower().endswith(".appimage")]
if len(appimages) != 1:
    sys.exit(f"The package must contain exactly one AppImage: {appimages}")
PY
echo 'Unsigned AppImage package (Linux trusts HTTPS and the feed hashes; see trust.rs).'
ls -la "$packages"
sha256sum "$packages"/*.AppImage "$packages"/*.nupkg
