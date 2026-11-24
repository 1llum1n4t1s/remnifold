use anyhow::{Result, bail};
use remnifold::{
    app_update, capture, common, connect, hooks, live_test, local, mcp, organization, provision,
    topics_setup, updater,
};
use serde_json::Value;
use std::io::{self, IsTerminal, Read};

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
fn run(startup: app_update::Startup) -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command = match args.first().map(String::as_str) {
        Some(command) => command,
        // 引数なしの既定は stdio の MCP だが、人が直接起動した場合（Setup 直後の初回起動、スタートメニュー）は
        // stdin を待っても何も起きない。MCP クライアントは stdin をパイプで渡すため、Setup（Velopack）で
        // 導入されたビルドが端末から起動されたときだけ案内に切り替える。開発ビルドや従来の install 版の
        // 素の起動は、従来どおり MCP として動く。
        None => {
            // macOS・Linux の Setup 版には Velopack のインストールフックが無い。.pkg は導入後に引数なしで
            // 1回起動し、AppImage は利用者が引数なしで起動するので、ここで Claude Code・Codex とログイン時の
            // 適用を登録する。Windows では、インストールフックの3秒の打ち切りで終わらなかった登録をここでも
            // やり直す（登録済みなら状態ファイルを読むだけで終わる。管理外のビルドでは何もしない）。
            app_update::ensure_registered();
            if startup.first_run || (app_update::is_managed() && io::stdin().is_terminal()) {
                return app_update::welcome(startup.first_run);
            }
            "mcp"
        }
    };
    match command {
        "--version" | "-V" => println!("remnifold {}", env!("CARGO_PKG_VERSION")),
        "--help" | "-h" | "help" => println!(
            "{}",
            remnifold::locale::tr_with("cli.help", &[("version", env!("CARGO_PKG_VERSION"))])
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
                    if result["pending"].as_u64().unwrap_or(0) > 0 {
                        print!(
                            "{}",
                            serde_json::json!({"systemMessage":remnifold::locale::tr_with("hook.saved_notice", &[("saved", &result["saved"].to_string()), ("pending", &result["pending"].to_string())])})
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
        // ログオン時に起動される。保留中の更新を、導入先に他のプロセスが無いときだけ適用する。
        // 適用しなかった理由（実行中のプロセスなど）は終了コード 0 の通常の結果として返し、
        // 署名者の検証に通らなかった場合と実行の失敗だけを非 0 にする。
        "update-apply" => {
            let outcome = app_update::update_apply()?;
            println!("{}", outcome.to_json());
            if matches!(outcome, app_update::ApplyOutcome::Untrusted(_)) {
                std::process::exit(2);
            }
        }
        // Setup 版の解除。macOS・Linux には Velopack のアンインストーラーが無いため、.app をゴミ箱へ入れる
        // （AppImage を消す）前に利用者が実行する（--apply が無ければ予定だけを表示する）。
        "uninstall-setup" => println!(
            "{}",
            serde_json::to_string_pretty(&app_update::uninstall_setup(&args[1..])?)?
        ),
        // Codex プラグインとしての導入・更新は廃止した。固定の失敗コードで install を案内する。
        "plugin-update" | "plugin-bootstrap" => updater::retired(&args[0])?,
        _ => bail!("{}", remnifold::locale::tr("cli.unknown_command")),
    }
    Ok(())
}
fn main() {
    // Velopack の処理（フック引数・初回起動の判定）は、引数の解析より前に全サブコマンド共通で行う。
    // 起動時の自動適用は切ってある（適用は update-apply だけ）。
    let startup = app_update::startup();
    if let Err(error) = run(startup) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
