use anyhow::{Result, bail};
use remnifold::{
    capture, common, connect, hooks, live_test, local, mcp, organization, provision, topics_setup,
    updater,
};
use serde_json::Value;
use std::io::{self, Read};

fn input() -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Input is too large.");
    }
    let value = serde_json::from_slice::<Value>(&bytes)?;
    if !value.is_object() {
        bail!("Input must be an object.");
    }
    Ok(value)
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str).unwrap_or("mcp") {
        "--version" | "-V" => println!("remnifold {}", env!("CARGO_PKG_VERSION")),
        "--help" | "-h" | "help" => println!(
            "Remnifold {}\n\nUsage: remnifold <command>\n  mcp                 Run the stdio MCP server (default)\n  hook <event>        Run a Claude Code or Codex lifecycle hook\n  capture             Capture completed turns from stdin JSON\n  import [options]    Import Codex history (dry run unless --apply)\n  setup [options]     Connect an existing self-hosted server\n  server-setup ...    Provision and deploy a Cloudflare server\n  topics-setup ...    Plan or queue finite unclassified enrichment\n  server-live-test    Explicitly verify configured remote server\n  status              Check connection and recall settings\n  install ...         Verify, install or update the standalone app\n  connect <tool> ...  Register the installed app with claude-code or codex\n\nNormal client use requires no Node.js runtime.",
            env!("CARGO_PKG_VERSION")
        ),
        "mcp" => mcp::start()?,
        "hook" => {
            let Some(event) = args.get(1) else {
                bail!("A hook event is required.")
            };
            let Ok(payload) = input() else { return Ok(()) };
            match hooks::run_hook(event, &payload, None) {
                Ok(value) => {
                    if value.as_object().is_some_and(|map| !map.is_empty()) {
                        print!("{}", serde_json::to_string(&value)?);
                    }
                }
                Err(error) if event == "Stop" => {
                    eprint!("{}", capture::failure_message(&error));
                    std::process::exit(1);
                }
                Err(_) => bail!("Remnifold hook failed."),
            }
        }
        "capture" => {
            let result = (|| {
                let payload =
                    input().map_err(|_| anyhow::Error::new(capture::CaptureFailure("payload")))?;
                let config = common::load_config(None)
                    .map_err(|_| anyhow::Error::new(capture::CaptureFailure("configuration")))?;
                capture::capture(&payload, &config)
            })();
            match result {
                Ok(result) => {
                    if result["saved"].as_u64().unwrap_or(0) > 0
                        || result["pending"].as_u64().unwrap_or(0) > 0
                    {
                        print!(
                            "{}",
                            serde_json::json!({"systemMessage":format!("Remnifold: saved {} documents; pending {}.",result["saved"],result["pending"])})
                        );
                    }
                }
                Err(error) => {
                    eprint!("{}", capture::failure_message(&error));
                    std::process::exit(1);
                }
            }
        }
        "import" => capture::run_import(&args[1..])?,
        "setup" => local::setup(&args[1..])?,
        "server-setup" => provision::run(&args[1..])?,
        "topics-setup" => topics_setup::run(&args[1..])?,
        "memory-organize" => organization::run(&args[1..])?,
        "server-live-test" => live_test::run(&args[1..])?,
        "status" => local::status()?,
        "install" => updater::run_install(&args[1..])?,
        "connect" => connect::run(&args[1..])?,
        // Codex プラグインとしての導入・更新は廃止した。固定の失敗コードで install を案内する。
        "plugin-update" | "plugin-bootstrap" => updater::retired(&args[0])?,
        _ => bail!("Unknown command. Use remnifold --help."),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
