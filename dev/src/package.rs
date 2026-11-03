use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub const TARGETS: [&str; 5] = [
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];
pub fn executable(target: &str) -> Result<&'static str> {
    ensure!(
        TARGETS.contains(&target),
        "unsupported native target: {target}"
    );
    Ok(if target.contains("windows") {
        "bin/remnifold.exe"
    } else {
        "bin/remnifold"
    })
}
pub fn safe_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "absolute path required: {}",
        path.display()
    );
    for part in path.components() {
        ensure!(
            !matches!(part, Component::ParentDir),
            "parent traversal rejected"
        );
    }
    for ancestor in path.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "linked path rejected: {}",
                ancestor.display()
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "reparse path rejected: {}",
                    ancestor.display()
                );
            }
        }
    }
    Ok(())
}
fn allowed(name: &str) -> bool {
    if !portable_name(name) {
        return false;
    }
    if name.split('/').any(|p| {
        matches!(
            p,
            "node_modules" | "test" | "tests" | "__tests__" | "." | ".."
        ) || p.starts_with('.')
    }) && name != ".codex-plugin/plugin.json"
    {
        return false;
    }
    if name.contains(".test.") {
        return false;
    }
    matches!(
        name,
        ".codex-plugin/plugin.json"
            | "README.md"
            | "CHANGELOG.md"
            | "LICENSE"
            | "LICENSE.md"
            | "scripts/setup-server.ps1"
            | "server/package.json"
            | "server/tsconfig.json"
            | "server/wrangler.example.jsonc"
    ) || name.starts_with("server/src/") && name.ends_with(".ts")
        || name.starts_with("server/migrations/") && name.ends_with(".sql")
        || name.starts_with("skills/") && [".md", ".yaml", ".yml"].iter().any(|s| name.ends_with(s))
}
fn portable_name(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().any(|b| b < 32 || b"\\:<>\"|?*".contains(&b))
        && name.split('/').all(|part| {
            if part.is_empty() || matches!(part, "." | "..") || part.ends_with(['.', ' ']) {
                return false;
            }
            let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
            !["con", "prn", "aux", "nul"].contains(&stem.as_str())
                && !(stem.len() == 4
                    && (stem.starts_with("com") || stem.starts_with("lpt"))
                    && (b'1'..=b'9').contains(&stem.as_bytes()[3]))
        })
}
fn collect(repo: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        safe_path(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect(repo, &path, files)?;
        } else if meta.is_file() {
            let name = path
                .strip_prefix(repo)?
                .to_str()
                .context("non Unicode package path")?
                .replace('\\', "/");
            if allowed(&name) {
                files.insert(name, fs::read(path)?);
            }
        } else {
            bail!("nonregular package input");
        }
    }
    Ok(())
}
fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn valid_version(version: &str) -> bool {
    let (version, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    let identifiers = |value: &str, numeric_guard: bool| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!numeric_guard
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || part == "0"
                    || !part.starts_with('0'))
        })
    };
    if !build.is_none_or(|s| identifiers(s, false)) {
        return false;
    }
    let (core, pre) = version
        .split_once('-')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    core.split('.').count() == 3
        && core.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
        && pre.is_none_or(|s| identifiers(s, true))
}
fn native_version(repo: &Path) -> Result<String> {
    let path = repo.join("native/Cargo.toml");
    safe_path(&path)?;
    let source = fs::read_to_string(path)?;
    let mut package = false;
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            package = line == "[package]";
            continue;
        }
        if !package {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "version" {
                let value = value.trim();
                let version = value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                    .context("native package.version must be a quoted scalar")?;
                return Ok(version.into());
            }
        }
    }
    bail!("native package.version is missing")
}
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn decode_archive(reader: impl Read) -> Result<Vec<u8>> {
    decode_archive_bounded(reader, crate::release_manifest::MAX_EXPANDED)
}
fn decode_archive_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut expanded = Vec::new();
    GzDecoder::new(reader)
        .take(limit + 1)
        .read_to_end(&mut expanded)?;
    ensure!(
        expanded.len() as u64 <= limit,
        "oversized expanded native archive"
    );
    Ok(expanded)
}
fn identity(bytes: &[u8], target: &str) -> Result<()> {
    let arm = target.starts_with("aarch64");
    if target.contains("windows") {
        ensure!(bytes.get(..2) == Some(b"MZ"), "invalid PE executable");
        let offset =
            u32::from_le_bytes(bytes.get(60..64).context("truncated PE")?.try_into()?) as usize;
        ensure!(
            bytes.get(offset..offset + 4) == Some(b"PE\0\0")
                && bytes.get(offset + 4..offset + 6) == Some(&[0x64, 0x86]),
            "PE target mismatch"
        );
    } else if target.contains("linux") {
        ensure!(
            bytes.get(..6) == Some(&[0x7f, b'E', b'L', b'F', 2, 1]),
            "invalid ELF64 executable"
        );
        ensure!(
            bytes.get(18..20) == Some(if arm { &[183, 0] } else { &[62, 0] }),
            "ELF target mismatch"
        );
    } else {
        ensure!(
            bytes.get(..4) == Some(&[0xcf, 0xfa, 0xed, 0xfe]),
            "invalid Mach-O64 executable"
        );
        ensure!(
            bytes.get(4..8) == Some(if arm { &[12, 0, 0, 1] } else { &[7, 0, 0, 1] }),
            "Mach-O target mismatch"
        );
    }
    Ok(())
}
fn expected(
    repo: &Path,
    target: &str,
    binary: &Path,
    commit: &str,
) -> Result<BTreeMap<String, Vec<u8>>> {
    safe_path(repo)?;
    safe_path(binary)?;
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "exact lowercase source SHA required"
    );
    let exe = executable(target)?;
    let mut files = BTreeMap::new();
    for name in [
        ".codex-plugin/plugin.json",
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
        "LICENSE.md",
        "scripts/setup-server.ps1",
        "server/package.json",
        "server/tsconfig.json",
        "server/wrangler.example.jsonc",
    ] {
        let path = repo.join(name);
        if path.exists() {
            safe_path(&path)?;
            ensure!(
                fs::metadata(&path)?.is_file(),
                "regular source file required"
            );
            files.insert(name.into(), fs::read(path)?);
        }
    }
    for name in ["server/src", "server/migrations", "skills"] {
        collect(repo, &repo.join(name), &mut files)?;
    }
    for name in [
        ".codex-plugin/plugin.json",
        "README.md",
        "server/package.json",
        "server/tsconfig.json",
        "server/wrangler.example.jsonc",
    ] {
        ensure!(files.contains_key(name), "missing package source: {name}");
    }
    let plugin: Value = serde_json::from_slice(&files[".codex-plugin/plugin.json"])?;
    let version = plugin["version"]
        .as_str()
        .context("missing plugin version")?;
    ensure!(
        plugin["name"] == "remnifold" && plugin["interface"]["displayName"] == "Remnifold",
        "plugin identity mismatch"
    );
    for value in [
        &plugin["description"],
        &plugin["interface"]["shortDescription"],
    ] {
        ensure!(
            value.as_str().is_some_and(|s| !s.trim().is_empty()),
            "plugin description missing"
        );
    }
    ensure!(
        plugin["skills"] == "./skills/" && plugin["mcpServers"] == "./.mcp.json",
        "portable plugin references required"
    );
    ensure!(valid_version(version), "plugin SemVer required");
    ensure!(
        native_version(repo)? == version,
        "version mismatch: native/Cargo.toml"
    );
    for name in ["package.json", "server/package.json"] {
        let value: Value = serde_json::from_slice(&fs::read(repo.join(name))?)?;
        ensure!(value["version"] == version, "version mismatch: {name}");
    }
    let bytes = fs::read(binary)?;
    identity(&bytes, target)?;
    files.insert(exe.into(), bytes);
    files.insert(".mcp.json".into(), json_bytes(&json!({"mcpServers":{"remnifold":{"command":format!("./{exe}"),"args":["mcp"],"cwd":"."}}}))?);
    let mut hooks: Value = serde_json::from_slice(&fs::read(repo.join("hooks/hooks.json"))?)?;
    let events = hooks["hooks"].as_object_mut().context("missing hooks")?;
    ensure!(events.len() == 4, "unexpected hook events");
    for event in ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"] {
        let registrations = events
            .get_mut(event)
            .and_then(Value::as_array_mut)
            .context("missing hook event")?;
        ensure!(registrations.len() == 1, "unexpected hook registrations");
        let commands = registrations[0]["hooks"]
            .as_array_mut()
            .context("missing hook commands")?;
        ensure!(
            commands.len() == 1
                && commands[0]["type"] == "command"
                && commands[0]["timeout"].as_f64().unwrap_or(0.0) > 0.0,
            "invalid hook contract"
        );
        commands[0]["command"] = json!(format!("\"${{PLUGIN_ROOT}}/{exe}\" hook {event}"));
    }
    files.insert("hooks/hooks.json".into(), json_bytes(&hooks)?);
    let digests: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name, hash(bytes)))
        .collect();
    files.insert("native-build.json".into(), json_bytes(&json!({"schema":1,"target":target,"sourceCommit":commit,"version":version,"files":digests}))?);
    Ok(files)
}
/// 既存出力を拒否して新規アーカイブを作る。中間ディレクトリや削除は使わない。
pub fn build(
    repo: &Path,
    target: &str,
    binary: &Path,
    output: &Path,
    commit: &str,
) -> Result<PathBuf> {
    safe_path(output)?;
    ensure!(
        !output.starts_with(repo) || output.starts_with(repo.join("dist")),
        "repository archive output must be under dist"
    );
    ensure!(
        output
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".tar.gz")),
        "archive output must end with .tar.gz"
    );
    let files = expected(repo, target, binary, commit)?;
    fs::create_dir_all(output.parent().context("archive parent missing")?)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let mut archive = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    for (name, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(if name.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        archive.append_data(&mut header, format!("remnifold/{name}"), bytes.as_slice())?;
    }
    archive.into_inner()?.finish()?.flush()?;
    verify(repo, target, binary, output, commit)?;
    Ok(output.to_path_buf())
}
pub fn verify(
    repo: &Path,
    target: &str,
    binary: &Path,
    archive: &Path,
    commit: &str,
) -> Result<()> {
    safe_path(archive)?;
    ensure!(
        fs::metadata(archive)?.is_file()
            && fs::metadata(archive)?.len() <= crate::release_manifest::MAX_ARCHIVE,
        "invalid archive file"
    );
    let expected = expected(repo, target, binary, commit)?;
    let mut actual = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let expanded = decode_archive(fs::File::open(archive)?)?;
    for entry in tar::Archive::new(expanded.as_slice()).entries()?.raw(true) {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        let raw =
            String::from_utf8(entry.path_bytes().into_owned()).context("non UTF-8 archive path")?;
        let canonical = raw.trim_end_matches('/');
        ensure!(
            portable_name(canonical) && seen.insert(canonical.to_lowercase()),
            "unsafe or duplicate archive name"
        );
        ensure!(
            entry.header().as_ustar().is_some(),
            "USTAR package required"
        );
        ensure!(
            path.components().all(|p| matches!(p, Component::Normal(_))),
            "unsafe archive path"
        );
        if entry.header().entry_type().is_dir() {
            ensure!(
                entry.header().mode()? == 0o755 && entry.size() == 0,
                "invalid archive directory mode or size"
            );
            ensure!(
                path == Path::new("remnifold")
                    || expected
                        .keys()
                        .any(|name| Path::new(&format!("remnifold/{name}")).starts_with(&path)),
                "unexpected archive directory"
            );
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            "archive links and special entries rejected"
        );
        let name = path
            .strip_prefix("remnifold")?
            .to_str()
            .context("non Unicode archive name")?
            .replace('\\', "/");
        ensure!(
            expected.contains_key(&name) && !actual.contains_key(&name),
            "unexpected or duplicate archive entry: {name}"
        );
        ensure!(
            entry.header().mode()?
                == if name.starts_with("bin/") {
                    0o755
                } else {
                    0o644
                },
            "archive permission mismatch: {name}"
        );
        total += entry.size();
        ensure!(
            total <= crate::release_manifest::MAX_EXPANDED,
            "oversized native archive"
        );
        let mut bytes = Vec::new();
        entry
            .take(crate::release_manifest::MAX_EXPANDED + 1)
            .read_to_end(&mut bytes)?;
        actual.insert(name, bytes);
    }
    ensure!(
        actual == expected,
        "archive contents differ from source/binary/build identity"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoded_limit_includes_tar_headers_and_padding() {
        let tar = vec![0u8; 2048];
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        let bytes = gzip.finish().unwrap();
        assert_eq!(decode_archive_bounded(bytes.as_slice(), 2048).unwrap(), tar);
        assert!(decode_archive_bounded(bytes.as_slice(), 2047).is_err());
    }
    #[test]
    fn version_contract_matches_semver_accepted_by_the_native_updater() {
        for value in [
            "0.0.0",
            "1.0.15",
            "1.0.16-e2e",
            "1.0.15-alpha.0+abc.01",
            "1.0.15+abc",
        ] {
            assert!(valid_version(value), "{value}");
        }
        for value in [
            "01.0.15",
            "1.00.15",
            "1.0.15-a..b",
            "1.0.15-01",
            "1.0.15+",
            "1.0.15+a..b",
            "1.0.15+a+b",
        ] {
            assert!(!valid_version(value), "{value}");
        }
    }
    #[test]
    fn targets_require_matching_binary_headers() {
        let mut pe = vec![0u8; 128];
        pe[..2].copy_from_slice(b"MZ");
        pe[60] = 64;
        pe[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        assert!(identity(&pe, TARGETS[0]).is_ok());
        assert!(identity(&pe, TARGETS[3]).is_err());
        for (target, arm) in [(TARGETS[3], false), (TARGETS[4], true)] {
            let mut elf = vec![0u8; 64];
            elf[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
            elf[18] = if arm { 183 } else { 62 };
            assert!(identity(&elf, target).is_ok());
            assert!(identity(&elf, if arm { TARGETS[3] } else { TARGETS[4] }).is_err());
        }
        for (target, arm) in [(TARGETS[1], true), (TARGETS[2], false)] {
            let mut mach = vec![0xcf, 0xfa, 0xed, 0xfe];
            mach.extend_from_slice(if arm { &[12, 0, 0, 1] } else { &[7, 0, 0, 1] });
            assert!(identity(&mach, target).is_ok());
            assert!(identity(&mach, if arm { TARGETS[2] } else { TARGETS[1] }).is_err());
        }
        assert!(executable("unsupported").is_err());
    }
    #[test]
    fn source_allowlist_remains_a_subset_of_published_native_updater_contract() {
        for name in [
            "server/src/shared/memory-index.ts",
            "server/src/index.ts",
            "skills/setup-remnifold/SKILL.md",
            "scripts/setup-server.ps1",
            "LICENSE.md",
        ] {
            assert!(allowed(name), "{name}");
        }
        for name in [
            "client/memory-index.mjs",
            "server/pnpm-lock.yaml",
            "server/src/.env.ts",
            "server/src/tests/secret.ts",
            "server/src/index.test.ts",
            "native/src/main.rs",
            "server/wrangler.jsonc",
        ] {
            assert!(!allowed(name), "{name}");
        }
    }
    #[test]
    fn exact_archive_source_binary_hooks_and_metadata_are_verified() -> Result<()> {
        let root = std::env::var_os("REMNIFOLD_VALIDATION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("target/validation")
            });
        safe_path(&root)?;
        let root = root.join(format!("package-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let repo = root.join("source");
        for dir in [
            ".codex-plugin",
            "server/src",
            "server/migrations",
            "skills",
            "hooks",
            "native",
        ] {
            fs::create_dir_all(repo.join(dir))?;
        }
        for name in ["package.json", "server/package.json"] {
            fs::write(repo.join(name), b"{\"version\":\"1.0.15\"}")?;
        }
        fs::write(
            repo.join("native/Cargo.toml"),
            "[package]\nname = \"remnifold\"\nversion = \"1.0.15\"\n",
        )?;
        fs::write(
            repo.join(".codex-plugin/plugin.json"),
            serde_json::to_vec(
                &json!({"name":"remnifold","version":"1.0.15","description":"fixture","interface":{"displayName":"Remnifold","shortDescription":"fixture"},"skills":"./skills/","mcpServers":"./.mcp.json"}),
            )?,
        )?;
        for name in [
            "README.md",
            "LICENSE.md",
            "server/tsconfig.json",
            "server/wrangler.example.jsonc",
            "server/src/index.ts",
            "server/migrations/0001.sql",
            "skills/guide.md",
        ] {
            fs::write(repo.join(name), b"fixture source")?;
        }
        let mut hooks = json!({"hooks":{}});
        for event in ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"] {
            hooks["hooks"][event] = json!([{"matcher":"preserved","hooks":[{"type":"command","timeout":12,"async":true,"statusMessage":"preserved","command":"old"}]}]);
        }
        fs::write(repo.join("hooks/hooks.json"), serde_json::to_vec(&hooks)?)?;
        let binary = root.join("remnifold.exe");
        let mut pe = vec![0u8; 128];
        pe[..2].copy_from_slice(b"MZ");
        pe[60] = 64;
        pe[64..70].copy_from_slice(b"PE\0\0\x64\x86");
        fs::write(&binary, &pe)?;
        let archive = root.join("distribution.tar.gz");
        let commit = "a".repeat(40);
        build(&repo, TARGETS[0], &binary, &archive, &commit)?;
        assert!(build(&repo, TARGETS[0], &binary, &archive, &commit).is_err());
        let files = expected(&repo, TARGETS[0], &binary, &commit)?;
        for mode in [0o644, 0o4755, 0o777] {
            let changed = root.join(format!("mode-{mode:o}.tar.gz"));
            let mut tar = tar::Builder::new(GzEncoder::new(
                fs::File::create(&changed)?,
                Compression::default(),
            ));
            for (name, bytes) in &files {
                let mut header = tar::Header::new_ustar();
                header.set_size(bytes.len() as u64);
                header.set_mode(if name.starts_with("bin/") {
                    mode
                } else {
                    0o644
                });
                header.set_mtime(0);
                header.set_cksum();
                tar.append_data(&mut header, format!("remnifold/{name}"), bytes.as_slice())?;
            }
            tar.into_inner()?.finish()?;
            assert!(
                verify(&repo, TARGETS[0], &binary, &changed, &commit).is_err(),
                "mode-only tampering was accepted: {mode:o}"
            );
        }
        let launch: Value = serde_json::from_slice(&files["hooks/hooks.json"])?;
        assert_eq!(launch["hooks"]["Stop"][0]["matcher"], "preserved");
        assert_eq!(launch["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert_eq!(
            launch["hooks"]["Stop"][0]["hooks"][0]["statusMessage"],
            "preserved"
        );
        assert!(verify(&repo, TARGETS[0], &binary, &archive, &"b".repeat(40)).is_err());
        pe.push(1);
        fs::write(&binary, &pe)?;
        assert!(verify(&repo, TARGETS[0], &binary, &archive, &commit).is_err());
        pe.pop();
        fs::write(&binary, &pe)?;
        fs::write(
            repo.join("native/Cargo.toml"),
            "[package]\nversion = \"1.0.16\"\n",
        )?;
        assert!(verify(&repo, TARGETS[0], &binary, &archive, &commit).is_err());
        fs::write(
            repo.join("native/Cargo.toml"),
            "[package]\nversion = \"1.0.15\"\n",
        )?;
        fs::write(repo.join("README.md"), b"modified source")?;
        assert!(verify(&repo, TARGETS[0], &binary, &archive, &commit).is_err());
        fs::write(repo.join("server/package.json"), b"{\"version\":\"2.0.0\"}")?;
        assert!(expected(&repo, TARGETS[0], &binary, &commit).is_err());
        assert!(
            build(
                &repo,
                TARGETS[0],
                &binary,
                &repo.join("forbidden.tar.gz"),
                &commit
            )
            .is_err()
        );
        Ok(())
    }
}
