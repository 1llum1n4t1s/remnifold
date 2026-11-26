//! 単独アプリとして導入した Remnifold を、Claude Code・Codex へ登録する。
//!
//! 登録はどれも導入先の固定の実行ファイル（crate::paths::app_executable）を指し、更新で版が変わっても
//! 登録をやり直さなくてよい。既定では登録内容を表示するだけで、--apply を付けたときだけ書き込む。
//!
//! Claude Code:
//! - MCP サーバーは公式 CLI（`claude mcp add --scope user`）で登録する。CLI が見つからなければ、
//!   実行すべきコマンドを表示して利用者に任せる（設定ファイルを直接書き換えない）。
//! - フック（SessionStart・UserPromptSubmit・Stop・PreToolUse）は settings.json へ統合する。
//!   既存の Remnifold 用の項目（旧ランチャー経由を含む）だけを置き換え、他の設定は保つ。
//!   自動想起（`recallMode`）が off のときは、想起だけに使う SessionStart・UserPromptSubmit を登録せず、
//!   既にある Remnifold の項目も外す（保存の Stop と読み取り許可の PreToolUse は残す）。
//!   壊れた settings.json は書き換えずに止め、初回の書換え前に元の内容を退避する。
//!
//! Codex:
//! - MCP サーバーを公式 CLI（`codex mcp add`）で登録する。フックはプラグイン以外から登録する公式手段が
//!   無いため登録しない（索引の自動想起と保存は Claude Code のフックで行う）。
//!
//! 解除（`--remove`）は、登録の逆を自分の項目だけに行う。settings.json の他のフック・設定、他の MCP 登録は
//! 保つ。Setup（Velopack）版のインストール・更新・アンインストールの各フックからは、
//! [`register_tool_for_setup`] / [`unregister_for_setup`] を通して同じ処理を呼ぶ（失敗しても例外にせず、
//! 結果を文字列で返す。フックの失敗でインストールや更新を止めないため）。
use crate::common::{atomic_write, home_dir};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Setup（Velopack）のフックから呼ぶ CLI 1回あたりの上限（既定）。Velopack はインストール・アンインストール後の
/// フックを30秒、更新後のフックを15秒で打ち切る。1回を3秒に制限し（remove と add の間で打ち切られると登録が
/// 消えたままになるため、無制限に待たせない）、更新後のフックはさらに合計の時間予算（`app_update::registration`）で
/// 次の CLI の登録を始める前に打ち切る。これで全体が15秒に収まる。
pub const HOOK_CLI_TIMEOUT: Duration = Duration::from_secs(3);

/// フック以外（ログオン時の `update-apply`・引数なしの起動・`mcp` の背景スレッド）で登録をやり直すときの上限。
/// Velopack の打ち切りが無いので長めにし、フックで3秒に収まらなかった登録をここで完了させる。
pub const RETRY_CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// このプロセスで、時間制限付きの CLI 呼び出し（`bounded`）に使う上限（ミリ秒）。プロセスごとに役割が1つ
/// （フック・ログオン時の適用・MCP など）なので、起動の入口で一度だけ [`set_cli_timeout`] で決める。
static CLI_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(HOOK_CLI_TIMEOUT.as_millis() as u64);

/// 時間制限付きの CLI 呼び出しの上限を変える（フック以外の入口から、登録をやり直す前に呼ぶ）。
pub fn set_cli_timeout(timeout: Duration) {
    CLI_TIMEOUT_MS.store(
        timeout.as_millis() as u64,
        std::sync::atomic::Ordering::SeqCst,
    );
}

fn cli_timeout() -> Duration {
    Duration::from_millis(CLI_TIMEOUT_MS.load(std::sync::atomic::Ordering::SeqCst))
}

/// CLI を1回実行する。終了コード 0 なら標準出力を返し、非 0 なら None。起動できない・時間切れは Err。
/// `bounded` のときは、子プロセスの木ごと時間で打ち切る（フック用）。
fn run_cli<S: AsRef<OsStr>>(cli: &Path, args: &[S], bounded: bool) -> Result<Option<String>> {
    let mut command = Command::new(cli);
    command.args(args);
    // macOS の Setup 版は、インストーラーの `open` やログイン時の launchd から、最小限の PATH
    // （/usr/bin:/bin:/usr/sbin:/sbin）で起動されることがある。npm で入れた CLI は `#!/usr/bin/env node` の
    // スクリプトで、node は CLI と同じフォルダー（Homebrew など）にあることが多い。CLI のフォルダーを
    // PATH の先頭に足し、子プロセスがその隣の node を見つけられるようにする。
    // Linux の AppImage も、ファイルマネージャーからの起動では利用者のシェルの PATH を持たないことがあるため同じ扱い。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(dir) = cli.parent() {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
    }
    if bounded {
        return match crate::updater::process::run(command, cli_timeout(), 64 * 1024) {
            Ok(stdout) => Ok(Some(String::from_utf8_lossy(&stdout).into_owned())),
            Err(error) if error.to_string() == "CLI_FAILED" => Ok(None),
            Err(error) => Err(error),
        };
    }
    let output = command
        .output()
        .map_err(|_| anyhow::anyhow!("The command could not be started."))?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// 登録するフック。マッチャー・タイムアウト・非同期・表示文言はパッケージの hooks.json と揃える。
/// 最後の値は、自動想起（注入）だけに使うフックか（`recallMode` が off なら登録しない）。
/// イベント名・マッチャー・タイムアウト（秒）・非同期・表示文言・想起専用か。
type HookDef = (
    &'static str,
    Option<&'static str>,
    u64,
    bool,
    &'static str,
    bool,
);

const HOOKS: [HookDef; 4] = [
    (
        "PreToolUse",
        Some("^mcp__remnifold__|^mcp__toolport__toolport_call_tool$"),
        5,
        false,
        "Checking tool access...",
        false,
    ),
    (
        "SessionStart",
        None,
        30,
        false,
        "Loading memory index...",
        true,
    ),
    (
        "UserPromptSubmit",
        None,
        12,
        false,
        "Searching memory index...",
        true,
    ),
    ("Stop", None, 120, true, "Saving memory index...", false),
];

/// 自動想起（注入）が有効か。フックの実行時（`hooks::run_hook`）と同じ判定にする: 接続設定を読めない
/// ときも同じ代わりの設定から [`crate::common::read_settings`] で `recallMode` を読み、無ければ off。
pub fn recall_enabled() -> bool {
    setup_settings().recall
}

/// Claude Code・Codex への MCP の自動登録の方針（`remnifold.json` の `mcpRegistration`）。
/// `Auto`（既定）は Toolport が既に提供しているクライアントへの新しい直接登録を省き、`Direct` は常に直接登録し、
/// `Off` は自動の追加・修復をしない（アンインストール時の解除は続ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpMode {
    #[default]
    Auto,
    Direct,
    Off,
}

impl McpMode {
    /// 設定値から方針を決める。無い（null）は `Auto`。小文字の完全一致（`auto`/`direct`/`off`）だけを受け付け、大文字・前後の空白付きなど不正な値は `Auto` とし、2つ目を false にする。
    pub fn from_setting(value: &Value) -> (McpMode, bool) {
        match value {
            Value::Null => (McpMode::Auto, true),
            Value::String(text) => match text.as_str() {
                "auto" => (McpMode::Auto, true),
                "direct" => (McpMode::Direct, true),
                "off" => (McpMode::Off, true),
                _ => (McpMode::Auto, false),
            },
            _ => (McpMode::Auto, false),
        }
    }
}

/// Setup の登録が使う接続設定（1回だけ読み、判定と書き込みで同じ値を使う）。
#[derive(Debug, Clone, Copy)]
pub struct SetupSettings {
    /// 自動想起（注入）が有効か。
    pub recall: bool,
    /// MCP の自動登録の方針。
    pub mode: McpMode,
    /// `mcpRegistration` が不正な値で、`Auto` として扱ったか（ログ用）。
    pub invalid_mode: bool,
}

/// 接続設定（`remnifold.json`）から [`SetupSettings`] を読む。読めないときは [`recall_enabled`] と同じ代わりの設定を使う。
pub fn setup_settings() -> SetupSettings {
    use crate::common::{Config, codex_home, load_config, read_settings};
    let config = load_config(None).unwrap_or_else(|_| Config {
        base_url: String::new(),
        api_key: String::new(),
        raw: json!({}),
        codex_home: codex_home(),
        roots: crate::paths::Roots::current(),
    });
    let settings = read_settings(&config);
    let (mode, valid) = McpMode::from_setting(&settings["mcpRegistration"]);
    SetupSettings {
        recall: settings["recallMode"] != "off",
        mode,
        invalid_mode: !valid,
    }
}

/// Toolport の判定が Remnifold の MCP サーバーを見分けるための製品情報。
pub const TOOLPORT_PRODUCT: crate::toolport::Product = crate::toolport::Product {
    binary_stem: "remnifold",
    mode_arg: "mcp",
};

/// 手動の `connect` の結果へ、Toolport が既に提供している場合の重複の通知を付ける（登録は止めない）。
fn attach_toolport_notice(result: &mut Value, detection: crate::toolport::Detection) {
    if let crate::toolport::Detection::Served { server_id, command } = detection {
        result["toolport"] = json!({"serverId": server_id, "command": command, "duplicate": true});
    }
}

/// `recall` のときに登録するフックのイベント名。
fn registered_events(recall: bool) -> Vec<&'static str> {
    HOOKS
        .iter()
        .filter(|hook| recall || !hook.5)
        .map(|hook| hook.0)
        .collect()
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(tool) = args.first().map(String::as_str) else {
        bail!(
            "Usage: remnifold connect <claude-code|codex> [--remove] [--apply] [--executable <path>] [--cli <path>]"
        );
    };
    if matches!(tool, "--help" | "-h") {
        println!(
            "{}",
            crate::locale::tr_with(
                "connect.help",
                &[(
                    "executable",
                    &crate::paths::app_executable(&crate::paths::app_root())
                        .display()
                        .to_string()
                )]
            )
        );
        return Ok(());
    }
    let mut apply = false;
    let mut remove = false;
    let mut executable = None;
    let mut cli = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--apply" => apply = true,
            "--remove" => remove = true,
            "--executable" | "--cli" => {
                let flag = args[index].clone();
                index += 1;
                let value = args
                    .get(index)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("{flag} requires a value."))?;
                let path = crate::common::absolute_path(Path::new(value))?;
                if flag == "--executable" {
                    executable = Some(path);
                } else {
                    cli = Some(path);
                }
            }
            _ => bail!("Unknown connect option. Use remnifold connect --help."),
        }
        index += 1;
    }
    if remove {
        // 解除は実行ファイルが無くても行える（アンインストール後の掃除）。--executable を指定したときだけ、
        // その実行ファイルを指す登録に限って消す。省略時は、どこを指していても Remnifold の登録を消す。
        let result = match tool {
            "claude-code" => claude_code_remove(executable.as_deref(), cli, apply, false)?,
            "codex" => codex_remove(executable.as_deref(), cli, apply, false)?,
            _ => bail!("Unknown tool. Use claude-code or codex."),
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let executable =
        executable.unwrap_or_else(|| crate::paths::app_executable(&crate::paths::app_root()));
    ensure!(
        executable.is_file(),
        "Remnifold is not installed at {}. Run remnifold install --version <version> --apply first.",
        executable.display()
    );
    let (mut result, client) = match tool {
        "claude-code" => (
            claude_code(&executable, cli, apply, false, recall_enabled())?,
            crate::toolport::ClientId::ClaudeCode,
        ),
        "codex" => (
            codex(&executable, cli, apply, false)?,
            crate::toolport::ClientId::Codex,
        ),
        _ => bail!("Unknown tool. Use claude-code or codex."),
    };
    // 登録は判定に関わらず従来どおり行い、Toolport が既に提供しているときだけ重複を知らせる。
    attach_toolport_notice(
        &mut result,
        crate::toolport::detect(&TOOLPORT_PRODUCT, client, &crate::toolport::Env::process()),
    );
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// フックのコマンド文字列。Claude Code は Windows でも Git Bash でフックを実行するため、
/// 区切りを / にした実行ファイルのパスを二重引用符で囲む。引用を壊す文字を含むパスは拒否する。
fn hook_command(executable: &Path, event: &str) -> Result<String> {
    let path = executable.to_string_lossy().replace('\\', "/");
    ensure!(
        !path.contains(['"', '$', '`', '\n', '\r', '\\']),
        "The Remnifold executable path contains characters that cannot be used in a hook command."
    );
    Ok(format!("\"{path}\" hook {event}"))
}

/// この項目が Remnifold の登録か（旧ランチャー経由を含む）。
fn is_remnifold_entry(entry: &Value, event: &str) -> bool {
    entry["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|hook| {
            let command = hook["command"].as_str().unwrap_or("");
            command.to_ascii_lowercase().contains("remnifold")
                && command.trim_end().ends_with(&format!(" hook {event}"))
        })
    })
}

/// settings.json の値へ Remnifold のフックを統合する（何度実行しても同じ結果になる）。
/// `recall` が false なら想起用のフックは登録せず、既にある Remnifold の項目を外す（空になったイベントは消す）。
fn merge_hooks(settings: &mut Value, executable: &Path, recall: bool) -> Result<()> {
    ensure!(
        settings.is_object(),
        "Claude Code settings must be a JSON object; it was not changed."
    );
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }
    ensure!(
        settings["hooks"].is_object(),
        "Claude Code settings has a non-object \"hooks\" value; it was not changed."
    );
    for (event, matcher, timeout, asynchronous, status, recall_only) in HOOKS {
        let mut entries = match settings["hooks"].get(event) {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => bail!(
                "Claude Code settings has a non-array hooks.{event} value; it was not changed."
            ),
        };
        entries.retain(|entry| !is_remnifold_entry(entry, event));
        if recall_only && !recall {
            if entries.is_empty() {
                settings["hooks"]
                    .as_object_mut()
                    .expect("checked above")
                    .remove(event);
            } else {
                settings["hooks"][event] = Value::Array(entries);
            }
            continue;
        }
        let mut hook = json!({
            "type":"command",
            "command": hook_command(executable, event)?,
            "timeout": timeout,
            "statusMessage": status
        });
        if asynchronous {
            hook["async"] = json!(true);
        }
        let mut entry = json!({"hooks":[hook]});
        if let Some(matcher) = matcher {
            entry["matcher"] = json!(matcher);
        }
        entries.push(entry);
        settings["hooks"][event] = Value::Array(entries);
    }
    Ok(())
}

fn claude_settings_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".claude"))
        .join("settings.json")
}

fn claude_code(
    executable: &Path,
    cli: Option<PathBuf>,
    apply: bool,
    bounded: bool,
    recall: bool,
) -> Result<Value> {
    let mut result = claude_hooks(executable, apply, recall)?;
    let mcp_args = mcp_add_args(executable, true);
    result["mcpCommand"] = json!(format!("claude {}", display_args(&mcp_args)));
    if !apply {
        return Ok(result);
    }
    result["mcp"] = register_mcp(cli.or_else(|| find_cli("claude")), &mcp_args, true, bounded)?;
    Ok(result)
}

/// Claude Code のフック（settings.json）だけを統合する。MCP の登録（CLI）は行わない。
/// Toolport が MCP を提供している・自動登録を止めている場合でも、フックは統合する。
fn claude_hooks(executable: &Path, apply: bool, recall: bool) -> Result<Value> {
    let settings_path = claude_settings_path();
    let mut settings = match fs::read(&settings_path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_err(|_| {
            anyhow::anyhow!("Claude Code settings is not valid JSON; it was not changed.")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => bail!("Claude Code settings could not be read."),
    };
    let before = settings.clone();
    merge_hooks(&mut settings, executable, recall)?;
    let mut result = json!({
        "tool":"claude-code",
        "applied":apply,
        "executable":executable,
        "settings":settings_path,
        "hooks": registered_events(recall),
        "recallHooks": recall,
        "hooksChanged": settings != before
    });
    if !apply {
        return Ok(result);
    }
    if settings != before {
        // 初回の書換え前だけ元の内容を退避し、利用者が戻せるようにする。
        let backup = settings_path.with_file_name("settings.json.remnifold-backup");
        if settings_path.exists() && !backup.exists() {
            fs::copy(&settings_path, &backup)?;
            result["backup"] = json!(backup);
        }
        atomic_write(
            &settings_path,
            &[serde_json::to_vec_pretty(&settings)?, b"\n".to_vec()].concat(),
            false,
        )?;
    }
    Ok(result)
}

fn codex(executable: &Path, cli: Option<PathBuf>, apply: bool, bounded: bool) -> Result<Value> {
    let mcp_args = mcp_add_args(executable, false);
    let mut result = json!({
        "tool":"codex",
        "applied":apply,
        "executable":executable,
        "mcpCommand": format!("codex {}", display_args(&mcp_args)),
        "hooks":"not registered: Codex has no supported way to register hooks outside plugins"
    });
    if apply {
        result["mcp"] = register_mcp(cli.or_else(|| find_cli("codex")), &mcp_args, false, bounded)?;
    }
    Ok(result)
}

/// パスを比較用に正規化する（区切りを / に揃え、小文字にする。Windows のパスは大小を区別しない）。
fn normalize_for_match(text: &str) -> String {
    text.replace('\\', "/").to_ascii_lowercase()
}

/// このフック項目のコマンドが、指定の実行ファイルを指しているか。
fn entry_points_at(entry: &Value, executable: &Path) -> bool {
    let needle = normalize_for_match(&executable.to_string_lossy());
    entry["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|hook| {
            normalize_for_match(hook["command"].as_str().unwrap_or("")).contains(&needle)
        })
    })
}

/// settings.json の値から Remnifold のフックだけを取り除く（何度実行しても同じ結果になる）。
/// `only_executable` を指定すると、その実行ファイルを指す項目に限る。空になったイベントと
/// `hooks` は削除する。形が想定外（配列でない等）の部分は触らない。
fn remove_hooks(settings: &mut Value, only_executable: Option<&Path>) -> Result<()> {
    ensure!(
        settings.is_object(),
        "Claude Code settings must be a JSON object; it was not changed."
    );
    let Some(hooks) = settings.get_mut("hooks") else {
        return Ok(());
    };
    ensure!(
        hooks.is_object(),
        "Claude Code settings has a non-object \"hooks\" value; it was not changed."
    );
    let map = hooks.as_object_mut().expect("checked above");
    for (event, ..) in HOOKS {
        let Some(entries) = map.get_mut(event) else {
            continue;
        };
        let Some(items) = entries.as_array_mut() else {
            bail!("Claude Code settings has a non-array hooks.{event} value; it was not changed.");
        };
        items.retain(|entry| {
            !(is_remnifold_entry(entry, event)
                && only_executable.is_none_or(|exe| entry_points_at(entry, exe)))
        });
        if items.is_empty() {
            map.remove(event);
        }
    }
    if map.is_empty() {
        settings
            .as_object_mut()
            .expect("checked above")
            .remove("hooks");
    }
    Ok(())
}

/// MCP の登録が、指定の実行ファイルを指しているか（`mcp get remnifold` の出力で確かめる）。
/// 確かめられない（CLI が失敗した・登録が無い・時間切れ）ときは false。
fn mcp_points_at(cli: &Path, executable: &Path, bounded: bool) -> bool {
    run_cli(cli, &["mcp", "get", "remnifold"], bounded)
        .ok()
        .flatten()
        .is_some_and(|stdout| {
            normalize_for_match(&stdout)
                .contains(&normalize_for_match(&executable.to_string_lossy()))
        })
}

fn unregister_mcp(
    cli: Option<PathBuf>,
    claude: bool,
    only_executable: Option<&Path>,
    bounded: bool,
) -> Result<Value> {
    let Some(cli) = cli else {
        return Ok(
            json!({"status":"manual","reason":"CLI not found; remove the remnifold MCP entry yourself"}),
        );
    };
    if let Some(executable) = only_executable
        && !mcp_points_at(&cli, executable, bounded)
    {
        return Ok(
            json!({"status":"kept","reason":"the remnifold MCP entry is absent or points at another executable"}),
        );
    }
    let mut remove = vec!["mcp", "remove", "remnifold"];
    if claude {
        remove.extend(["--scope", "user"]);
    }
    let removed = run_cli(&cli, &remove, bounded)
        .map_err(|_| anyhow::anyhow!("The MCP removal command could not be completed."))?;
    if removed.is_some() {
        return Ok(json!({"status":"removed","cli":cli}));
    }
    // 登録が無いときに CLI が非 0 で終わる実装がある。その場合は「既に無い」ので成功として扱い、
    // 登録が残っているのに失敗したときだけエラーにする。
    if run_cli(&cli, &["mcp", "get", "remnifold"], bounded)
        .ok()
        .flatten()
        .is_none()
    {
        return Ok(json!({"status":"absent","cli":cli}));
    }
    bail!("The MCP removal command failed.")
}

fn claude_code_remove(
    executable: Option<&Path>,
    cli: Option<PathBuf>,
    apply: bool,
    bounded: bool,
) -> Result<Value> {
    let settings_path = claude_settings_path();
    // settings.json が無ければ何も作らない（Claude Code 未使用の環境に空のファイルを残さない）。
    let (mut settings, exists) = match fs::read(&settings_path) {
        Ok(bytes) => (
            serde_json::from_slice::<Value>(&bytes).map_err(|_| {
                anyhow::anyhow!("Claude Code settings is not valid JSON; it was not changed.")
            })?,
            true,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (json!({}), false),
        Err(_) => bail!("Claude Code settings could not be read."),
    };
    let before = settings.clone();
    remove_hooks(&mut settings, executable)?;
    let mut result = json!({
        "tool":"claude-code",
        "action":"remove",
        "applied":apply,
        "settings":settings_path,
        "hooksChanged": settings != before,
        "mcpCommand":"claude mcp remove remnifold --scope user"
    });
    if !apply {
        return Ok(result);
    }
    if exists && settings != before {
        let backup = settings_path.with_file_name("settings.json.remnifold-backup");
        if !backup.exists() {
            fs::copy(&settings_path, &backup)?;
            result["backup"] = json!(backup);
        }
        atomic_write(
            &settings_path,
            &[serde_json::to_vec_pretty(&settings)?, b"\n".to_vec()].concat(),
            false,
        )?;
    }
    result["mcp"] = unregister_mcp(
        cli.or_else(|| find_cli("claude")),
        true,
        executable,
        bounded,
    )?;
    Ok(result)
}

fn codex_remove(
    executable: Option<&Path>,
    cli: Option<PathBuf>,
    apply: bool,
    bounded: bool,
) -> Result<Value> {
    let mut result = json!({
        "tool":"codex",
        "action":"remove",
        "applied":apply,
        "mcpCommand":"codex mcp remove remnifold"
    });
    if apply {
        result["mcp"] = unregister_mcp(
            cli.or_else(|| find_cli("codex")),
            false,
            executable,
            bounded,
        )?;
    }
    Ok(result)
}

/// Setup 版が登録する CLI の名前（`claude` と `codex`）。Setup 版の登録は全 OS 共通で
/// `app_update::registration` が状態ファイルとあわせて [`register_tool_for_setup`] を呼ぶ。
pub const SETUP_TOOLS: [&str; 2] = ["claude", "codex"];

/// [`register_tool_for_setup`] の1つの CLI の結果。
pub struct SetupOutcome {
    /// CLI の名前（`claude` / `codex`）。
    pub tool: &'static str,
    /// 登録まで済んだか。CLI が見つからない・失敗した場合は false（次の機会に再試行してよい）。
    pub done: bool,
    /// 診断ログ用の1行。
    pub line: String,
}

/// 1つの CLI の登録に最悪でかかる時間。`mcp get`・`remove`・`add` の3回が、それぞれ上限まで待つ場合。
/// フックの時間予算（[`crate::app_update::registration`]）が、次の CLI を始めてよいかの判断に使う。
pub fn tool_registration_worst_case() -> Duration {
    cli_timeout() * 3
}

/// 指定の CLI を1つだけ登録する（Setup 版の登録で、未完了の CLI をやり直すために使う）。
/// 未知の名前は CLI が無いものとして扱う。どの失敗も例外にしない。`recall` は [`recall_enabled`] の値。
pub fn register_tool_for_setup(
    executable: &Path,
    tool: &'static str,
    recall: bool,
) -> SetupOutcome {
    let claude = tool == "claude";
    let Some(cli) = SETUP_TOOLS
        .contains(&tool)
        .then(|| find_cli(tool))
        .flatten()
    else {
        return SetupOutcome {
            tool,
            done: false,
            line: format!("{tool}: CLI not found; skipped"),
        };
    };
    let outcome = if claude {
        claude_code(executable, Some(cli), true, true, recall)
    } else {
        codex(executable, Some(cli), true, true)
    };
    match outcome {
        Ok(value) => SetupOutcome {
            tool,
            done: true,
            line: format!(
                "{tool}: {}",
                value["mcp"]["status"].as_str().unwrap_or("done")
            ),
        },
        Err(error) => SetupOutcome {
            tool,
            done: false,
            line: format!("{tool}: failed: {error}"),
        },
    }
}

/// 更新の後や自動想起の切り替えの後に、Claude Code の settings.json にある Remnifold のフックだけを
/// 最新の定義（`recall` に応じたイベント）へ張り直す。
/// CLI は呼ばない（MCP の登録は実行ファイルのパスが変わらない限り有効なままで、使用中のセッションの
/// 設定を CLI で外して付け直すことを避けるため）。この実行ファイルを指す Remnifold のフックが1つも
/// 無ければ（利用者が外した・未登録）、何も書かない。返す bool は、張り直しが済んだ（変更不要を含む）か。
pub fn refresh_claude_hooks_for_setup(executable: &Path, recall: bool) -> (bool, String) {
    let outcome = (|| -> Result<bool> {
        let settings_path = claude_settings_path();
        let bytes = match fs::read(&settings_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => bail!("Claude Code settings could not be read."),
        };
        let mut settings = serde_json::from_slice::<Value>(&bytes).map_err(|_| {
            anyhow::anyhow!("Claude Code settings is not valid JSON; it was not changed.")
        })?;
        let ours = HOOKS.iter().any(|(event, ..)| {
            settings["hooks"][event].as_array().is_some_and(|items| {
                items.iter().any(|entry| {
                    is_remnifold_entry(entry, event) && entry_points_at(entry, executable)
                })
            })
        });
        if !ours {
            return Ok(false);
        }
        let before = settings.clone();
        merge_hooks(&mut settings, executable, recall)?;
        if settings == before {
            return Ok(false);
        }
        let backup = settings_path.with_file_name("settings.json.remnifold-backup");
        if !backup.exists() {
            fs::copy(&settings_path, &backup)?;
        }
        atomic_write(
            &settings_path,
            &[serde_json::to_vec_pretty(&settings)?, b"\n".to_vec()].concat(),
            false,
        )?;
        Ok(true)
    })();
    match outcome {
        Ok(true) => (true, "claude: hooks updated".to_string()),
        Ok(false) => (true, "claude: hooks unchanged".to_string()),
        Err(error) => (false, format!("claude: hooks failed: {error}")),
    }
}

/// Toolport が MCP を提供している（または自動登録を止めている）Claude Code で、フックだけを統合する
/// （CLI は呼ばない。settings.json が無ければ作る）。返す bool は、統合が済んだ（変更不要を含む）か。
pub fn integrate_claude_hooks_for_setup(executable: &Path, recall: bool) -> (bool, String) {
    match claude_hooks(executable, true, recall) {
        Ok(value) => (
            true,
            if value["hooksChanged"] == true {
                "claude: hooks integrated (MCP registration not needed)".to_string()
            } else {
                "claude: hooks unchanged (MCP registration not needed)".to_string()
            },
        ),
        Err(error) => (false, format!("claude: hooks failed: {error}")),
    }
}

/// Claude Code の設定フォルダー（`~/.claude`、`CLAUDE_CONFIG_DIR` があればそれ）があるか。
pub fn claude_home_exists() -> bool {
    claude_settings_path().parent().is_some_and(Path::is_dir)
}

/// その CLI が見つかるか（プロセスを起動しない軽い確認。登録をやり直すべきかの判定に使う）。
pub fn cli_available(tool: &str) -> bool {
    find_cli(tool).is_some()
}

/// Setup（Velopack）のアンインストールフックから呼ぶ解除。導入先の実行ファイルを指す登録だけを消し、
/// 別の場所（旧 install 版など）を指す登録は保つ。失敗は例外にせず結果の行で返す。
pub fn unregister_for_setup(executable: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for (tool, claude) in [("claude", true), ("codex", false)] {
        let Some(cli) = find_cli(tool) else {
            // CLI が無ければ MCP は解除できないが、Claude のフックは settings.json から外せる。
            if claude {
                lines.push(
                    match claude_code_remove(Some(executable), None, true, true) {
                        Ok(_) => "claude: CLI not found; hooks removed, MCP entry left".to_string(),
                        Err(error) => format!("claude: failed: {error}"),
                    },
                );
            } else {
                lines.push(format!("{tool}: CLI not found on PATH; skipped"));
            }
            continue;
        };
        let outcome = if claude {
            claude_code_remove(Some(executable), Some(cli), true, true)
        } else {
            codex_remove(Some(executable), Some(cli), true, true)
        };
        lines.push(match outcome {
            Ok(value) => format!(
                "{tool}: {}",
                value["mcp"]["status"].as_str().unwrap_or("done")
            ),
            Err(error) => format!("{tool}: failed: {error}"),
        });
    }
    lines
}

fn mcp_add_args(executable: &Path, claude: bool) -> Vec<String> {
    let mut args = vec!["mcp".to_string(), "add".to_string()];
    if claude {
        args.extend(["--scope".to_string(), "user".to_string()]);
    }
    args.extend([
        "remnifold".to_string(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        "mcp".to_string(),
    ]);
    args
}

/// [`mcp_add_args`] の引数から、登録する実行ファイル（`--` の直後）を取り出す。
fn add_executable(add: &[String]) -> Option<&str> {
    let separator = add.iter().position(|arg| arg == "--")?;
    add.get(separator + 1).map(String::as_str)
}

fn display_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.contains([' ', '"']) {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 既存の remnifold 登録を外してから登録し直す（同じ名前の古い登録を残さない）。
fn register_mcp(
    cli: Option<PathBuf>,
    add: &[String],
    claude: bool,
    bounded: bool,
) -> Result<Value> {
    let Some(cli) = cli else {
        return Ok(json!({"status":"manual","reason":"CLI not found; run mcpCommand yourself"}));
    };
    // 既にこの実行ファイルを指す登録なら、外さずにそのまま使う。外してから付け直す間に打ち切られると
    // 登録が消えたままになるため、必要なときだけ remove → add を行う（旧導入先を指す登録の置き換えなど）。
    // `add` の引数の先頭（`mcp add ... -- <実行ファイル> mcp`）から実行ファイルを取り出して照合する。
    if let Some(executable) = add_executable(add)
        && mcp_points_at(&cli, Path::new(executable), bounded)
    {
        return Ok(json!({"status":"unchanged","cli":cli}));
    }
    let mut remove = vec!["mcp", "remove", "remnifold"];
    if claude {
        remove.extend(["--scope", "user"]);
    }
    // 登録が無くて失敗しても構わない（外した後に登録し直すだけ）。打ち切りや起動の失敗も、ここでは無視する。
    let _ = run_cli(&cli, &remove, bounded);
    let added = run_cli(&cli, add, bounded)
        .map_err(|_| anyhow::anyhow!("The MCP registration command could not be completed."))?;
    ensure!(added.is_some(), "The MCP registration command failed.");
    Ok(json!({"status":"registered","cli":cli}))
}

/// PATH から実行ファイルを探す（Windows は .exe だけを対象にし、スクリプトのラッパーは使わない）。
///
/// macOS では PATH に無ければ、CLI の代表的な導入先も探す。Setup 版の初回起動（インストーラーの `open`）や
/// ログイン時の起動は launchd の最小限の PATH で動くため、PATH だけでは `~/.local/bin/claude`
/// （Claude Code の公式導入先）や Homebrew の `codex` を見つけられないため。PATH を先に探すので、
/// PATH に置いた CLI（検証用のスタブを含む）が常に優先される。
fn find_cli(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let dirs: Vec<PathBuf> = dirs
        .into_iter()
        .chain(macos_cli_fallback_dirs(&home_dir()))
        .collect();
    #[cfg(target_os = "linux")]
    let dirs: Vec<PathBuf> = dirs
        .into_iter()
        .chain(linux_cli_fallback_dirs(&home_dir()))
        .collect();
    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

/// Linux で PATH の後に探す、CLI の代表的な導入先（探す順）。AppImage をファイルマネージャーから起動した場合や、
/// XDG autostart から起動した場合は、ログインシェルで足される `~/.local/bin` などが PATH に無いことがあるため。
/// 全 OS でコンパイルし、単体テストで順序を確かめる。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_cli_fallback_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if home.is_absolute() {
        // Claude Code の公式インストーラー、旧来のローカル導入、npm の利用者単位の導入先。
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".npm-global/bin"));
    }
    // ディストリビューションの外から入れたもの（npm の全体導入など）と、Linuxbrew。
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/home/linuxbrew/.linuxbrew/bin"));
    dirs
}

/// macOS で PATH の後に探す、CLI の代表的な導入先（探す順）。
#[cfg(target_os = "macos")]
fn macos_cli_fallback_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if home.is_absolute() {
        // Claude Code の公式インストーラー、旧来のローカル導入、npm の利用者単位の導入先。
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".npm-global/bin"));
    }
    // Homebrew（Apple Silicon と旧来の場所）。
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_arguments_name_the_registered_executable() {
        // 登録済みの確認（`mcp get`）は、add の `--` の直後の実行ファイルで照合する。
        let executable =
            Path::new(r"C:\Users\利用者\AppData\Local\RemnifoldApp\current\remnifold.exe");
        for claude in [true, false] {
            let args = mcp_add_args(executable, claude);
            assert_eq!(
                add_executable(&args),
                Some(executable.to_string_lossy().as_ref())
            );
        }
        assert_eq!(
            add_executable(&["mcp".to_string(), "add".to_string()]),
            None
        );
        assert_eq!(add_executable(&["--".to_string()]), None);
    }

    #[test]
    fn cli_timeout_defaults_to_the_hook_bound_and_can_be_raised() {
        // 既定はフック用の3秒。フック以外の入口が長めの上限へ切り替える。
        assert_eq!(cli_timeout(), HOOK_CLI_TIMEOUT);
        set_cli_timeout(RETRY_CLI_TIMEOUT);
        assert_eq!(cli_timeout(), RETRY_CLI_TIMEOUT);
        set_cli_timeout(HOOK_CLI_TIMEOUT);
    }

    #[test]
    fn hooks_merge_replaces_only_remnifold_entries_and_is_idempotent() {
        let exe = std::env::temp_dir().join("Remnifold App/bin/remnifold.exe");
        let mut settings = json!({
            "model":"keep",
            "hooks":{
                "SessionStart":[
                    {"hooks":[{"type":"command","command":"bash \"C:/Users/u/.claude/hooks/remnifold.sh\" hook SessionStart"}]},
                    {"hooks":[{"type":"command","command":"other-tool start"}]}
                ],
                "Notification":[{"hooks":[{"type":"command","command":"notify"}]}]
            }
        });
        merge_hooks(&mut settings, &exe, true).unwrap();
        let once = settings.clone();
        merge_hooks(&mut settings, &exe, true).unwrap();
        assert_eq!(settings, once);
        assert_eq!(settings["model"], "keep");
        assert_eq!(
            settings["hooks"]["Notification"][0]["hooks"][0]["command"],
            "notify"
        );
        let start = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0]["hooks"][0]["command"], "other-tool start");
        let expected = format!(
            "\"{}\" hook SessionStart",
            exe.to_string_lossy().replace('\\', "/")
        );
        assert_eq!(start[1]["hooks"][0]["command"], expected);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            "^mcp__remnifold__|^mcp__toolport__toolport_call_tool$"
        );
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["timeout"], 120);
    }

    #[test]
    fn hooks_merge_follows_the_recall_mode_in_both_directions() {
        let exe = std::env::temp_dir().join("Remnifold App/bin/remnifold.exe");
        let mut settings = json!({
            "hooks":{
                "SessionStart":[{"hooks":[{"type":"command","command":"other-tool start"}]}]
            }
        });
        merge_hooks(&mut settings, &exe, true).unwrap();
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
        // off にすると想起用の項目だけが外れ、他社の項目・保存・読み取り許可は残る。
        merge_hooks(&mut settings, &exe, false).unwrap();
        let off = settings.clone();
        merge_hooks(&mut settings, &exe, false).unwrap();
        assert_eq!(settings, off);
        assert_eq!(
            settings["hooks"]["SessionStart"],
            json!([{"hooks":[{"type":"command","command":"other-tool start"}]}])
        );
        assert!(settings["hooks"].get("UserPromptSubmit").is_none());
        assert!(settings["hooks"]["Stop"].is_array());
        assert!(settings["hooks"]["PreToolUse"].is_array());
        // 戻すと再び登録される。
        merge_hooks(&mut settings, &exe, true).unwrap();
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
        assert_eq!(registered_events(false), ["PreToolUse", "Stop"]);
        assert_eq!(registered_events(true).len(), 4);
    }

    #[test]
    fn hooks_merge_refuses_malformed_settings_and_unsafe_paths() {
        let exe = std::env::temp_dir().join("bin/remnifold");
        assert!(merge_hooks(&mut json!([]), &exe, true).is_err());
        assert!(merge_hooks(&mut json!({"hooks":[]}), &exe, true).is_err());
        assert!(merge_hooks(&mut json!({"hooks":{"Stop":{}}}), &exe, false).is_err());
        assert!(hook_command(Path::new("/tmp/a\"b/remnifold"), "Stop").is_err());
        assert!(hook_command(Path::new("/tmp/$HOME/remnifold"), "Stop").is_err());
    }

    #[test]
    fn hooks_removal_keeps_other_entries_and_is_idempotent() {
        let exe = std::env::temp_dir().join("Remnifold App/bin/remnifold.exe");
        let other = std::env::temp_dir().join("Elsewhere/remnifold.exe");
        let mut settings = json!({
            "model":"keep",
            "hooks":{
                "SessionStart":[{"hooks":[{"type":"command","command":"other-tool start"}]}],
                "Notification":[{"hooks":[{"type":"command","command":"notify"}]}]
            }
        });
        merge_hooks(&mut settings, &exe, true).unwrap();
        // 別の実行ファイルを指す指定では何も消えない。
        let mut untouched = settings.clone();
        remove_hooks(&mut untouched, Some(&other)).unwrap();
        assert_eq!(untouched, settings);
        // 自分の項目だけが消え、他社・他の設定は残る。
        remove_hooks(&mut settings, Some(&exe)).unwrap();
        let once = settings.clone();
        remove_hooks(&mut settings, None).unwrap();
        assert_eq!(settings, once);
        assert_eq!(settings["model"], "keep");
        assert_eq!(
            settings["hooks"]["Notification"][0]["hooks"][0]["command"],
            "notify"
        );
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            1
        );
        assert!(settings["hooks"].get("Stop").is_none());
        // Remnifold しか無い設定では hooks ごと消える。
        let mut only = json!({"hooks":{}});
        merge_hooks(&mut only, &exe, true).unwrap();
        remove_hooks(&mut only, None).unwrap();
        assert_eq!(only, json!({}));
        // 形が想定外の設定は変更せず拒否する。
        assert!(remove_hooks(&mut json!({"hooks":{"Stop":{}}}), None).is_err());
        assert!(remove_hooks(&mut json!([]), None).is_err());
    }

    /// macOS の代替の探索先は、利用者単位の場所を Homebrew より先に探す（相対の HOME は使わない）。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_fallback_dirs_prefer_user_locations() {
        let dirs = macos_cli_fallback_dirs(Path::new("/Users/a"));
        assert_eq!(dirs[0], Path::new("/Users/a/.local/bin"));
        assert!(dirs.iter().all(|dir| dir.is_absolute()));
        assert!(
            dirs.iter()
                .position(|d| d == Path::new("/opt/homebrew/bin"))
                .unwrap()
                > dirs
                    .iter()
                    .position(|d| d == Path::new("/Users/a/.claude/local"))
                    .unwrap()
        );
        assert!(
            macos_cli_fallback_dirs(Path::new(""))
                .iter()
                .all(|dir| !dir.starts_with(".local"))
        );
    }

    /// Linux の代替の探索先も、利用者単位の場所を先に探し、相対の HOME は使わない。
    #[test]
    fn linux_fallback_dirs_prefer_user_locations() {
        // どの OS でも絶対パスになる HOME で確かめる（Windows では "/home/a" は絶対パスでない）。
        let home = std::env::temp_dir().join("home-a");
        let dirs = linux_cli_fallback_dirs(&home);
        assert_eq!(dirs[0], home.join(".local/bin"));
        assert!(
            dirs.iter()
                .position(|d| d == Path::new("/usr/local/bin"))
                .unwrap()
                > dirs
                    .iter()
                    .position(|d| d == &home.join(".claude/local"))
                    .unwrap()
        );
        assert!(
            linux_cli_fallback_dirs(Path::new("rel"))
                .iter()
                .all(|dir| !dir.starts_with("rel"))
        );
    }

    #[test]
    fn mcp_registration_points_at_the_fixed_executable() {
        let exe = Path::new("/opt/Remnifold App/bin/remnifold");
        assert_eq!(
            display_args(&mcp_add_args(exe, true)),
            "mcp add --scope user remnifold -- \"/opt/Remnifold App/bin/remnifold\" mcp"
        );
        assert_eq!(
            display_args(&mcp_add_args(exe, false)),
            "mcp add remnifold -- \"/opt/Remnifold App/bin/remnifold\" mcp"
        );
    }

    #[test]
    fn the_registration_mode_defaults_to_auto_and_flags_invalid_values() {
        assert_eq!(McpMode::from_setting(&Value::Null), (McpMode::Auto, true));
        assert_eq!(
            McpMode::from_setting(&json!("direct")),
            (McpMode::Direct, true)
        );
        assert_eq!(McpMode::from_setting(&json!("off")), (McpMode::Off, true));
        assert_eq!(McpMode::from_setting(&json!("Off")), (McpMode::Auto, false));
        assert_eq!(
            McpMode::from_setting(&json!(" off ")),
            (McpMode::Auto, false)
        );
        assert_eq!(McpMode::from_setting(&json!("auto")), (McpMode::Auto, true));
        assert_eq!(
            McpMode::from_setting(&json!("sometimes")),
            (McpMode::Auto, false)
        );
        assert_eq!(McpMode::from_setting(&json!(true)), (McpMode::Auto, false));
    }

    #[test]
    fn a_manual_connect_only_reports_the_duplicate_when_toolport_serves_it() {
        use crate::toolport::Detection;
        let mut result = json!({"tool": "codex"});
        attach_toolport_notice(&mut result, Detection::NotServed);
        attach_toolport_notice(&mut result, Detection::Unknown("x".into()));
        assert!(result.get("toolport").is_none());
        attach_toolport_notice(
            &mut result,
            Detection::Served {
                server_id: "remnifold-1".into(),
                command: "C:/r.exe".into(),
            },
        );
        assert_eq!(
            result["toolport"],
            json!({"serverId": "remnifold-1", "command": "C:/r.exe", "duplicate": true})
        );
    }
}
