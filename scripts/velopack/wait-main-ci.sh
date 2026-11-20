#!/usr/bin/env bash
# release-velopack.yml の gate ジョブが使う。配信の起点（版タグ v<x.y.z>）を確かめ、「同じコミットを既定ブランチ
# （main）へ push したときに起動した ci.yml が最後まで成功したこと」を確認できるまで待つ。確認できたら 0、できなければ
# 非 0 で終わる。
#
# 目的:
#   Setup（Velopack）は main のコミットに付けた軽量タグ v<x.y.z> の push で配信する（GitHub Release は作らない）。
#   公開バケットへ書く前に、そのコミットが main で速い検査（ci.yml の server-contracts・windows-native-contracts・
#   windows-client-contracts）に通っていることを要求する。全ターゲットのパッケージ作成・updater-test は main の push では
#   走らず、版タグの push で起動する ci.yml の run が配信と並行して行う事後確認で、その成否はここでは待たない
#   （待つのは head_branch が既定ブランチの run だけで、タグの run は数えない）。
#   そうしないと CI が失敗したコミットの Setup が公開バケットに載ってしまう（AGENTS.md の公開境界）。
#   厳格な install 経路（releases/<version>/index.json と publish-r2）は 1.0.32 で凍結したため、publish-r2 の成功は
#   求めない（main の ci.yml では native-package と publish-r2 は skipped で、run は success になる）。
#   このスクリプトは Secrets も Environment も持たない gate ジョブで動き、読み取り専用の権限
#   （actions: read, contents: read）だけを使う。
#
# 合格条件（すべて。前半の 5 つは待つ前に無条件で確かめ、満たさなければ待たずに失敗する）:
#   - 起動の ref がタグで（GITHUB_REF_TYPE == tag、GITHUB_REF == refs/tags/<名前>）、名前が厳密な v<x.y.z>
#     （先頭の 0 なし。v1 や v1.2.3-rc などは拒否）。
#   - タグ名が v<.codex-plugin/plugin.json の version>。
#   - タグが軽量タグ（object.type == commit）で、GITHUB_SHA を指す。注釈付きタグは拒否。
#   - GITHUB_SHA が既定ブランチの祖先（compare API の status が ahead / identical で behind_by が 0）。
#     depth=1 の checkout では祖先を判定できないため API で見る。
#   - GITHUB_SHA を head とし、既定ブランチへの push で起動した ci.yml の run がある（最新の run / attempt を使う）。
#     その run が completed かつ success。failure / cancelled / timed_out などは待たずに即失敗にする。
#     既定ブランチ以外（release/* など）の run は数えない。
#   - 成功を確認した後にも、タグが同じコミットを指したままである（待つ間にタグが動かされていない）。
#
# 入力（環境変数）:
#   GITHUB_REPOSITORY, GITHUB_SHA, GITHUB_REF, GITHUB_REF_TYPE, GITHUB_REF_NAME   Actions が設定する値。
#   GH_TOKEN                                         gh 用（Actions では github.token）。
#   GATE_TIMEOUT_SECONDS (既定 8400 = 140 分)         これを過ぎたら失敗。ジョブの timeout-minutes より短くする。
#   GATE_DISCOVERY_SECONDS (既定 900 = 15 分)          この間に該当する ci.yml の run が 1 件も見つからなければ失敗
#                                                    （まとめて push した途中のコミットなど、main の push の head で
#                                                    なかったコミットにタグを付けた場合。main の head へ付け直す）。
#   GATE_INTERVAL_SECONDS (既定 30)                   ポーリング間隔。
#   REMNIFOLD_PLUGIN_MANIFEST (既定 .codex-plugin/plugin.json)  版を読むファイル（検証用に差し替えられる）。
#   GH_BIN (既定 gh) / PYTHON_BIN (既定 python3)      検証用にスタブへ差し替えるための上書き先。
#
# JSON の解釈は jq ではなく python3 の標準ライブラリで行う（Actions の ubuntu-latest に標準で入っており、
# 手元の検証用スタブ gh と同じ経路で動かせる）。実行は `python3 -I` で、作業ディレクトリのファイルを読み込ませない。
set -euo pipefail

gh_bin="${GH_BIN:-gh}"
python_bin="${PYTHON_BIN:-python3}"
timeout_seconds="${GATE_TIMEOUT_SECONDS:-8400}"
discovery_seconds="${GATE_DISCOVERY_SECONDS:-900}"
interval_seconds="${GATE_INTERVAL_SECONDS:-30}"
manifest="${REMNIFOLD_PLUGIN_MANIFEST:-.codex-plugin/plugin.json}"
# 一時的な API 失敗（ネットワーク・5xx）は数回だけ許す。連続したら失敗にする。
max_api_failures=5

repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
sha="${GITHUB_SHA:?GITHUB_SHA is required}"
ref="${GITHUB_REF:?GITHUB_REF is required}"
ref_type="${GITHUB_REF_TYPE:?GITHUB_REF_TYPE is required}"
tag="${GITHUB_REF_NAME:?GITHUB_REF_NAME is required}"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "Unexpected commit id." >&2; exit 1; }
[[ "$repository" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || { echo "Unexpected repository name." >&2; exit 1; }
[[ "$timeout_seconds" =~ ^[0-9]+$ && "$discovery_seconds" =~ ^[0-9]+$ && "$interval_seconds" =~ ^[0-9]+$ ]] || { echo "Invalid timing settings." >&2; exit 1; }
# タグ以外（ブランチの push・ブランチからの dispatch）では配信しない。タグ名は URL へ入れるため、厳密な形だけを許す。
[[ "$ref_type" == tag ]] || { echo "Publication starts only from a version tag, not from $ref." >&2; exit 1; }
[[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || { echo "Not a version tag (v<x.y.z>): $tag" >&2; exit 1; }
[[ "$ref" == "refs/tags/$tag" ]] || { echo "The ref $ref does not match the tag $tag." >&2; exit 1; }

# 判定本体。標準入力の JSON（version だけはファイル）と引数から、次のいずれか 1 行だけを標準出力へ出す。
#   wait <理由>          まだ決まらない（待って再確認する）
#   none <理由>          該当する run がまだ無い（発見の期限まで待つ）
#   fail <理由>          失敗が確定した
#   ok <内容>            合格
decide='
import json, re, sys
mode = sys.argv[1]
if mode == "version":
    path, tag = sys.argv[2:4]
    try:
        with open(path, encoding="utf-8") as handle:
            version = json.load(handle).get("version")
    except (OSError, ValueError, AttributeError) as error:
        print("fail cannot read the package version: %s" % type(error).__name__); sys.exit(0)
    if not isinstance(version, str) or not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        print("fail unexpected package version"); sys.exit(0)
    if tag != "v" + version:
        print("fail the tag %s does not match the package version %s" % (tag, version)); sys.exit(0)
    print("ok %s" % version); sys.exit(0)
data = json.load(sys.stdin)
if mode == "tag":
    tag, sha = sys.argv[2:4]
    if not isinstance(data, dict) or data.get("ref") != "refs/tags/" + tag:
        print("fail the tag ref refs/tags/%s was not found exactly" % tag); sys.exit(0)
    target = data.get("object") or {}
    if target.get("type") != "commit":
        print("fail refs/tags/%s is not a lightweight tag (object type %s)" % (tag, target.get("type"))); sys.exit(0)
    if target.get("sha") != sha:
        print("fail refs/tags/%s points at %s, not at %s" % (tag, target.get("sha"), sha)); sys.exit(0)
    print("ok tag"); sys.exit(0)
if mode == "default":
    branch = data.get("default_branch") if isinstance(data, dict) else None
    if not isinstance(branch, str) or not re.fullmatch(r"[A-Za-z0-9._][A-Za-z0-9._/-]*", branch) or ".." in branch:
        print("fail unexpected default branch"); sys.exit(0)
    print("ok %s" % branch); sys.exit(0)
if mode == "compare":
    status = data.get("status") if isinstance(data, dict) else None
    behind = data.get("behind_by") if isinstance(data, dict) else None
    if status not in ("ahead", "identical") or behind != 0:
        print("fail the commit is not an ancestor of the default branch (%s, behind by %s)" % (status, behind)); sys.exit(0)
    print("ok ancestor"); sys.exit(0)
if mode == "runs":
    sha, branch, repository = sys.argv[2:5]
    all_runs = data.get("workflow_runs", [])
    if len(all_runs) >= 100:
        print("fail CI run discovery limit reached"); sys.exit(0)
    runs = [
        r for r in all_runs
        if r.get("head_sha") == sha
        and r.get("head_branch") == branch
        and r.get("event") == "push"
        and r.get("path") == ".github/workflows/ci.yml"
        and (r.get("repository") or {}).get("full_name") == repository
        and (r.get("head_repository") or {}).get("full_name") == repository
    ]
    if not runs:
        print("none no ci.yml push run on %s for this commit yet" % branch); sys.exit(0)
    latest = max(runs, key=lambda r: (r.get("id", 0), r.get("run_attempt", 0)))
    status, conclusion = latest.get("status"), latest.get("conclusion")
    if status != "completed":
        print("wait ci.yml run %s attempt %s is %s" % (latest.get("id"), latest.get("run_attempt"), status)); sys.exit(0)
    if conclusion != "success":
        print("fail ci.yml run %s attempt %s concluded %s" % (latest.get("id"), latest.get("run_attempt"), conclusion)); sys.exit(0)
    print("ok %s %s" % (latest.get("id"), latest.get("run_attempt"))); sys.exit(0)
print("fail unknown mode")
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
# 待つ前の検査用。成功するまで間隔をあけて取り直す（連続失敗の上限で fetch が終了させる）。
fetch_required() {
  until fetch "$1"; do
    sleep "$interval_seconds"
  done
}

decide_with() {
  printf '%s' "$1" | "$python_bin" -I -c "$decide" "${@:2}"
}
# ok なら内容を標準出力へ、それ以外は理由を出して失敗する。
require_ok() {
  case "$1" in
    ok\ *) printf '%s\n' "${1#ok }" ;;
    fail\ *) echo "${1#fail }" >&2; exit 1 ;;
    *) echo "Unexpected verdict: $1" >&2; exit 1 ;;
  esac
}

verify_tag() {
  fetch_required "repos/$repository/git/ref/tags/$tag"
  require_ok "$(decide_with "$response" tag "$tag" "$sha")" >/dev/null
}

# 1. 版ファイルとの一致（API を呼ぶ前に確かめる）。
version="$(require_ok "$("$python_bin" -I -c "$decide" version "$manifest" "$tag" </dev/null)")"
# 2. 軽量タグで、このコミットを指す。
verify_tag
# 3. 既定ブランチの祖先。
fetch_required "repos/$repository"
default_branch="$(require_ok "$(decide_with "$response" default)")"
fetch_required "repos/$repository/compare/$sha...$default_branch?per_page=1"
require_ok "$(decide_with "$response" compare)" >/dev/null
echo "Tag $tag (version $version) is a lightweight tag on $sha, an ancestor of $default_branch."

# 4. 同じコミットの既定ブランチへの push で起動した ci.yml の成功を待つ。
deadline=$((SECONDS + timeout_seconds))
discovery_deadline=$((SECONDS + discovery_seconds))
while true; do
  if fetch "repos/$repository/actions/workflows/ci.yml/runs?head_sha=$sha&event=push&branch=$default_branch&per_page=100"; then
    verdict="$(decide_with "$response" runs "$sha" "$default_branch" "$repository")"
    case "$verdict" in
      ok\ *)
        read -r _ run_id run_attempt <<<"$verdict"
        [[ "$run_id" =~ ^[0-9]+$ && "$run_attempt" =~ ^[0-9]+$ ]] || { echo "Unexpected run identity." >&2; exit 1; }
        # 待つ間にタグが動かされていないこと。
        verify_tag
        echo "ci.yml run $run_id attempt $run_attempt on $default_branch succeeded for $sha; $tag still points at it."
        exit 0
        ;;
      fail\ *) echo "${verdict#fail }" >&2; exit 1 ;;
      none\ *)
        echo "${verdict#none }"
        if (( SECONDS >= discovery_deadline )); then
          echo "No ci.yml push run on $default_branch was found for $sha. Tag a commit that was the head of a push to $default_branch." >&2
          exit 1
        fi
        ;;
      wait\ *) echo "${verdict#wait }" ;;
      *) echo "Unexpected verdict: $verdict" >&2; exit 1 ;;
    esac
  fi
  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for ci.yml on $default_branch to succeed for this commit." >&2
    exit 1
  fi
  sleep "$interval_seconds"
done
