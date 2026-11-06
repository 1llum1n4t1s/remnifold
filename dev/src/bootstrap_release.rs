//! GitHub が署名する immutable release で、検証済みの同一 Windows バイナリを配る。
use crate::release_manifest as manifest;
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Method, StatusCode, blocking::Client};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Command,
    time::Duration,
};
#[path = "../../native/src/updater/path_safety.rs"]
mod path_safety;
#[path = "../../native/src/updater/private_work.rs"]
mod private_work;
#[path = "../../native/src/updater/process.rs"]
mod process;

const ASSET: &str = "remnifold-bootstrap-x86_64-pc-windows-msvc.exe";
trait Remote {
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        bytes: Option<&[u8]>,
    ) -> Result<Option<Value>>;
}
struct GitHub {
    http: Client,
    token: String,
}
impl Remote for GitHub {
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        bytes: Option<&[u8]>,
    ) -> Result<Option<Value>> {
        let origin = if bytes.is_some() {
            "https://uploads.github.com"
        } else {
            "https://api.github.com"
        };
        let mut request = self
            .http
            .request(
                method.clone(),
                format!("{origin}/repos/{}/{path}", manifest::REPOSITORY),
            )
            .bearer_auth(&self.token)
            .header("User-Agent", "remnifold-bootstrap-publisher")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10");
        if let Some(value) = body {
            request = request.json(&value);
        }
        if let Some(bytes) = bytes {
            request = request
                .header("Content-Type", "application/octet-stream")
                .body(bytes.to_vec());
        }
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("GitHub bootstrap request failed"))?;
        if method == Method::GET && response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "GitHub bootstrap request rejected (HTTP {}); no existing release, tag or asset is replaced",
            response.status()
        );
        let mut bytes = Vec::new();
        response
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .context("GitHub bootstrap response failed")?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "Oversized GitHub release response"
        );
        Ok(Some(manifest::parse_strict(&bytes)?))
    }
}
fn require_safe_gh(bytes: &[u8]) -> Result<()> {
    let valid = (|| -> Option<bool> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        let line = lines.next()?;
        let version_and_date = line.strip_prefix("gh version ")?;
        let (version, date) = version_and_date.split_once(" (")?;
        let date = date.strip_suffix(')')?;
        if date.len() != 10
            || !date.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 4 | 7) {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            return None;
        }
        let parts = version
            .split('.')
            .map(|part| {
                if part.is_empty()
                    || (part.len() > 1 && part.starts_with('0'))
                    || !part.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                part.parse::<u64>().ok()
            })
            .collect::<Option<Vec<_>>>()?;
        if parts.len() != 3 {
            return None;
        }
        if let Some(url) = lines.next() {
            if url != format!("https://github.com/cli/cli/releases/tag/v{version}") {
                return None;
            }
        }
        if lines.next().is_some() {
            return None;
        }
        Some((parts[0], parts[1], parts[2]) >= (2, 93, 0))
    })()
    .unwrap_or(false);
    ensure!(valid, "GH_VERSION_UNSAFE");
    Ok(())
}
fn gh_environment(
    command: &mut Command,
    token: Option<&str>,
    temporary: &Path,
    authentication: bool,
) {
    command
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env("GH_HOST", "github.com")
        .env("TEMP", temporary)
        .env("TMP", temporary)
        .env("TMPDIR", temporary);
    if !authentication {
        command.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    }
    if let Some(token) = token {
        command.env("GH_TOKEN", token);
    }
}
fn gh(args: &[&str], token: Option<&str>, temporary: &Path, capture: bool) -> Result<Vec<u8>> {
    let mut command = Command::new("gh");
    command.args(args);
    gh_environment(
        &mut command,
        token,
        temporary,
        args == ["auth", "token", "--hostname", "github.com"],
    );
    let bytes = process::run(command, Duration::from_secs(90), 1024 * 1024)
        .context("Native GitHub CLI command failed")?;
    if capture {
        ensure!(
            bytes.len() <= 16384,
            "GitHub authentication output rejected"
        );
        Ok(bytes)
    } else {
        Ok(Vec::new())
    }
}
fn get(remote: &impl Remote, path: &str) -> Result<Option<Value>> {
    remote.request(Method::GET, path, None, None)
}
fn tag_identity(remote: &impl Remote, tag: &str, commit: &str) -> Result<bool> {
    let Some(reference) = get(remote, &format!("git/ref/tags/{tag}"))? else {
        return Ok(false);
    };
    ensure!(
        reference["ref"] == format!("refs/tags/{tag}"),
        "Bootstrap tag identity mismatch"
    );
    let mut object = reference["object"].clone();
    for _ in 0..8 {
        let sha = object["sha"].as_str().context("Missing tag object")?;
        ensure!(manifest::hex(sha, 40), "Invalid tag SHA");
        if object["type"] == "commit" {
            ensure!(
                sha == commit,
                "Existing bootstrap tag targets another commit"
            );
            return Ok(true);
        }
        ensure!(object["type"] == "tag", "Unsupported tag object");
        object =
            get(remote, &format!("git/tags/{sha}"))?.context("Missing annotated tag")?["object"]
                .clone();
    }
    bail!("Bootstrap tag nesting limit")
}
fn asset_matches(release: &Value, digest: &str, size: u64) -> Result<bool> {
    let assets = release["assets"]
        .as_array()
        .context("Missing bootstrap release assets")?;
    ensure!(assets.len() <= 1, "Unexpected bootstrap release assets");
    let Some(asset) = assets.first() else {
        return Ok(false);
    };
    ensure!(
        asset["name"] == ASSET
            && asset["state"] == "uploaded"
            && asset["size"].as_u64() == Some(size)
            && asset["digest"] == format!("sha256:{digest}"),
        "Existing bootstrap asset content mismatch"
    );
    Ok(true)
}
fn release_identity(release: &Value, tag: &str) -> Result<u64> {
    ensure!(
        release["tag_name"] == tag && release["prerelease"] == false,
        "Bootstrap release identity mismatch"
    );
    release["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Missing bootstrap release ID")
}
fn publish(
    remote: &impl Remote,
    version: &str,
    commit: &str,
    binary: &[u8],
    path: &Path,
    apply: bool,
) -> Result<()> {
    let settings = get(remote, "immutable-releases").context("Cannot read release immutability: an owner must enable immutable releases and provide read access before publication")?
        .context("Immutable releases are disabled; repository owner must enable Settings > General > Releases > Release immutability")?;
    ensure!(
        settings["enabled"] == true,
        "Immutable releases are disabled; repository owner must enable release immutability before publication"
    );
    let tag = format!("v{version}");
    let tag_exists = tag_identity(remote, &tag, commit)?;
    let digest = crate::package::hash(binary);
    let mut release = get(remote, &format!("releases/tags/{tag}"))?;
    if let Some(value) = &release {
        release_identity(value, &tag)?;
        ensure!(
            tag_exists || (value["draft"] == true && value["target_commitish"] == commit),
            "Existing bootstrap release has no verified tag or exact draft target"
        );
        let matches = asset_matches(value, &digest, binary.len() as u64)?;
        if value["draft"] == false {
            ensure!(
                value["immutable"] == true && matches,
                "Existing bootstrap release is mutable or incomplete"
            );
            state(path, &tag, "existing-publication-accepted")
                .context("Existing remote publication was accepted; local receipt save failed. Do not republish or replace assets")?;
            println!("Reused existing immutable bootstrap release {tag}");
            return Ok(());
        }
        ensure!(value["draft"] == true, "Invalid release draft state");
    }
    if !apply {
        println!(
            "Validated bootstrap publication plan for {tag}; pass --apply to create/publish the immutable release"
        );
        return Ok(());
    }
    if release.is_none() {
        // 競合はエラーとして停止し、次の invocation で全証拠を読み直す。
        release = Some(remote.request(Method::POST, "releases", Some(json!({"tag_name":tag,"target_commitish":commit,"name":tag,"draft":true,"prerelease":false,"body":"Verified native Windows bootstrap. Verify this immutable release and asset with the official GitHub CLI before execution."})), None)?.context("Missing created release")?);
    }
    let release = release.context("Missing bootstrap release")?;
    let id = release_identity(&release, &tag)?;
    ensure!(
        release["draft"] == true,
        "Bootstrap release changed before upload"
    );
    if !asset_matches(&release, &digest, binary.len() as u64)? {
        remote
            .request(
                Method::POST,
                &format!("releases/{id}/assets?name={ASSET}"),
                None,
                Some(binary),
            )?
            .context("Missing uploaded bootstrap asset")?;
    }
    let current = get(remote, &format!("releases/{id}"))?.context("Bootstrap draft disappeared")?;
    ensure!(
        release_identity(&current, &tag)? == id
            && asset_matches(&current, &digest, binary.len() as u64)?,
        "Bootstrap draft changed before publication"
    );
    // GitHub は draft に付く tag を publish 時に作る場合がある。
    let current_tag_exists = tag_identity(remote, &tag, commit)?;
    ensure!(
        current_tag_exists
            || (current["draft"] == true && !tag_exists && current["target_commitish"] == commit),
        "Bootstrap tag disappeared or draft target changed"
    );
    if current["draft"] == true {
        let settings = get(remote, "immutable-releases")?
            .context("Cannot reconfirm release immutability before publication")?;
        ensure!(
            settings["enabled"] == true,
            "Release immutability changed before publication"
        );
        state(path, &tag, "publication-attempted-outcome-unknown")?;
        remote
            .request(
                Method::PATCH,
                &format!("releases/{id}"),
                Some(json!({"draft":false,"make_latest":"false"})),
                None,
            )
            .map_err(|error| anyhow::anyhow!("Bootstrap publication request failed or outcome is unknown: {error}. Before retrying, inspect the release and this invocation receipt; do not replace the release or assets"))?;
    } else {
        ensure!(
            current["draft"] == false && current["immutable"] == true,
            "Concurrent bootstrap release lacks immutable identity evidence"
        );
    }
    state(path, &tag, "publication-accepted")
        .context("Remote publication was accepted; local receipt save failed. Do not republish or replace assets")?;
    println!("GitHub accepted bootstrap publication {tag}");
    Ok(())
}
fn state(path: &Path, tag: &str, status: &str) -> Result<()> {
    crate::package::safe_path(path)?;
    let parent = path.parent().context("Bootstrap state parent missing")?;
    let _guard = private_work::PrivateWorkGuard::open(parent)?;
    let destination = path.with_extension("release-state.json");
    private_work::validate_receipt(&destination)?;
    let temporary = parent.join(format!(
        "bootstrap-release-state-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec(
        &json!({"schema":1,"repository":manifest::REPOSITORY,"tag":tag,"status":status}),
    )?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    private_work::validate_receipt(&temporary)?;
    private_work::validate_receipt(&destination)?;
    fs::rename(&temporary, &destination)?;
    private_work::validate_receipt(&destination)?;
    Ok(())
}
pub fn run(repo: &Path, args: &[String]) -> Result<()> {
    let mut apply = false;
    let mut options = std::collections::BTreeMap::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--apply" {
            ensure!(!apply, "Duplicate --apply");
            apply = true;
            continue;
        }
        ensure!(
            matches!(arg.as_str(), "--source-commit" | "--work-root"),
            "Unknown bootstrap-release option"
        );
        let value = args
            .next()
            .filter(|value| !value.starts_with("--"))
            .context("Missing bootstrap-release option value")?;
        ensure!(
            options.insert(arg.as_str(), value.as_str()).is_none(),
            "Duplicate bootstrap-release option"
        );
    }
    let version =
        serde_json::from_slice::<Value>(&fs::read(repo.join("package.json"))?)?["version"]
            .as_str()
            .context("Missing version")?
            .to_owned();
    let commit = *options
        .get("--source-commit")
        .context("--source-commit <exact released SHA> is required")?;
    ensure!(
        manifest::version(&version) && manifest::hex(commit, 40),
        "Exact release version and source commit required"
    );
    crate::source::verify_checkout(repo, &commit)?;
    let root = std::path::PathBuf::from(
        options
            .get("--work-root")
            .context("--work-root <absolute external output> is required")?,
    );
    crate::package::safe_path(&root)?;
    let codex_home = codex_home()?;
    publication_roots(repo, &root, &codex_home)?;
    #[cfg(windows)]
    ensure!(
        root.starts_with("D:/Codex/remnifold"),
        "Bootstrap work root must be under D:/Codex/remnifold"
    );
    let _root_guard = private_work::PrivateWorkGuard::open(&root)?;
    publication_roots(repo, &root, &codex_home)?;
    let invocation = root.join(format!("bootstrap-release-{}", uuid::Uuid::new_v4()));
    let _invocation_guard = private_work::PrivateWorkGuard::open(&invocation)?;
    let temporary = invocation.join("tmp");
    let _temporary_guard = private_work::PrivateWorkGuard::open(&temporary)?;
    println!("Bootstrap publication invocation: {}", invocation.display());
    let version_output = gh(&["--version"], None, &temporary, true)
        .map_err(|_| anyhow::anyhow!("GH_VERSION_UNSAFE"))?;
    require_safe_gh(&version_output)?;
    let token = String::from_utf8(gh(
        &["auth", "token", "--hostname", "github.com"],
        None,
        &temporary,
        true,
    )?)
    .context("Invalid GitHub authentication output")?;
    let token = token.trim();
    ensure!(
        !token.is_empty() && !token.contains(['\r', '\n']),
        "Invalid GitHub authentication output"
    );
    let binary = crate::r2::bootstrap_binary(&version, commit, &invocation, token)?;
    let path = invocation.join(ASSET);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?
        .write_all(&binary)?;
    let remote = GitHub {
        http: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .build()?,
        token: token.into(),
    };
    publish(&remote, &version, commit, &binary, &path, apply)
}
fn publication_roots(repo: &Path, work: &Path, codex_home: &Path) -> Result<()> {
    let work = path_safety::physical_path(work)?;
    for protected in [repo, codex_home] {
        let protected = path_safety::physical_path(protected)?;
        ensure!(
            !path_safety::within(&protected, &work) && !path_safety::within(&work, &protected),
            "INVALID_WORK_ROOT"
        );
    }
    Ok(())
}
fn codex_home() -> Result<std::path::PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .context("Cannot resolve Codex home")?;
    let configured = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let configured = match configured.strip_prefix("~") {
        Ok(suffix) => home.join(suffix),
        Err(_) => configured,
    };
    let configured = if configured.is_absolute() {
        configured
    } else {
        std::env::current_dir()?.join(configured)
    };
    let mut normalized = std::path::PathBuf::new();
    for part in configured.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                ensure!(normalized.pop(), "Cannot resolve Codex home");
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };
    struct Fixture {
        replies: RefCell<VecDeque<(Method, String, Option<Value>)>>,
        writes: Cell<usize>,
        fail_patch: bool,
        after_patch_link: Option<(std::path::PathBuf, std::path::PathBuf)>,
    }
    impl Fixture {
        fn new(replies: Vec<(Method, &str, Option<Value>)>) -> Self {
            Self {
                replies: RefCell::new(
                    replies
                        .into_iter()
                        .map(|(m, p, v)| (m, p.into(), v))
                        .collect(),
                ),
                writes: Cell::new(0),
                fail_patch: false,
                after_patch_link: None,
            }
        }
    }
    impl Remote for Fixture {
        fn request(
            &self,
            method: Method,
            path: &str,
            _: Option<Value>,
            _: Option<&[u8]>,
        ) -> Result<Option<Value>> {
            if method != Method::GET {
                self.writes.set(self.writes.get() + 1);
            }
            let (expected_method, expected_path, result) = self
                .replies
                .borrow_mut()
                .pop_front()
                .context("Unexpected fixture request")?;
            ensure!(
                method == expected_method && path == expected_path,
                "Fixture request mismatch: {method} {path}"
            );
            ensure!(
                !(self.fail_patch && method == Method::PATCH),
                "Fixture PATCH outcome unknown (HTTP 503)"
            );
            if method == Method::PATCH {
                if let Some((receipt, linked)) = &self.after_patch_link {
                    fs::hard_link(receipt, linked)?;
                }
            }
            Ok(result)
        }
    }
    fn tag() -> Value {
        json!({"ref":"refs/tags/v1.0.17","object":{"type":"commit","sha":"a".repeat(40)}})
    }
    fn release(draft: bool, immutable: bool, assets: bool) -> Value {
        json!({"id":7,"tag_name":"v1.0.17","target_commitish":"a".repeat(40),"draft":draft,"immutable":immutable,"prerelease":false,
            "assets": if assets { vec![json!({"name":ASSET,"state":"uploaded","size":3,"digest":format!("sha256:{}",crate::package::hash(b"exe"))})] } else { vec![] }})
    }
    fn fixture_root(name: &str) -> Result<std::path::PathBuf> {
        let root = std::env::temp_dir();
        #[cfg(unix)]
        let root = root.canonicalize()?;
        Ok(root.join(format!("{name}-{}", uuid::Uuid::new_v4())))
    }
    fn run_fixture(f: &Fixture, apply: bool) -> Result<()> {
        let root = fixture_root("bootstrap-release-fixture")?;
        let _guard = private_work::PrivateWorkGuard::open(&root)?;
        publish(
            f,
            "1.0.17",
            &"a".repeat(40),
            b"exe",
            &root.join(ASSET),
            apply,
        )
    }
    #[test]
    fn disabled_or_unavailable_settings_prevent_writes() {
        for settings in [None, Some(json!({"enabled":false})), Some(json!({}))] {
            let f = Fixture::new(vec![(Method::GET, "immutable-releases", settings)]);
            assert!(run_fixture(&f, true).is_err());
            assert_eq!(f.writes.get(), 0);
        }
    }
    #[test]
    fn dry_run_never_creates_release_or_tag() {
        let f = Fixture::new(vec![
            (
                Method::GET,
                "immutable-releases",
                Some(json!({"enabled":true})),
            ),
            (Method::GET, "git/ref/tags/v1.0.17", None),
            (Method::GET, "releases/tags/v1.0.17", None),
        ]);
        run_fixture(&f, false).unwrap();
        assert_eq!(f.writes.get(), 0);
        assert!(f.replies.borrow().is_empty());
    }
    #[test]
    fn existing_release_requires_exact_asset_and_immutable_evidence() {
        for (draft, immutable, asset) in [
            (false, true, true),
            (false, false, true),
            (false, true, false),
        ] {
            let f = Fixture::new(vec![
                (
                    Method::GET,
                    "immutable-releases",
                    Some(json!({"enabled":true})),
                ),
                (Method::GET, "git/ref/tags/v1.0.17", Some(tag())),
                (
                    Method::GET,
                    "releases/tags/v1.0.17",
                    Some(release(draft, immutable, asset)),
                ),
            ]);
            assert_eq!(run_fixture(&f, true).is_ok(), immutable && asset);
            assert_eq!(f.writes.get(), 0);
            assert!(f.replies.borrow().is_empty());
        }
    }
    #[test]
    fn conflicting_tag_or_asset_is_never_replaced() {
        let mut wrong = tag();
        wrong["object"]["sha"] = json!("b".repeat(40));
        let f = Fixture::new(vec![
            (
                Method::GET,
                "immutable-releases",
                Some(json!({"enabled":true})),
            ),
            (Method::GET, "git/ref/tags/v1.0.17", Some(wrong)),
        ]);
        assert!(run_fixture(&f, true).is_err());
        assert_eq!(f.writes.get(), 0);
        let mut wrong = release(true, false, true);
        wrong["assets"][0]["digest"] = json!(format!("sha256:{}", "b".repeat(64)));
        let f = Fixture::new(vec![
            (
                Method::GET,
                "immutable-releases",
                Some(json!({"enabled":true})),
            ),
            (Method::GET, "git/ref/tags/v1.0.17", Some(tag())),
            (Method::GET, "releases/tags/v1.0.17", Some(wrong)),
        ]);
        assert!(run_fixture(&f, true).is_err());
        assert_eq!(f.writes.get(), 0);
    }
    #[test]
    fn draft_retry_and_concurrent_publication_verify_without_overwrite() {
        for (immutable, tag_exists) in [(true, true), (false, true), (true, false)] {
            let f = Fixture::new(vec![
                (
                    Method::GET,
                    "immutable-releases",
                    Some(json!({"enabled":true})),
                ),
                (Method::GET, "git/ref/tags/v1.0.17", None),
                (
                    Method::GET,
                    "releases/tags/v1.0.17",
                    Some(release(true, false, true)),
                ),
                (
                    Method::GET,
                    "releases/7",
                    Some(release(false, immutable, true)),
                ),
                (Method::GET, "git/ref/tags/v1.0.17", tag_exists.then(tag)),
            ]);
            assert_eq!(run_fixture(&f, true).is_ok(), immutable && tag_exists);
            assert_eq!(f.writes.get(), 0);
            assert!(f.replies.borrow().is_empty());
        }
    }
    #[test]
    fn first_publication_stops_after_one_accepted_patch() {
        for (fail_patch, fail_receipt) in [(false, false), (true, false), (false, true)] {
            let mut f = Fixture::new(vec![
                (
                    Method::GET,
                    "immutable-releases",
                    Some(json!({"enabled":true})),
                ),
                (Method::GET, "git/ref/tags/v1.0.17", None),
                (Method::GET, "releases/tags/v1.0.17", None),
                (Method::POST, "releases", Some(release(true, false, false))),
                (
                    Method::POST,
                    "releases/7/assets?name=remnifold-bootstrap-x86_64-pc-windows-msvc.exe",
                    Some(json!({"id":9})),
                ),
                (Method::GET, "releases/7", Some(release(true, false, true))),
                (Method::GET, "git/ref/tags/v1.0.17", None),
                (
                    Method::GET,
                    "immutable-releases",
                    Some(json!({"enabled":true})),
                ),
                (
                    Method::PATCH,
                    "releases/7",
                    Some(release(false, true, true)),
                ),
            ]);
            f.fail_patch = fail_patch;
            let root = fixture_root("bootstrap-release-patch-fixture").unwrap();
            let _guard = private_work::PrivateWorkGuard::open(&root).unwrap();
            let asset = root.join(ASSET);
            if fail_receipt {
                f.after_patch_link = Some((
                    asset.with_extension("release-state.json"),
                    root.join("receipt-link"),
                ));
            }
            let result = publish(&f, "1.0.17", &"a".repeat(40), b"exe", &asset, true);
            assert_eq!(result.is_ok(), !fail_patch && !fail_receipt);
            if fail_patch {
                let error = result.as_ref().unwrap_err().to_string();
                assert!(error.contains("publication request failed or outcome is unknown"));
                assert!(error.contains("HTTP 503"));
                assert!(error.contains("inspect the release and this invocation receipt"));
                assert!(error.contains("do not replace the release or assets"));
            }
            if fail_receipt {
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains("Remote publication was accepted; local receipt save failed")
                );
                assert!(error.contains("Do not republish or replace assets"));
            }
            let saved: Value = serde_json::from_slice(
                &fs::read(asset.with_extension("release-state.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                saved["status"],
                if fail_patch || fail_receipt {
                    "publication-attempted-outcome-unknown"
                } else {
                    "publication-accepted"
                }
            );
            assert_eq!(f.writes.get(), 3);
            assert!(f.replies.borrow().is_empty());
        }
    }
    #[test]
    fn changed_settings_prevent_draft_publication() {
        let f = Fixture::new(vec![
            (
                Method::GET,
                "immutable-releases",
                Some(json!({"enabled":true})),
            ),
            (Method::GET, "git/ref/tags/v1.0.17", Some(tag())),
            (
                Method::GET,
                "releases/tags/v1.0.17",
                Some(release(true, false, true)),
            ),
            (Method::GET, "releases/7", Some(release(true, false, true))),
            (Method::GET, "git/ref/tags/v1.0.17", Some(tag())),
            (
                Method::GET,
                "immutable-releases",
                Some(json!({"enabled":false})),
            ),
        ]);
        assert!(run_fixture(&f, true).is_err());
        assert_eq!(f.writes.get(), 0);
    }
    #[test]
    fn state_replacement_is_atomic_and_rejects_linked_targets() -> Result<()> {
        let root = fixture_root("bootstrap-release-state-test")?;
        let _guard = private_work::PrivateWorkGuard::open(&root)?;
        let asset = root.join(ASSET);
        state(&asset, "v1.0.17", "publication-attempted-outcome-unknown")?;
        state(&asset, "v1.0.17", "publication-accepted")?;
        let saved: Value =
            serde_json::from_slice(&fs::read(asset.with_extension("release-state.json"))?)?;
        assert_eq!(saved["status"], "publication-accepted");
        assert!(fs::read_dir(&root)?.all(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|extension| extension != "tmp")
        }));
        let sentinel = root.join("sentinel.json");
        fs::write(&sentinel, b"unchanged")?;
        let linked = root.join("linked.exe");
        fs::hard_link(&sentinel, linked.with_extension("release-state.json"))?;
        assert!(state(&linked, "v1.0.17", "publication-accepted").is_err());
        assert_eq!(fs::read(&sentinel)?, b"unchanged");
        let directory = root.join("directory.exe");
        fs::create_dir(directory.with_extension("release-state.json"))?;
        assert!(state(&directory, "v1.0.17", "publication-accepted").is_err());
        Ok(())
    }
    #[test]
    #[ignore]
    fn gh_environment_fixture() {
        let expected = std::env::var_os("REMNIFOLD_GH_EXPECTED_TEMP").expect("fixture temp");
        for name in ["TEMP", "TMP", "TMPDIR"] {
            assert_eq!(std::env::var_os(name), Some(expected.clone()));
        }
        assert_eq!(std::env::var("GH_HOST").unwrap(), "github.com");
        assert_eq!(std::env::var("GH_TOKEN").unwrap(), "fixture-token");
        for name in [
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(std::env::var_os(name).is_none());
        }
    }
    #[test]
    fn native_gh_environment_uses_only_private_invocation_temp() -> Result<()> {
        let root = fixture_root("bootstrap-release-gh-environment")?;
        let _guard = private_work::PrivateWorkGuard::open(&root)?;
        let mut child = Command::new(std::env::current_exe()?);
        child.args([
            "--ignored",
            "--exact",
            "bootstrap_release::tests::gh_environment_fixture",
        ]);
        child.env("REMNIFOLD_GH_EXPECTED_TEMP", &root);
        for name in [
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GITHUB_TOKEN",
        ] {
            child.env(name, "fixture-unused-token");
        }
        gh_environment(&mut child, Some("fixture-token"), &root, false);
        process::run(child, Duration::from_secs(15), 16384)?;
        Ok(())
    }
    #[test]
    fn unsafe_gh_versions_are_rejected_before_token_lookup() {
        for version in [
            "gh version 2.92.0 (2026-05-01)\n",
            "gh version 2.93.0-rc1 (2026-05-01)\n",
            "gh version 02.93.0 (2026-05-01)\n",
            "gh version 2.93.0\n",
            "gh version 2.93.0 (unknown)\n",
            "gh version 2.93.0 (2026-05-01)\nhttps://untrusted.invalid/\n",
            "gh version 2.93.0 (2026-05-01)\nextra\nextra\n",
            "gh version 9999999999999999999999.0.0 (2026-05-01)\n",
            "unknown\n",
        ] {
            assert_eq!(
                require_safe_gh(version.as_bytes()).unwrap_err().to_string(),
                "GH_VERSION_UNSAFE"
            );
        }
    }
    #[test]
    fn patched_stable_gh_versions_are_accepted() {
        for version in ["2.93.0", "2.100.1", "3.0.0"] {
            require_safe_gh(format!("gh version {version} (2026-05-28)\nhttps://github.com/cli/cli/releases/tag/v{version}\n").as_bytes()).unwrap();
        }
    }
    #[test]
    #[ignore]
    fn gh_auth_environment_fixture() {
        assert_eq!(std::env::var("GH_TOKEN").unwrap(), "fixture-owner-token");
        assert_eq!(
            std::env::var("GITHUB_TOKEN").unwrap(),
            "fixture-owner-fallback"
        );
        for name in ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"] {
            assert!(std::env::var_os(name).is_none());
        }
    }
    #[test]
    fn github_com_environment_authentication_is_preserved_only_for_token_lookup() -> Result<()> {
        let root = fixture_root("bootstrap-release-gh-auth-environment")?;
        let _guard = private_work::PrivateWorkGuard::open(&root)?;
        let mut child = Command::new(std::env::current_exe()?);
        child
            .args([
                "--ignored",
                "--exact",
                "bootstrap_release::tests::gh_auth_environment_fixture",
            ])
            .env("GH_TOKEN", "fixture-owner-token")
            .env("GITHUB_TOKEN", "fixture-owner-fallback")
            .env("GH_ENTERPRISE_TOKEN", "fixture-unused-token")
            .env("GITHUB_ENTERPRISE_TOKEN", "fixture-unused-token");
        gh_environment(&mut child, None, &root, true);
        process::run(child, Duration::from_secs(15), 16384)?;
        let mut version = Command::new("gh");
        version
            .env("GH_TOKEN", "fixture-owner-token")
            .env("GITHUB_TOKEN", "fixture-owner-fallback");
        gh_environment(&mut version, None, &root, false);
        for name in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ] {
            assert!(
                version
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
        Ok(())
    }
    #[test]
    fn publication_roots_reject_both_directions_and_codex_home_overlap() -> Result<()> {
        let root = fixture_root("bootstrap-release-physical-roots")?;
        let _guard = private_work::PrivateWorkGuard::open(&root)?;
        let source = root.join("source");
        let work = root.join("work");
        let codex = root.join("codex");
        for dir in [&source, &work, &codex] {
            fs::create_dir(dir)?;
        }
        assert!(publication_roots(&source, &work, &codex).is_ok());
        for invalid in [
            &root,
            &source,
            &source.join("missing/work"),
            &codex.join("missing/work"),
        ] {
            assert!(publication_roots(&source, invalid, &codex).is_err());
        }
        #[cfg(windows)]
        {
            let case_alias = std::path::PathBuf::from(source.to_string_lossy().to_uppercase());
            assert!(publication_roots(&source, &case_alias, &codex).is_err());
        }
        let managed_source = codex.join("worktrees/repository");
        fs::create_dir_all(&managed_source)?;
        assert!(publication_roots(&managed_source, &work, &codex).is_ok());
        assert!(
            publication_roots(
                &managed_source,
                &managed_source.join("missing/work"),
                &codex
            )
            .is_err()
        );
        assert!(publication_roots(&managed_source, &root, &codex).is_err());
        Ok(())
    }
}
