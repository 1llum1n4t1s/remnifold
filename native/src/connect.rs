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
//!   壊れた settings.json は書き換えずに止め、初回の書換え前に元の内容を退避する。
//! Codex:
//! - MCP サーバーを公式 CLI（`codex mcp add`）で登録する。フックはプラグイン以外から登録する公式手段が
//!   無いため登録しない（索引の自動想起と保存は Claude Code のフックで行う）。
use crate::common::{atomic_write, home_dir};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// 登録するフック。マッチャー・タイムアウト・非同期・表示文言はパッケージの hooks.json と揃える。
const HOOKS: [(&str, Option<&str>, u64, bool, &str); 4] = [
    (
        "PreToolUse",
        Some("^mcp__remnifold__"),
        5,
        false,
        "Checking memory access...",
    ),
    ("SessionStart", None, 30, false, "Loading memory index..."),
    (
        "UserPromptSubmit",
        None,
        12,
        false,
        "Searching memory index...",
    ),
    ("Stop", None, 120, true, "Saving memory index..."),
];

pub fn run(args: &[String]) -> Result<()> {
    let Some(tool) = args.first().map(String::as_str) else {
        bail!(
            "Usage: remnifold connect <claude-code|codex> [--apply] [--executable <path>] [--cli <path>]"
        );
    };
    if matches!(tool, "--help" | "-h") {
        println!(
            "Usage: remnifold connect <claude-code|codex> [--apply] [--executable <path>] [--cli <path>]\nRegisters the installed Remnifold ({}) with Claude Code (MCP server and hooks) or Codex (MCP server).\nWithout --apply, only the planned registration is shown.",
            crate::paths::app_executable(&crate::paths::app_root()).display()
        );
        return Ok(());
    }
    let mut apply = false;
    let mut executable = None;
    let mut cli = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--apply" => apply = true,
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
    let executable =
        executable.unwrap_or_else(|| crate::paths::app_executable(&crate::paths::app_root()));
    ensure!(
        executable.is_file(),
        "Remnifold is not installed at {}. Run remnifold install --version <version> --apply first.",
        executable.display()
    );
    let result = match tool {
        "claude-code" => claude_code(&executable, cli, apply)?,
        "codex" => codex(&executable, cli, apply)?,
        _ => bail!("Unknown tool. Use claude-code or codex."),
    };
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
fn merge_hooks(settings: &mut Value, executable: &Path) -> Result<()> {
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
    for (event, matcher, timeout, asynchronous, status) in HOOKS {
        let mut entries = match settings["hooks"].get(event) {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => bail!(
                "Claude Code settings has a non-array hooks.{event} value; it was not changed."
            ),
        };
        entries.retain(|entry| !is_remnifold_entry(entry, event));
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

fn claude_code(executable: &Path, cli: Option<PathBuf>, apply: bool) -> Result<Value> {
    let settings_path = claude_settings_path();
    let mut settings = match fs::read(&settings_path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_err(|_| {
            anyhow::anyhow!("Claude Code settings is not valid JSON; it was not changed.")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => bail!("Claude Code settings could not be read."),
    };
    let before = settings.clone();
    merge_hooks(&mut settings, executable)?;
    let mcp_args = mcp_add_args(executable, true);
    let mut result = json!({
        "tool":"claude-code",
        "applied":apply,
        "executable":executable,
        "settings":settings_path,
        "hooks": HOOKS.iter().map(|(event, ..)| event).collect::<Vec<_>>(),
        "hooksChanged": settings != before,
        "mcpCommand": format!("claude {}", display_args(&mcp_args))
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
    result["mcp"] = register_mcp(cli.or_else(|| find_cli("claude")), &mcp_args, true)?;
    Ok(result)
}

fn codex(executable: &Path, cli: Option<PathBuf>, apply: bool) -> Result<Value> {
    let mcp_args = mcp_add_args(executable, false);
    let mut result = json!({
        "tool":"codex",
        "applied":apply,
        "executable":executable,
        "mcpCommand": format!("codex {}", display_args(&mcp_args)),
        "hooks":"not registered: Codex has no supported way to register hooks outside plugins"
    });
    if apply {
        result["mcp"] = register_mcp(cli.or_else(|| find_cli("codex")), &mcp_args, false)?;
    }
    Ok(result)
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
fn register_mcp(cli: Option<PathBuf>, add: &[String], claude: bool) -> Result<Value> {
    let Some(cli) = cli else {
        return Ok(json!({"status":"manual","reason":"CLI not found; run mcpCommand yourself"}));
    };
    let mut remove = Command::new(&cli);
    remove.args(["mcp", "remove", "remnifold"]);
    if claude {
        remove.args(["--scope", "user"]);
    }
    let _ = remove.output();
    let output = Command::new(&cli)
        .args(add)
        .output()
        .map_err(|_| anyhow::anyhow!("The MCP registration command could not be started."))?;
    ensure!(
        output.status.success(),
        "The MCP registration command failed (exit {}).",
        output.status.code().unwrap_or(-1)
    );
    Ok(json!({"status":"registered","cli":cli}))
}

/// PATH から実行ファイルを探す（Windows は .exe だけを対象にし、スクリプトのラッパーは使わない）。
fn find_cli(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        merge_hooks(&mut settings, &exe).unwrap();
        let once = settings.clone();
        merge_hooks(&mut settings, &exe).unwrap();
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
            "^mcp__remnifold__"
        );
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["timeout"], 120);
    }

    #[test]
    fn hooks_merge_refuses_malformed_settings_and_unsafe_paths() {
        let exe = std::env::temp_dir().join("bin/remnifold");
        assert!(merge_hooks(&mut json!([]), &exe).is_err());
        assert!(merge_hooks(&mut json!({"hooks":[]}), &exe).is_err());
        assert!(merge_hooks(&mut json!({"hooks":{"Stop":{}}}), &exe).is_err());
        assert!(hook_command(Path::new("/tmp/a\"b/remnifold"), "Stop").is_err());
        assert!(hook_command(Path::new("/tmp/$HOME/remnifold"), "Stop").is_err());
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
}
