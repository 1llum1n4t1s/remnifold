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
    time::Duration,
};
#[cfg(test)]
use std::{thread, time::Instant};
mod bootstrap;
#[path = "hook_command.rs"]
mod hook_command;
mod path_safety;
mod private_work;
mod process;
use path_safety::{isolated_physical_roots, no_links, path_equal, physical_path, within};
mod r2;
pub use bootstrap::run as run_bootstrap;
#[cfg(test)]
#[path = "test_links.rs"]
pub(crate) mod test_links;

type Files = BTreeMap<String, Vec<u8>>;
const LIMIT: u64 = crate::release_manifest::MAX_ARCHIVE;
fn sha(bytes: &[u8]) -> String {
    crate::common::encode_hex(&Sha256::digest(bytes))
}
fn blob(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()));
    hash.update(bytes);
    crate::common::encode_hex(&hash.finalize())
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

    #[cfg(windows)]
    #[test]
    fn windows_hook_prerequisites_reject_missing_git_or_non_executable_pathext() {
        let executable = std::env::current_exe().unwrap();
        for value in [None, Some(""), Some(".BAT;.CMD"), Some(".EXE ")] {
            assert_eq!(
                windows_hook_prerequisites(&executable, value)
                    .unwrap_err()
                    .to_string(),
                "WINDOWS_PATHEXT_EXE_REQUIRED"
            );
        }
        for value in [".EXE", ".COM;.exe;.BAT;.CMD", ";.COM;.exe;;.BAT;.CMD;"] {
            windows_hook_prerequisites(&executable, Some(value)).unwrap();
        }
        let missing =
            executable.with_file_name(format!("missing-git-{}.exe", uuid::Uuid::new_v4()));
        assert_eq!(
            windows_hook_prerequisites(&missing, Some(".EXE"))
                .unwrap_err()
                .to_string(),
            "WINDOWS_GIT_BASH_REQUIRED"
        );
        assert_eq!(
            windows_hook_prerequisites(executable.parent().unwrap(), Some(".EXE"))
                .unwrap_err()
                .to_string(),
            "WINDOWS_GIT_BASH_REQUIRED"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_hook_resolution_rejects_script_and_alternate_extension_candidates() {
        windows_hook_resolution(Some(".COM;.EXE;.BAT;.CMD"), |extension| {
            Ok(extension.eq_ignore_ascii_case(".EXE"))
        })
        .unwrap();
        windows_hook_resolution(Some(";.EXE;;"), |extension| Ok(extension == ".EXE")).unwrap();
        assert_eq!(
            windows_hook_resolution(Some(";.EXE;;"), |extension| Ok(
                extension.is_empty() || extension == ".EXE"
            ))
            .unwrap_err()
            .to_string(),
            "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
        );
        for hostile in ["", ".ps1", ".COM", ".BAT", ".CMD"] {
            assert_eq!(
                windows_hook_resolution(Some(".EXE;.COM;.BAT;.CMD"), |extension| Ok(extension
                    .eq_ignore_ascii_case(".EXE")
                    || extension.eq_ignore_ascii_case(hostile)))
                .unwrap_err()
                .to_string(),
                "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
            );
        }
        assert_eq!(
            windows_hook_resolution(Some(".EXE;../bad"), |extension| Ok(extension == ".EXE"))
                .unwrap_err()
                .to_string(),
            "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
        );
        assert!(
            windows_hook_resolution(Some(".EXE"), |_| bail!("fixture metadata unavailable"))
                .is_err()
        );
        assert!(path_equal(
            &windows_program_files().unwrap(),
            Path::new(r"C:\Program Files")
        ));
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

    fn zip_archive(names: &[&str]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in names {
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(b"bounded tar payload").unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn accepts_only_one_expected_regular_zip_entry() {
        let name = "remnifold.tar.gz";
        assert_eq!(
            read_zip(&zip_archive(&[name]), name).unwrap(),
            b"bounded tar payload"
        );
        assert!(read_zip(&zip_archive(&[name, "extra"]), name).is_err());
        assert!(read_zip(&zip_archive(&["../outside"]), name).is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(name, "../outside", zip::write::SimpleFileOptions::default())
            .unwrap();
        assert!(read_zip(&writer.finish().unwrap().into_inner(), name).is_err());
    }

    #[test]
    fn rejects_zip_cloaking_and_oversized_metadata_before_reading_payload() {
        let name = "remnifold.tar.gz";
        let mut cloaked = zip_archive(&[name]);
        assert_eq!(&cloaked[..4], b"PK\x03\x04");
        assert_eq!(&cloaked[30..30 + name.len()], name.as_bytes());
        // ZIP中央ディレクトリとローカルヘッダーが異なる名前を主張する入力を拒否する。
        let hostile = b"../outside/evil!";
        assert_eq!(hostile.len(), name.len());
        cloaked[30..30 + name.len()].copy_from_slice(hostile);
        assert!(read_zip(&cloaked, name).is_err());
        let mut oversized = zip_archive(&[name]);
        let central = oversized
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        oversized[22..26].copy_from_slice(&((LIMIT + 1) as u32).to_le_bytes());
        oversized[central + 24..central + 28].copy_from_slice(&((LIMIT + 1) as u32).to_le_bytes());
        assert_eq!(
            read_zip(&oversized, name).unwrap_err().to_string(),
            "UNSAFE_ARCHIVE"
        );
    }

    #[test]
    fn accepts_github_deflate_data_descriptor_entries() {
        let name = "remnifold.tar.gz";
        let mut bytes = zip_archive(&[name]);
        let central = bytes
            .windows(4)
            .position(|part| part == b"PK\x01\x02")
            .unwrap();
        let mut descriptor = b"PK\x07\x08".to_vec();
        descriptor.extend_from_slice(&bytes[14..26]);
        // GitHub配布ZIP同様、ローカルのサイズは0で、実値を後続descriptorに置く。
        let flags = u16::from_le_bytes(bytes[6..8].try_into().unwrap()) | 8;
        bytes[6..8].copy_from_slice(&flags.to_le_bytes());
        bytes[central + 8..central + 10].copy_from_slice(&flags.to_le_bytes());
        bytes[14..26].fill(0);
        bytes.splice(central..central, descriptor.iter().copied());
        let end = bytes
            .windows(4)
            .rposition(|part| part == b"PK\x05\x06")
            .unwrap();
        bytes[end + 16..end + 20]
            .copy_from_slice(&((central + descriptor.len()) as u32).to_le_bytes());
        assert_eq!(read_zip(&bytes, name).unwrap(), b"bounded tar payload");
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
fn read_zip(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(zip.len() == 1, "UNEXPECTED_ARTIFACT_CONTENTS");
    let entry = zip.by_index_with_options(
        0,
        zip::read::ZipReadOptions::new().enforce_header_consistency(true),
    )?;
    ensure!(
        entry.name()?.as_ref() == name
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
    codex: Vec<String>,
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

#[cfg(windows)]
fn windows_hook_resolution(
    pathext: Option<&str>,
    mut exists: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    let extensions = pathext
        .map(|value| value.split(';').collect::<Vec<_>>())
        .unwrap_or_default();
    ensure!(
        extensions
            .iter()
            .any(|extension| extension.eq_ignore_ascii_case(".EXE")),
        "WINDOWS_PATHEXT_EXE_REQUIRED"
    );
    // PowerShell は PATHEXT より先に .ps1 も解決する。前提を保守的に固定する。
    ensure!(
        !exists("")? && !exists(".ps1")?,
        "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
    );
    let mut executable = false;
    for extension in extensions
        .into_iter()
        .filter(|extension| !extension.is_empty())
    {
        ensure!(
            extension.starts_with('.')
                && extension.len() <= 32
                && extension[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric()),
            "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
        );
        if exists(extension)? {
            ensure!(
                extension.eq_ignore_ascii_case(".EXE"),
                "WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"
            );
            executable = true;
        }
    }
    ensure!(executable, "WINDOWS_GIT_BASH_REQUIRED");
    Ok(())
}

#[cfg(windows)]
fn windows_program_files() -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
    };
    let mut raw = std::ptr::null_mut();
    let status =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut raw) };
    ensure!(
        status >= 0 && !raw.is_null(),
        "WINDOWS_GIT_BASH_UNSUPPORTED_LOCATION"
    );
    struct Folder(*mut u16);
    impl Drop for Folder {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
    let folder = Folder(raw);
    let length = (0..32768)
        .find(|&index| unsafe { *folder.0.add(index) == 0 })
        .ok_or_else(|| anyhow::anyhow!("WINDOWS_GIT_BASH_UNSUPPORTED_LOCATION"))?;
    Ok(PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(folder.0, length)
    })))
}

#[cfg(windows)]
fn windows_hook_prerequisites(shell: &Path, pathext: Option<&str>) -> Result<()> {
    let program_files = windows_program_files()?;
    let local_c = matches!(program_files.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(b'C' | b'c') | std::path::Prefix::VerbatimDisk(b'C' | b'c')));
    ensure!(
        local_c
            && no_links(&program_files, false).is_ok()
            && no_links(Path::new(r"C:\Program Files"), false).is_ok()
            && path_equal(&program_files, Path::new(r"C:\Program Files")),
        "WINDOWS_GIT_BASH_UNSUPPORTED_LOCATION"
    );
    ensure!(
        no_links(shell, false).is_ok()
            && fs::metadata(shell).is_ok_and(|metadata| metadata.is_file()),
        "WINDOWS_GIT_BASH_REQUIRED"
    );
    let mut signature = [0; 2];
    ensure!(
        fs::File::open(shell)
            .and_then(|mut file| file.read_exact(&mut signature))
            .is_ok()
            && signature == *b"MZ",
        "WINDOWS_GIT_BASH_REQUIRED"
    );
    let base = shell.with_extension("");
    windows_hook_resolution(pathext, |extension| {
        let mut candidate = base.as_os_str().to_owned();
        candidate.push(extension);
        match fs::symlink_metadata(Path::new(&candidate)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => bail!("WINDOWS_GIT_BASH_UNSAFE_RESOLUTION"),
        }
    })?;
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
            let code = if message
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
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
    isolated_physical_roots(&source, &work, &codex_home)?;
    let _private_work = private_work::PrivateWorkGuard::open(&work)?;
    isolated_physical_roots(&source, &work, &codex_home)?;
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
        gh_log_escape: std::sync::OnceLock::new(),
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
    private_work::validate_receipt(&receipt_path)?;
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
        let cached = hashes_at(&cache_path(&installed)?)?;
        ensure!(
            installed["version"] == *version && hashes(&local) == cached,
            "INSTALLED_PLUGIN_CONTENT_MISMATCH"
        );
        if native {
            verify_native(&local, commit, version, target, &tree)?;
            no_links(&receipt_path, false)?;
            private_work::validate_receipt(&receipt_path)?;
            let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
            verify_receipt_schema(&receipt, &tree)?;
            r2::verify_receipt(&receipt, &local, &context, version, commit, target)?;
        } else {
            verify_sources(&local, &tree, false)?;
        }
        return Ok(
            json!({"status":"verified","version":version,"sha":commit,"files":local.len(),"workRoot":work}),
        );
    }
    let distribution = r2::config(&context, &tree, commit)?;
    let staging = work.join(format!("update-{version}-{}", uuid::Uuid::new_v4()));
    let (incoming, evidence, receipt) = if let Some(distribution) = distribution {
        ensure!(native, "R2_REQUIRES_NATIVE_PACKAGE");
        let publication = r2::published(&context, &distribution, commit, version, target, &tree)?;
        if args.contains_key("--check-published") {
            let mut result = publication.evidence;
            result["status"] = json!("published");
            return Ok(result);
        }
        no_links(&staging, true)?;
        fs::create_dir(&staging)?;
        (
            publication.files,
            publication.evidence,
            Some(publication.receipt),
        )
    } else {
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
        no_links(&staging, true)?;
        fs::create_dir(&staging)?;
        let incoming = if native {
            let bytes = execute(
                &context.gh,
                &strings(&[
                    "api",
                    "--hostname",
                    "github.com",
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
                    &format!("github.com/{repository}"),
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
        let receipt = if native {
            Some(
                json!({"schema":1,"sourceCommit":commit,"target":target,"manifestSha256":sha(incoming.get("native-build.json").unwrap()),"artifactDigest":digest,"artifactId":artifact_id,"runId":run_id}),
            )
        } else {
            None
        };
        (incoming, evidence, receipt)
    };
    #[cfg(windows)]
    if native && args.contains_key("--apply") {
        let hooks: Value = serde_json::from_slice(&incoming["hooks/hooks.json"])?;
        if hooks["hooks"]["Stop"][0]["hooks"][0]
            .get("commandWindows")
            .is_some()
        {
            windows_hook_prerequisites(
                Path::new(hook_command::WINDOWS_SH),
                env::var("PATHEXT").ok().as_deref(),
            )?;
        }
    }
    let wanted = hashes(&incoming);
    let local = hashes_at(&source)?;
    let cached = hashes_at(&cache_path(&installed)?)?;
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
    if let Some(receipt) = receipt {
        no_links(&receipt_path, true)?;
        private_work::validate_receipt(&receipt_path)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write as _;
        options
            .open(&receipt_path)?
            .write_all(&serde_json::to_vec_pretty(&receipt)?)?;
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
            hashes_at(&replacement)? == wanted,
            "SOURCE_COPY_VERIFICATION_FAILED"
        );
        // Same-volume rename preserves the previous source without deleting obsolete legacy assets.
        let backup = source.with_file_name(format!("remnifold-backup-{}", uuid::Uuid::new_v4()));
        no_links(&backup, true)?;
        ensure!(
            hashes_at(&source)? == local && hashes_at(&cache_path(&installed)?)? == cached,
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
        hashes_at(&source)? == wanted,
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
            && hashes_at(&source)? == wanted
            && hashes_at(&cache_path(&after)?)? == wanted,
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
