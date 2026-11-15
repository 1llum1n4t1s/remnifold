#!/usr/bin/env bash
# wait-ci-publication.sh を、スタブの gh で検証する（GitHub へは接続しない）。
#
# 確かめる 5 通り:
#   1. 成功                       ci.yml が success で、publish-r2 が 1 件成功 -> 0
#   2. 実行中から成功              1 回目は in_progress、次の確認で success      -> 0
#   3. 失敗                       ci.yml が failure                              -> 即 1（待たない）
#   4. publish-r2 が 0 件          ci.yml は success だが publish-r2 が無い        -> 1
#   5. publish-r2 が 2 件          最新 attempt に publish-r2 が 2 件             -> 1
# 追加: 別コミット・別ブランチの run は無視される（該当 run が無いので待ち続けて時間切れ -> 1）。
#
# 使い方: scripts/velopack/test-wait-ci-publication.sh
#   PYTHON_BIN で python3 の実行ファイルを差し替えられる。TEST_KEEP_TEMP=1 なら作業フォルダーを消さない。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="$here/wait-ci-publication.sh"
work="$(mktemp -d)"
cleanup() { [[ "${TEST_KEEP_TEMP:-}" == "1" ]] || rm -rf "$work"; }
trap cleanup EXIT

repository='1llum1n4t1s/remnifold'
sha='0123456789abcdef0123456789abcdef01234567'
branch='release/9.9.9'

# スタブ gh: `gh api <path>` だけを受け付け、シナリオごとの JSON を返す。
#   runs の問い合わせは $STUB_DIR/runs.N.json（N は呼び出し回数。無ければ最大の N）を返す。
#   jobs の問い合わせは $STUB_DIR/jobs.json を返す。
cat >"$work/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "api" ]] || { echo "stub gh: unsupported: $*" >&2; exit 2; }
path="$2"
echo "$path" >>"$STUB_DIR/calls.log"
case "$path" in
  *"/jobs?"*) cat "$STUB_DIR/jobs.json" ;;
  *"/runs?"*)
    count="$(cat "$STUB_DIR/count" 2>/dev/null || echo 0)"
    count=$((count + 1))
    echo "$count" >"$STUB_DIR/count"
    file="$STUB_DIR/runs.$count.json"
    if [[ ! -f "$file" ]]; then
      file="$(ls "$STUB_DIR"/runs.*.json | sort -V | tail -n 1)"
    fi
    cat "$file"
    ;;
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
# job の JSON を作る: 引数は "名前:結論" の並び。
jobs_json() {
  local items="" id=100 spec
  for spec in "$@"; do
    id=$((id + 1))
    items+="${items:+,}{\"id\":$id,\"name\":\"${spec%%:*}\",\"status\":\"completed\",\"conclusion\":\"${spec##*:}\"}"
  done
  printf '{"total_count":%d,"jobs":[%s]}' "$#" "$items"
}

failures=0
# scenario <名前> <期待する終了コード> <runs の JSON を 1 件以上> ... で、jobs は jobs_json の結果を JOBS に入れて渡す。
check() {
  local name="$1" expected="$2" dir="$work/$1" code=0
  mkdir -p "$dir"
  local started=$SECONDS
  # 時間切れの確認用に、間隔 1 秒・上限 3 秒へ縮める。
  STUB_DIR="$dir" GH_BIN="$work/gh" GITHUB_REPOSITORY="$repository" GITHUB_SHA="$sha" GITHUB_REF_NAME="$branch" \
    GATE_INTERVAL_SECONDS=1 GATE_TIMEOUT_SECONDS=3 PYTHON_BIN="${PYTHON_BIN:-python3}" \
    bash "$target" >"$dir/out.log" 2>&1 || code=$?
  local elapsed=$((SECONDS - started))
  if [[ "$code" == "$expected" ]]; then
    echo "ok   $name (exit $code, ${elapsed}s)"
  else
    echo "FAIL $name: expected exit $expected, got $code"
    sed 's/^/     | /' "$dir/out.log"
    failures=$((failures + 1))
  fi
}
seed() { # seed <名前>: ディレクトリを作る
  mkdir -p "$work/$1"
}

# 1. 成功
seed success
run_json completed success "$sha" "$branch" >"$work/success/runs.1.json"
jobs_json native-package-a:success publish-r2:success >"$work/success/jobs.json"
check success 0

# 2. 実行中から成功（2 回目の確認で completed になる。待つので 1 秒以上かかる）
seed progress
run_json in_progress "" "$sha" "$branch" >"$work/progress/runs.1.json"
run_json completed success "$sha" "$branch" >"$work/progress/runs.2.json"
jobs_json publish-r2:success >"$work/progress/jobs.json"
check progress 0

# 3. 失敗（即失敗。時間切れを待たないので、1 回しか問い合わせない）
seed failure
run_json completed failure "$sha" "$branch" >"$work/failure/runs.1.json"
jobs_json publish-r2:success >"$work/failure/jobs.json"
check failure 1
[[ "$(wc -l <"$work/failure/calls.log")" -eq 1 ]] || { echo "FAIL failure: should not poll again"; failures=$((failures + 1)); }

# 4. publish-r2 が 0 件（skipped の job だけ）
seed zero
run_json completed success "$sha" "$branch" >"$work/zero/runs.1.json"
jobs_json native-package-a:success publish-r2-other:success >"$work/zero/jobs.json"
check zero 1

# 5. publish-r2 が 2 件
seed two
run_json completed success "$sha" "$branch" >"$work/two/runs.1.json"
jobs_json publish-r2:success publish-r2:success >"$work/two/jobs.json"
check two 1

# 追加: publish-r2 が success でない（skipped）
seed skipped
run_json completed success "$sha" "$branch" >"$work/skipped/runs.1.json"
jobs_json publish-r2:skipped >"$work/skipped/jobs.json"
check skipped 1

# 追加: 別コミットの成功 run は使わない（該当 run が無いまま時間切れ）
seed other-commit
run_json completed success "ffffffffffffffffffffffffffffffffffffffff" "$branch" >"$work/other-commit/runs.1.json"
jobs_json publish-r2:success >"$work/other-commit/jobs.json"
check other-commit 1

if (( failures > 0 )); then
  echo "$failures check(s) failed." >&2
  exit 1
fi
echo "All checks passed."
