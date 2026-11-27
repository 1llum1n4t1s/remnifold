//! Setup 版の登録（Claude Code・Codex と、ログイン時の適用）を、必要なときだけ行う（全 OS 共通）。
//!
//! Windows では Velopack のフック（install では状態を忘れてから、updated ではそのまま）からも呼ぶ。フックは
//! CLI 1回を3秒で打ち切るため、終わらなかった CLI は `done` に入らず、ログオン時の `update-apply`・引数なしの
//! 起動・`mcp` の起動で（30秒の上限で）やり直す。MCP の登録が消えると Claude Code は `mcp` を起動しないので、
//! ログオン時の `update-apply` が確実な再試行の機会になる。
//!
//! Linux（AppImage）も、フックもインストーラーも無い点は macOS と同じなので、同じ判定・同じ状態ファイルを使う。
//! Linux で登録する実行ファイルは AppImage 本体のパスで、利用者が AppImage を移動すると「パスが変わった」として
//! 全てを登録し直す。更新（UpdateNix が同じパスへ `mv -f`）では版だけが変わる。
//!
//! # なぜ必要か
//! Velopack の高速フック（install / updated / uninstall）は Windows でしか呼ばれない（velopack crate
//! 1.2.161 の `VelopackApp` で、フックの登録関数自体が `cfg(target_os = "windows")`）。macOS の .pkg は
//! 導入後に `VELOPACK_FIRSTRUN=1 open <bundle>` でアプリを1回起動するだけで、更新（UpdateMac apply）は
//! アプリを起動しない。そこで、Remnifold 自身が起動したときに「登録が今の自分を指しているか」を確かめ、
//! 違うときだけ登録する。
//!
//! # いつ行うか
//! - 引数なしの起動（.pkg の初回起動・利用者の直接起動）: 同期的に行う。Claude Code が `mcp` を起動するのは
//!   登録の後なので、初心者にとって最初の登録の機会はこの起動しかない。`open` 経由では `VELOPACK_FIRSTRUN` が
//!   届かない場合もあるため、初回起動の判定には頼らない。
//! - `mcp` の起動: 背景の更新スレッドの最初に行う（MCP の応答を遅らせない）。
//! - ログオン時の `update-apply`: 適用の判定より前に行う（自動更新を切っていても行う）。
//! - `hook <event>` など他のサブコマンドでは行わない（毎プロンプト起動するため）。
//!
//! # 軽くする工夫（状態ファイル）
//! 状態フォルダーの `setup-registration.json` に、登録した実行ファイルのパス・版・登録済みの CLI を覚え、
//! 次の場合だけ処理する。普段の `mcp` 起動では、この状態ファイルと接続設定（`recallMode`）の読み込みと
//! PATH の走査だけで終わる。
//! - パスが変わった（初回・導入先の移動）: ログイン時の適用と、見つかった全ての CLI を登録する。
//! - 版だけが変わった（更新の後）: ログイン時の適用の定義と、Claude Code のフックだけを張り直す。
//!   MCP の登録はパスが同じなら有効なままなので CLI は呼ばない（使用中のセッションの設定を外して
//!   付け直すことを避けるため）。
//! - 同じパス・版で、前回見つからなかった・失敗した CLI が今は見つかる: その CLI だけを登録する。
//! - 自動想起（`remnifold.json` の `recallMode`）の on/off が、前回フックを登録したときと違う: Claude Code の
//!   フックだけを張り直す（off では想起用の SessionStart・UserPromptSubmit を外す）。利用者が設定を書き換えた後の
//!   最初の `mcp` 起動などで反映し、Claude Code はフックを起動時に読むため、その次のセッションから効く。
//!   この項目の無い古い状態ファイル（この変更より前の版）では1回だけ張り直す。これが既存の利用者の移行で、
//!   off の利用者の settings.json に残っている想起用のフックを外す（更新の後の張り直しでも同じく外れる）。
//!   張り直しは、この実行ファイルを指す Remnifold のフックがある場合だけ書き込むので、Claude の登録が
//!   `done` に無くても行う（利用者が `connect` で手動登録した場合も移行する）。
//!
//! - Toolport ゲートウェイが既に提供している CLI（[`crate::toolport`] の判定が `Served`: 自分と同じ実体の実行ファイル、
//!   `ServedElsewhere`: 別の実行ファイル）と、`remnifold.json` の `mcpRegistration` が `off` の CLI は、直接登録
//!   （CLI 呼び出し）を省いて状態ファイルの `skipped` に理由（`toolport` / `toolport-foreign` / `off`）を覚える
//!   （Claude Code はフックだけを統合する）。`done` は外さない。`skipped` が `toolport` か `toolport-foreign` で
//!   `auto` のときは、CLI の有無に関係なく起動のたびに判定し直す。`Served` なら `toolport`、`ServedElsewhere` なら
//!   `toolport-foreign`（記録した回だけ `app-update.log` に警告）へ記録し直し、クライアントから Toolport が消えた
//!   （`NotConnected`）ら直接登録へ戻す。`direct` は `skipped` も直接登録し、`off` は自動の追加・修復をしない。
//!
//! 複数のセッションが同時に起動しても二重に登録しないよう、`setup-registration.lock` を try-lock する。
use super::{SetupContext, log_line, logon};
use crate::connect;
use crate::toolport::{self, Detection};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::{Duration, Instant},
};

/// 登録の状態を覚えるファイル（状態フォルダー直下）。
pub const STATE_FILE: &str = "setup-registration.json";
/// 登録の排他に使うファイル（状態フォルダー直下）。
pub const LOCK_FILE: &str = "setup-registration.lock";

/// 前回の登録の状態。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct State {
    /// 登録した実行ファイルのパス。
    pub executable: Option<String>,
    /// 登録したときの版。
    pub version: Option<String>,
    /// 登録まで済んだ CLI（`claude` / `codex`）。
    pub done: BTreeSet<String>,
    /// ログイン時の適用を登録したか（別の導入の plist が残っていて触れなかった場合は false）。
    pub logon: bool,
    /// Claude Code のフックを、自動想起が有効（true）・無効（false）のどちらの定義で登録したか。
    /// 未登録・古い状態ファイルでは None。
    pub recall: Option<bool>,
    /// 直接登録を省いた CLI と理由（`"toolport"`: Toolport ゲートウェイが既に提供している、`"toolport-foreign"`: Toolport が
    /// 別の実行ファイルで提供している、`"off"`: `mcpRegistration` が off）。`done` とは別で、`done`（直接登録済み）の意味は変えない。古い状態ファイルでは空。
    pub skipped: BTreeMap<String, String>,
}

/// 何をするか。
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// 状態を作り直す（パスが変わった）。
    pub fresh: bool,
    /// ログイン時の適用を登録し直す。
    pub logon: bool,
    /// CLI で直接登録する CLI。
    pub tools: Vec<&'static str>,
    /// 直接登録を省いて `skipped` に記録する CLI と理由（Claude Code はフックだけを統合する）。
    pub skips: Vec<(&'static str, &'static str)>,
    /// Claude Code のフックだけを張り直す（更新の後・自動想起の切り替えの後）。
    pub refresh_hooks: bool,
}

/// `skipped` の理由。
const SKIP_TOOLPORT: &str = "toolport";
const SKIP_TOOLPORT_FOREIGN: &str = "toolport-foreign";
const SKIP_OFF: &str = "off";

fn skip_reason(reason: &str) -> &'static str {
    match reason {
        SKIP_TOOLPORT => SKIP_TOOLPORT,
        SKIP_TOOLPORT_FOREIGN => SKIP_TOOLPORT_FOREIGN,
        _ => SKIP_OFF,
    }
}

/// JSON から状態を作る。壊れていれば既定（未登録）として扱い、決して失敗しない。
pub fn parse(bytes: &[u8]) -> State {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return State::default();
    };
    State {
        executable: value["executable"].as_str().map(str::to_owned),
        version: value["version"].as_str().map(str::to_owned),
        done: value["done"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        logon: value["logon"].as_bool().unwrap_or(false),
        recall: value["recall"].as_bool(),
        skipped: value["skipped"]
            .as_object()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|(tool, reason)| {
                        let reason = reason.as_str()?;
                        [SKIP_TOOLPORT, SKIP_TOOLPORT_FOREIGN, SKIP_OFF]
                            .contains(&reason)
                            .then(|| (tool.clone(), reason.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 状態ファイルで `tool` が `toolport-foreign` として直接登録を省かれているか。
pub fn is_toolport_foreign(state: &State, tool: &str) -> bool {
    state.skipped.get(tool).map(String::as_str) == Some(SKIP_TOOLPORT_FOREIGN)
}

pub fn load(state_dir: &Path) -> State {
    let path = state_dir.join(STATE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => fs::read(&path).map(|b| parse(&b)).unwrap_or_default(),
        _ => State::default(),
    }
}

fn save(state_dir: &Path, state: &State) -> anyhow::Result<()> {
    let value = json!({
        "executable": state.executable,
        "version": state.version,
        "done": state.done.iter().collect::<Vec<_>>(),
        "logon": state.logon,
        "recall": state.recall,
        "skipped": state.skipped,
    });
    crate::common::atomic_write(
        &state_dir.join(STATE_FILE),
        &[serde_json::to_vec_pretty(&value)?, b"\n".to_vec()].concat(),
        false,
    )
}

/// 状態ファイルを消す（アンインストール用。次に導入したときに登録し直すため）。無ければ何もしない。
pub fn forget(state_dir: &Path) -> std::io::Result<bool> {
    match fs::remove_file(state_dir.join(STATE_FILE)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// [`plan_with_hooks_target`] を、Claude Code のフックの統合先がある前提で呼ぶ（テスト用）。
#[cfg(test)]
pub fn plan(
    state: &State,
    executable: &str,
    version: &str,
    recall: bool,
    mode: connect::McpMode,
    available: impl Fn(&str) -> bool,
    detect: impl Fn(&str) -> Detection,
) -> Option<Plan> {
    plan_with_hooks_target(
        state, executable, version, recall, mode, available, detect, true,
    )
}

/// 前回の状態と今の導入から、するべきことを決める。何もしなくてよければ None。
/// `recall` は今の自動想起の on/off、`mode` は MCP の自動登録の方針、`available` は CLI が見つかるかの判定、
/// `detect` は Toolport がその CLI（`claude` / `codex`）へ提供しているかの判定（どちらもテストで差し替える）。
///
/// `done`（直接登録済み）の CLI は、どの方針でも外さず、触らない。判定（`detect`）を呼ぶのは、状態を作り直すとき
/// （パスの変更）と、まだ登録も省略もしていない CLI（auto では CLI があるときだけ）、`skipped` の CLI（理由が
/// `toolport` / `toolport-foreign` の auto は CLI の有無に関係なく）だけで、普段の起動では呼ばない。
///
/// 省く CLI のうち Claude Code は、フックだけを統合する。統合先が無い（`claude_hooks_target` が false: Claude Code の
/// CLI も設定フォルダーも無い）ときは、フックを作らず `skipped` にも記録しない（次の起動でまた判断する）。
#[allow(clippy::too_many_arguments)]
pub fn plan_with_hooks_target(
    state: &State,
    executable: &str,
    version: &str,
    recall: bool,
    mode: connect::McpMode,
    available: impl Fn(&str) -> bool,
    detect: impl Fn(&str) -> Detection,
    claude_hooks_target: bool,
) -> Option<Plan> {
    let mut plan = plan_unchecked(state, executable, version, recall, mode, available, detect)?;
    if !claude_hooks_target {
        plan.skips.retain(|(tool, _)| *tool != "claude");
        // 統合先が無いままでは、他に仕事の無い起動で毎回やり直さない。
        if !plan.fresh
            && !plan.logon
            && plan.tools.is_empty()
            && plan.skips.is_empty()
            && !plan.refresh_hooks
        {
            return None;
        }
    }
    Some(plan)
}

fn plan_unchecked(
    state: &State,
    executable: &str,
    version: &str,
    recall: bool,
    mode: connect::McpMode,
    available: impl Fn(&str) -> bool,
    detect: impl Fn(&str) -> Detection,
) -> Option<Plan> {
    // 判定から、省く理由（Toolport が提供している）を決める。省かず直接登録するなら None。
    let judge = |tool: &str| match detect(tool) {
        Detection::Served { .. } => Some(SKIP_TOOLPORT),
        Detection::ServedElsewhere { .. } => Some(SKIP_TOOLPORT_FOREIGN),
        _ => None,
    };
    if state.executable.as_deref() != Some(executable) {
        // 新規インストール・パス変更: 状態を作り直し、クライアントごとに決める。
        let mut tools = Vec::new();
        let mut skips = Vec::new();
        for tool in connect::SETUP_TOOLS {
            match mode {
                connect::McpMode::Off => skips.push((tool, SKIP_OFF)),
                connect::McpMode::Auto => match judge(tool) {
                    Some(reason) => skips.push((tool, reason)),
                    None => tools.push(tool),
                },
                connect::McpMode::Direct => tools.push(tool),
            }
        }
        return Some(Plan {
            fresh: true,
            logon: true,
            tools,
            skips,
            refresh_hooks: false,
        });
    }
    let updated = state.version.as_deref() != Some(version);
    let mut tools: Vec<&'static str> = Vec::new();
    let mut skips: Vec<(&'static str, &'static str)> = Vec::new();
    for tool in connect::SETUP_TOOLS {
        if state.done.contains(tool) {
            continue;
        }
        let reason = state.skipped.get(tool).map(String::as_str);
        let retry_hooks = |tool: &str| tool == "claude" && state.recall.is_none();
        match (mode, reason) {
            // off は自動の追加・修復を止める。
            // ただし、省くと決めたのにフックの統合に失敗した Claude（recall が None のまま）は、フックだけやり直す。
            (connect::McpMode::Off, Some(reason)) => {
                if retry_hooks(tool) {
                    skips.push((tool, skip_reason(reason)));
                }
            }
            // 登録も省略もまだの CLI（前回 CLI が無かった・失敗した）は、省く理由を覚える。
            (connect::McpMode::Off, None) => skips.push((tool, SKIP_OFF)),
            // direct は、未登録のものも省いたものも直接登録する。
            (connect::McpMode::Direct, _) => {
                if available(tool) {
                    tools.push(tool);
                }
            }
            // 省いた理由が toolport / toolport-foreign: CLI の有無に関係なく毎回判定する。理由が変わるときだけ記録し直し
            // （同じ理由なら、フックの再試行以外は何もしない）、Toolport が消えた（NotConnected）ときだけ直接登録へ戻す。
            (connect::McpMode::Auto, Some(previous))
                if previous == SKIP_TOOLPORT || previous == SKIP_TOOLPORT_FOREIGN =>
            {
                let hooks = retry_hooks(tool);
                match detect(tool) {
                    Detection::Served { .. } => {
                        if previous != SKIP_TOOLPORT || hooks {
                            skips.push((tool, SKIP_TOOLPORT));
                        }
                    }
                    Detection::ServedElsewhere { .. } => {
                        if previous != SKIP_TOOLPORT_FOREIGN || hooks {
                            skips.push((tool, SKIP_TOOLPORT_FOREIGN));
                        }
                    }
                    Detection::NotConnected if available(tool) => tools.push(tool),
                    _ => {
                        if hooks {
                            skips.push((tool, skip_reason(previous)));
                        }
                    }
                }
            }
            // 省いた理由が off（方針が off から auto へ変わった）や、まだ登録も省略もしていない CLI:
            // CLI があるときだけ、新規と同じく判定する。
            (connect::McpMode::Auto, _) => {
                if available(tool) {
                    match judge(tool) {
                        Some(reason) => skips.push((tool, reason)),
                        None => tools.push(tool),
                    }
                }
            }
        }
    }
    let logon = updated || !state.logon;
    // 想起の切り替え（と古い状態ファイルからの移行）は、Claude の登録が `done` に無くても張り直す。
    // 張り直しはこの実行ファイルを指す Remnifold のフックがある場合だけ書き込むので、未登録なら何も変わらない。
    // 版の更新では、直接登録済み（done）だけでなく、Toolport・off で省いた（skipped）Claude のフックも張り直す。
    let hooks_known = state.done.contains("claude") || state.skipped.contains_key("claude");
    let refresh_hooks = (updated && hooks_known) || state.recall != Some(recall);
    if !logon && tools.is_empty() && skips.is_empty() && !refresh_hooks {
        return None;
    }
    Some(Plan {
        fresh: false,
        logon,
        tools,
        skips,
        refresh_hooks,
    })
}

/// `claude` / `codex` を Toolport の判定のクライアントへ変えて、環境から判定する。
/// `executable` は直接登録に使うのと同じ絶対パスで、Toolport の提供が自分と同じ実体かを比べる。
pub fn detect_tool(tool: &str, executable: &str) -> Detection {
    let client = match tool {
        "claude" => toolport::ClientId::ClaudeCode,
        "codex" => toolport::ClientId::Codex,
        _ => return Detection::NotConnected,
    };
    toolport::detect(
        &connect::TOOLPORT_PRODUCT,
        client,
        &toolport::Env::process(),
        executable,
    )
}

/// クライアントごとの判定結果の種類（`Unknown` は理由つき）を1行にする。サーバー id・コマンド・env は含めない。
fn detection_line(label: &str, tool: &str, detection: &Detection) -> String {
    match detection {
        Detection::Unknown(reason) => {
            format!("{label}: {tool}: toolport detection unknown ({reason})")
        }
        other => format!("{label}: {tool}: toolport detection {}", other.kind()),
    }
}

/// Toolport が別の実行ファイルで提供しているため直接登録を省いたことの警告（`app-update.log` 用）。
/// Toolport 側の command は書かず、自分の実行ファイルのパスだけを書く。
fn foreign_warning(label: &str, tool: &str, executable: &str) -> String {
    format!(
        "{label}: {tool}: Toolport serves a different remnifold executable; direct registration skipped. Point the Toolport server's command to {executable}"
    )
}

/// 更新後のフック（Velopack は15秒で打ち切る）で、CLI の登録に使ってよい時間の予算。15秒より十分小さくし、
/// ログオン時の適用・フックの張り直しと、最後に始めた CLI の待ちが収まるようにする。
#[cfg_attr(not(windows), allow(dead_code))]
pub const UPDATED_HOOK_BUDGET: Duration = Duration::from_secs(12);

/// `tools` を順に登録する。`budget` があるときは、次の CLI の登録を始める時点で `elapsed` と最悪の所要時間
/// `worst` の合計が予算を超えるなら、その CLI と以降を始めずに返す（remove と add の途中では打ち切らない
/// ため、始めた登録は最後まで行う）。返す 2 つ目は始めなかった CLI。
fn register_within(
    tools: &[&'static str],
    budget: Option<Duration>,
    worst: Duration,
    elapsed: impl Fn() -> Duration,
    mut register: impl FnMut(&'static str) -> connect::SetupOutcome,
) -> (Vec<connect::SetupOutcome>, Vec<&'static str>) {
    let mut outcomes = Vec::new();
    let mut skipped = Vec::new();
    for &tool in tools {
        if budget.is_some_and(|budget| elapsed() + worst > budget) {
            skipped.push(tool);
        } else {
            outcomes.push(register(tool));
        }
    }
    (outcomes, skipped)
}

/// 必要なら登録する。`label` は診断ログの見出し。失敗は例外にせず、診断ログにだけ残す。
pub fn ensure(context: &SetupContext, label: &str) {
    ensure_within(context, label, None);
}

/// [`ensure`] に、CLI の登録へ使う時間の予算を付けたもの（更新後のフック用。[`UPDATED_HOOK_BUDGET`]）。
/// 予算で始められなかった CLI は未完了のまま状態に残し、次のログオン時の適用などでやり直す。
pub fn ensure_within(context: &SetupContext, label: &str, budget: Option<Duration>) {
    let started = Instant::now();
    let state_dir = crate::paths::Roots::current().state;
    let executable = context.executable.to_string_lossy().into_owned();
    // 1回だけ読み、判定と書き込みで同じ値を使う（途中で設定が書き換わっても食い違わない）。
    let settings = connect::setup_settings();
    let recall = settings.recall;
    let mode = settings.mode;
    // 不正な値は早期 return の前に、この呼び出しで1回だけ記録する。
    if settings.invalid_mode {
        log_line(&format!(
            "{label}: mcpRegistration is not auto/direct/off; treated as auto"
        ));
    }
    // Claude Code のフックの統合先（CLI か設定フォルダー）が無ければ、フックを作らず skipped にも記録しない。
    let claude_target = connect::cli_available("claude") || connect::claude_home_exists();
    let make_plan = |state: &State, detect: &dyn Fn(&str) -> Detection| {
        plan_with_hooks_target(
            state,
            &executable,
            &context.version,
            recall,
            mode,
            connect::cli_available,
            detect,
            claude_target,
        )
    };
    if make_plan(&load(&state_dir), &|tool| detect_tool(tool, &executable)).is_none() {
        return;
    }
    // 他のプロセスが登録中なら任せる（二重に CLI を呼ばない）。
    let Some(_lock) = super::try_lock_named(&state_dir, LOCK_FILE) else {
        return;
    };
    // ロックを取る間に他のプロセスが終えていることがある。読み直して決め直す。
    let previous = load(&state_dir);
    // 決めるために判定したクライアントの結果を、判定を重ねずにそのまま記録する。
    let judged = std::cell::RefCell::new(Vec::new());
    let Some(plan) = make_plan(&previous, &|tool| {
        let detection = detect_tool(tool, &executable);
        judged
            .borrow_mut()
            .push((tool.to_owned(), detection.clone()));
        detection
    }) else {
        return;
    };
    // 登録処理が動く回は、判定した種類を1行ずつ記録する（秘密値は含めない）。
    for (tool, detection) in judged.borrow().iter() {
        log_line(&detection_line(label, tool, detection));
    }
    let previous_skipped = previous.skipped.clone();
    let mut state = if plan.fresh {
        State {
            executable: Some(executable.clone()),
            ..State::default()
        }
    } else {
        previous
    };
    state.version = Some(context.version.clone());
    if plan.logon {
        match logon::register(&context.app_id, &context.executable) {
            Ok(result) => {
                state.logon = !matches!(result, logon::Registration::ForeignKept);
                log_line(&format!("{label}: logon apply {result:?}"));
            }
            Err(error) => {
                state.logon = false;
                log_line(&format!("{label}: logon apply failed: {error}"));
            }
        }
    }
    // 直接登録を省く CLI は、CLI を呼ばずに記録する。Claude Code はフック（settings.json）だけを統合する
    // （失敗したら記録せず、次の機会にやり直す）。
    let integrates_hooks = plan.skips.iter().any(|(tool, _)| *tool == "claude");
    if integrates_hooks {
        let (integrated, line) =
            connect::integrate_claude_hooks_for_setup(&context.executable, recall);
        state.recall = integrated.then_some(recall);
        log_line(&format!("{label}: {line}"));
    } else if plan.refresh_hooks {
        let (refreshed, line) =
            connect::refresh_claude_hooks_for_setup(&context.executable, recall);
        // 失敗したら覚えている定義を消し、次の機会に張り直す。
        state.recall = refreshed.then_some(recall);
        log_line(&format!("{label}: {line}"));
    }
    // 省くと決めた CLI は、Claude のフックの統合に失敗しても記録する（recall が None のままなので、次の起動で
    // `mcpRegistration` が off でもフックだけをやり直す）。
    for (tool, reason) in &plan.skips {
        state
            .skipped
            .insert((*tool).to_owned(), (*reason).to_owned());
        log_line(&format!(
            "{label}: {tool}: MCP registration not added ({reason})"
        ));
        // 警告は、foreign を記録した回だけ出す（同じ理由のままフックだけ再試行した回は出さない）。
        let recorded = previous_skipped.get(*tool).map(String::as_str);
        if *reason == SKIP_TOOLPORT_FOREIGN && (plan.fresh || recorded != Some(*reason)) {
            log_line(&foreign_warning(label, tool, &executable));
        }
    }
    let (outcomes, skipped) = register_within(
        &plan.tools,
        budget,
        connect::tool_registration_worst_case(),
        || started.elapsed(),
        |tool| connect::register_tool_for_setup(&context.executable, tool, recall),
    );
    for outcome in outcomes {
        if outcome.done {
            state.done.insert(outcome.tool.to_string());
            state.skipped.remove(outcome.tool);
        } else {
            state.done.remove(outcome.tool);
        }
        if outcome.tool == "claude" {
            state.recall = outcome.done.then_some(recall);
        }
        log_line(&format!("{label}: {}", outcome.line));
    }
    for tool in skipped {
        log_line(&format!(
            "{label}: {tool}: not started (hook time budget); retried at the next logon"
        ));
    }
    if let Err(error) = save(&state_dir, &state) {
        log_line(&format!("{label}: registration state not saved: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use connect::McpMode;

    /// 既定の方針（auto）で、Toolport の判定はどのクライアントも NotConnected のもの。
    fn plan(
        state: &State,
        executable: &str,
        version: &str,
        recall: bool,
        available: impl Fn(&str) -> bool,
    ) -> Option<Plan> {
        super::plan(
            state,
            executable,
            version,
            recall,
            McpMode::Auto,
            available,
            |_| Detection::NotConnected,
        )
    }

    fn served(_: &str) -> Detection {
        Detection::Served {
            server_id: "remnifold-1".into(),
            command: "C:/r.exe".into(),
        }
    }

    fn registered_state() -> State {
        State {
            executable: Some("/A.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        }
    }

    fn skipped(state: &State, items: &[(&str, &str)]) -> State {
        State {
            done: BTreeSet::new(),
            skipped: items
                .iter()
                .map(|(tool, reason)| (tool.to_string(), reason.to_string()))
                .collect(),
            ..state.clone()
        }
    }

    fn done(state: &State, tools: &[&str]) -> State {
        State {
            done: tools.iter().map(|t| t.to_string()).collect(),
            ..state.clone()
        }
    }

    fn outcome(tool: &'static str) -> connect::SetupOutcome {
        connect::SetupOutcome {
            tool,
            done: true,
            line: String::new(),
        }
    }

    #[test]
    fn a_hook_budget_stops_before_a_registration_that_might_not_finish() {
        use std::cell::Cell;
        let worst = Duration::from_secs(9);
        let clock = Cell::new(Duration::ZERO);
        // Each registration takes its worst case: the second one must not start within 12 seconds.
        let (done, skipped) = register_within(
            &["claude", "codex"],
            Some(UPDATED_HOOK_BUDGET),
            worst,
            || clock.get(),
            |tool| {
                clock.set(clock.get() + worst);
                outcome(tool)
            },
        );
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].tool, "claude");
        assert_eq!(skipped, vec!["codex"]);
        // A fast first registration leaves room for the second.
        let clock = Cell::new(Duration::ZERO);
        let (done, skipped) = register_within(
            &["claude", "codex"],
            Some(UPDATED_HOOK_BUDGET),
            worst,
            || clock.get(),
            |tool| {
                clock.set(clock.get() + Duration::from_secs(1));
                outcome(tool)
            },
        );
        assert_eq!((done.len(), skipped.len()), (2, 0));
        // Without a budget (install hook, logon retry) everything runs.
        let clock = Cell::new(Duration::ZERO);
        let (done, skipped) = register_within(
            &["claude", "codex"],
            None,
            worst,
            || clock.get(),
            |tool| {
                clock.set(clock.get() + worst);
                outcome(tool)
            },
        );
        assert_eq!((done.len(), skipped.len()), (2, 0));
    }

    #[test]
    fn a_new_or_moved_install_registers_everything() {
        let everything = Plan {
            fresh: true,
            logon: true,
            tools: vec!["claude", "codex"],
            skips: vec![],
            refresh_hooks: false,
        };
        assert_eq!(
            plan(&State::default(), "/A.app/x", "1.0.0", false, |_| false),
            Some(everything)
        );
        let registered = State {
            executable: Some("/Old.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        };
        assert!(
            plan(&registered, "/A.app/x", "1.0.0", false, |_| true)
                .unwrap()
                .fresh
        );
    }

    #[test]
    fn nothing_happens_when_everything_is_current() {
        let state = State {
            executable: Some("/A.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        };
        assert_eq!(plan(&state, "/A.app/x", "1.0.0", false, |_| true), None);
        // 見つからなかった CLI は、見つかるまでは何もしない（毎回の起動でプロセスを起動しない）。
        let partial = done(&state, &["claude"]);
        assert_eq!(plan(&partial, "/A.app/x", "1.0.0", false, |_| false), None);
        // 見つかったら、その CLI だけを登録する。
        assert_eq!(
            plan(&partial, "/A.app/x", "1.0.0", false, |tool| tool == "codex"),
            Some(Plan {
                fresh: false,
                logon: false,
                tools: vec!["codex"],
                skips: vec![],
                refresh_hooks: false
            })
        );
    }

    #[test]
    fn an_update_refreshes_the_logon_entry_and_hooks_without_cli_calls() {
        let state = State {
            executable: Some("/A.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        };
        assert_eq!(
            plan(&state, "/A.app/x", "1.0.1", false, |_| true),
            Some(Plan {
                fresh: false,
                logon: true,
                tools: vec![],
                skips: vec![],
                refresh_hooks: true
            })
        );
        // Claude を登録していなければ、フックは張り直さない。
        assert!(
            !plan(
                &done(&state, &["codex"]),
                "/A.app/x",
                "1.0.1",
                false,
                |_| false
            )
            .unwrap()
            .refresh_hooks
        );
    }

    #[test]
    fn a_changed_recall_mode_refreshes_only_the_claude_hooks() {
        let state = State {
            executable: Some("/A.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        };
        let refresh = Some(Plan {
            fresh: false,
            logon: false,
            tools: vec![],
            skips: vec![],
            refresh_hooks: true,
        });
        // off → on と on → off の両方向で、CLI を呼ばずにフックだけを張り直す。
        assert_eq!(plan(&state, "/A.app/x", "1.0.0", true, |_| true), refresh);
        let on = State {
            recall: Some(true),
            ..state.clone()
        };
        assert_eq!(plan(&on, "/A.app/x", "1.0.0", false, |_| true), refresh);
        assert_eq!(plan(&on, "/A.app/x", "1.0.0", true, |_| true), None);
        // 項目の無い古い状態ファイルでは1回だけ張り直す。
        let legacy = State {
            recall: None,
            ..state.clone()
        };
        assert_eq!(plan(&legacy, "/A.app/x", "1.0.0", false, |_| true), refresh);
        // Claude の登録が `done` に無くても（手動の connect など）切り替えと移行は張り直す。
        let manual = done(&state, &["codex"]);
        assert_eq!(plan(&manual, "/A.app/x", "1.0.0", true, |_| false), refresh);
        let manual_legacy = State {
            recall: None,
            ..manual.clone()
        };
        assert_eq!(
            plan(&manual_legacy, "/A.app/x", "1.0.0", false, |_| false),
            refresh
        );
        // 一致していれば何もしない。
        assert_eq!(plan(&manual, "/A.app/x", "1.0.0", false, |_| false), None);
    }

    #[test]
    fn state_round_trips_and_broken_files_mean_unregistered() {
        let dir = std::env::temp_dir().join(format!("remnifold-reg-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir), State::default());
        let state = State {
            executable: Some("/A.app/x".into()),
            version: Some("9.0.0".into()),
            done: ["codex".to_string()].into(),
            logon: true,
            recall: Some(false),
            skipped: BTreeMap::new(),
        };
        save(&dir, &state).unwrap();
        assert_eq!(load(&dir), state);
        fs::write(dir.join(STATE_FILE), b"{ broken").unwrap();
        assert_eq!(load(&dir), State::default());
        assert!(forget(&dir).unwrap());
        assert!(!forget(&dir).unwrap());
    }

    #[test]
    fn skipped_round_trips_and_a_state_file_without_it_still_loads() {
        let dir = std::env::temp_dir().join(format!("remnifold-reg-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let state = skipped(
            &registered_state(),
            &[("claude", "toolport"), ("codex", "off")],
        );
        save(&dir, &state).unwrap();
        assert_eq!(load(&dir), state);
        // 古い状態ファイル（skipped が無い）。
        let legacy = parse(
            br#"{"executable":"/A.app/x","version":"1.0.0","done":["claude"],"logon":true,"recall":false}"#,
        );
        assert!(legacy.skipped.is_empty());
        assert_eq!(legacy.done, BTreeSet::from(["claude".to_string()]));
        // 未知の理由・型の違う値は読み捨てる。
        let odd = parse(br#"{"skipped":{"claude":"toolport","codex":"later","x":1}}"#);
        assert_eq!(odd.skipped.len(), 1);
    }

    #[test]
    fn a_new_install_skips_what_toolport_serves_and_registers_the_rest() {
        let fresh = |mode, detect: fn(&str) -> Detection| {
            super::plan(
                &State::default(),
                "/A.app/x",
                "1.0.0",
                false,
                mode,
                |_| true,
                detect,
            )
            .unwrap()
        };
        // auto で両方が Served: CLI で登録するものは無い。
        let plan = fresh(McpMode::Auto, served);
        assert!(plan.fresh && plan.tools.is_empty());
        assert_eq!(
            plan.skips,
            vec![("claude", "toolport"), ("codex", "toolport")]
        );
        // Claude だけが Served。
        let plan = fresh(McpMode::Auto, |tool| {
            if tool == "claude" {
                served(tool)
            } else {
                Detection::NotServed
            }
        });
        assert_eq!(plan.tools, vec!["codex"]);
        assert_eq!(plan.skips, vec![("claude", "toolport")]);
        // Unknown は判断せず直接登録する。direct は判定を無視する。
        assert_eq!(
            fresh(McpMode::Auto, |_| Detection::Unknown("x".into()))
                .tools
                .len(),
            2
        );
        let plan = fresh(McpMode::Direct, served);
        assert_eq!((plan.tools.len(), plan.skips.len()), (2, 0));
        // off は直接登録せず、省いた理由を off として覚える。
        let plan = fresh(McpMode::Off, served);
        assert!(plan.tools.is_empty());
        assert_eq!(plan.skips, vec![("claude", "off"), ("codex", "off")]);
    }

    #[test]
    fn an_update_refreshes_the_hooks_of_a_skipped_claude_without_touching_the_rest() {
        let state = skipped(
            &registered_state(),
            &[("claude", "toolport"), ("codex", "toolport")],
        );
        // 版だけの更新: Toolport のまま（NotConnected でない）なら、ログオンの適用とフックだけ。
        let update = super::plan(
            &state,
            "/A.app/x",
            "1.0.1",
            false,
            McpMode::Auto,
            |_| true,
            served,
        );
        assert_eq!(
            update,
            Some(Plan {
                fresh: false,
                logon: true,
                tools: vec![],
                skips: vec![],
                refresh_hooks: true
            })
        );
        // Codex だけが skipped で Claude が done でも、Claude の done だけでフックは張り直す（従来どおり）。
        let claude_done = State {
            done: ["claude".to_string()].into(),
            ..skipped(&state, &[("codex", "toolport")])
        };
        assert!(
            plan(&claude_done, "/A.app/x", "1.0.1", false, |_| false)
                .unwrap()
                .refresh_hooks
        );
        // 版が同じなら、skipped は何も起こさない（判定が Served のまま）。
        assert_eq!(
            super::plan(
                &state,
                "/A.app/x",
                "1.0.0",
                false,
                McpMode::Auto,
                |_| true,
                served
            ),
            None
        );
    }

    #[test]
    fn a_skipped_toolport_registration_returns_to_direct_only_when_the_gateway_is_gone() {
        // codex は直接登録済み（done）で、判定にも方針にも関わらない。
        let state = State {
            done: ["codex".to_string()].into(),
            ..skipped(&registered_state(), &[("claude", "toolport")])
        };
        let decide = |detection: Detection, available: bool, mode| {
            super::plan(
                &state,
                "/A.app/x",
                "1.0.0",
                false,
                mode,
                move |tool| available && tool == "claude",
                move |_| detection.clone(),
            )
        };
        // NotConnected: 直接登録へ戻す。CLI が無ければ待つ。
        assert_eq!(
            decide(Detection::NotConnected, true, McpMode::Auto)
                .unwrap()
                .tools,
            vec!["claude"]
        );
        assert_eq!(decide(Detection::NotConnected, false, McpMode::Auto), None);
        // NotServed・Unknown・Served は何もしない（Toolport 側で無効にした場合は足さない）。
        for detection in [
            Detection::NotServed,
            Detection::Unknown("x".into()),
            served("claude"),
        ] {
            assert_eq!(decide(detection, true, McpMode::Auto), None);
        }
        // direct は skipped を直接登録する。off は何もしない。
        assert_eq!(
            decide(served("claude"), true, McpMode::Direct)
                .unwrap()
                .tools,
            vec!["claude"]
        );
        assert_eq!(decide(Detection::NotConnected, true, McpMode::Off), None);
    }

    #[test]
    fn skipped_off_is_judged_again_as_new_when_the_mode_returns_to_auto() {
        let state = skipped(&registered_state(), &[("claude", "off"), ("codex", "off")]);
        let auto = |detect: fn(&str) -> Detection| {
            super::plan(
                &state,
                "/A.app/x",
                "1.0.0",
                false,
                McpMode::Auto,
                |_| true,
                detect,
            )
            .unwrap()
        };
        let plan = auto(served);
        assert!(plan.tools.is_empty());
        assert_eq!(
            plan.skips,
            vec![("claude", "toolport"), ("codex", "toolport")]
        );
        assert_eq!(
            auto(|_| Detection::NotConnected).tools,
            vec!["claude", "codex"]
        );
    }

    fn elsewhere(_: &str) -> Detection {
        Detection::ServedElsewhere {
            server_id: "remnifold-2".into(),
            command: "C:/other/r.exe".into(),
        }
    }

    #[test]
    fn toolport_foreign_is_recorded_and_judged_every_start_in_auto() {
        let plan_for = |state: &State, available: bool, detect: fn(&str) -> Detection| {
            super::plan(
                state,
                "/A.app/x",
                "1.0.0",
                false,
                McpMode::Auto,
                move |_| available,
                detect,
            )
        };
        // 新規（パスが違う）: ServedElsewhere は foreign を記録する。
        let fresh = plan_for(&State::default(), true, elsewhere).unwrap();
        assert!(fresh.tools.is_empty());
        assert_eq!(
            fresh.skips,
            vec![
                ("claude", "toolport-foreign"),
                ("codex", "toolport-foreign")
            ]
        );
        // toolport -> ServedElsewhere: foreign に変える。
        let toolport = skipped(
            &registered_state(),
            &[("claude", "toolport"), ("codex", "toolport")],
        );
        let toolport = State {
            done: BTreeSet::new(),
            ..toolport
        };
        assert_eq!(
            plan_for(&toolport, false, elsewhere).unwrap().skips,
            vec![
                ("claude", "toolport-foreign"),
                ("codex", "toolport-foreign")
            ]
        );
        let foreign = State {
            done: BTreeSet::new(),
            ..skipped(
                &registered_state(),
                &[
                    ("claude", "toolport-foreign"),
                    ("codex", "toolport-foreign"),
                ],
            )
        };
        // foreign -> Served: toolport に変える（CLI が無くても判定する）。
        assert_eq!(
            plan_for(&foreign, false, served).unwrap().skips,
            vec![("claude", "toolport"), ("codex", "toolport")]
        );
        // foreign -> ServedElsewhere・NotServed・Unknown: 何もしない。
        for detect in [
            elsewhere as fn(&str) -> Detection,
            |_| Detection::NotServed,
            |_| Detection::Unknown("x".into()),
        ] {
            assert_eq!(plan_for(&foreign, true, detect), None);
        }
        // foreign -> NotConnected: CLI があれば直接登録し、無ければ何もしない。
        assert_eq!(
            plan_for(&foreign, true, |_| Detection::NotConnected)
                .unwrap()
                .tools,
            vec!["claude", "codex"]
        );
        assert_eq!(plan_for(&foreign, false, |_| Detection::NotConnected), None);
        // direct は直接登録し、off は何もしない。
        let mode = |mode| {
            super::plan(
                &foreign,
                "/A.app/x",
                "1.0.0",
                false,
                mode,
                |_| true,
                elsewhere,
            )
        };
        assert_eq!(
            mode(McpMode::Direct).unwrap().tools,
            vec!["claude", "codex"]
        );
        assert_eq!(mode(McpMode::Off), None);
    }

    #[test]
    fn a_foreign_claude_keeps_its_reason_when_the_hooks_are_retried() {
        let failed = State {
            recall: None,
            done: BTreeSet::new(),
            ..skipped(
                &registered_state(),
                &[
                    ("claude", "toolport-foreign"),
                    ("codex", "toolport-foreign"),
                ],
            )
        };
        // 同じ判定のままでも、フックだけやり直すので、理由を保って記録し直す（Claude だけ）。
        for (mode, detect) in [
            (McpMode::Auto, elsewhere as fn(&str) -> Detection),
            (McpMode::Auto, |_| Detection::NotServed),
            (McpMode::Off, elsewhere),
        ] {
            let plan =
                super::plan(&failed, "/A.app/x", "1.0.0", false, mode, |_| true, detect).unwrap();
            assert!(plan.tools.is_empty());
            assert_eq!(plan.skips, vec![("claude", "toolport-foreign")]);
        }
        // 状態ファイルは新しい理由を読み、旧版の知らない理由は捨てる。
        let parsed = parse(br#"{"skipped":{"claude":"toolport-foreign","codex":"future"}}"#);
        assert_eq!(parsed.skipped.len(), 1);
        assert_eq!(parsed.skipped["claude"], "toolport-foreign");
    }

    #[test]
    fn a_skipped_claude_whose_hook_integration_failed_retries_the_hooks_even_when_off() {
        // 省くと決めたが統合に失敗して recall が None のまま。
        let failed = State {
            recall: None,
            ..skipped(&registered_state(), &[("claude", "off"), ("codex", "off")])
        };
        let retry = |mode| super::plan(&failed, "/A.app/x", "1.0.0", false, mode, |_| true, served);
        // off でも Claude のフックだけをやり直す（CLI は呼ばない）。Codex は何もしない。
        let plan = retry(McpMode::Off).unwrap();
        assert!(plan.tools.is_empty());
        assert_eq!(plan.skips, vec![("claude", "off")]);
        // 成功して recall が入った後は、off は何もしない。
        let done = State {
            recall: Some(false),
            ..failed.clone()
        };
        assert_eq!(
            super::plan(
                &done,
                "/A.app/x",
                "1.0.0",
                false,
                McpMode::Off,
                |_| true,
                served
            ),
            None
        );
        // toolport で省いた Claude も、統合に失敗していれば（Served のままでも）やり直す。
        let toolport = State {
            recall: None,
            ..skipped(&registered_state(), &[("claude", "toolport")])
        };
        let plan = super::plan(
            &toolport,
            "/A.app/x",
            "1.0.0",
            false,
            McpMode::Auto,
            |_| true,
            served,
        )
        .unwrap();
        // （skipped に無い codex は新規と同じく判定される。）
        assert_eq!(
            plan.skips,
            vec![("claude", "toolport"), ("codex", "toolport")]
        );
    }

    #[test]
    fn an_unfinished_client_is_handled_by_the_mode_when_it_is_processed_later() {
        // codex が done にも skipped にも無い（前回 CLI が無かった・失敗した）。
        let state = State {
            done: ["claude".to_string()].into(),
            ..registered_state()
        };
        let later = |mode, available: bool, detect: fn(&str) -> Detection| {
            super::plan(
                &state,
                "/A.app/x",
                "1.0.0",
                false,
                mode,
                move |_| available,
                detect,
            )
        };
        // auto: CLI があれば判定し、Served は skipped=toolport、それ以外は直接登録。
        assert_eq!(
            later(McpMode::Auto, true, served).unwrap().skips,
            vec![("codex", "toolport")]
        );
        assert_eq!(
            later(McpMode::Auto, true, |_| Detection::NotServed)
                .unwrap()
                .tools,
            vec!["codex"]
        );
        // CLI が無ければ判定もしない。
        assert_eq!(
            later(McpMode::Auto, false, |_| panic!("no CLI, no detection")),
            None
        );
        // direct は判定せず直接登録する。off は skipped=off を覚える。
        let direct = later(McpMode::Direct, true, |_| panic!("direct does not detect")).unwrap();
        assert_eq!(direct.tools, vec!["codex"]);
        assert_eq!(
            later(McpMode::Off, true, served).unwrap().skips,
            vec![("codex", "off")]
        );
    }

    #[test]
    fn a_skipped_off_client_without_its_cli_is_not_judged_when_the_mode_returns_to_auto() {
        let state = skipped(&registered_state(), &[("claude", "off")]);
        let plan = super::plan(
            &state,
            "/A.app/x",
            "1.0.0",
            false,
            McpMode::Auto,
            |_| false,
            |_| panic!("no CLI, no detection"),
        );
        assert_eq!(plan, None);
    }

    #[test]
    fn done_is_never_dropped_and_not_judged_in_any_mode() {
        let state = registered_state();
        for mode in [McpMode::Auto, McpMode::Direct, McpMode::Off] {
            // 判定が呼ばれたらテストを失敗させる。
            let plan = super::plan(
                &state,
                "/A.app/x",
                "1.0.0",
                false,
                mode,
                |_| true,
                |_| panic!("done registrations must not be judged"),
            );
            assert_eq!(plan, None, "{mode:?}");
        }
    }
}
