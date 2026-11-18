#!/usr/bin/env bash
# wait-main-ci.sh を、スタブの gh で検証する（GitHub へは接続しない）。
#
# 確かめること:
#   success          軽量タグ・main の祖先・版一致で、main の push の ci.yml が success          -> 0
#   progress         1 回目は in_progress、次の確認で success                                    -> 0
#   failure          ci.yml が failure                                                         -> 即 1（runs を 1 回しか問い合わせない）
#   release-branch   成功 run が release/* の push のもので、main の run が無い                   -> 発見の期限で 1
#   other-commit     別コミットの成功 run だけがある                                              -> 発見の期限で 1
#   annotated        注釈付きタグ（object.type が tag）                                           -> 1（runs を問い合わせない）
#   moved-tag        タグが別のコミットを指す                                                     -> 1（runs を問い合わせない）
#   moved-later      成功を確認した後の再確認で、タグが別のコミットを指している                      -> 1
#   not-ancestor     main の祖先でない（diverged）                                                -> 1（runs を問い合わせない）
#   version          タグと plugin.json の版が違う                                                -> 1（API を呼ばない）
#   branch-ref       ブランチの ref から起動した                                                  -> 1（API を呼ばない）
#   rc-tag / zero    v9.9.9-rc1、v09.9.9 のような版タグでない名前                                 -> 1（API を呼ばない）
#
# 使い方: scripts/velopack/test-wait-main-ci.sh
#   PYTHON_BIN で python3 の実行ファイルを差し替えられる。TEST_KEEP_TEMP=1 なら作業フォルダーを消さない。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="$here/wait-main-ci.sh"
work="$(mktemp -d)"
cleanup() { [[ "${TEST_KEEP_TEMP:-}" == "1" ]] || rm -rf "$work"; }
trap cleanup EXIT

repository='1llum1n4t1s/remnifold'
sha='0123456789abcdef0123456789abcdef01234567'
other='ffffffffffffffffffffffffffffffffffffffff'
tag='v9.9.9'

printf '{"name":"remnifold","version":"9.9.9"}' >"$work/plugin.json"
printf '{"name":"remnifold","version":"9.9.8"}' >"$work/plugin-old.json"

# スタブ gh: `gh api <path>` だけを受け付け、シナリオごとの JSON を返す。
#   runs の問い合わせは $STUB_DIR/runs.N.json（N は呼び出し回数。無ければ最大の N）を返す。
#   タグの問い合わせは $STUB_DIR/tag.N.json（同様）を返す。
#   repos/<repo> は既定ブランチ main、compare は $STUB_DIR/compare.json（無ければ ahead / 0）を返す。
cat >"$work/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "api" ]] || { echo "stub gh: unsupported: $*" >&2; exit 2; }
path="$2"
echo "$path" >>"$STUB_DIR/calls.log"
numbered() { # numbered <名前>: 呼び出し回数ごとのファイルを返す
  local count file
  count="$(cat "$STUB_DIR/$1.count" 2>/dev/null || echo 0)"
  count=$((count + 1))
  echo "$count" >"$STUB_DIR/$1.count"
  file="$STUB_DIR/$1.$count.json"
  if [[ ! -f "$file" ]]; then
    file="$(ls "$STUB_DIR"/"$1".*.json | sort -V | tail -n 1)"
  fi
  cat "$file"
}
case "$path" in
  repos/1llum1n4t1s/remnifold) printf '{"default_branch":"main"}' ;;
  */git/ref/tags/*) numbered tag ;;
  */compare/*)
    if [[ -f "$STUB_DIR/compare.json" ]]; then cat "$STUB_DIR/compare.json"; else printf '{"status":"ahead","behind_by":0,"ahead_by":3}'; fi
    ;;
  *"/runs?"*) numbered runs ;;
  *) echo "stub gh: unexpected path: $path" >&2; exit 2 ;;
esac
STUB
chmod +x "$work/gh"

# run の JSON を作る: $1=status $2=conclusion(空なら null) $3=head_sha $4=head_branch
run_json() {
  local conclusion="null"
  [[ -n "$2" ]] && conclusion="\"$2\""
  printf '{"total_count":1,"workflow_runs":[{"id":4242,"run_attempt":2,"status":"%s","conclusion":%s,"head_sha":"%s","head_branch":"%s","event":"push","path":".github/workflows/ci.yml","repository":{"full_name":"%s"},"head_repository":{"full_name":"%s"}}]}' \
    "$1" "$conclusion" "$3" "$4" "$repository" "$repository"
}
# タグの JSON を作る: $1=object.type $2=object.sha
tag_json() {
  printf '{"ref":"refs/tags/%s","object":{"type":"%s","sha":"%s"}}' "$tag" "$1" "$2"
}

failures=0
# seed <名前>: 既定の成功の材料（軽量タグ・main の成功 run）を置いたディレクトリを作る。シナリオで上書きする。
seed() {
  mkdir -p "$work/$1"
  tag_json commit "$sha" >"$work/$1/tag.1.json"
  run_json completed success "$sha" main >"$work/$1/runs.1.json"
}
# check <名前> <期待する終了コード> [VAR=値 ...]: 既定の環境に上書きを足して実行する。
check() {
  local name="$1" expected="$2" dir="$work/$1" code=0
  shift 2
  local started=$SECONDS
  # 時間切れの確認用に、間隔 1 秒・発見の期限 2 秒・上限 4 秒へ縮める。
  env STUB_DIR="$dir" GH_BIN="$work/gh" GITHUB_REPOSITORY="$repository" GITHUB_SHA="$sha" \
    GITHUB_REF="refs/tags/$tag" GITHUB_REF_TYPE=tag GITHUB_REF_NAME="$tag" \
    REMNIFOLD_PLUGIN_MANIFEST="$work/plugin.json" \
    GATE_INTERVAL_SECONDS=1 GATE_DISCOVERY_SECONDS=2 GATE_TIMEOUT_SECONDS=4 PYTHON_BIN="${PYTHON_BIN:-python3}" \
    "$@" bash "$target" >"$dir/out.log" 2>&1 || code=$?
  local elapsed=$((SECONDS - started))
  if [[ "$code" == "$expected" ]]; then
    echo "ok   $name (exit $code, ${elapsed}s)"
  else
    echo "FAIL $name: expected exit $expected, got $code"
    sed 's/^/     | /' "$dir/out.log"
    failures=$((failures + 1))
  fi
}
# calls <名前> <パターン>: そのシナリオで、パターンに合う API 呼び出しの回数を返す。
calls() {
  if [[ -f "$work/$1/calls.log" ]]; then grep -c -- "$2" "$work/$1/calls.log" || true; else echo 0; fi
}
expect_calls() { # expect_calls <名前> <パターン> <回数>
  local actual
  actual="$(calls "$1" "$2")"
  [[ "$actual" == "$3" ]] || { echo "FAIL $1: expected $3 call(s) matching $2, got $actual"; failures=$((failures + 1)); }
}

seed success
check success 0
# タグは待つ前と成功の後の 2 回確かめる。
expect_calls success '/git/ref/tags/v9.9.9' 2
expect_calls success 'branch=main' 1

seed progress
run_json in_progress "" "$sha" main >"$work/progress/runs.1.json"
run_json completed success "$sha" main >"$work/progress/runs.2.json"
check progress 0

seed failure
run_json completed failure "$sha" main >"$work/failure/runs.1.json"
check failure 1
expect_calls failure '/runs?' 1

seed release-branch
run_json completed success "$sha" release/9.9.9 >"$work/release-branch/runs.1.json"
check release-branch 1
grep -q 'No ci.yml push run on main' "$work/release-branch/out.log" || { echo "FAIL release-branch: expected the discovery failure"; failures=$((failures + 1)); }

seed other-commit
run_json completed success "$other" main >"$work/other-commit/runs.1.json"
check other-commit 1

seed annotated
tag_json tag "$sha" >"$work/annotated/tag.1.json"
check annotated 1
expect_calls annotated '/runs?' 0

seed moved-tag
tag_json commit "$other" >"$work/moved-tag/tag.1.json"
check moved-tag 1
expect_calls moved-tag '/runs?' 0

seed moved-later
tag_json commit "$other" >"$work/moved-later/tag.2.json"
check moved-later 1
expect_calls moved-later '/runs?' 1

seed not-ancestor
printf '{"status":"diverged","behind_by":1,"ahead_by":2}' >"$work/not-ancestor/compare.json"
check not-ancestor 1
expect_calls not-ancestor '/runs?' 0

seed behind
printf '{"status":"behind","behind_by":1,"ahead_by":0}' >"$work/behind/compare.json"
check behind 1

seed version
check version 1 REMNIFOLD_PLUGIN_MANIFEST="$work/plugin-old.json"
expect_calls version '' 0

seed branch-ref
check branch-ref 1 GITHUB_REF=refs/heads/v9.9.9 GITHUB_REF_TYPE=branch
expect_calls branch-ref '' 0

seed rc-tag
check rc-tag 1 GITHUB_REF=refs/tags/v9.9.9-rc1 GITHUB_REF_NAME=v9.9.9-rc1
expect_calls rc-tag '' 0

seed zero
check zero 1 GITHUB_REF=refs/tags/v09.9.9 GITHUB_REF_NAME=v09.9.9
expect_calls zero '' 0

seed mismatched-ref
check mismatched-ref 1 GITHUB_REF=refs/tags/v9.9.8
expect_calls mismatched-ref '' 0

if (( failures > 0 )); then
  echo "$failures check(s) failed." >&2
  exit 1
fi
echo "All checks passed."
