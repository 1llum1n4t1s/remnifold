#!/usr/bin/env bash
# macOS（Apple Silicon）の Setup（Velopack の .pkg）と自動更新の一式を梱包する。
#
# 出力（<work>/packages/）は R2 へ置く一式（<id>-osx-Setup.pkg・<id>-osx-Portable.zip・releases.osx.json・
# <id>-<版>-osx-full.nupkg など）。配信は scripts/velopack/publish-r2.ps1 -Channel osx が行う。
# 配信元は GitHub Actions の macos-latest ランナー（.github/workflows/release-velopack.yml の macos-installer。
# 一時キーチェーンに証明書と公証の API キーを入れて --sign で梱包し、同じ job が配信する）。手元の Apple Silicon の
# Mac でも実行できる（実行は本人の明示の依頼があるときだけ）。共通仕様は D:\Dev\APP-DISTRIBUTION.md。
#
# 使い方:
#   pack-macos.sh <空の絶対パスの作業フォルダー> [--sign] [--pack-id <id>] [--version <x.y.z>] [--binary <path>]
#                 [--cargo-target-dir <絶対パス>]
#
#   --cargo-target-dir  cargo build の target ディレクトリ（既定は <work>/target）。CI が署名用のキーチェーンを
#            用意する前に同じコマンド（既定の機能、--release --target aarch64-apple-darwin）でビルドしておけば、
#            梱包では再コンパイルされない。--binary とは併用できない。--sign と併用できる。
#
#   --sign   Developer ID で .app を署名し、.pkg を Developer ID Installer で署名し、notarytool で公証して
#            .app と .pkg にチケットを貼る（vpk の --signAppIdentity / --signInstallIdentity / --notaryProfile）。
#            次の環境変数が必要（値はログへ出さない）:
#              REMNIFOLD_MAC_SIGN_APP_IDENTITY      例 "Developer ID Application: <name> (<team>)"
#              REMNIFOLD_MAC_SIGN_INSTALL_IDENTITY  例 "Developer ID Installer: <name> (<team>)"
#              REMNIFOLD_MAC_NOTARY_PROFILE         notarytool store-credentials で作ったプロファイル名
#              REMNIFOLD_MAC_KEYCHAIN               上の証明書とプロファイルを入れたキーチェーンの絶対パス
#                                                  （一時キーチェーンを推奨。ログインキーチェーンに残さない）。
#                                                  このスクリプトを実行するのと同じセッションで解錠しておく。
#            梱包の間だけ、このキーチェーンを利用者の検索リストの先頭に足し、終了時に元へ戻す（productbuild 用）。
#   検証用（本番では使わない）:
#   --pack-id  別の packId（例 RemnifoldVelopackTest）。--version  別の版。
#   --binary   ビルド済みの remnifold（update-feed-test 付きなど）。指定すると cargo build を行わない。
#              --sign とは併用できない（テスト用の機能を含むビルドに本番の署名・公証をしないため）。
#              --pack-id / --version は --sign と併用できる（署名・公証の検証用の一式を作るため）。
#
# アプリは適用前に、nupkg を UpdateMac と同じ方法で展開した .app を、codesign の要件（Apple 発行の
# Developer ID Application、チーム ID）で検証する（native/src/app_update/trust.rs）。--sign のときは、
# 配信前にここで同じ検査を通し、通らない一式は作らない（配信すると全利用者の更新が適用前に拒否される）。
set -euo pipefail
umask 022

usage() {
  echo 'usage: pack-macos.sh <absolute-empty-work-directory> [--sign] [--pack-id <id>] [--version <x.y.z>] [--binary <path>] [--cargo-target-dir <absolute-path>]' >&2
  exit 2
}
[[ $# -ge 1 ]] || usage
work="$1"; shift
sign=0; pack_id=RemnifoldApp; version=''; binary=''; cargo_target_dir=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --sign) sign=1 ;;
    --pack-id) [[ $# -ge 2 ]] || usage; pack_id="$2"; shift ;;
    --version) [[ $# -ge 2 ]] || usage; version="$2"; shift ;;
    --binary) [[ $# -ge 2 ]] || usage; binary="$2"; shift ;;
    --cargo-target-dir) [[ $# -ge 2 ]] || usage; cargo_target_dir="$2"; shift ;;
    *) usage ;;
  esac
  shift
done

[[ "$work" = /* ]] || { echo 'Specify an absolute path.' >&2; exit 1; }
[[ -d "$work" && ! -L "$work" && -z "$(ls -A "$work")" ]] || { echo 'The work directory must be an empty, ordinary directory.' >&2; exit 1; }
[[ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ]] || { echo 'Pack on Apple Silicon macOS.' >&2; exit 1; }
[[ "$pack_id" =~ ^[A-Za-z][A-Za-z0-9]*$ ]] || { echo 'The packId must be alphanumeric.' >&2; exit 1; }
[[ -z "$version" || "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'The version must be x.y.z.' >&2; exit 1; }
if [[ "$sign" = 1 && -n "$binary" ]]; then
  echo '--sign cannot be combined with --binary (a prebuilt binary may contain test-only features).' >&2
  exit 1
fi
if [[ -n "$binary" ]]; then
  [[ "$binary" = /* && -f "$binary" ]] || { echo 'The binary must be an absolute path to an existing file.' >&2; exit 1; }
fi
if [[ -n "$cargo_target_dir" ]]; then
  [[ -z "$binary" ]] || { echo '--cargo-target-dir cannot be combined with --binary.' >&2; exit 1; }
  [[ "$cargo_target_dir" = /* ]] || { echo 'The cargo target directory must be an absolute path.' >&2; exit 1; }
fi
# vpk は起動時に新しい版を確認しに行く。SSH の非対話セッションではこの確認が戻らないことがあるため
# （実機で確認）、--skip-updates で切る。版は固定の 1.2.161 を要求する。
vpk_help="$(vpk --skip-updates --help 2>&1 | sed $'s/\x1b\\[[0-9;]*m//g')"
grep -q 'Velopack CLI 1\.2\.161' <<<"$vpk_help" || { echo 'Velopack CLI 1.2.161 is required.' >&2; exit 1; }

# 本人の Apple Developer のチーム ID（公開情報）。trust.rs の EXPECTED_TEAM_ID と同じ値。
team_id='SL228Y8UUR'
requirement="anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team_id\""
signing=()
if [[ "$sign" = 1 ]]; then
  for name in REMNIFOLD_MAC_SIGN_APP_IDENTITY REMNIFOLD_MAC_SIGN_INSTALL_IDENTITY REMNIFOLD_MAC_NOTARY_PROFILE REMNIFOLD_MAC_KEYCHAIN; do
    [[ -n "${!name:-}" ]] || { echo "Set $name to sign and notarize." >&2; exit 1; }
  done
  [[ "$REMNIFOLD_MAC_KEYCHAIN" = /* && -f "$REMNIFOLD_MAC_KEYCHAIN" ]] || { echo 'REMNIFOLD_MAC_KEYCHAIN must be an absolute path to a keychain file.' >&2; exit 1; }
  # 署名者のチーム ID が期待どおりであることを、梱包の前に確かめる（別のチームの証明書で一式を作らない）。
  [[ "$REMNIFOLD_MAC_SIGN_APP_IDENTITY" == *"($team_id)" ]] || { echo 'The application identity is not for the expected team.' >&2; exit 1; }
  [[ "$REMNIFOLD_MAC_SIGN_INSTALL_IDENTITY" == *"($team_id)" ]] || { echo 'The installer identity is not for the expected team.' >&2; exit 1; }
  signing=(
    --signAppIdentity "$REMNIFOLD_MAC_SIGN_APP_IDENTITY"
    --signInstallIdentity "$REMNIFOLD_MAC_SIGN_INSTALL_IDENTITY"
    --notaryProfile "$REMNIFOLD_MAC_NOTARY_PROFILE"
    --keychain "$REMNIFOLD_MAC_KEYCHAIN"
  )
  # codesign は、キーチェーンが「この SSH セッションで」解錠されていないと errSecInternalComponent で失敗する
  # （別のセッションでの解錠は効かない。実機で確認）。解錠は呼び出し側が同じセッションで済ませておく
  # （パスワードをこのスクリプトの引数・環境変数で受け取らないため）。
  # productbuild（.pkg の署名）は vpk の --keychain を使わず、利用者のキーチェーン検索リストから
  # Developer ID Installer を探す（実機で確認）。梱包の間だけ検索リストの先頭に足し、終了時に必ず元へ戻す。
  original_keychains=()
  while IFS= read -r item; do
    item="$(printf '%s' "$item" | sed 's/^[[:space:]]*"//;s/"[[:space:]]*$//')"
    [[ -n "$item" ]] && original_keychains+=("$item")
  done < <(security list-keychains -d user)
  restore_keychains() { security list-keychains -d user -s ${original_keychains[@]+"${original_keychains[@]}"}; }
  trap restore_keychains EXIT
  security list-keychains -d user -s "$REMNIFOLD_MAC_KEYCHAIN" ${original_keychains[@]+"${original_keychains[@]}"}
fi

repo="$(cd "$(dirname "$0")/../.." && pwd -P)"
case "$work/" in
  "$repo/"*) echo 'The work directory must be outside the repository.' >&2; exit 1 ;;
esac
mkdir "$work/payload" "$work/packages" "$work/tmp" "$work/verify"
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
  # --cargo-target-dir を渡された場合は、その target を使う（事前に同じコマンドでビルド済みなら再コンパイルしない）。
  cargo_target="${cargo_target_dir:-$work/target}"
  cargo build --locked --release --target aarch64-apple-darwin --target-dir "$cargo_target" --bin remnifold
  cp "$cargo_target/aarch64-apple-darwin/release/remnifold" "$work/payload/remnifold"
fi
chmod 755 "$work/payload/remnifold"
[[ "$(lipo -archs "$work/payload/remnifold")" = arm64 ]] || { echo 'The executable is not Apple Silicon only.' >&2; exit 1; }

# packDir には実行ファイルだけを置く。vpk が .app（Info.plist・UpdateMac・sq.version）を組み立てる。
# Contents/MacOS には Mach-O 以外を置かない（README などのデータを置くと、codesign がそれらの署名を拡張属性に
# 書き、nupkg・zip を通すと失われて検証に通らなくなるため）。
# packId は作業フォルダーと衝突しない名前（RemnifoldApp）。バンドル ID も packId から決める。
# entitlements は空（macos.entitlements）。vpk は省略時に .NET 向けの既定（JIT・未署名メモリ・DYLD 環境変数・
# ライブラリ検証の無効化）を付けるが、Rust の Remnifold はどれも要らず、DYLD 環境変数の許可はライブラリ注入を
# 許してしまうため。codesign（AMFI）の解析器は XML のコメントを受け付けないので、ファイルには説明を書かない。
bundle_id="com.kagayoi.$(tr '[:upper:]' '[:lower:]' <<<"$pack_id")"
vpk --skip-updates pack \
  --packId "$pack_id" --packTitle Remnifold --packAuthors Kagayoi \
  --packVersion "$version" --channel osx --runtime osx-arm64 \
  --mainExe remnifold --packDir "$work/payload" --outputDir "$work/packages" \
  --bundleId "$bundle_id" --delta None \
  --signEntitlements "$repo/scripts/velopack/macos.entitlements" \
  ${signing[@]+"${signing[@]}"}

packages="$work/packages"
nupkg="$packages/$pack_id-$version-osx-full.nupkg"
setup="$packages/$pack_id-osx-Setup.pkg"
for required in "releases.osx.json" "$(basename "$nupkg")" "$(basename "$setup")" "$pack_id-osx-Portable.zip"; do
  [[ -f "$packages/$required" ]] || { echo "Expected output was not generated: $required" >&2; exit 1; }
done
# 版付きの不変名の Setup も置く（固定名 Setup は最新版を指す URL として毎回置き換える）。
cp "$setup" "$packages/$pack_id-$version-osx-Setup.pkg"

# nupkg を UpdateMac と同じ規則（lib/<任意>/ を外す、*.__symlink はリンクとして復元、ファイルは 755）で展開し、
# アプリが適用前に行うのと同じ検査を通す。主実行ファイルと UpdateMac はちょうど1つずつであること。
app="$work/verify/Remnifold.app"
python3 - "$nupkg" "$app" <<'PY'
import os, re, sys, zipfile
nupkg, app = sys.argv[1:]
prefix = re.compile(r"lib[\\/][^\\/]*[\\/]")
main = updater = 0
links = []
with zipfile.ZipFile(nupkg) as archive:
    for info in archive.infolist():
        name = info.filename
        if "\\" in name or ":" in name or name.startswith("/") or ".." in name.split("/"):
            sys.exit(f"Entry name not allowed: {name}")
        main += name.lower() == "lib/app/contents/macos/remnifold"
        updater += name.lower() == "lib/app/contents/macos/updatemac"
        if not prefix.match(name) or name.endswith("/"):
            continue
        relative = prefix.sub("", name, count=1)
        target = os.path.join(app, relative)
        if relative.endswith(".__symlink"):
            links.append((target[: -len(".__symlink")], archive.read(info).decode().rstrip("/")))
            continue
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as out:
            out.write(archive.read(info))
        os.chmod(target, 0o755)
for link, destination in links:
    os.makedirs(os.path.dirname(link), exist_ok=True)
    os.symlink(destination, link)
if main != 1 or updater != 1:
    sys.exit("The package must contain exactly one main executable and one UpdateMac.")
PY
if [[ "$sign" = 1 ]]; then
  codesign --verify --deep --strict "-R=$requirement" "$app" || { echo 'The packaged app does not satisfy the Developer ID requirement.' >&2; exit 1; }
  # アプリは適用前に、nupkg 内の .app に貼られた公証チケットも spctl で確かめる。同じ判定をここで通す。
  # 出力は変数に取ってから照合する（パイプで grep -q が先に終わると、pipefail で誤って失敗になるため）。
  assessment="$(spctl --assess --type execute -vv "$app" 2>&1)" || { echo 'The packaged app is not accepted by Gatekeeper.' >&2; exit 1; }
  grep -q 'source=Notarized Developer ID' <<<"$assessment" || { echo 'The packaged app is not notarized.' >&2; exit 1; }
  # 配布物そのものの確認: .pkg の署名と公証（チケットが貼られていること）。
  pkgutil --check-signature "$setup" >/dev/null
  xcrun stapler validate "$setup" >/dev/null
  spctl --assess --type install "$setup"
  echo 'Verified the Developer ID signature of the packaged app and the notarized installer.'
else
  echo 'Unsigned package (validation only). Do not publish it.'
fi
ls -la "$packages"
shasum -a 256 "$packages"/*.pkg "$packages"/*.nupkg
