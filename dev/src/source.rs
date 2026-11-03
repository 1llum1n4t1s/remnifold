//! 配布元commitと作業ツリー・配布ソースの同一性を検証する。
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Component, Path},
    process::Command,
};

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_CONTENT: u64 = 512 * 1024 * 1024;

// 既存配布済みnative updaterのnative_source契約を維持する。
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
    ) || name.starts_with("skills/")
        && [".md", ".yaml", ".yml"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || name.starts_with("server/src/") && name.ends_with(".ts")
        || name.starts_with("server/migrations/") && name.ends_with(".sql")
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(repo)
        .output()
        .context("Git source verification unavailable")?;
    ensure!(output.status.success(), "Git source verification failed");
    ensure!(
        output.stdout.len() <= 32 * 1024 * 1024,
        "Git source tree exceeds verification bound"
    );
    Ok(output.stdout)
}

fn tree(repo: &Path, commit: &str) -> Result<BTreeMap<String, String>> {
    let bytes = git(repo, &["ls-tree", "-r", "-z", commit])?;
    let mut entries = BTreeMap::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = std::str::from_utf8(record).context("Non-UTF-8 Git source tree")?;
        let (metadata, name) = record.split_once('\t').context("Invalid Git tree record")?;
        let fields: Vec<_> = metadata.split(' ').collect();
        ensure!(
            fields.len() == 3 && matches!(fields[0], "100644" | "100755") && fields[1] == "blob",
            "Linked or nonregular tracked source rejected"
        );
        ensure!(
            Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
                && !name.contains('\\'),
            "Unsafe Git source path"
        );
        ensure!(
            entries.insert(name.into(), fields[2].into()).is_none(),
            "Duplicate Git source path"
        );
    }
    ensure!(!entries.is_empty(), "Empty source commit rejected");
    Ok(entries)
}

fn blob(size: u64, mut input: impl Read) -> Result<String> {
    ensure!(
        size <= MAX_CONTENT,
        "Source file exceeds verification bound"
    );
    let mut hash = Sha1::new();
    hash.update(format!("blob {size}\0").as_bytes());
    let mut buffer = [0; 64 * 1024];
    let mut count = 0;
    loop {
        let bytes = input.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        count += bytes as u64;
        ensure!(count <= size, "Source changed during verification");
        hash.update(&buffer[..bytes]);
    }
    ensure!(count == size, "Incomplete source bytes");
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn verify_checkout(repo: &Path, commit: &str) -> Result<()> {
    crate::package::safe_path(repo)?;
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Exact lowercase source commit SHA required"
    );
    ensure!(
        git(repo, &["cat-file", "-t", commit])? == b"commit\n",
        "Source identity must be a commit object"
    );
    git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--quiet",
            "--ignore-submodules=none",
            commit,
            "--",
        ],
    )?;
    ensure!(
        git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?.is_empty(),
        "Untracked source prevents commit verification"
    );
    // Git diff normalizes filters/EOLs and may trust index stat flags. Raw blob checks
    // also cover build inputs, generated-hook templates, and root version metadata.
    for (name, expected) in tree(repo, commit)? {
        let path = repo.join(name);
        crate::package::safe_path(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Nonregular tracked source rejected"
        );
        ensure!(
            blob(metadata.len(), fs::File::open(path)?)? == expected,
            "Tracked source bytes differ from commit"
        );
    }
    Ok(())
}

pub fn verify(repo: &Path, archive: &Path, commit: &str) -> Result<()> {
    verify_checkout(repo, commit)?;
    crate::package::safe_path(archive)?;
    let metadata = fs::symlink_metadata(archive)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_ARCHIVE,
        "Invalid source archive"
    );
    let expected: BTreeMap<_, _> = tree(repo, commit)?
        .into_iter()
        .filter(|(name, _)| native_source(name))
        .collect();
    ensure!(!expected.is_empty(), "Commit has no distributed sources");
    let mut actual = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for entry in tar::Archive::new(GzDecoder::new(fs::File::open(archive)?))
        .entries()?
        .raw(true)
    {
        let entry = entry?;
        let name =
            String::from_utf8(entry.path_bytes().into_owned()).context("Non-UTF-8 archive path")?;
        let name = name.trim_end_matches('/');
        ensure!(
            Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
                && !name.contains('\\')
                && !name.contains(':')
                && seen.insert(name.to_lowercase()),
            "Unsafe or duplicate archive path"
        );
        ensure!(
            name == "remnifold" || name.starts_with("remnifold/"),
            "Archive source escaped package root"
        );
        total = total
            .checked_add(entry.size())
            .context("Archive size overflow")?;
        ensure!(
            total <= MAX_CONTENT,
            "Archive content exceeds verification bound"
        );
        ensure!(
            entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
            "Archive link or special entry rejected"
        );
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let source = name
            .strip_prefix("remnifold/")
            .context("Invalid archive source root")?;
        if native_source(source) {
            actual.insert(source.to_owned(), blob(entry.size(), entry)?);
        }
    }
    ensure!(
        actual == expected,
        "Archive source paths or Git blob hashes differ from commit"
    );
    // Recheck in case inputs changed while the archive was read.
    verify_checkout(repo, commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::path::PathBuf;
    fn archive(path: &Path, entries: &[(&str, &[u8])]) -> Result<()> {
        let mut tar = tar::Builder::new(GzEncoder::new(
            fs::File::create(path)?,
            Compression::default(),
        ));
        for (name, bytes) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("remnifold/{name}"), *bytes)?;
        }
        tar.into_inner()?.finish()?;
        Ok(())
    }
    #[test]
    fn commit_source_verification_rejects_dirty_untracked_missing_and_changed_sources() -> Result<()>
    {
        let base = std::env::var_os("REMNIFOLD_VALIDATION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir()
                    .canonicalize()
                    .expect("test temporary root required")
            });
        let root = base.join(format!("source-{}", uuid::Uuid::new_v4()));
        crate::package::safe_path(&root)?;
        fs::create_dir_all(&root)?;
        let repo = root.join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init"])?;
        git(&repo, &["config", "core.autocrlf", "false"])?;
        git(&repo, &["config", "core.eol", "lf"])?;
        git(&repo, &["config", "core.hooksPath", "no-hooks"])?;
        fs::create_dir_all(repo.join("native/src"))?;
        fs::write(repo.join("README.md"), b"source\n")?;
        fs::write(repo.join("native/src/main.rs"), b"fn main() {}\n")?;
        fs::write(repo.join(".gitignore"), b"ignored\n")?;
        git(&repo, &["add", "."])?;
        git(
            &repo,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "isolated fixture",
            ],
        )?;
        let commit = String::from_utf8(git(&repo, &["rev-parse", "HEAD"])?)?
            .trim()
            .to_owned();
        let package = root.join("source.tar.gz");
        archive(&package, &[("README.md", b"source\n")])?;
        verify(&repo, &package, &commit)?;
        fs::write(repo.join("ignored"), b"ignored tool cache")?;
        verify(&repo, &package, &commit)?;
        fs::write(
            repo.join("native/src/main.rs"),
            b"fn main() { panic!(); }\n",
        )?;
        assert!(verify(&repo, &package, &commit).is_err());
        fs::write(repo.join("native/src/main.rs"), b"fn main() {}\n")?;
        fs::write(repo.join("untracked.ts"), b"extra source")?;
        assert!(verify(&repo, &package, &commit).is_err());
        git(&repo, &["add", "untracked.ts"])?;
        assert!(verify(&repo, &package, &commit).is_err());
        // Separate fixture state keeps every failure's data without deleting anything.
        let clean = root.join("clean");
        git(
            &root,
            &[
                "-c",
                "core.autocrlf=false",
                "clone",
                "--no-hardlinks",
                repo.to_str().context("fixture path")?,
                clean.to_str().context("fixture path")?,
            ],
        )?;
        git(&clean, &["config", "core.autocrlf", "false"])?;
        verify(&clean, &package, &commit)?;
        let missing = root.join("missing.tar.gz");
        archive(&missing, &[])?;
        assert!(verify(&clean, &missing, &commit).is_err());
        let changed = root.join("changed.tar.gz");
        archive(&changed, &[("README.md", b"altered\n")])?;
        assert!(verify(&clean, &changed, &commit).is_err());
        assert!(verify_checkout(&clean, &"F".repeat(40)).is_err());
        assert!(verify_checkout(&clean, &"0".repeat(40)).is_err());
        git(
            &clean,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "tag.gpgsign=false",
                "tag",
                "-a",
                "fixture-tag",
                "-m",
                "isolated annotated tag",
            ],
        )?;
        let tag = String::from_utf8(git(&clean, &["rev-parse", "refs/tags/fixture-tag"])?)?
            .trim()
            .to_owned();
        assert!(verify_checkout(&clean, &tag).is_err());
        Ok(())
    }
}
