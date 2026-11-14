//! macOS・Linux の Setup 版の登録（Claude Code・Codex と、ログイン時の適用）を、必要なときだけ行う。
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
//! - `hook <event>` など他のサブコマンドでは行わない（毎プロンプト起動するため）。
//!
//! # 軽くする工夫（状態ファイル）
//! 状態フォルダーの `setup-registration.json` に、登録した実行ファイルのパス・版・登録済みの CLI を覚え、
//! 次の場合だけ処理する。普段の `mcp` 起動では、ファイル1つの読み込みと PATH の走査だけで終わる。
//! - パスが変わった（初回・導入先の移動）: ログイン時の適用と、見つかった全ての CLI を登録する。
//! - 版だけが変わった（更新の後）: ログイン時の適用の定義と、Claude Code のフックだけを張り直す。
//!   MCP の登録はパスが同じなら有効なままなので CLI は呼ばない（使用中のセッションの設定を外して
//!   付け直すことを避けるため）。
//! - 同じパス・版で、前回見つからなかった・失敗した CLI が今は見つかる: その CLI だけを登録する。
//!
//! 複数のセッションが同時に起動しても二重に登録しないよう、`setup-registration.lock` を try-lock する。
use super::{SetupContext, log_line, logon};
use crate::connect;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

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
}

/// 何をするか。
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// 状態を作り直す（パスが変わった）。
    pub fresh: bool,
    /// ログイン時の適用を登録し直す。
    pub logon: bool,
    /// CLI で登録する CLI。
    pub tools: Vec<&'static str>,
    /// Claude Code のフックだけを張り直す（更新の後）。
    pub refresh_hooks: bool,
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
    }
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

/// 前回の状態と今の導入から、するべきことを決める。何もしなくてよければ None。
/// `available` は CLI が見つかるかの判定（テストで差し替える）。
pub fn plan(
    state: &State,
    executable: &str,
    version: &str,
    available: impl Fn(&str) -> bool,
) -> Option<Plan> {
    if state.executable.as_deref() != Some(executable) {
        return Some(Plan {
            fresh: true,
            logon: true,
            tools: connect::SETUP_TOOLS.to_vec(),
            refresh_hooks: false,
        });
    }
    let updated = state.version.as_deref() != Some(version);
    let tools: Vec<&'static str> = connect::SETUP_TOOLS
        .into_iter()
        .filter(|tool| !state.done.contains(*tool) && available(tool))
        .collect();
    let logon = updated || !state.logon;
    let refresh_hooks = updated && state.done.contains("claude");
    if !logon && tools.is_empty() && !refresh_hooks {
        return None;
    }
    Some(Plan {
        fresh: false,
        logon,
        tools,
        refresh_hooks,
    })
}

/// 必要なら登録する。`label` は診断ログの見出し。失敗は例外にせず、診断ログにだけ残す。
pub fn ensure(context: &SetupContext, label: &str) {
    let state_dir = crate::paths::Roots::current().state;
    let executable = context.executable.to_string_lossy().into_owned();
    if plan(
        &load(&state_dir),
        &executable,
        &context.version,
        connect::cli_available,
    )
    .is_none()
    {
        return;
    }
    // 他のプロセスが登録中なら任せる（二重に CLI を呼ばない）。
    let Some(_lock) = super::try_lock_named(&state_dir, LOCK_FILE) else {
        return;
    };
    // ロックを取る間に他のプロセスが終えていることがある。読み直して決め直す。
    let previous = load(&state_dir);
    let Some(plan) = plan(
        &previous,
        &executable,
        &context.version,
        connect::cli_available,
    ) else {
        return;
    };
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
    if plan.refresh_hooks {
        log_line(&format!(
            "{label}: {}",
            connect::refresh_claude_hooks_for_setup(&context.executable)
        ));
    }
    for outcome in connect::register_tools_for_setup(&context.executable, &plan.tools) {
        if outcome.done {
            state.done.insert(outcome.tool.to_string());
        } else {
            state.done.remove(outcome.tool);
        }
        log_line(&format!("{label}: {}", outcome.line));
    }
    if let Err(error) = save(&state_dir, &state) {
        log_line(&format!("{label}: registration state not saved: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(state: &State, tools: &[&str]) -> State {
        State {
            done: tools.iter().map(|t| t.to_string()).collect(),
            ..state.clone()
        }
    }

    #[test]
    fn a_new_or_moved_install_registers_everything() {
        let everything = Plan {
            fresh: true,
            logon: true,
            tools: vec!["claude", "codex"],
            refresh_hooks: false,
        };
        assert_eq!(
            plan(&State::default(), "/A.app/x", "1.0.0", |_| false),
            Some(everything)
        );
        let registered = State {
            executable: Some("/Old.app/x".into()),
            version: Some("1.0.0".into()),
            done: ["claude".to_string(), "codex".to_string()].into(),
            logon: true,
        };
        assert!(
            plan(&registered, "/A.app/x", "1.0.0", |_| true)
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
        };
        assert_eq!(plan(&state, "/A.app/x", "1.0.0", |_| true), None);
        // 見つからなかった CLI は、見つかるまでは何もしない（毎回の起動でプロセスを起動しない）。
        let partial = done(&state, &["claude"]);
        assert_eq!(plan(&partial, "/A.app/x", "1.0.0", |_| false), None);
        // 見つかったら、その CLI だけを登録する。
        assert_eq!(
            plan(&partial, "/A.app/x", "1.0.0", |tool| tool == "codex"),
            Some(Plan {
                fresh: false,
                logon: false,
                tools: vec!["codex"],
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
        };
        assert_eq!(
            plan(&state, "/A.app/x", "1.0.1", |_| true),
            Some(Plan {
                fresh: false,
                logon: true,
                tools: vec![],
                refresh_hooks: true
            })
        );
        // Claude を登録していなければ、フックは張り直さない。
        assert!(
            !plan(&done(&state, &["codex"]), "/A.app/x", "1.0.1", |_| false)
                .unwrap()
                .refresh_hooks
        );
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
        };
        save(&dir, &state).unwrap();
        assert_eq!(load(&dir), state);
        fs::write(dir.join(STATE_FILE), b"{ broken").unwrap();
        assert_eq!(load(&dir), State::default());
        assert!(forget(&dir).unwrap());
        assert!(!forget(&dir).unwrap());
    }
}
