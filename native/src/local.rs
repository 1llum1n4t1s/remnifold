//! 接続の設定と診断。Cloudflareリソースの作成はサーバー側のセットアップに任せる。
use crate::{
    common::{self, Config, text},
    search,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
};

pub fn setup(args: &[String]) -> Result<()> {
    let mut base_url = String::new();
    let mut home = common::codex_home();
    let mut force = false;
    let mut stdin_key = false;
    let mut max_memories = None;
    let mut max_recent = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--force" => force = true,
            "--api-key-stdin" => stdin_key = true,
            "--base-url" | "--codex-home" | "--max-memories" | "--max-recent-tasks" => {
                let option = &args[index];
                index += 1;
                let value = args
                    .get(index)
                    .filter(|s| !s.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("{option} requires a value."))?;
                match option.as_str() {
                    "--base-url" => base_url = value.clone(),
                    "--codex-home" => home = common::absolute_path(&PathBuf::from(value))?,
                    "--max-memories" => {
                        let n: u64 = value.parse().map_err(|_| {
                            anyhow::anyhow!("max-memories must be from 1 through 5.")
                        })?;
                        if !(1..=5).contains(&n) {
                            bail!("max-memories must be from 1 through 5.");
                        }
                        max_memories = Some(n);
                    }
                    _ => {
                        let n: u64 = value.parse().map_err(|_| {
                            anyhow::anyhow!("max-recent-tasks must be from 0 through 3.")
                        })?;
                        if n > 3 {
                            bail!("max-recent-tasks must be from 0 through 3.");
                        }
                        max_recent = Some(n);
                    }
                }
            }
            "--help" | "-h" => {
                println!(
                    "remnifold setup --base-url <HTTPS endpoint> [--codex-home <directory>] [--force] [--api-key-stdin] [--max-memories 1..5] [--max-recent-tasks 0..3]\nThe API key is read from the existing connection, environment, or a hidden prompt."
                );
                return Ok(());
            }
            _ => bail!("Unknown setup option. Use remnifold setup --help."),
        }
        index += 1;
    }
    if base_url.is_empty() {
        base_url = std::env::var("REMNIFOLD_API_URL").unwrap_or_default();
    }
    let base_url = common::normalize_endpoint(base_url.trim())?;
    let path = home.join("remnifold.json");
    if path.exists() && !force {
        bail!("Configuration already exists. Use --force to replace it.");
    }
    let mut raw = json!({});
    for name in ["remnifold.json", "kagayoi-memory.json", "supermemory.json"] {
        let file = home.join(name);
        if !file.exists() {
            continue;
        }
        raw = serde_json::from_slice::<Value>(
            &fs::read(file)
                .map_err(|_| anyhow::anyhow!("Existing configuration could not be read."))?,
        )
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| anyhow::anyhow!("Existing configuration must contain valid JSON."))?;
        break;
    }
    let mut key = String::new();
    if stdin_key {
        io::stdin()
            .take(64 * 1024 + 1)
            .read_to_string(&mut key)
            .map_err(|_| anyhow::anyhow!("API key could not be read."))?;
        if key.len() > 64 * 1024 {
            bail!("API key input is too large.");
        }
        key = key.trim().to_owned();
    } else {
        for name in [
            "REMNIFOLD_API_KEY",
            "KAGAYOI_MEMORY_API_KEY",
            "SUPERMEMORY_CODEX_API_KEY",
            "CLOUDFLARE_MEMORY_API_KEY",
        ] {
            if let Ok(value) = std::env::var(name) {
                if !value.trim().is_empty() {
                    key = value.trim().to_owned();
                    break;
                }
            }
        }
        if key.is_empty()
            && common::normalize_endpoint(text(&raw["baseUrl"]))
                .ok()
                .as_deref()
                == Some(base_url.as_str())
        {
            key = text(&raw["apiKey"]).trim().to_owned();
        }
        if key.is_empty() {
            key = rpassword::prompt_password("Memory API key: ")
                .map_err(|_| {
                    anyhow::anyhow!(
                        "API key could not be read. Use --api-key-stdin for noninteractive setup."
                    )
                })?
                .trim()
                .to_owned();
        }
    }
    if key.is_empty() {
        bail!("An API key is required.");
    }
    let config = Config {
        base_url: base_url.clone(),
        api_key: key.clone(),
        raw: raw.clone(),
        codex_home: home,
    };
    let session = common::api(&config, "/v3/session", "GET", None, 8_000)
        .map_err(|_| anyhow::anyhow!("The authenticated memory session could not be verified."))?;
    if text(&session["user"]["id"]).trim().is_empty() {
        bail!("The server returned an invalid session response.");
    }
    raw["baseUrl"] = json!(base_url);
    raw["apiKey"] = json!(key);
    if raw.get("recallMode").is_none() {
        raw["recallMode"] = json!("direct");
    }
    if raw.get("maxMemories").is_none() {
        raw["maxMemories"] = json!(3);
    }
    if raw.get("similarityThreshold").is_none() {
        raw["similarityThreshold"] = json!(0.7);
    }
    if let Some(n) = max_memories {
        raw["maxMemories"] = json!(n);
    }
    if let Some(n) = max_recent {
        raw["maxRecentTasks"] = json!(n);
    }
    common::atomic_write(&path, &serde_json::to_vec_pretty(&raw)?, true)
        .map_err(|_| anyhow::anyhow!("Connection configuration could not be saved."))?;
    println!("Connected and wrote {}.", path.display());
    Ok(())
}

pub fn status() -> Result<()> {
    let config = common::load_config(None)?;
    let settings = common::read_settings(&config);
    let context = common::get_read_context(&std::env::current_dir()?, &settings)?;
    let connected = common::api(&config, "/v3/session", "GET", None, 8_000).is_ok();
    let discovery = if connected {
        search::discover_search_containers(&config).ok()
    } else {
        None
    };
    println!(
        "Remnifold status\nAPI reachability: {}\nStorage: configured Cloudflare D1\nDefault storage container: memories\nWorkspace provenance tag: {}",
        if connected {
            "connected"
        } else {
            "unavailable"
        },
        text(&context["containerTag"])
    );
    if let Some(discovery) = discovery {
        println!(
            "Topic search scope: {} nonempty spaces discovered by the API{}",
            discovery["tags"].as_array().map_or(0, Vec::len),
            if discovery["complete"] == true {
                ""
            } else {
                " (incomplete at the 100-space API limit)"
            }
        );
    } else {
        println!(
            "Topic search scope: unavailable; global topic search cannot claim complete coverage"
        );
    }
    println!(
        "Auto-recall: {}; document index only\nSession start: no index is injected until a topic query is available\nDetails: getDocument(documentId)\nCodex built-in memory files: not read by this integration\nMCP reachability: check whoAmI separately",
        text(&settings["recallMode"])
    );
    Ok(())
}
