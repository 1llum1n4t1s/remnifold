#!/usr/bin/env bash
# Linux の Setup（Velopack の AppImage）版の隔離 E2E。.github/workflows/velopack-linux.yml から呼ぶ。
#
# 使い方:
#   e2e-linux.sh <空の絶対パスの作業フォルダー> --old <9.0.0 の packages フォルダー> --new <9.0.1 の packages フォルダー>
# packages フォルダーは scripts/velopack/pack-linux.sh --pack-id RemnifoldVelopackTest --binary <update-feed-test 付き>
# の出力。本物の利用者環境に触れないよう、テスト用 packId 以外は受け付けず、HOME・XDG_CONFIG_HOME・REMNIFOLD_HOME・
# CLAUDE_CONFIG_DIR・CODEX_HOME・TMPDIR を作業フォルダーの配下に閉じ、PATH の先頭に引数を記録するだけのスタブ
# `claude` / `codex` を置く。Velopack の Linux の取得先は /var/tmp/velopack/<packId>/packages に固定されている
# （HOME に従わない。velopack crate の locator）ため、テスト用 packId のその場所だけを最初に空にする。
#
# 確かめること（各段階で assert し、失敗したら非 0 で終わる）:
#   A. 導入（AppImage の配置）→ 引数なしの起動で Claude Code・Codex への登録と XDG autostart の作成。2回目は何もしない。
#   B. 複数の `mcp` を同時に起動しても、取得はちょうど1回。
#   C. `mcp` が動いている間の `update-apply` は busy（適用しない・強制終了しない）。
#   D. 置き場を他人が書けるようにすると untrusted。戻して、他のプロセスが無ければ適用し、AppImage が 9.0.1 になる。
#      更新後の起動は版だけを更新し、CLI を呼ばない。保留が無くなれば nothingPending。
#   E. `uninstall-setup` は予定だけを示し、`--apply` で登録・autostart・登録の状態を外す。
set -euo pipefail
umask 022

pack_id=RemnifoldVelopackTest
usage() { echo 'usage: e2e-linux.sh <absolute-empty-work-directory> --old <packages-9.0.0> --new <packages-9.0.1>' >&2; exit 2; }
[[ $# -ge 1 ]] || usage
work="$1"; shift
old=''; new=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --old) [[ $# -ge 2 ]] || usage; old="$2"; shift ;;
    --new) [[ $# -ge 2 ]] || usage; new="$2"; shift ;;
    *) usage ;;
  esac
  shift
done
[[ "$work" = /* && -d "$work" && ! -L "$work" && -z "$(ls -A "$work")" ]] || { echo 'The work directory must be an empty, ordinary, absolute directory.' >&2; exit 1; }
[[ -d "$old" && -d "$new" ]] || usage
case "$(uname -m)" in
  x86_64) channel=linux-x64 ;;
  aarch64) channel=linux-arm64 ;;
  *) echo 'Unsupported CPU.' >&2; exit 1 ;;
esac

step() { printf '\n==== %s\n' "$*"; }
fail() { printf 'ASSERT FAILED: %s\n' "$*" >&2; exit 1; }

# ───────────── 隔離 ─────────────
mkdir -p "$work"/{home,xdg,rhome,claude,codex,tmp,stubs,feed,"Apps dir",x}
export HOME="$work/home" XDG_CONFIG_HOME="$work/xdg" REMNIFOLD_HOME="$work/rhome"
export CLAUDE_CONFIG_DIR="$work/claude" CODEX_HOME="$work/codex" TMPDIR="$work/tmp"
export STUB_LOG_DIR="$work/stubs"
unset APPIMAGE APPDIR REMNIFOLD_UPDATE_FEED_DIR REMNIFOLD_UPDATE_ALLOW_UNSIGNED || true
state="$REMNIFOLD_HOME/remnifold"
autostart="$XDG_CONFIG_HOME/autostart/com.kagayoi.$pack_id.update.desktop"
packages="/var/tmp/velopack/$pack_id/packages"
# テスト用 packId の取得先だけを空にする（本番の RemnifoldApp には触れない）。
[[ "$pack_id" = RemnifoldVelopackTest ]] || fail 'unexpected packId'
rm -rf "/var/tmp/velopack/$pack_id"

# 引数を記録し、`mcp add/get/remove` を状態ファイルで模すだけのスタブ。
for tool in claude codex; do
  cat > "$STUB_LOG_DIR/$tool" <<'STUB'
#!/usr/bin/env bash
# 記録先は環境変数に頼らず、スタブ自身の場所から決める（呼び出し側が環境を絞っても記録できるように）。
dir="$(cd "$(dirname "$0")" && pwd)"
name="$(basename "$0")"
printf '%s\n' "$*" >> "$dir/$name.log"
mcp="$dir/$name.mcp"
case "${1:-} ${2:-}" in
  'mcp add') shift 2; printf '%s\n' "$*" > "$mcp" ;;
  'mcp get') [[ -f "$mcp" ]] || exit 1; cat "$mcp" ;;
  'mcp remove') [[ -f "$mcp" ]] || exit 1; rm -f "$mcp" ;;
esac
exit 0
STUB
  chmod 755 "$STUB_LOG_DIR/$tool"
  : > "$STUB_LOG_DIR/$tool.log"
done
export PATH="$STUB_LOG_DIR:$PATH"

# 導入: 利用者が AppImage を置く（空白を含むフォルダーで、引用の扱いも確かめる）。
app="$work/Apps dir/$pack_id.AppImage"
cp "$old/$pack_id-$channel.AppImage" "$app"
chmod 755 "$app"
app="$(realpath "$app")"
log="$state/app-update.log"

# AppImage の APPIMAGE が自分の AppImage と一致する、ランタイム（exe が AppImage 本体）以外のプロセス。
app_pids() {
  local dir value exe
  for dir in /proc/[0-9]*; do
    # head で途中を打ち切ると pipefail で失敗扱いになりうるため、最初の行は bash で取る。
    value="$(tr '\0' '\n' < "$dir/environ" 2>/dev/null | sed -n 's/^APPIMAGE=//p')" || continue
    value="${value%%$'\n'*}"
    [[ -n "$value" && "$(realpath -m "$value")" = "$app" ]] || continue
    exe="$(readlink -f "$dir/exe" 2>/dev/null || true)"
    [[ "$exe" = "$app" ]] && continue
    printf '%s\n' "${dir#/proc/}"
  done
}

diagnostics() {
  local status=$?
  if [[ $status -ne 0 ]]; then
    {
      echo '---- diagnostics ----'
      if [[ -n "${APPIMAGE_EXTRACT_AND_RUN:-}" ]]; then echo 'mode: extract-and-run'; else echo 'mode: fuse'; fi
      echo '-- app-update.log'; cat "$log" 2>/dev/null || true
      echo '-- app-update.json'; cat "$state/app-update.json" 2>/dev/null || true
      echo '-- setup-registration.json'; cat "$state/setup-registration.json" 2>/dev/null || true
      echo '-- autostart'; cat "$autostart" 2>/dev/null || true
      echo '-- stubs'; tail -n +1 "$STUB_LOG_DIR"/*.log 2>/dev/null || true
      echo '-- packages'; ls -la "$packages" 2>/dev/null || true
      echo '-- processes'
      for pid in $(app_pids); do echo "$pid $(readlink "/proc/$pid/exe" 2>/dev/null)"; done
      echo '-- mcp stderr'; tail -n +1 "$work"/mcp-*.err 2>/dev/null || true
      echo '-- update-apply (apply) output'; tail -n +1 "$work"/apply.out "$work"/apply.err 2>/dev/null || true
    } >&2
  fi
  # 残ったプロセスを止める（ランナーの後始末に任せず、次の step へ持ち越さない）。
  for pid in $(app_pids); do kill "$pid" 2>/dev/null || true; done
  exit "$status"
}
trap diagnostics EXIT

# AppImage の中の sq.version の版（ランタイムの --appimage-extract で展開する。FUSE を使わない）。
appimage_version() {
  local dir; dir="$(mktemp -d "$work/x/v.XXXXXX")"
  ( cd "$dir" && env -u APPIMAGE_EXTRACT_AND_RUN -u NO_CLEANUP "$app" --appimage-extract >/dev/null 2>&1 ) || return 1
  sed -n 's:.*<version>\(.*\)</version>.*:\1:p' "$dir/squashfs-root/usr/bin/sq.version"
}

wait_until() {
  local seconds="$1"; shift
  for _ in $(seq 1 "$((seconds * 2))"); do
    if "$@"; then return 0; fi
    sleep 0.5
  done
  return 1
}

json() { python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); print(eval(sys.argv[2], {"v": v}))' "$@"; }

# ───────────── 起動方式（FUSE か、展開して実行か） ─────────────
step 'launch mode'
if out="$(timeout 60 "$app" --version </dev/null 2>&1)" && grep -q '^remnifold ' <<<"$out"; then
  echo "FUSE mount: $out"
else
  echo "FUSE launch failed ($out); falling back to APPIMAGE_EXTRACT_AND_RUN=1"
  # 展開先を子の終了ごとに消すと、同時に動く mcp や更新器の実行ファイルが消えるため、残す（検証のみ）。
  export APPIMAGE_EXTRACT_AND_RUN=1 NO_CLEANUP=1
  out="$(timeout 60 "$app" --version </dev/null 2>&1)" || fail "the AppImage does not start: $out"
  grep -q '^remnifold ' <<<"$out" || fail "unexpected --version output: $out"
fi
[[ "$(appimage_version)" = 9.0.0 ]] || fail 'the installed AppImage is not 9.0.0'

# ───────────── A. 引数なしの起動で登録 ─────────────
step 'A. registration on a launch with no arguments'
mkdir -p "$state"
# 背景の確認がこの段階で先に1日1回の確認を使わないよう、確認済みにしておく（B で外す）。
printf '{"last_checked_at": %s}\n' "$(date +%s)" > "$state/app-update.json"
# 端末でない引数なしの起動は、登録の後に mcp として stdin を待つ。stdin を閉じて終わらせる。
timeout 60 "$app" </dev/null >/dev/null 2>"$work/mcp-a.err" || fail 'the launch with no arguments failed'
registration="$state/setup-registration.json"
[[ -f "$registration" ]] || fail 'setup-registration.json was not written'
[[ "$(json "$registration" 'v["executable"]')" = "$app" ]] || fail 'the registered executable is not the AppImage'
[[ "$(json "$registration" 'v["version"]')" = 9.0.0 ]] || fail 'the registered version is not 9.0.0'
[[ "$(json "$registration" 'sorted(v["done"])')" = "['claude', 'codex']" ]] || fail 'both CLIs must be registered'
[[ "$(json "$registration" 'v["logon"]')" = True ]] || fail 'the logon entry was not registered'
[[ -f "$autostart" ]] || fail "the autostart entry was not created: $autostart"
grep -qxF "X-Remnifold-Executable=$app" "$autostart" || fail 'the autostart entry does not point at the AppImage'
grep -qxF "Exec=/usr/bin/env \"REMNIFOLD_HOME=$REMNIFOLD_HOME\" \"$app\" update-apply" "$autostart" || fail 'unexpected Exec line'
grep -qxF 'X-GNOME-Autostart-enabled=true' "$autostart" || fail 'autostart is not enabled'
grep -qxF "mcp add --scope user remnifold -- $app mcp" "$STUB_LOG_DIR/claude.log" || fail 'claude mcp add was not called with the AppImage'
grep -qxF "mcp add remnifold -- $app mcp" "$STUB_LOG_DIR/codex.log" || fail 'codex mcp add was not called with the AppImage'
python3 - "$CLAUDE_CONFIG_DIR/settings.json" "$app" <<'PY' || fail 'Claude Code hooks do not point at the AppImage'
import json, sys
settings, app = json.load(open(sys.argv[1])), sys.argv[2]
commands = [h["command"] for event in ("SessionStart", "UserPromptSubmit", "Stop", "PreToolUse") for e in settings["hooks"][event] for h in e["hooks"]]
assert commands and all(f'"{app}" hook ' in c for c in commands), commands
PY
claude_calls="$(wc -l < "$STUB_LOG_DIR/claude.log")"; codex_calls="$(wc -l < "$STUB_LOG_DIR/codex.log")"
# 2回目の起動は状態ファイルを読むだけで、CLI を呼ばない。
timeout 60 "$app" </dev/null >/dev/null 2>>"$work/mcp-a.err" || fail 'the second launch failed'
[[ "$(wc -l < "$STUB_LOG_DIR/claude.log")" = "$claude_calls" && "$(wc -l < "$STUB_LOG_DIR/codex.log")" = "$codex_calls" ]] || fail 'the second launch called a CLI again'
echo 'registered once; the second launch was a no-op'

# ───────────── B. 複数の mcp を同時に起動しても取得は1回 ─────────────
step 'B. concurrent mcp sessions download once'
cp "$new/releases.$channel.json" "$new/$pack_id-9.0.1-$channel-full.nupkg" "$work/feed/"
export REMNIFOLD_UPDATE_FEED_DIR="$work/feed"
printf '{}\n' > "$state/app-update.json"
[[ ! -e "$packages" || -z "$(find "$packages" -maxdepth 1 -name '*.nupkg')" ]] || fail 'a package was downloaded before B'
for i in 1 2 3; do
  # 各 mcp の stdin は sleep の出力。sleep を止めると EOF で mcp が終わる（目印の秒数で、この sleep だけを止める）。
  sleep 6007 | "$app" mcp >/dev/null 2>"$work/mcp-$i.err" &
done
has_three() { [[ "$(app_pids | wc -l)" -eq 3 ]]; }
wait_until 60 has_three || fail "expected three running mcp processes, found $(app_pids | wc -l)"
downloaded() { grep -q 'auto-update: downloaded 9.0.1' "$log" 2>/dev/null; }
wait_until 180 downloaded || fail 'the update was not downloaded'
# 他の2つが追いかけて取得しないことを確かめるため、少し待ってから数える。
sleep 5
[[ "$(grep -c 'auto-update: downloaded' "$log")" = 1 ]] || fail 'the update was downloaded more than once'
[[ "$(grep -c 'auto-update' "$log")" = 1 ]] || fail 'another session also checked the feed'
[[ "$(find "$packages" -maxdepth 1 -name '*.nupkg' | wc -l)" = 1 && -f "$packages/$pack_id-9.0.1-$channel-full.nupkg" ]] || fail 'unexpected package folder contents'
[[ "$(json "$state/app-update.json" 'v.get("last_checked_at") is not None')" = True ]] || fail 'last_checked_at was not recorded'
[[ "$(wc -l < "$STUB_LOG_DIR/claude.log")" = "$claude_calls" && "$(wc -l < "$STUB_LOG_DIR/codex.log")" = "$codex_calls" ]] || fail 'mcp startup called a CLI again'
echo 'downloaded exactly once'

# ───────────── C. 他のプロセスがあれば busy ─────────────
step 'C. update-apply is busy while sessions run'
result="$(timeout 60 "$app" update-apply </dev/null)" || fail "update-apply failed while busy: $result"
echo "$result"
[[ "$(python3 -c 'import json,sys; v=json.loads(sys.argv[1]); print(v["status"], v.get("processes"))' "$result")" = 'busy 3' ]] || fail "expected busy with three processes: $result"
[[ "$(app_pids | wc -l)" -eq 3 ]] || fail 'update-apply stopped a running session'
[[ "$(appimage_version)" = 9.0.0 ]] || fail 'the AppImage changed while busy'

# ───────────── D. 置き場の検査と適用 ─────────────
step 'D. apply when nothing else runs'
pkill -x -f 'sleep 6007' 2>/dev/null || true
no_sessions() { [[ -z "$(app_pids)" ]]; }
wait_until 60 no_sessions || fail "mcp processes did not exit: $(app_pids | tr '\n' ' ')"
# 置き場を他人が書けると、署名の無い Linux では差し替えを防げないため適用しない（終了コード 2）。
chmod o+w "$packages"
set +e
result="$(timeout 60 "$app" update-apply </dev/null)"; code=$?
set -e
chmod o-w "$packages"
echo "$result (exit $code)"
[[ $code = 2 && "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["status"])' "$result")" = untrusted ]] || fail 'a world-writable package folder must be refused'
# 適用では更新器（UpdateNix）が標準出力・標準エラーを引き継いで後に残るため、コマンド置換のパイプで受けると
# 更新器が終わるまで戻らない（timeout も効かない）。ファイルへ逃がし、1行目（update-apply の JSON）で判定する。
# 完了は下の AppImage の置き換わりのポーリングで待つ。
timeout 60 "$app" update-apply </dev/null >"$work/apply.out" 2>"$work/apply.err" || fail "update-apply failed: $(cat "$work/apply.out" "$work/apply.err")"
result="$(sed -n '1p' "$work/apply.out")"
echo "$result"
[[ "$result" = '{"status":"applied","version":"9.0.1"}' ]] || fail "expected applied 9.0.1: $result"
# UpdateNix は nupkg の中の AppImage を取り出して置き換える。その中身と一致すれば入れ替わった。
expected="$(python3 -c 'import hashlib,sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); n=[x for x in z.namelist() if x.lower().endswith(".appimage")]; assert len(n)==1, n; print(hashlib.sha256(z.read(n[0])).hexdigest())' "$new/$pack_id-9.0.1-$channel-full.nupkg")"
replaced() { [[ "$(sha256sum "$app" | cut -d ' ' -f 1)" = "$expected" ]]; }
wait_until 120 replaced || fail 'the AppImage was not replaced with 9.0.1'
[[ -x "$app" ]] || fail 'the replaced AppImage is not executable'
[[ "$(appimage_version)" = 9.0.1 ]] || fail 'the AppImage is not 9.0.1 after the update'
# 更新器（APPIMAGE を引き継ぐ）が終わり、誰も起動し直されない（再起動しない）こと。
wait_until 60 no_sessions || fail "the updater did not finish or the app was relaunched: $(app_pids | tr '\n' ' ')"
# 更新後の最初の起動は、版だけを更新し、CLI を呼ばない（MCP の登録はパスが同じなら有効なまま）。
timeout 60 "$app" </dev/null >/dev/null 2>"$work/mcp-d.err" || fail 'the launch after the update failed'
[[ "$(json "$registration" 'v["version"]')" = 9.0.1 ]] || fail 'the registration state did not record 9.0.1'
[[ "$(wc -l < "$STUB_LOG_DIR/claude.log")" = "$claude_calls" && "$(wc -l < "$STUB_LOG_DIR/codex.log")" = "$codex_calls" ]] || fail 'the launch after the update called a CLI'
result="$(timeout 60 "$app" update-apply </dev/null)" || fail "update-apply failed after the update: $result"
[[ "$result" = '{"status":"nothingPending"}' ]] || fail "expected nothingPending: $result"
echo 'applied 9.0.1'

# ───────────── E. 解除 ─────────────
step 'E. uninstall-setup'
# 登録のときにも「外してから足す」ための remove が呼ばれているので、解除で増えたことを数で確かめる。
claude_removes="$(grep -cxF 'mcp remove remnifold --scope user' "$STUB_LOG_DIR/claude.log" || true)"
codex_removes="$(grep -cxF 'mcp remove remnifold' "$STUB_LOG_DIR/codex.log" || true)"
plan="$(timeout 60 "$app" uninstall-setup </dev/null)" || fail 'uninstall-setup (plan) failed'
[[ "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["status"])' "$plan")" = planned && -f "$autostart" ]] || fail 'the plan must not change anything'
removed="$(timeout 60 "$app" uninstall-setup --apply </dev/null)" || fail 'uninstall-setup --apply failed'
echo "$removed"
[[ "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["status"])' "$removed")" = removed ]] || fail 'uninstall-setup did not report removed'
[[ ! -e "$autostart" ]] || fail 'the autostart entry was not removed'
[[ ! -e "$registration" ]] || fail 'setup-registration.json was not removed'
[[ "$(grep -cxF 'mcp remove remnifold --scope user' "$STUB_LOG_DIR/claude.log")" -gt "$claude_removes" ]] || fail 'claude mcp remove was not called'
[[ "$(grep -cxF 'mcp remove remnifold' "$STUB_LOG_DIR/codex.log")" -gt "$codex_removes" ]] || fail 'codex mcp remove was not called'
[[ ! -e "$STUB_LOG_DIR/claude.mcp" && ! -e "$STUB_LOG_DIR/codex.mcp" ]] || fail 'an MCP entry is still registered'
python3 - "$CLAUDE_CONFIG_DIR/settings.json" "$app" <<'PY' || fail 'Claude Code hooks still point at the AppImage'
import json, sys
settings, app = json.load(open(sys.argv[1])), sys.argv[2]
assert app not in json.dumps(settings), settings
PY

step 'all Linux Setup E2E assertions passed'
