#!/bin/bash
# 信頼した GitHub ソースから取得して実行する初回取得用。既定ではインストールしない。
set -euo pipefail
umask 077
repo=1llum1n4t1s/remnifold
origin=https://remnifold-updates.kagayoi.com
version= work= apply=0 download_only=0
usage() { printf '%s\n' 'bootstrap-plugin.sh --version <version> [--work-root <absolute-external-directory>] [--apply | --download-only]' 'Requires native gh, curl, tar, and sha256sum or shasum. Default: verify publication only.'; }
fail() { printf '%s\n' "Bootstrap rejected: $1" >&2; exit 1; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) [ "$#" -ge 2 ] && [ -z "$version" ] || fail 'version argument'; version=$2; shift 2 ;;
    --work-root) [ "$#" -ge 2 ] && [ -z "$work" ] || fail 'work-root argument'; work=$2; shift 2 ;;
    --apply) [ "$apply" = 0 ] || fail 'duplicate apply'; apply=1; shift ;;
    --download-only) [ "$download_only" = 0 ] || fail 'duplicate download-only'; download_only=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) fail 'unknown argument' ;;
  esac
done
[ "$apply$download_only" != 11 ] || fail 'apply and download-only are mutually exclusive'
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]] || fail 'version must be SemVer'
prerelease=${version%%+*}; prerelease=${prerelease#*-}
if [[ "${version%%+*}" = *-* ]]; then
  old_ifs=$IFS; IFS=.; read -r -a identifiers <<<"$prerelease"; IFS=$old_ifs
  for part in "${identifiers[@]}"; do [[ ! "$part" =~ ^0[0-9]+$ ]] || fail 'numeric prerelease identifier'; done
fi
for tool in gh curl tar head mkfifo; do command -v "$tool" >/dev/null 2>&1 || fail "missing $tool"; done
if command -v sha256sum >/dev/null 2>&1; then hash_tool=sha256sum; else command -v shasum >/dev/null 2>&1 || fail 'missing SHA-256 tool'; hash_tool=shasum; fi
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-gnu ;;
  *) fail 'unsupported platform' ;;
esac
# /tmp 自体がリンクの macOS では既定の親だけを物理パスに解決する。
if [ -z "$work" ]; then work="$(cd "${TMPDIR:-/tmp}" && pwd -P)/remnifold-bootstrap"; fi
[[ "$work" = /* ]] && [[ "$work" != *$'\n'* ]] && [[ "$work" != *$'\r'* ]] || fail 'absolute work root required'
case "$work/" in *'/../'*|*'/./'*|*'//'*) fail 'noncanonical work root' ;; esac
check_links() {
  local p=$1
  while [ "$p" != / ]; do [ ! -L "$p" ] || fail 'linked ancestor'; p=${p%/*}; [ -n "$p" ] || p=/; done
}
check_acl() {
  if [ "$(uname -s)" = Darwin ]; then
    # macOS の標準 home にある everyone deny delete は権限を増やさない。
    # 曖昧な形式や allow は、mode にかかわらず拒否する。
    local entries ace
    entries=$(ls -lde "$1" | sed '1d') || fail 'cannot inspect directory ACL'
    while IFS= read -r ace; do
      [ -n "$ace" ] || continue
      [[ "$ace" =~ ^[[:space:]]*[0-9]+:\ (user|group):[^[:space:]]+\ deny\ [a-z_]+(,[a-z_]+)*$ ]] || fail 'untrusted or unsupported directory ACL'
    done <<<"$entries"
  fi
}
check_links "$work"
scan=$(dirname "$work")
while :; do
  if [ -d "$scan" ]; then
    check_acl "$scan"
    case "$(uname -s)" in Darwin) parent_mode=$(stat -f '%Mp%Lp' "$scan");; *) parent_mode=$(stat -c '%a' "$scan");; esac
    [[ "$parent_mode" =~ ^[0-7]{3,4}$ ]] || fail 'ancestor permissions'
    case "$(uname -s)" in Darwin) parent_uid=$(stat -f '%u' "$scan");; *) parent_uid=$(stat -c '%u' "$scan");; esac
    [ "$parent_uid" = 0 ] || [[ -O "$scan" ]] || fail 'untrusted ancestor owner'
    if [ $((8#$parent_mode & 0022)) != 0 ]; then
      [ $((8#$parent_mode & 01000)) != 0 ] || fail 'writable non-sticky ancestor'
    fi
  fi
  [ "$scan" != / ] || break
  scan=$(dirname "$scan")
done
script_dir=$(cd "$(dirname "$0")" && pwd -P)
script_file="$script_dir/$(basename "$0")"
case "$script_file" in "$work/"*) fail 'save the trusted bootstrap script outside the work root';; esac
codex_home=${CODEX_HOME:-"${USERPROFILE:-$HOME}/.codex"}
[[ "$codex_home" = /* ]] || fail 'CODEX_HOME must be absolute'
codex_home=${codex_home%/}
case "$codex_home/" in *'/../'*|*'/./'*|*'//'*) fail 'noncanonical Codex home' ;; esac
check_links "$codex_home"
# 既存親の物理パスで比較する。明示されたリンク経由の work root は上で拒否。
overlap() { case "$1/" in "$2/"*) return 0;; esac; case "$2/" in "$1/"*) return 0;; esac; return 1; }
scan=$script_dir
while :; do
  if [ -e "$scan/.git" ] || [ -e "$scan/.codex-plugin/plugin.json" ]; then
    overlap "$work" "$scan" && fail 'work root overlaps bootstrap repository or plugin source'
    break
  fi
  [ "$scan" != / ] || break
  scan=$(dirname "$scan")
done
overlap "$work" "$codex_home" && fail 'work root overlaps Codex home'
scan=$work
while [ "$scan" != / ]; do
  [ ! -e "$scan/.git" ] && [ ! -e "$scan/.codex-plugin/plugin.json" ] || fail 'work root is inside a repository or plugin'
  scan=${scan%/*}; [ -n "$scan" ] || scan=/
done
mkdir -p "$work"; check_links "$work"
check_acl "$work"
[[ -O "$work" ]] || fail 'work root must be owned by the current user'
case "$(uname -s)" in Darwin) mode=$(stat -f '%Mp%Lp' "$work");; *) mode=$(stat -c '%a' "$work");; esac
[[ "$mode" =~ ^[0-7]{3,4}$ ]] && [ $((8#$mode & 0022)) = 0 ] || fail 'work root must not be writable by other users'
session=$(mktemp -d "$work/bootstrap.XXXXXXXX")
check_acl "$session"
printf '%s\n' "Bootstrap state retained at $session"
# 初回取得物・診断を保持。既存データを削除しない。実行中の子だけ停止する。
active=
# 各子を独立したプロセスグループにして、期限切れ時の孫プロセスも停止する。
set -m
stop_child() {
  if [ -n "${active:-}" ]; then
    kill -TERM -- "-$active" 2>/dev/null || true
    kill -KILL -- "-$active" 2>/dev/null || true
    wait "$active" 2>/dev/null || true
    active=
  fi
}
trap stop_child EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
counter=0
run_bound() {
  local cap=$1 seconds=$2 out=$3; shift 3
  local err="$out.stderr" started=$SECONDS rc=0
  : >"$out"
  : >"$err"
  (
    set +e
    set +m
    mkfifo "$out.stderr.pipe" || exit 1
    head -c 65537 <"$out.stderr.pipe" >"$err" &
    stderr_reader=$!
    "$@" 2>"$out.stderr.pipe" | head -c "$((cap+1))" >"$out"
    command_status=$?
    wait "$stderr_reader"
    reader_status=$?
    [ "$reader_status" = 0 ] || command_status=$reader_status
    exit "$command_status"
  ) & active=$!
  while kill -0 "$active" 2>/dev/null; do
    if [ $((SECONDS-started)) -ge "$seconds" ] || [ "$(wc -c <"$out")" -gt "$cap" ] || [ "$(wc -c <"$err")" -gt 65536 ]; then
      stop_child; fail 'subprocess time/output bound'
    fi
    sleep 1
  done
  wait "$active" || rc=$?; active=
  [ "$rc" = 0 ] && [ "$(wc -c <"$out")" -le "$cap" ] && [ "$(wc -c <"$err")" -le 65536 ] || fail 'subprocess failed (diagnostics retained)'
}
query() {
  counter=$((counter+1)); result="$session/query-$counter.txt"
  run_bound 1048576 120 "$result" gh api --hostname github.com "repos/$repo${1:+/$1}" --jq "$2"
  value=$(cat "$result")
}
query '' 'if .full_name == "1llum1n4t1s/remnifold" and (.id|type)=="number" then .id else error("repository identity") end'
repository_id=$value; [[ "$repository_id" =~ ^[1-9][0-9]*$ ]] || fail 'repository ID'
query 'actions/workflows/ci.yml' 'if .path == ".github/workflows/ci.yml" and .name == "CI" then .id else error("workflow identity") end'
workflow_id=$value; [[ "$workflow_id" =~ ^[1-9][0-9]*$ ]] || fail 'workflow ID'
query "git/ref/heads/release/$version" 'if .object.type == "commit" then .object.sha else error("release identity") end'
commit=$value; [[ "$commit" =~ ^[0-9a-f]{40}$ ]] || fail 'commit identity'
query "contents/.codex-plugin/plugin.json?ref=$commit" '.content | @base64d | fromjson | .version'
[ "$value" = "$version" ] || fail 'release version'
query "contents/.github/remnifold-distribution.json?ref=$commit" '.content | @base64d | fromjson | if .schema == 1 and .repository == "1llum1n4t1s/remnifold" and .stagingBucket == "remnifold-builds" and .publicBucket == "remnifold-updates" and .publicBaseUrl == "https://remnifold-updates.kagayoi.com" and .statusContext == "remnifold/r2/v1" and .workflowPath == ".github/workflows/ci.yml" then "r2" else error("distribution identity") end'
[ "$value" = r2 ] || fail 'R2 configuration'
identity=".name == \"CI\" and .head_sha == \"$commit\" and .event == \"push\" and .head_branch == \"release/$version\" and .path == \".github/workflows/ci.yml\" and .workflow_id == $workflow_id and .repository.full_name == \"$repo\" and .head_repository.full_name == \"$repo\" and .repository.id == $repository_id and .head_repository.id == $repository_id"
query "actions/runs?head_sha=$commit&event=push&per_page=100" "if (.workflow_runs|length) >= 100 then error(\"run discovery limit\") else [.workflow_runs[] | select($identity)] | sort_by([.id,.run_attempt]) | last | if .status == \"completed\" and .conclusion == \"success\" then [.id,.run_attempt] | @tsv else error(\"latest CI not successful\") end end"
IFS=$'\t' read -r witness_run witness_attempt extra <<<"$value"
[[ "$witness_run" =~ ^[1-9][0-9]*$ ]] && [[ "$witness_attempt" =~ ^[1-9][0-9]*$ ]] && [ -z "${extra:-}" ] || fail 'CI run identity'
query "actions/runs/$witness_run/attempts/$witness_attempt/jobs?per_page=100" "if (.jobs|length) >= 100 or .total_count != (.jobs|length) then error(\"job discovery limit\") else [.jobs[] | select(.name == \"publish-r2\")] | if length != 1 then error(\"publisher job count\") else .[0] | if .run_id == $witness_run and .head_sha == \"$commit\" and .status == \"completed\" and .conclusion == \"success\" then .id else error(\"publisher job identity\") end end end"
job=$value; [[ "$job" =~ ^[1-9][0-9]*$ ]] || fail 'publisher job ID'
run_bound 1048576 30 "$session/gh-api-help.txt" gh api --help
logs="$session/publisher.log"
if LC_ALL=C grep -q -- '--allow-escape-sequences' "$session/gh-api-help.txt"; then
  run_bound 4194304 120 "$logs" gh api --hostname github.com --allow-escape-sequences "repos/$repo/actions/jobs/$job/logs"
else
  run_bound 4194304 120 "$logs" gh api --hostname github.com "repos/$repo/actions/jobs/$job/logs"
fi
manifest_seen=0; witness_version=; assets=0; seen=' '; selected_hash= selected_size= selected_key=
while IFS= read -r line || [ -n "$line" ]; do
  line=${line%$'\r'}
  case "$line" in *REMNIFOLD_R2_MANIFEST*|*REMNIFOLD_R2_ASSET*) ;; *) continue ;; esac
  if [[ "$line" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z\  ]]; then line=${line#* }; fi
  if [[ "$line" =~ ^REMNIFOLD_R2_MANIFEST_V[12]\ ([^\ ]+)\ ([0-9a-f]{40})\ ([1-9][0-9]*)\ ([1-9][0-9]*)\ ([0-9a-f]{64})$ ]]; then
    [ "$manifest_seen" = 0 ] && [ "${BASH_REMATCH[1]}" = "$version" ] && [ "${BASH_REMATCH[2]}" = "$commit" ] || fail 'manifest witness'
    publication_run=${BASH_REMATCH[3]}; publication_attempt=${BASH_REMATCH[4]}; manifest_hash=${BASH_REMATCH[5]}; manifest_seen=1
    marker=${line%% *}; marker_version=${marker##*_V}
    [ -z "$witness_version" ] || [ "$witness_version" = "$marker_version" ] || fail 'mixed witness versions'
    witness_version=$marker_version
  elif [[ "$line" =~ ^REMNIFOLD_R2_ASSET_V[12]\ ([a-z0-9_-]+)\ ([0-9a-f]{64})\ ([1-9][0-9]{0,8})\ ([a-zA-Z0-9_./+-]+)$ ]]; then
    t=${BASH_REMATCH[1]}; h=${BASH_REMATCH[2]}; n=${BASH_REMATCH[3]}; k=${BASH_REMATCH[4]}
    marker=${line%% *}; marker_version=${marker##*_V}
    [ -z "$witness_version" ] || [ "$witness_version" = "$marker_version" ] || fail 'mixed witness versions'
    witness_version=$marker_version
    [ "$marker_version/$t" != '2/x86_64-apple-darwin' ] || fail 'unsupported witness target'
    case "$t" in x86_64-pc-windows-msvc|aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) fail 'unknown witness target' ;; esac
    case "$seen" in *" $t "*) fail 'duplicate witness target' ;; esac
    [ "$n" -le 134217728 ] || fail 'archive size bound'
    seen="$seen$t "; assets=$((assets+1))
    printf '%s\t%s\t%s\t%s\n' "$t" "$h" "$n" "$k" >>"$session/assets.tsv"
    if [ "$t" = "$target" ]; then selected_hash=$h; selected_size=$n; selected_key=$k; fi
  else fail 'malformed publication witness'; fi
done <"$logs"
expected_assets=4; [ "$witness_version" != 1 ] || expected_assets=5
[ "$manifest_seen" = 1 ] && [ "$assets" = "$expected_assets" ] && [ -n "$selected_key" ] || fail 'incomplete publication witness'
while IFS=$'\t' read -r t h n k; do
  [ "$k" = "releases/$version/$commit/$publication_run-$publication_attempt/$t/$h.tar.gz" ] || fail 'noncanonical archive key'
done <"$session/assets.tsv"
query "actions/runs/$publication_run/attempts/$publication_attempt" "if $identity and .id == $publication_run and .run_attempt == $publication_attempt and .status == \"completed\" and .conclusion == \"success\" then \"publication\" else error(\"publication CI identity\") end"
[ "$value" = publication ] || fail 'publication identity'
archive="$session/archive.tar.gz"
url="$origin/${selected_key//+/%2B}"
download_archive() {
  # 古い curl の chunked 応答でも、実ストリームを最大値+1 バイトで止める。
  # pipefail により curl の失敗/SIGPIPE も拒否し、stderr の 200 だけを成功とする。
  curl -q --proto '=https' --proto-redir '=https' --max-redirs 0 --connect-timeout 20 --max-time 120 --max-filesize 134217728 --fail --silent --show-error --write-out '%{stderr}%{http_code}' "$url" | head -c 134217729
}
run_bound 134217728 150 "$archive" download_archive
[ "$(cat "$archive.stderr")" = 200 ] && [ "$(wc -c <"$archive")" -eq "$selected_size" ] || fail 'archive response/size'
if [ "$hash_tool" = sha256sum ]; then actual=$(sha256sum "$archive"); else actual=$(shasum -a 256 "$archive"); fi
[ "${actual%% *}" = "$selected_hash" ] || fail 'archive SHA-256'
if [ "$download_only" = 1 ]; then printf '%s\n' "Verified archive: $archive"; exit 0; fi
if [ "$apply" = 0 ]; then
  query "git/trees/$commit?recursive=1" 'if .truncated == false and (.tree | type) == "array" then any(.tree[]; .type == "blob" and .path == "native/src/updater/bootstrap.rs") else error("bootstrap capability tree") end'
  [ "$value" = true ] || fail 'release lacks native publication checking; use --download-only or a trusted current native plugin-bootstrap with --version set to this release'
fi
member=remnifold/bin/remnifold
run_bound 1024 60 "$session/member.txt" tar -tzf "$archive" "$member"
[ "$(cat "$session/member.txt")" = "$member" ] || fail 'executable member must be unique'
run_bound 4096 60 "$session/member-type.txt" tar -tvzf "$archive" "$member"
case "$(cat "$session/member-type.txt")" in -*) ;; *) fail 'executable must be a regular file' ;; esac
mkdir -p "$session/remnifold/bin"
binary="$session/$member"
run_bound 134217728 120 "$binary" tar -xOzf "$archive" "$member"
[ -s "$binary" ] || fail 'empty executable'
chmod 700 "$binary"
printf '%s\n' "Verified $target archive from CI $witness_run attempt $witness_attempt, publisher job $job."
if [ "$apply" = 1 ]; then
  run_bound 1048576 600 "$session/check-published.txt" "$binary" plugin-update --version "$version" --work-root "$session/update" --check-published
  run_bound 1048576 600 "$session/apply.txt" "$binary" plugin-update --version "$version" --work-root "$session/update" --apply
  run_bound 1048576 600 "$session/verify-only.txt" "$binary" plugin-update --version "$version" --work-root "$session/update" --verify-only
  printf '%s\n' 'Installation verified. Start a new Codex task and review the current hook trust settings.'
  printf '%s\n' "Keep the verification receipt under $session/update for verification without retained R2 objects or CI logs; GitHub access remains required."
else
  run_bound 1048576 600 "$session/check-published.txt" "$binary" plugin-bootstrap --version "$version" --work-root "$session/update"
  printf '%s\n' 'Publication verified; installation was not changed and no verification receipt was created. Use --apply to install.'
fi
