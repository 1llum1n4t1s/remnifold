//! Local plugin updates. Downloaded content is never executed before publication verification.
use anyhow::{Result, bail, ensure};
use base64::Engine;
use flate2::read::GzDecoder;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Files = BTreeMap<String, Vec<u8>>;
const LIMIT: u64 = 128 * 1024 * 1024;
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn blob(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()));
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn target() -> Result<&'static str> {
    Ok(match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => bail!("UNSUPPORTED_NATIVE_TARGET"),
    })
}
fn executable(target: &str) -> &'static str {
    if target.contains("windows") {
        "bin/remnifold.exe"
    } else {
        "bin/remnifold"
    }
}
fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        env::var_os("USERPROFILE")
            .or_else(|| env::var_os("HOME"))
            .ok_or_else(|| anyhow::anyhow!("HOME_MISSING"))?,
    ))
}
fn absolute(value: &str) -> Result<PathBuf> {
    let expanded = if let Some(suffix) = value.strip_prefix("~/") {
        home()?.join(suffix)
    } else {
        PathBuf::from(value)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()?.join(expanded)
    };
    let mut normal = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                ensure!(normal.pop(), "INVALID_PATH");
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    Ok(normal)
}
fn no_links(path: &Path, missing: bool) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(info) => {
                ensure!(!info.file_type().is_symlink(), "LINK_PATH_REJECTED");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(info.file_attributes() & 0x400 == 0, "LINK_PATH_REJECTED");
                }
            }
            Err(error) if missing && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().any(|b| b < 32 || b"\\:<>\"|?*".contains(&b))
        && name.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' ']) && {
                let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
                !["con", "prn", "aux", "nul"].contains(&stem.as_str())
                    && !(stem.len() == 4
                        && (stem.starts_with("com") || stem.starts_with("lpt"))
                        && (b'1'..=b'9').contains(&stem.as_bytes()[3]))
            }
        })
}
fn native_source(name: &str) -> bool {
    if name.contains(".test.")
        || name
            .split('/')
            .any(|part| ["node_modules", "test", "tests", "__tests__"].contains(&part))
    {
        return false;
    }
    matches!(
        name,
        ".codex-plugin/plugin.json"
            | "README.md"
            | "CHANGELOG.md"
            | "LICENSE"
            | "LICENSE.md"
            | "scripts/setup-server.mjs"
            | "scripts/setup-server.ps1"
            | "client/unicode-text.mjs"
            | "server/package.json"
            | "server/pnpm-lock.yaml"
            | "server/pnpm-workspace.yaml"
            | "server/tsconfig.json"
            | "server/wrangler.example.jsonc"
    ) || (name.starts_with("skills/")
        && [".md", ".yaml", ".yml"]
            .iter()
            .any(|suffix| name.ends_with(suffix)))
        || (name.starts_with("server/src/") && name.ends_with(".ts"))
        || (name.starts_with("server/migrations/") && name.ends_with(".sql"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(name: &str, kind: u8, trailing: bool) -> Vec<u8> {
        let mut header = tar::Header::new_ustar();
        header.set_path(name).unwrap();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::new(kind));
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend([0; 1024]);
        if trailing {
            bytes.push(1);
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&bytes).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn rejects_unsafe_archives_before_extracting() {
        assert!(parse_tar(&archive("remnifold/README.md", b'0', false)).is_ok());
        assert!(parse_tar(&archive("other/README.md", b'0', false)).is_err());
        assert!(parse_tar(&archive("remnifold/README.md", b'2', false)).is_err());
        assert!(parse_tar(&archive("remnifold/README.md", b'x', false)).is_err());
        assert!(parse_tar(&archive("remnifold/README.md", b'0', true)).is_err());
        for name in [
            "../README.md",
            "remnifold/../README.md",
            "remnifold/con.txt",
            "remnifold/a\\b",
            "remnifold/a.",
        ] {
            assert!(!safe_name(name), "{name}");
        }
    }
}
fn legacy_source(name: &str) -> bool {
    if name
        .split('/')
        .any(|part| ["node_modules", "test", "tests", "__tests__"].contains(&part))
        || name.contains(".test.")
    {
        return false;
    }
    native_source(name)
        || matches!(
            name,
            ".mcp.json"
                | "hooks/hooks.json"
                | "scripts/hook-launcher.mjs"
                | "scripts/mcp-launcher.mjs"
                | "scripts/setup-client.ps1"
        )
        || (name.starts_with("client/") && (name.ends_with(".mjs") || name.ends_with(".js")))
        || (name.starts_with("scripts/setup-")
            && (name.ends_with(".mjs") || name.ends_with(".ps1")))
}
fn allowed(name: &str) -> bool {
    legacy_source(name)
        || matches!(
            name,
            "native-build.json" | "bin/remnifold" | "bin/remnifold.exe"
        )
}
fn files_at(root: &Path) -> Result<Files> {
    no_links(root, false)?;
    fn walk(root: &Path, here: &Path, result: &mut Files) -> Result<()> {
        for item in fs::read_dir(here)? {
            let item = item?;
            let path = item.path();
            no_links(&path, false)?;
            let info = fs::metadata(&path)?;
            let name = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            ensure!(safe_name(&name), "UNKNOWN_LOCAL_FILE");
            if info.is_dir() {
                ensure!(item.file_name() != ".git", "SOURCE_MUST_NOT_BE_GIT");
                walk(root, &path, result)?;
            } else {
                ensure!(
                    info.is_file() && allowed(&name) && info.len() <= LIMIT,
                    "UNKNOWN_LOCAL_FILE"
                );
                result.insert(name, fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut result = Files::new();
    walk(root, root, &mut result)?;
    Ok(result)
}
fn hashes(files: &Files) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(name, bytes)| (name.clone(), sha(bytes)))
        .collect()
}
fn parse_tar(bytes: &[u8]) -> Result<Files> {
    let mut expanded = Vec::new();
    GzDecoder::new(bytes)
        .take(LIMIT + 1)
        .read_to_end(&mut expanded)
        .map_err(|_| anyhow::anyhow!("INVALID_ARCHIVE"))?;
    ensure!(expanded.len() as u64 <= LIMIT, "INVALID_ARCHIVE");
    let mut offset = 0usize;
    let mut result = Files::new();
    let mut names = BTreeSet::new();
    while offset + 512 <= expanded.len() {
        let header = &expanded[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            ensure!(
                expanded[offset..].iter().all(|byte| *byte == 0) && !result.is_empty(),
                "UNSAFE_ARCHIVE"
            );
            return Ok(result);
        }
        let text = |start: usize, length: usize| -> Result<&str> {
            let data = &header[start..start + length];
            let end = data
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(data.len());
            std::str::from_utf8(&data[..end]).map_err(|_| anyhow::anyhow!("INVALID_ARCHIVE"))
        };
        let octal = |start: usize, length: usize| -> Result<usize> {
            let value = text(start, length)?.trim();
            ensure!(
                !value.is_empty() && value.bytes().all(|byte| (b'0'..=b'7').contains(&byte)),
                "INVALID_ARCHIVE"
            );
            usize::from_str_radix(value, 8).map_err(|_| anyhow::anyhow!("INVALID_ARCHIVE"))
        };
        let checksum = header
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    32usize
                } else {
                    *byte as usize
                }
            })
            .sum::<usize>();
        ensure!(octal(148, 8)? == checksum, "INVALID_ARCHIVE");
        let prefix = text(345, 155)?;
        let leaf = text(0, 100)?;
        let full = if prefix.is_empty() {
            leaf.to_string()
        } else {
            format!("{prefix}/{leaf}")
        };
        let name = full.strip_suffix('/').unwrap_or(&full);
        let size = octal(124, 12)?;
        let kind = header[156];
        ensure!(
            size as u64 <= LIMIT
                && offset
                    .checked_add(512)
                    .and_then(|offset| offset.checked_add(size))
                    .is_some_and(|end| end <= expanded.len()),
            "UNSAFE_ARCHIVE"
        );
        ensure!(
            name == "remnifold" || name.starts_with("remnifold/"),
            "UNSAFE_ARCHIVE"
        );
        ensure!(
            safe_name(name) && names.insert(name.to_lowercase()),
            "UNSAFE_ARCHIVE"
        );
        ensure!([0, b'0', b'5'].contains(&kind), "UNSAFE_ARCHIVE");
        if kind == b'5' {
            ensure!(size == 0, "UNSAFE_ARCHIVE");
        } else {
            let relative = name
                .strip_prefix("remnifold/")
                .ok_or_else(|| anyhow::anyhow!("UNSAFE_ARCHIVE"))?;
            ensure!(allowed(relative), "UNSAFE_ARCHIVE");
            result.insert(
                relative.into(),
                expanded[offset + 512..offset + 512 + size].to_vec(),
            );
        }
        offset += 512 + size.div_ceil(512) * 512;
    }
    bail!("INVALID_ARCHIVE")
}
fn read_zip(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(zip.len() == 1, "UNEXPECTED_ARTIFACT_CONTENTS");
    let entry = zip.by_index(0)?;
    ensure!(
        entry.name() == name
            && !entry.is_dir()
            && entry.size() <= LIMIT
            && entry
                .unix_mode()
                .is_none_or(|mode| mode & 0o170000 != 0o120000),
        "UNSAFE_ARCHIVE"
    );
    let mut data = Vec::new();
    entry.take(LIMIT + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= LIMIT, "INVALID_ARCHIVE");
    Ok(data)
}
fn execute(command: &[String], args: &[String], work: &Path) -> Result<Vec<u8>> {
    ensure!(
        !command.is_empty()
            && ![".cmd", ".bat"]
                .iter()
                .any(|suffix| command[0].to_ascii_lowercase().ends_with(suffix)),
        "INVALID_COMMAND"
    );
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .args(args)
        .env("TEMP", work)
        .env("TMP", work)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x08000000);
    }
    let mut child = process.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("CLI_PIPE_FAILED"))?;
    let reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stdout.take(LIMIT + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "CLI_FAILED");
            break;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("CLI_TIMEOUT");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("CLI_PIPE_FAILED"))??;
    ensure!(bytes.len() as u64 <= LIMIT, "CLI_OUTPUT_TOO_LARGE");
    Ok(bytes)
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}
struct Context {
    gh: Vec<String>,
    codex: Vec<String>,
    repository: String,
    work: PathBuf,
}
impl Context {
    fn cli(&self, command: &[String], args: &[String], code: &str) -> Result<Value> {
        serde_json::from_slice(
            &execute(command, args, &self.work).map_err(|_| anyhow::anyhow!("{code}"))?,
        )
        .map_err(|_| anyhow::anyhow!("{code}"))
    }
    fn api(&self, suffix: &str) -> Result<Value> {
        self.cli(
            &self.gh,
            &strings(&["api", &format!("repos/{}/{suffix}", self.repository)]),
            "GITHUB_QUERY_FAILED",
        )
    }
    fn installed(&self, source: &Path) -> Result<Value> {
        let list = self.cli(
            &self.codex,
            &strings(&["plugin", "list", "--marketplace", "personal", "--json"]),
            "PLUGIN_LIST_FAILED",
        )?;
        let matches: Vec<_> = list["installed"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("PLUGIN_LIST_FAILED"))?
            .iter()
            .filter(|item| item["pluginId"] == "remnifold@personal")
            .collect();
        ensure!(matches.len() == 1, "INSTALLED_PLUGIN_NOT_UNIQUE");
        let item = matches[0];
        ensure!(
            item["installed"] == true
                && item["enabled"] == true
                && item["source"]["source"] == "local",
            "INSTALLED_PLUGIN_CONFIG_MISMATCH"
        );
        let actual = absolute(
            item["source"]["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INSTALLED_PLUGIN_CONFIG_MISMATCH"))?,
        )?;
        ensure!(
            path_equal(&actual, source),
            "INSTALLED_PLUGIN_CONFIG_MISMATCH"
        );
        ensure!(
            valid_version(item["version"].as_str().unwrap_or("")),
            "INSTALLED_PLUGIN_CONFIG_MISMATCH"
        );
        Ok(item.clone())
    }
}
fn path_equal(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        // GetFinalPathNameByHandle がUnicodeケース差や8.3名を実在パスへ正規化する。
        if let (Ok(left), Ok(right)) = (fs::canonicalize(a), fs::canonicalize(b)) {
            if left == right {
                return true;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(left), Ok(right)) = (fs::metadata(a), fs::metadata(b)) {
            if left.dev() == right.dev() && left.ino() == right.ino() {
                return true;
            }
        }
    }
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}
fn valid_version(value: &str) -> bool {
    regex::Regex::new(
        r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
    )
    .unwrap()
    .is_match(value)
}
fn within(parent: &Path, child: &Path) -> bool {
    child
        .ancestors()
        .any(|ancestor| path_equal(parent, ancestor))
}
fn command_config(config: &Value, name: &str, fallback: Vec<String>) -> Result<Vec<String>> {
    let Some(value) = config
        .get("commands")
        .and_then(|commands| commands.get(name))
    else {
        return Ok(fallback);
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("INVALID_COMMAND"))?;
    let result: Vec<String> = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(String::from)
                .ok_or_else(|| anyhow::anyhow!("INVALID_COMMAND"))
        })
        .collect::<Result<_>>()?;
    ensure!(!result.is_empty(), "INVALID_COMMAND");
    Ok(result)
}
fn codex_command() -> Result<Vec<String>> {
    #[cfg(windows)]
    {
        let root = PathBuf::from(
            env::var_os("LOCALAPPDATA").ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA_MISSING"))?,
        )
        .join("OpenAI/Codex/bin");
        no_links(&root, false)?;
        fn walk(
            root: &Path,
            depth: usize,
            found: &mut Vec<(std::time::SystemTime, PathBuf)>,
        ) -> Result<()> {
            if depth > 3 {
                return Ok(());
            }
            for entry in fs::read_dir(root)? {
                let path = entry?.path();
                let info = fs::symlink_metadata(&path)?;
                if info.file_type().is_symlink() {
                    continue;
                }
                if info.is_dir() {
                    walk(&path, depth + 1, found)?;
                } else if path
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("codex.exe"))
                {
                    found.push((info.modified()?, path));
                }
            }
            Ok(())
        }
        let mut found = Vec::new();
        walk(&root, 0, &mut found)?;
        found.sort();
        let path = found
            .last()
            .ok_or_else(|| anyhow::anyhow!("NATIVE_CODEX_NOT_FOUND"))?
            .1
            .to_string_lossy()
            .into_owned();
        Ok(vec![path])
    }
    #[cfg(not(windows))]
    {
        Ok(strings(&["codex"]))
    }
}

fn verify_native(
    files: &Files,
    commit: &str,
    version: &str,
    target: &str,
    tree: &Value,
) -> Result<Value> {
    let manifest: Value = serde_json::from_slice(
        files
            .get("native-build.json")
            .ok_or_else(|| anyhow::anyhow!("INVALID_NATIVE_MANIFEST"))?,
    )?;
    ensure!(
        manifest["schema"] == 1
            && manifest["sourceCommit"] == commit
            && manifest["version"] == version
            && manifest["target"] == target,
        "NATIVE_BUILD_IDENTITY_MISMATCH"
    );
    let entries = manifest["files"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("INVALID_NATIVE_MANIFEST"))?;
    ensure!(
        files.len() == entries.len() + 1 && files.contains_key(executable(target)),
        "PACKAGE_FILE_SET_MISMATCH"
    );
    for (name, hash) in entries {
        ensure!(
            native_source(name)
                || [".mcp.json", "hooks/hooks.json", executable(target)].contains(&name.as_str()),
            "PACKAGE_FILE_SET_MISMATCH"
        );
        ensure!(
            hash.as_str().is_some_and(|expected| hex(expected, 64)
                && files.get(name).is_some_and(|bytes| sha(bytes) == expected)),
            "PACKAGE_HASH_MISMATCH"
        );
    }
    verify_sources(files, tree, true)?;
    let plugin: Value = serde_json::from_slice(
        files
            .get(".codex-plugin/plugin.json")
            .ok_or_else(|| anyhow::anyhow!("INVALID_RELEASE_MANIFEST"))?,
    )?;
    ensure!(
        plugin["version"] == version && plugin["name"] == "remnifold",
        "RELEASE_VERSION_MISMATCH"
    );
    let mcp: Value = serde_json::from_slice(
        files
            .get(".mcp.json")
            .ok_or_else(|| anyhow::anyhow!("PLUGIN_VALIDATION_FAILED"))?,
    )?;
    ensure!(
        mcp == json!({"mcpServers":{"remnifold":{"command":format!("./{}",executable(target)),"args":["mcp"],"cwd":"."}}}),
        "PLUGIN_VALIDATION_FAILED"
    );
    let hooks: Value = serde_json::from_slice(
        files
            .get("hooks/hooks.json")
            .ok_or_else(|| anyhow::anyhow!("PLUGIN_VALIDATION_FAILED"))?,
    )?;
    let events = ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"];
    ensure!(
        hooks["hooks"]
            .as_object()
            .is_some_and(|object| object.len() == events.len()),
        "PLUGIN_VALIDATION_FAILED"
    );
    for event in events {
        let registration = &hooks["hooks"][event];
        let command = &registration[0]["hooks"][0];
        ensure!(
            registration
                .as_array()
                .is_some_and(|array| array.len() == 1)
                && registration[0]["hooks"]
                    .as_array()
                    .is_some_and(|array| array.len() == 1),
            "PLUGIN_VALIDATION_FAILED"
        );
        ensure!(
            command["type"] == "command"
                && command["command"]
                    == format!("\"${{PLUGIN_ROOT}}/{}\" hook {event}", executable(target))
                && command["timeout"]
                    .as_u64()
                    .is_some_and(|timeout| timeout > 0),
            "PLUGIN_VALIDATION_FAILED"
        );
    }
    Ok(manifest)
}
fn verify_sources(files: &Files, tree: &Value, native: bool) -> Result<()> {
    ensure!(tree["truncated"] == false, "INCOMPLETE_RELEASE_TREE");
    let entries = tree["tree"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("INCOMPLETE_RELEASE_TREE"))?;
    let filter = |name: &str| {
        if native {
            native_source(name)
        } else {
            legacy_source(name)
        }
    };
    let mut expected = BTreeMap::new();
    let mut names = BTreeSet::new();
    for entry in entries {
        let Some(name) = entry["path"].as_str() else {
            continue;
        };
        if entry["type"] != "blob" || !filter(name) {
            continue;
        }
        ensure!(
            safe_name(name)
                && names.insert(name.to_lowercase())
                && ["100644", "100755"].contains(&entry["mode"].as_str().unwrap_or(""))
                && hex(entry["sha"].as_str().unwrap_or(""), 40),
            "UNSAFE_RELEASE_TREE"
        );
        expected.insert(name, entry["sha"].as_str().unwrap());
    }
    let actual: BTreeSet<_> = files
        .keys()
        .filter(|name| filter(name))
        .map(String::as_str)
        .collect();
    ensure!(
        actual == expected.keys().copied().collect()
            && expected.contains_key(".codex-plugin/plugin.json"),
        "PACKAGE_FILE_SET_MISMATCH"
    );
    if !native {
        ensure!(files.len() == expected.len(), "PACKAGE_FILE_SET_MISMATCH");
    }
    for (name, hash) in expected {
        ensure!(
            files.get(name).is_some_and(|bytes| blob(bytes) == hash),
            "PACKAGE_HASH_MISMATCH"
        );
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    match update(args) {
        Ok(result) => {
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            let code = if message.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
                message.as_str()
            } else {
                "LOCAL_PLUGIN_UPDATE_FAILED"
            };
            println!("{}", json!({"status":"blocked","code":code}));
            Err(anyhow::anyhow!("LOCAL_PLUGIN_UPDATE_FAILED"))
        }
    }
}

fn update(argv: &[String]) -> Result<Value> {
    let mut args = BTreeMap::new();
    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        ensure!(!args.contains_key(flag), "INVALID_ARGUMENTS");
        match flag {
            "--version" | "--work-root" | "--config" => {
                index += 1;
                let value = argv
                    .get(index)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("INVALID_ARGUMENTS"))?;
                args.insert(flag, value.clone());
            }
            "--check-published" | "--apply" | "--verify-only" => {
                args.insert(flag, String::new());
            }
            _ => bail!("INVALID_ARGUMENTS"),
        }
        index += 1;
    }
    let version = args
        .get("--version")
        .ok_or_else(|| anyhow::anyhow!("INVALID_VERSION"))?;
    ensure!(valid_version(version), "INVALID_VERSION");
    ensure!(
        ["--check-published", "--apply", "--verify-only"]
            .iter()
            .filter(|flag| args.contains_key(**flag))
            .count()
            == 1,
        "INVALID_ARGUMENTS"
    );
    let config: Value = if let Some(config) = args.get("--config") {
        serde_json::from_slice(&fs::read(absolute(config)?)?)?
    } else {
        json!({"localPlugin":{"id":"remnifold@personal","repository":"1llum1n4t1s/remnifold","workflow":"CI","artifact":"remnifold","sourcePath":"~/plugins/remnifold"}})
    };
    let settings = &config["localPlugin"];
    let repository = settings["repository"].as_str().unwrap_or("");
    ensure!(
        regex::Regex::new(r"^[\w.-]+/[\w.-]+$")?.is_match(repository)
            && settings["id"] == "remnifold@personal"
            && settings["workflow"] == "CI"
            && settings["artifact"] == "remnifold",
        "INVALID_CONFIG"
    );
    let source = absolute(
        settings["sourcePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("INVALID_CONFIG"))?,
    )?;
    let codex_home = absolute(&env::var("CODEX_HOME").unwrap_or_else(|_| "~/.codex".into()))?;
    let default_work = if cfg!(windows) {
        format!("D:/Codex/remnifold/plugin-auto-update-{version}")
    } else {
        format!("~/.cache/remnifold/plugin-auto-update-{version}")
    };
    let work = absolute(
        args.get("--work-root")
            .map(String::as_str)
            .unwrap_or(&default_work),
    )?;
    ensure!(
        !within(&source, &work)
            && !within(&work, &source)
            && !within(&codex_home, &work)
            && !within(&codex_home, &source)
            && !work
                .ancestors()
                .any(|ancestor| ancestor.join(".git").exists()),
        "INVALID_WORK_ROOT"
    );
    #[cfg(windows)]
    ensure!(
        within(&absolute("D:/Codex/remnifold")?, &work),
        "INVALID_WORK_ROOT"
    );
    no_links(&work, true)?;
    no_links(&source, false)?;
    fs::create_dir_all(&work)?;
    let gh = command_config(
        &config,
        "gh",
        strings(&[if cfg!(windows) { "gh.exe" } else { "gh" }]),
    )?;
    let codex = if config["commands"].get("codex").is_some() {
        command_config(&config, "codex", Vec::new())?
    } else {
        codex_command()?
    };
    let context = Context {
        gh,
        codex,
        repository: repository.into(),
        work: work.clone(),
    };
    let reference = context.api(&format!("git/ref/heads/release/{version}"))?;
    let commit = reference["object"]["sha"].as_str().unwrap_or("");
    ensure!(
        reference["ref"] == format!("refs/heads/release/{version}")
            && reference["object"]["type"] == "commit"
            && hex(commit, 40),
        "RELEASE_REF_MISMATCH"
    );
    let published = context.api(&format!("contents/.codex-plugin/plugin.json?ref={commit}"))?;
    let encoded = published["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("INVALID_RELEASE_MANIFEST"))?
        .split_whitespace()
        .collect::<String>();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("INVALID_RELEASE_MANIFEST"))?;
    let published: Value = serde_json::from_slice(&decoded)
        .map_err(|_| anyhow::anyhow!("INVALID_RELEASE_MANIFEST"))?;
    ensure!(published["version"] == *version, "RELEASE_VERSION_MISMATCH");
    let tree = context.api(&format!("git/trees/{commit}?recursive=1"))?;
    ensure!(
        tree["truncated"] == false && tree["tree"].is_array(),
        "INCOMPLETE_RELEASE_TREE"
    );
    let native = tree["tree"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["path"] == "native/Cargo.toml" && entry["type"] == "blob")
    });
    let target = target()?;
    let artifact_name = if native {
        format!("remnifold-{target}")
    } else {
        "remnifold".into()
    };
    let archive_name = format!("{artifact_name}.tar.gz");
    let receipt_path = work.join(format!("verified-{commit}-{target}.json"));
    let installed = context.installed(&source)?;
    let cache_path = |installed: &Value| -> Result<PathBuf> {
        Ok(codex_home.join("plugins/cache/personal/remnifold").join(
            installed["version"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INSTALLED_PLUGIN_CONFIG_MISMATCH"))?,
        ))
    };
    if args.contains_key("--verify-only") {
        let local = files_at(&source)?;
        let cached = files_at(&cache_path(&installed)?)?;
        ensure!(
            installed["version"] == *version && hashes(&local) == hashes(&cached),
            "INSTALLED_PLUGIN_CONTENT_MISMATCH"
        );
        if native {
            verify_native(&local, commit, version, target, &tree)?;
            no_links(&receipt_path, false)?;
            let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
            ensure!(
                receipt["sourceCommit"] == commit
                    && receipt["target"] == target
                    && receipt["manifestSha256"] == sha(local.get("native-build.json").unwrap()),
                "NATIVE_RECEIPT_MISMATCH"
            );
        } else {
            verify_sources(&local, &tree, false)?;
        }
        return Ok(
            json!({"status":"verified","version":version,"sha":commit,"files":local.len(),"workRoot":work}),
        );
    }
    let runs = context.api(&format!(
        "actions/runs?head_sha={commit}&event=push&per_page=100"
    ))?;
    let run = runs["workflow_runs"]
        .as_array()
        .and_then(|runs| {
            runs.iter()
                .filter(|run| {
                    run["name"] == "CI"
                        && run["head_sha"] == commit
                        && run["event"] == "push"
                        && run["head_branch"] == format!("release/{version}")
                })
                .max_by_key(|run| {
                    (
                        run["id"].as_u64().unwrap_or(0),
                        run["run_attempt"].as_u64().unwrap_or(0),
                    )
                })
        })
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_NOT_SUCCESSFUL"))?;
    ensure!(
        run["status"] == "completed" && run["conclusion"] == "success",
        "RELEASE_CI_NOT_SUCCESSFUL"
    );
    let run_id = run["id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_NOT_SUCCESSFUL"))?;
    let artifacts = context.api(&format!("actions/runs/{run_id}/artifacts?per_page=100"))?;
    let artifact = artifacts["artifacts"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .filter(|item| item["name"] == artifact_name)
                .max_by_key(|item| item["id"].as_u64().unwrap_or(0))
        })
        .ok_or_else(|| anyhow::anyhow!("RELEASE_ARTIFACT_UNAVAILABLE"))?;
    ensure!(
        artifact["expired"] == false
            && artifact["expires_at"]
                .as_str()
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .is_some_and(|date| date > chrono::Utc::now()),
        "RELEASE_ARTIFACT_UNAVAILABLE"
    );
    let artifact_id = artifact["id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("RELEASE_ARTIFACT_UNAVAILABLE"))?;
    let digest = artifact["digest"].as_str().unwrap_or("");
    if native {
        ensure!(
            digest
                .strip_prefix("sha256:")
                .is_some_and(|hash| hex(hash, 64)),
            "ARTIFACT_DIGEST_UNAVAILABLE"
        );
    }
    let evidence = json!({"version":version,"sha":commit,"runId":run_id,"artifactId":artifact_id,"workRoot":work});
    if args.contains_key("--check-published") {
        let mut result = evidence;
        result["status"] = json!("published");
        return Ok(result);
    }
    let staging = work.join(format!("update-{version}-{}", uuid::Uuid::new_v4()));
    no_links(&staging, true)?;
    fs::create_dir(&staging)?;
    let incoming = if native {
        let bytes = execute(
            &context.gh,
            &strings(&[
                "api",
                &format!("repos/{repository}/actions/artifacts/{artifact_id}/zip"),
            ]),
            &work,
        )
        .map_err(|_| anyhow::anyhow!("ARTIFACT_DOWNLOAD_FAILED"))?;
        ensure!(
            format!("sha256:{}", sha(&bytes)) == digest,
            "ARTIFACT_DIGEST_MISMATCH"
        );
        parse_tar(&read_zip(&bytes, &archive_name)?)?
    } else {
        execute(
            &context.gh,
            &strings(&[
                "run",
                "download",
                &run_id.to_string(),
                "--repo",
                repository,
                "--name",
                &artifact_name,
                "--dir",
                &staging.to_string_lossy(),
            ]),
            &work,
        )
        .map_err(|_| anyhow::anyhow!("ARTIFACT_DOWNLOAD_FAILED"))?;
        let entries: Vec<_> = fs::read_dir(&staging)?.collect::<std::io::Result<_>>()?;
        ensure!(
            entries.len() == 1 && entries[0].file_name().to_string_lossy() == archive_name,
            "UNEXPECTED_ARTIFACT_CONTENTS"
        );
        no_links(&staging.join(&archive_name), false)?;
        parse_tar(&fs::read(staging.join(&archive_name))?)?
    };
    if native {
        verify_native(&incoming, commit, version, target, &tree)?;
    } else {
        verify_sources(&incoming, &tree, false)?;
    }
    let wanted = hashes(&incoming);
    let local = hashes(&files_at(&source)?);
    let cached = hashes(&files_at(&cache_path(&installed)?)?);
    for (name, hash) in &local {
        ensure!(
            cached.get(name) == Some(hash) || wanted.get(name) == Some(hash),
            "LOCAL_SOURCE_MODIFIED"
        );
    }
    for name in cached.keys() {
        ensure!(
            local.contains_key(name) || (!wanted.contains_key(name) && local == wanted),
            "LOCAL_SOURCE_MODIFIED"
        );
    }
    // A minimal receipt survives CI retention expiry. It authenticates the installed build manifest.
    if native {
        let receipt = json!({"schema":1,"sourceCommit":commit,"target":target,"manifestSha256":sha(incoming.get("native-build.json").unwrap()),"artifactDigest":digest,"artifactId":artifact_id,"runId":run_id});
        no_links(&receipt_path, true)?;
        fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt)?)?;
    }
    if installed["version"] == *version && local == wanted && cached == wanted {
        let mut result = evidence;
        result["status"] = json!("already-current");
        result["files"] = json!(wanted.len());
        return Ok(result);
    }
    if local != wanted {
        let replacement =
            source.with_file_name(format!("remnifold-incoming-{}", uuid::Uuid::new_v4()));
        no_links(&replacement, true)?;
        fs::create_dir(&replacement)?;
        for (name, bytes) in &incoming {
            let destination = replacement.join(name);
            fs::create_dir_all(destination.parent().unwrap())?;
            fs::write(&destination, bytes)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if native {
                fs::set_permissions(
                    replacement.join(executable(target)),
                    fs::Permissions::from_mode(0o755),
                )?;
            }
        }
        ensure!(
            hashes(&files_at(&replacement)?) == wanted,
            "SOURCE_COPY_VERIFICATION_FAILED"
        );
        // Same-volume rename preserves the previous source without deleting obsolete legacy assets.
        let backup = source.with_file_name(format!("remnifold-backup-{}", uuid::Uuid::new_v4()));
        no_links(&backup, true)?;
        ensure!(
            hashes(&files_at(&source)?) == local
                && hashes(&files_at(&cache_path(&installed)?)?) == cached,
            "LOCAL_SOURCE_MODIFIED"
        );
        fs::rename(&source, &backup)?;
        if let Err(error) = fs::rename(&replacement, &source) {
            let _ = fs::rename(&backup, &source);
            return Err(error.into());
        }
        // Kept beside source because the external work root may be on a different volume.
        fs::write(
            staging.join("backup-path.json"),
            serde_json::to_vec(&json!({"backupPath":backup}))?,
        )?;
    }
    ensure!(
        hashes(&files_at(&source)?) == wanted,
        "SOURCE_COPY_VERIFICATION_FAILED"
    );
    let added = context.cli(
        &context.codex,
        &strings(&["plugin", "add", "remnifold@personal", "--json"]),
        "PLUGIN_ADD_FAILED",
    )?;
    let after = context.installed(&source)?;
    ensure!(
        after["version"] == *version
            && hashes(&files_at(&source)?) == wanted
            && hashes(&files_at(&cache_path(&after)?)?) == wanted,
        "INSTALLED_PLUGIN_CONTENT_MISMATCH"
    );
    let mut result = evidence;
    result["status"] = json!("updated");
    result["files"] = json!(wanted.len());
    result["stagingPath"] = json!(staging);
    for name in ["pendingTrust", "hookTrustRequired", "hooksTrusted"] {
        if added[name].is_boolean() {
            result[name] = added[name].clone();
        }
    }
    Ok(result)
}
