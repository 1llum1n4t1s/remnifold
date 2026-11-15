#!/usr/bin/env bash
# release-velopack.yml の gate ジョブが使う。「このコミットの ci.yml が最後まで成功し、R2 への公開 job
# (publish-r2) が成功済みであること」を確認できるまで待ち、確認できたら 0、できなければ非 0 で終わる。
#
# 目的:
#   release/* への push では ci.yml と release-velopack.yml が同時に起動する。Setup（Velopack）の配信は、
#   厳格な install 用の公開（releases/<version>/index.json）が先に成功してからでないと始めない。
#   そうしないと CI が失敗した版の Setup だけが公開バケットに載ってしまう（AGENTS.md の公開境界）。
#   このスクリプトは Secrets も Environment も持たない gate ジョブで動き、読み取り専用の actions: read だけを使う。
#
# 合格条件（すべて）:
#   - GITHUB_SHA を head とする、同じブランチの push で起動した ci.yml の run がある（最新の run / attempt を使う）。
#   - その run が completed かつ success。failure / cancelled / timed_out などは待たずに即失敗にする。
#   - その最新 attempt に、名前が publish-r2 の job がちょうど 1 件あり、success である。
#     （publish-r2 が skipped / 0 件 / 複数なら失敗。strict 公開の witness は「唯一成功した publish-r2 job」だから。）
#
# 入力（環境変数）:
#   GITHUB_REPOSITORY, GITHUB_SHA, GITHUB_REF_NAME   Actions が設定する値（ブランチ名は release/<版>）。
#   GH_TOKEN                                         gh 用（Actions では github.token）。
#   GATE_TIMEOUT_SECONDS (既定 8400 = 140 分)         これを過ぎたら失敗。ジョブの timeout-minutes より短くする。
#   GATE_INTERVAL_SECONDS (既定 30)                   ポーリング間隔。
#   GH_BIN (既定 gh) / PYTHON_BIN (既定 python3)      検証用にスタブへ差し替えるための上書き先。
#
# JSON の解釈は jq ではなく python3 の標準ライブラリで行う（Actions の ubuntu-latest に標準で入っており、
# 手元の検証用スタブ gh と同じ経路で動かせる）。実行は `python3 -I` で、作業ディレクトリのファイルを読み込ませない。
set -euo pipefail

gh_bin="${GH_BIN:-gh}"
python_bin="${PYTHON_BIN:-python3}"
timeout_seconds="${GATE_TIMEOUT_SECONDS:-8400}"
interval_seconds="${GATE_INTERVAL_SECONDS:-30}"
# 一時的な API 失敗（ネットワーク・5xx）は数回だけ許す。連続したら失敗にする。
max_api_failures=5

repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
sha="${GITHUB_SHA:?GITHUB_SHA is required}"
branch="${GITHUB_REF_NAME:?GITHUB_REF_NAME is required}"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "Unexpected commit id." >&2; exit 1; }
[[ "$repository" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || { echo "Unexpected repository name." >&2; exit 1; }
# ブランチ名は URL へ入れるため、release/<版> に使う文字だけを許す。
[[ "$branch" =~ ^release/[A-Za-z0-9._-]+$ ]] || { echo "Not a release branch: $branch" >&2; exit 1; }
[[ "$timeout_seconds" =~ ^[0-9]+$ && "$interval_seconds" =~ ^[0-9]+$ ]] || { echo "Invalid timing settings." >&2; exit 1; }

# 判定本体。標準入力の JSON と引数から、次のいずれか 1 行だけを標準出力へ出す。
#   wait <理由>          まだ決まらない（待って再確認する）
#   fail <理由>          失敗が確定した
#   run <id> <attempt>   最新の run が success（次に job を確認する）
#   ok <job id>          publish-r2 が唯一かつ成功（合格）
decide='
import json, sys
mode = sys.argv[1]
data = json.load(sys.stdin)
if mode == "runs":
    sha, branch, repository = sys.argv[2:5]
    runs = [
        r for r in data.get("workflow_runs", [])
        if r.get("head_sha") == sha
        and r.get("head_branch") == branch
        and r.get("event") == "push"
        and r.get("path") == ".github/workflows/ci.yml"
        and (r.get("repository") or {}).get("full_name") == repository
        and (r.get("head_repository") or {}).get("full_name") == repository
    ]
    if len(data.get("workflow_runs", [])) >= 100:
        print("fail CI run discovery limit reached"); sys.exit(0)
    if not runs:
        print("wait no ci.yml run for this commit yet"); sys.exit(0)
    latest = max(runs, key=lambda r: (r.get("id", 0), r.get("run_attempt", 0)))
    status, conclusion = latest.get("status"), latest.get("conclusion")
    if status != "completed":
        print("wait ci.yml run %s attempt %s is %s" % (latest.get("id"), latest.get("run_attempt"), status)); sys.exit(0)
    if conclusion != "success":
        print("fail ci.yml run %s attempt %s concluded %s" % (latest.get("id"), latest.get("run_attempt"), conclusion)); sys.exit(0)
    print("run %s %s" % (latest["id"], latest["run_attempt"]))
elif mode == "jobs":
    jobs = data.get("jobs", [])
    if data.get("total_count", len(jobs)) > len(jobs):
        print("fail job list is incomplete"); sys.exit(0)
    publishers = [j for j in jobs if j.get("name") == "publish-r2"]
    if len(publishers) != 1:
        print("fail expected exactly one publish-r2 job, found %d" % len(publishers)); sys.exit(0)
    job = publishers[0]
    if job.get("status") != "completed" or job.get("conclusion") != "success":
        print("fail publish-r2 job is %s/%s" % (job.get("status"), job.get("conclusion"))); sys.exit(0)
    print("ok %s" % job.get("id"))
'

api_failures=0
# gh api の結果を変数 response へ入れる（$(...) の中で呼ぶと失敗回数と exit が親へ届かないため、直接呼ぶ）。
# 失敗は連続回数を数え、上限を超えたら即失敗にする。
response=""
fetch() {
  if response="$("$gh_bin" api "$1")"; then
    api_failures=0
    return 0
  fi
  api_failures=$((api_failures + 1))
  echo "GitHub API request failed ($api_failures/$max_api_failures): $1" >&2
  if (( api_failures >= max_api_failures )); then
    echo "Giving up after repeated GitHub API failures." >&2
    exit 1
  fi
  return 1
}

decide_with() {
  printf '%s' "$1" | "$python_bin" -I -c "$decide" "${@:2}"
}

deadline=$((SECONDS + timeout_seconds))
while true; do
  if fetch "repos/$repository/actions/workflows/ci.yml/runs?head_sha=$sha&event=push&branch=$branch&per_page=100"; then
    verdict="$(decide_with "$response" runs "$sha" "$branch" "$repository")"
    case "$verdict" in
      fail\ *) echo "${verdict#fail }" >&2; exit 1 ;;
      run\ *)
        read -r _ run_id run_attempt <<<"$verdict"
        [[ "$run_id" =~ ^[0-9]+$ && "$run_attempt" =~ ^[0-9]+$ ]] || { echo "Unexpected run identity." >&2; exit 1; }
        # 最新 attempt の job だけを見る（過去の attempt の publish-r2 は数えない）。
        if fetch "repos/$repository/actions/runs/$run_id/attempts/$run_attempt/jobs?per_page=100"; then
          jobs_verdict="$(decide_with "$response" jobs)"
          case "$jobs_verdict" in
            ok\ *) echo "ci.yml run $run_id attempt $run_attempt succeeded with publish-r2 job ${jobs_verdict#ok }."; exit 0 ;;
            fail\ *) echo "${jobs_verdict#fail }" >&2; exit 1 ;;
            *) echo "Unexpected verdict: $jobs_verdict" >&2; exit 1 ;;
          esac
        fi
        ;;
      wait\ *) echo "${verdict#wait }" ;;
      *) echo "Unexpected verdict: $verdict" >&2; exit 1 ;;
    esac
  fi
  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for ci.yml to publish this commit." >&2
    exit 1
  fi
  sleep "$interval_seconds"
done
