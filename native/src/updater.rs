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
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
#[cfg(test)]
use std::{thread, time::Instant};
mod install;
pub use install::{retired, run as run_install};
#[path = "hook_command.rs"]
mod hook_command;
mod path_safety;
mod private_work;
mod process;
use path_safety::{isolated_from_remnifold_roots, no_links, physical_path, within};
#[cfg(test)]
use path_safety::{isolated_physical_roots, path_equal};
mod r2;
#[cfg(test)]
#[path = "test_links.rs"]
pub(crate) mod test_links;

type Files = BTreeMap<String, Vec<u8>>;
const LIMIT: u64 = crate::release_manifest::MAX_ARCHIVE;
// SHA-256 の小文字16進表記と、固定長の小文字16進判定は共通実装を使う。
// 同じ判定を複数箇所で持つと、配布物検証の規則がずれたときに追跡しにくくなるため。
use crate::common::sha256 as sha;
use crate::release_manifest::hex;
fn blob(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()));
    hash.update(bytes);
    crate::common::encode_hex(&hash.finalize())
}
fn target() -> Result<&'static str> {
    Ok(match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
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
/// 作業ルートを省略したときの親フォルダ。
/// Windows は利用者ごとの `%LOCALAPPDATA%\remnifold`、それ以外は `~/.cache/remnifold`。
/// どの利用者・ドライブ構成でも存在する場所だけを使い、特定の環境のフォルダを前提にしない。
/// 明示した `--work-root` はリポジトリ・プラグイン・Codex home の外であることなどを別途検証する。
pub(crate) fn default_work_parent() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = match env::var_os("LOCALAPPDATA") {
        Some(path) => PathBuf::from(path),
        None => home()?.join("AppData/Local"),
    };
    #[cfg(not(windows))]
    let base = home()?.join(".cache");
    ensure!(base.is_absolute(), "INVALID_WORK_ROOT");
    Ok(base.join("remnifold"))
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

    #[test]
    fn native_hook_overrides_accept_legacy_or_exact_git_bash_only() {
        let events = ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"];
        let legacy = |target: &str| {
            let mut hooks = json!({"hooks":{}});
            for event in events {
                hooks["hooks"][event] = json!([{"hooks":[{"type":"command","timeout":12,"command":format!("\"${{PLUGIN_ROOT}}/{}\" hook {event}",executable(target))}]}]);
            }
            hooks
        };
        let verify = |hooks: &Value, target: &str| -> Result<Value> {
            let commit = "a".repeat(40);
            let version = "1.0.0";
            let mut files = Files::from([
                (
                    ".codex-plugin/plugin.json".into(),
                    serde_json::to_vec(&json!({"name":"remnifold","version":version}))?,
                ),
                (
                    ".mcp.json".into(),
                    serde_json::to_vec(
                        &json!({"mcpServers":{"remnifold":{"command":format!("./{}",executable(target)),"args":["mcp"],"cwd":"."}}}),
                    )?,
                ),
                ("hooks/hooks.json".into(), serde_json::to_vec(hooks)?),
                (
                    executable(target).into(),
                    b"fixture executable bytes".to_vec(),
                ),
            ]);
            let tree = json!({"truncated":false,"tree":[{"path":".codex-plugin/plugin.json","type":"blob","mode":"100644","sha":blob(&files[".codex-plugin/plugin.json"])}]});
            let hashes: BTreeMap<_, _> = files
                .iter()
                .map(|(name, bytes)| (name, sha(bytes)))
                .collect();
            let manifest = serde_json::to_vec(
                &json!({"schema":1,"target":target,"sourceCommit":commit,"version":version,"files":hashes}),
            )?;
            files.insert("native-build.json".into(), manifest);
            verify_native(&files, &commit, version, target, &tree)
        };
        let windows = "x86_64-pc-windows-msvc";
        let old = legacy(windows);
        assert!(verify(&old, windows).is_ok());
        let mut current = old.clone();
        for event in events {
            current["hooks"][event][0]["hooks"][0]["commandWindows"] =
                json!(hook_command::windows_hook_command(event).unwrap());
        }
        assert!(verify(&current, windows).is_ok());
        // 全旧形式か全新形式だけを許可し、Stop 以外からの前提検査迂回も防ぐ。
        for selected in 1u8..15 {
            let mut mixed = old.clone();
            for (index, event) in events.iter().enumerate() {
                if selected & (1 << index) != 0 {
                    mixed["hooks"][*event][0]["hooks"][0]["commandWindows"] =
                        json!(hook_command::windows_hook_command(event).unwrap());
                }
            }
            assert_eq!(
                verify(&mixed, windows).unwrap_err().to_string(),
                "PLUGIN_VALIDATION_FAILED"
            );
        }
        for event in events {
            for value in [
                Value::Null,
                json!(1),
                json!("untrusted override"),
                json!(
                    hook_command::windows_hook_command(event)
                        .unwrap()
                        .replace(" -p -c ", " -c ")
                ),
                json!(hook_command::windows_hook_command("PreToolUse").unwrap() + " "),
            ] {
                let mut invalid = current.clone();
                invalid["hooks"][event][0]["hooks"][0]["commandWindows"] = value;
                assert_eq!(
                    verify(&invalid, windows).unwrap_err().to_string(),
                    "PLUGIN_VALIDATION_FAILED"
                );
            }
            for base in [&old, &current] {
                let mut alias = base.clone();
                alias["hooks"][event][0]["hooks"][0]["command_windows"] =
                    json!(hook_command::windows_hook_command(event).unwrap());
                assert_eq!(
                    verify(&alias, windows).unwrap_err().to_string(),
                    "PLUGIN_VALIDATION_FAILED"
                );
            }
        }
        for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
            let mut hooks = legacy(target);
            assert!(verify(&hooks, target).is_ok());
            hooks["hooks"]["Stop"][0]["hooks"][0]["commandWindows"] =
                json!(hook_command::windows_hook_command("Stop").unwrap());
            assert_eq!(
                verify(&hooks, target).unwrap_err().to_string(),
                "PLUGIN_VALIDATION_FAILED"
            );
        }
    }

    // 独立プロセスでのみ起動する、標準出力継承の再現用 fixture。
    #[test]
    #[ignore]
    fn inherited_stdout_fixture() {
        if std::env::args().any(|arg| arg == "--show-output") {
            thread::sleep(Duration::from_secs(2));
            return;
        }
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "updater::tests::inherited_stdout_fixture",
                "--ignored",
                "--nocapture",
                "--show-output",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
    }

    #[test]
    fn child_exit_does_not_remove_stdout_deadline() {
        let command = vec![
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        let args = strings(&[
            "--exact",
            "updater::tests::inherited_stdout_fixture",
            "--ignored",
            "--nocapture",
        ]);
        let start = Instant::now();
        let error = execute_with_timeout(
            &command,
            &args,
            &std::env::temp_dir(),
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "CLI_TIMEOUT");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn physical_roots_resolve_missing_suffixes_and_valid_external_aliases() {
        let root = physical_path(&std::env::temp_dir())
            .unwrap()
            .join(format!("remnifold-updater-roots-{}", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        no_links(&std::env::temp_dir(), false).unwrap();
        #[cfg(not(windows))]
        no_links(&root, true).unwrap();
        fs::create_dir(&root).unwrap();
        for directory in ["source", "work", "external"] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        let work = root.join("work/not-created");
        let source = root.join("source");
        assert!(path_equal(
            &physical_path(&work).unwrap(),
            &physical_path(&root.join("work"))
                .unwrap()
                .join("not-created")
        ));
        assert!(isolated_physical_roots(&source, &work, &root.join("external/missing")).is_ok());
        assert!(isolated_physical_roots(&source, &work, &root.join("work")).is_err());
        assert!(!work.exists());
        #[cfg(any(unix, windows))]
        {
            fn alias(alias: &Path, target: &Path) {
                test_links::directory_link(target, alias).unwrap();
            }
            for name in ["work", "source", "external"] {
                alias(&root.join(format!("alias-{name}")), &root.join(name));
            }
            assert!(isolated_physical_roots(&source, &work, &root.join("alias-work")).is_err());
            assert!(
                isolated_physical_roots(
                    &source,
                    &root.join("work"),
                    &root.join("alias-work/missing")
                )
                .is_err()
            );
            assert!(
                isolated_physical_roots(&source, &work, &root.join("alias-source/missing"))
                    .is_err()
            );
            assert!(
                isolated_physical_roots(&source, &work, &root.join("alias-external/missing"))
                    .is_ok()
            );
            assert!(!work.exists());
        }
        #[cfg(windows)]
        assert!(path_equal(
            &physical_path(&work).unwrap(),
            &physical_path(&PathBuf::from(work.to_string_lossy().to_uppercase())).unwrap()
        ));
        eprintln!("retained updater root-boundary fixture: {}", root.display());
    }

    #[cfg(windows)]
    #[test]
    fn missing_unicode_case_variants_share_the_same_windows_root() {
        let root = physical_path(&std::env::temp_dir()).unwrap();
        let source = root.join("unicode-source-uncreated");
        let work = root.join("Ä/work");
        let codex = root.join("ä/work");
        assert!(isolated_physical_roots(&source, &work, &codex).is_err());
        assert!(path_equal(&work, &codex));
    }

    #[test]
    fn r2_release_tree_rejects_a_downgraded_legacy_receipt_offline() {
        let tree = json!({"tree":[{"path":".github/remnifold-distribution.json","type":"blob"}]});
        assert!(verify_receipt_schema(&json!({"schema":1}), &tree).is_err());
        assert!(verify_receipt_schema(&json!({"schema":2}), &tree).is_ok());
        assert!(verify_receipt_schema(&json!({"schema":1}), &json!({"tree":[]})).is_ok());
    }

    #[test]
    fn preserves_sha256_and_git_blob_identities() {
        for (input, digest, object) in [
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                "f2ba8f84ab5c1bce84a7b441cb1959cfc7093b7f",
            ),
        ] {
            assert_eq!(sha(input.as_bytes()), digest);
            assert_eq!(crate::common::sha256(input), digest);
            assert_eq!(blob(input.as_bytes()), object);
        }
    }

    #[test]
    fn streamed_hash_matches_bytes_and_retries_interrupted_reads() {
        struct InterruptedReader<'a> {
            remaining: &'a [u8],
            interrupt: bool,
        }
        impl Read for InterruptedReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.remaining.read(buffer)
            }
        }
        let large: Vec<u8> = (0..(3 * 64 * 1024 + 17)).map(|index| index as u8).collect();
        for bytes in [b"".as_slice(), b"abc".as_slice(), large.as_slice()] {
            assert_eq!(hash_reader(bytes).unwrap(), sha(bytes));
            assert_eq!(
                hash_reader(InterruptedReader {
                    remaining: bytes,
                    interrupt: false
                })
                .unwrap(),
                sha(bytes)
            );
        }
    }

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
    walk_files(root, &mut |path| Ok(fs::read(path)?))
}
fn hashes_at(root: &Path) -> Result<BTreeMap<String, String>> {
    walk_files(root, &mut |path| hash_reader(fs::File::open(path)?))
}
fn hash_reader(mut reader: impl Read) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(crate::common::encode_hex(&hash.finalize())),
            Ok(count) => hash.update(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
fn walk_files<T>(
    root: &Path,
    read: &mut impl FnMut(&Path) -> Result<T>,
) -> Result<BTreeMap<String, T>> {
    no_links(root, false)?;
    fn walk<T>(
        root: &Path,
        here: &Path,
        result: &mut BTreeMap<String, T>,
        read: &mut impl FnMut(&Path) -> Result<T>,
    ) -> Result<()> {
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
                walk(root, &path, result, read)?;
            } else {
                ensure!(
                    info.is_file() && allowed(&name) && info.len() <= LIMIT,
                    "UNKNOWN_LOCAL_FILE"
                );
                result.insert(name, read(&path)?);
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result, read)?;
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
fn execute(command: &[String], args: &[String], work: &Path) -> Result<Vec<u8>> {
    execute_with_timeout(command, args, work, Duration::from_secs(120))
}
fn execute_with_timeout(
    command: &[String],
    args: &[String],
    work: &Path,
    timeout: Duration,
) -> Result<Vec<u8>> {
    execute_with_limits(command, args, work, timeout, LIMIT)
}
fn execute_with_limits(
    command: &[String],
    args: &[String],
    work: &Path,
    timeout: Duration,
    limit: u64,
) -> Result<Vec<u8>> {
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
        .env("TMPDIR", work)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::run(process, timeout, limit)
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}
struct Context {
    gh: Vec<String>,
    repository: String,
    work: PathBuf,
    gh_log_escape: std::sync::OnceLock<bool>,
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
            &strings(&[
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{}/{suffix}", self.repository),
            ]),
            "GITHUB_QUERY_FAILED",
        )
    }
}
fn valid_version(value: &str) -> bool {
    regex::Regex::new(
        r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
    )
    .unwrap()
    .is_match(value)
}
fn verify_receipt_schema(receipt: &Value, tree: &Value) -> Result<()> {
    if tree["tree"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["path"] == ".github/remnifold-distribution.json")
    }) {
        ensure!(receipt["schema"] == 2, "NATIVE_RECEIPT_MISMATCH");
    }
    Ok(())
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
    let overrides = events
        .iter()
        .filter(|event| {
            hooks["hooks"][**event][0]["hooks"][0]
                .get("commandWindows")
                .is_some()
        })
        .count();
    ensure!(
        overrides == 0 || (target.ends_with("windows-msvc") && overrides == events.len()),
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
                && (command.get("commandWindows").is_none()
                    || (target.ends_with("windows-msvc")
                        && command["commandWindows"].as_str()
                            == hook_command::windows_hook_command(event).as_deref()))
                && command.get("command_windows").is_none()
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
