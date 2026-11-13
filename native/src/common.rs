use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::{collections::HashSet, fs, process::Command, sync::OnceLock, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub raw: Value,
    /// Codex の会話記録（sessions）と旧来の設定・状態を読む場所。Remnifold の新しい書込み先ではない。
    pub codex_home: PathBuf,
    /// Remnifold 自身の設定・状態の置き場所（crate::paths を参照）。
    pub roots: crate::paths::Roots,
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                HEX[(byte >> 4) as usize] as char,
                HEX[(byte & 15) as usize] as char,
            ]
        })
        .collect()
}
pub fn sha256(value: impl AsRef<[u8]>) -> String {
    encode_hex(&Sha256::digest(value.as_ref()))
}
pub fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
pub fn truncate_utf16(value: &str, limit: usize) -> String {
    let mut length = 0;
    value
        .chars()
        .take_while(|c| {
            length += c.len_utf16();
            length <= limit
        })
        .collect()
}
pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex"))
}
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

use crate::paths::CONFIG_NAMES;
fn env_first(names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub fn normalize_endpoint(value: &str) -> Result<String> {
    let endpoint =
        url::Url::parse(value).map_err(|_| anyhow::anyhow!("Remnifold endpoint is invalid."))?;
    let host = endpoint
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_lowercase();
    let local = host == "localhost"
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => ip.is_loopback(),
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
            }
        });
    if host.is_empty()
        || !(endpoint.scheme() == "https" || (local && endpoint.scheme() == "http"))
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || host == "supermemory.ai"
        || host.ends_with(".supermemory.ai")
    {
        bail!("Refusing an unsupported Remnifold endpoint.");
    }
    Ok(endpoint.to_string().trim_end_matches('/').to_owned())
}

// 接続設定を読む。`home` は旧来の Codex home（--codex-home の明示指定）で、
// 新しい場所に設定が無いときの移行元と、Codex の会話記録の場所として使う。
pub fn load_config(home: Option<&Path>) -> Result<Config> {
    let home = home.map(Path::to_path_buf).unwrap_or_else(codex_home);
    let roots = crate::paths::Roots::current();
    let env_url = env_first(&[
        "REMNIFOLD_API_URL",
        "KAGAYOI_MEMORY_API_URL",
        "SUPERMEMORY_API_URL",
    ]);
    let env_key = env_first(&[
        "REMNIFOLD_API_KEY",
        "KAGAYOI_MEMORY_API_KEY",
        "SUPERMEMORY_CODEX_API_KEY",
        "CLOUDFLARE_MEMORY_API_KEY",
    ]);
    let mut raw = json!({});
    // 移行元は既定の Codex home だけにする（--codex-home で明示した会話記録の場所からは移さない）。
    // 新旧どちらにも無い場合だけ、明示した場所の旧設定を（コピーせずに）読む。
    let explicit = || {
        CONFIG_NAMES
            .iter()
            .map(|name| home.join(name))
            .find(|path| path.exists())
    };
    if let Some(path) = crate::paths::config_file(&roots, &codex_home())?.or_else(explicit) {
        match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
        {
            Some(value) => raw = value,
            None if !env_url.is_empty() && !env_key.is_empty() => (),
            None => {
                bail!("Remnifold configuration must contain valid JSON. Values are never printed.")
            }
        }
    }
    let base_url = if env_url.is_empty() {
        text(&raw["baseUrl"]).trim().to_owned()
    } else {
        env_url
    };
    let api_key = if env_key.is_empty() {
        text(&raw["apiKey"]).trim().to_owned()
    } else {
        env_key
    };
    if base_url.is_empty() || api_key.is_empty() {
        bail!(
            "Remnifold configuration requires baseUrl and apiKey (file values may be overridden by REMNIFOLD_API_URL and REMNIFOLD_API_KEY). Values are never printed."
        );
    }
    let base_url = normalize_endpoint(&base_url)?;
    raw["baseUrl"] = json!(base_url);
    raw["apiKey"] = json!(api_key);
    Ok(Config {
        base_url,
        api_key,
        raw,
        codex_home: home,
        roots,
    })
}

pub fn read_settings(config: &Config) -> Value {
    // 接続ファイルが壊れ環境変数で接続できる場合も、設定は旧ファイルから回復する。
    // 新しい場所を先に、旧来の Codex home を後に見る。
    let raw = CONFIG_NAMES
        .iter()
        .flat_map(|name| [config.roots.config.join(name), config.codex_home.join(name)])
        .find_map(|path| {
            fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .filter(Value::is_object)
        })
        .unwrap_or_else(|| config.raw.clone());
    fn number(v: &Value, default: f64) -> f64 {
        let n = v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(default);
        if !n.is_finite() || n == 0.0 {
            default
        } else {
            n
        }
    }
    let memories = number(&raw["maxMemories"], 3.0).clamp(1.0, 5.0);
    let memories = if memories.fract() == 0.0 {
        json!(memories as u64)
    } else {
        json!(memories)
    };
    json!({
        "codexHome": config.codex_home.to_string_lossy(),
        "configHome": config.roots.config.to_string_lossy(),
        "recallMode": raw.get("recallMode").filter(|v| !v.is_null() && *v != "" && *v != false).cloned().unwrap_or(json!("direct")),
        "maxMemories": memories,
        "maxRecentTasks": raw["maxRecentTasks"].as_f64().filter(|n| n.is_finite() && n.fract()==0.0).map(|n| n.clamp(0.0,3.0) as i64).unwrap_or(2),
        "minimumSimilarity": number(&raw["similarityThreshold"], 0.70).clamp(0.65,0.95),
        "projectContainerTag": raw["projectContainerTag"], "userContainerTag": raw["userContainerTag"],
        "readContainerTags": raw.get("readContainerTags").filter(|v| v.is_array()).cloned().unwrap_or(json!([])),
        "sharedContainerTags": raw.get("sharedContainerTags").filter(|v| v.is_array()).cloned().unwrap_or(json!([]))
    })
}

pub fn git(args: &[&str], cwd: &Path) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub fn absolute_path(path: &Path) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for part in full.components() {
        match part {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn normalize_remote(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let mut normalized = if raw.contains("://") {
        url::Url::parse(raw)
            .map(|u| {
                format!(
                    "{}{}/{}",
                    u.host_str().unwrap_or("").to_lowercase(),
                    u.port().map(|p| format!(":{p}")).unwrap_or_default(),
                    u.path().trim_start_matches('/')
                )
            })
            .unwrap_or_else(|_| raw.to_owned())
    } else if let Some((host, path)) = raw.split_once(':') {
        format!(
            "{}/{}",
            host.rsplit('@').next().unwrap_or(host).to_lowercase(),
            path
        )
    } else {
        format!(
            "file:{}",
            absolute_path(Path::new(raw)).unwrap_or_default().display()
        )
    };
    normalized.truncate(normalized.find(['?', '#']).unwrap_or(normalized.len()));
    normalized = normalized.trim_end_matches('/').to_owned();
    if normalized.to_lowercase().ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    normalized.to_lowercase()
}

// 作業ディレクトリが属するリポジトリの最上位。Git 管理外なら作業ディレクトリ自身の絶対パス。
fn project_root(cwd: &Path) -> Result<PathBuf> {
    let git_root = git(&["rev-parse", "--show-toplevel"], cwd);
    Ok(if git_root.is_empty() {
        absolute_path(cwd)?
    } else {
        PathBuf::from(git_root)
    })
}

pub fn get_project_context(cwd: &Path) -> Result<Value> {
    Ok(project_context_at(&project_root(cwd)?))
}

// 解決済みの最上位からプロジェクト名と containerTag を求める。
// 読み取り文脈でも同じ最上位を使うため、git の子プロセスを重ねて起動しないよう分けている。
fn project_context_at(root: &Path) -> Value {
    let remote = normalize_remote(&git(&["remote", "get-url", "origin"], root));
    let name_source = if remote.is_empty() {
        root.to_string_lossy().into_owned()
    } else {
        remote.clone()
    };
    let basename = name_source.rsplit(['/', '\\']).next().unwrap_or("unknown");
    let without_suffix = if basename.to_lowercase().ends_with(".git") {
        &basename[..basename.len() - 4]
    } else {
        basename
    };
    let repo_name = if without_suffix.is_empty() {
        root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    } else {
        without_suffix
    };
    let project = regex::Regex::new("[^a-z0-9]+")
        .unwrap()
        .replace_all(&repo_name.to_lowercase(), "_")
        .trim_matches('_')
        .chars()
        .take(72)
        .collect::<String>();
    let project = if project.is_empty() {
        "unknown"
    } else {
        &project
    };
    let renamed = matches!(
        remote.as_str(),
        "github.com/1llum1n4t1s/remnifold" | "github.com/1llum1n4t1s/kagayoi-memory"
    );
    let identity = if renamed {
        "github.com/1llum1n4t1s/cloudflare-supermemory".to_owned()
    } else if remote.is_empty() {
        format!("path:{}", root.display())
    } else {
        remote.clone()
    };
    let provenance = if renamed {
        "cloudflare_supermemory"
    } else {
        project
    };
    let display = if remote == "github.com/1llum1n4t1s/remnifold" {
        "kagayoi_memory"
    } else {
        project
    };
    json!({"projectName":display,"containerTag":format!("repo_{provenance}__{}",&sha256(identity)[..16])})
}

pub fn get_read_context(cwd: &Path, settings: &Value) -> Result<Value> {
    let root = project_root(cwd)?;
    let mut project = project_context_at(&root);
    let hash = &sha256(root.to_string_lossy().as_bytes())[..16];
    let mut identity = git(&["config", "user.email"], &root);
    if identity.is_empty() {
        identity = env_first(&["USER", "USERNAME"]);
    }
    if identity.is_empty() {
        identity = env_first(&["HOSTNAME", "COMPUTERNAME"]);
    }
    if identity.is_empty() {
        identity = Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
    }
    let user_hash = &sha256(identity)[..16];
    let name = text(&project["projectName"]);
    let mut tags = vec![
        text(&project["containerTag"]).to_owned(),
        text(&settings["projectContainerTag"]).to_owned(),
        format!("user_project_{hash}"),
        format!("claudecode_project_{hash}"),
        format!("repo_{name}"),
        format!("codex_project_{hash}"),
        format!("opencode_project_{hash}"),
        format!("cursor_project_{hash}"),
    ];
    tags.extend(
        settings["readContainerTags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    let unique = |values: Vec<String>| {
        let mut seen = HashSet::new();
        values
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && seen.insert(s.clone()))
            .collect::<Vec<_>>()
    };
    let tags = unique(tags);
    let mut shared = vec![
        text(&settings["userContainerTag"]).to_owned(),
        format!("codex_user_{user_hash}"),
        format!("opencode_user_{user_hash}"),
        format!("cursor_user_{user_hash}"),
    ];
    shared.extend(
        settings["sharedContainerTags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    let shared: Vec<_> = unique(shared)
        .into_iter()
        .filter(|tag| !tags.contains(tag))
        .collect();
    let all = tags
        .iter()
        .chain(shared.iter())
        .cloned()
        .collect::<Vec<_>>();
    project["projectTags"] = json!(tags);
    project["sharedTags"] = json!(shared);
    project["readTags"] = json!(all);
    Ok(project)
}

#[derive(Debug)]
pub struct ApiError {
    pub status: Option<u16>,
    pub unsupported_global_search: bool,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "Remnifold API request failed (HTTP {status})"),
            None => write!(f, "Remnifold API request failed"),
        }
    }
}
impl std::error::Error for ApiError {}

pub fn api(
    config: &Config,
    path: &str,
    method: &str,
    body: Option<&Value>,
    timeout_ms: u64,
) -> Result<Value> {
    api_with_source(
        config,
        path,
        method,
        body,
        timeout_ms,
        "codex-kagayoi-memory",
    )
}
pub fn api_with_source(
    config: &Config,
    path: &str,
    method: &str,
    body: Option<&Value>,
    timeout_ms: u64,
    source: &str,
) -> Result<Value> {
    api_with_limit(config, path, method, body, timeout_ms, source, None)
}
pub fn api_bounded(config: &Config, path: &str, body: &Value, maximum_bytes: u64) -> Result<Value> {
    api_with_limit(
        config,
        path,
        "POST",
        Some(body),
        30_000,
        "codex-kagayoi-memory",
        Some(maximum_bytes),
    )
}
fn api_with_limit(
    config: &Config,
    path: &str,
    method: &str,
    body: Option<&Value>,
    timeout_ms: u64,
    source: &str,
    maximum_bytes: Option<u64>,
) -> Result<Value> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client initialization")
    });
    if !path.starts_with('/') {
        bail!("Invalid memory API path.");
    }
    let url = format!("{}{path}", config.base_url);
    let method =
        reqwest::Method::from_bytes(method.as_bytes()).context("Invalid memory API method")?;
    let mut request = client
        .request(method, url)
        .bearer_auth(&config.api_key)
        .header("Content-Type", "application/json")
        .header("x-sm-source", source)
        .timeout(Duration::from_millis(timeout_ms.max(1)));
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().map_err(|_| ApiError {
        status: None,
        unsupported_global_search: false,
    })?;
    let status = response.status();
    if let Some(maximum) = maximum_bytes {
        use std::io::Read;
        if !status.is_success() {
            return Err(ApiError {
                status: Some(status.as_u16()),
                unsupported_global_search: false,
            }
            .into());
        }
        let mut bytes = Vec::new();
        response
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Memory API response could not be read."))?;
        if bytes.len() as u64 > maximum {
            bail!("Memory API response exceeds the size limit.");
        }
        return serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Memory API returned invalid JSON."));
    }
    if !status.is_success() {
        let detail = response.json::<Value>().unwrap_or(Value::Null);
        let detail = detail
            .pointer("/error/message")
            .or_else(|| detail.get("error"))
            .or_else(|| detail.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let unsupported = status.as_u16()==400 && regex::Regex::new("(?i)container.?tag.*required|all.?containers.*(?:unsupported|invalid|unknown)|unsupported.*all.?containers").unwrap().is_match(detail);
        return Err(ApiError {
            status: Some(status.as_u16()),
            unsupported_global_search: unsupported,
        }
        .into());
    }
    if status.as_u16() == 204 {
        Ok(Value::Null)
    } else {
        response
            .json::<Value>()
            .map_err(|_| anyhow::anyhow!("Memory API returned invalid JSON."))
    }
}

/// 同一ディレクトリの一時ファイルを置き換え、接続・既読JSONの形式を保つ。
pub fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Missing state directory")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".remnifold-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            options.mode(0o600);
        }
    }
    let _ = private;
    let mut output = options.open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_and_utf16() {
        for value in [
            "http://localhost:4312/",
            "http://127.2.3.4:4312",
            "http://[::1]:4312",
            "https://memory.example/api",
        ] {
            assert!(normalize_endpoint(value).is_ok(), "{value}");
        }
        for value in [
            "http://example.com",
            "https://a:secret@example.com",
            "https://example.com/?key=secret",
            "https://supermemory.ai",
            "https://x.supermemory.ai",
            "file:///test",
            "https://example.com/#",
        ] {
            assert!(normalize_endpoint(value).is_err(), "{value}");
        }
        assert_eq!(truncate_utf16("a🦀b", 2), "a");
        assert_eq!(truncate_utf16("a🦀b", 3), "a🦀");
    }
    #[test]
    fn remotes_preserve_legacy_identity() {
        assert_eq!(
            normalize_remote("git@github.com:1llum1n4t1s/Remnifold.git"),
            "github.com/1llum1n4t1s/remnifold"
        );
        assert_eq!(
            normalize_remote(
                "https://user:password@GitHub.com/1llum1n4t1s/Remnifold.git?token=hidden"
            ),
            "github.com/1llum1n4t1s/remnifold"
        );
    }
    #[test]
    fn untracked_dot_git_folder_preserves_project_provenance() {
        let parent =
            std::env::temp_dir().join(format!("remnifold-context-{}", uuid::Uuid::new_v4()));
        let folder = parent.join("project.GIT");
        fs::create_dir_all(&folder).unwrap();
        let context = get_project_context(&folder).unwrap();
        assert_eq!(context["projectName"], "project");
        let expected = format!(
            "repo_project__{}",
            &sha256(format!("path:{}", folder.display()))[..16]
        );
        assert_eq!(context["containerTag"], expected);
        if std::env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            fs::remove_dir_all(parent).unwrap();
        }
    }
    #[test]
    fn legacy_settings_preserve_integral_json_numbers_and_recall_limits() {
        let home =
            std::env::temp_dir().join(format!("remnifold-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("remnifold.json"), b"unfinished").unwrap();
        fs::write(
            home.join("supermemory.json"),
            br#"{"maxMemories":4.0,"maxRecentTasks":0.0,"recallMode":"off"}"#,
        )
        .unwrap();
        let config = Config {
            base_url: String::new(),
            api_key: String::new(),
            raw: json!({}),
            codex_home: home.clone(),
            roots: crate::paths::Roots {
                config: home.join("unused-new-root"),
                state: home.join("unused-new-root"),
            },
        };
        let settings = read_settings(&config);
        assert_eq!(settings["maxMemories"].as_u64(), Some(4));
        assert_eq!(settings["maxRecentTasks"].as_u64(), Some(0));
        assert_eq!(settings["recallMode"], "off");
        if std::env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            fs::remove_dir_all(home).unwrap();
        }
    }
}
