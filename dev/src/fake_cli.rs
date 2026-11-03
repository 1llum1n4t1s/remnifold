//! 隔離した更新fixture。利用者の本物のGitHub/Codex CLIは呼び出さない。
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
pub fn dispatch() -> Option<Result<()>> {
    let mut args = std::env::args();
    let exe = PathBuf::from(args.next()?);
    let kind = exe.file_stem()?.to_str()?;
    if kind == "remnifold-e2e-probe" {
        return Some((|| {
            match args.next().as_deref() {
                Some("stdin-ignore") => std::thread::sleep(std::time::Duration::from_secs(10)),
                Some("flood-before-stdin") => {
                    std::io::stdout().write_all(&vec![b'x'; 128 * 1024])?;
                    let count = std::io::copy(&mut std::io::stdin(), &mut std::io::sink())?;
                    write!(std::io::stdout(), "READ={count}")?;
                }
                Some("immediate") => {}
                Some("stdout-grandchild") => {
                    // 親が終了しても孫がpipeを保持する。孫自身は必ず10秒で終了する。
                    std::process::Command::new(std::env::current_exe()?)
                        .arg("stdin-ignore")
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .spawn()?;
                }
                _ => bail!("unknown isolated subprocess probe"),
            };
            Ok(())
        })());
    }
    if !["gh", "codex"].contains(&kind) {
        return None;
    }
    Some((|| {
        let path = PathBuf::from(args.next().context("isolated fake CLI state missing")?);
        run(kind, &path, &args.collect::<Vec<_>>())
    })())
}
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    crate::package::safe_path(source)?;
    crate::package::safe_path(destination)?;
    ensure!(
        fs::metadata(source)?.is_dir(),
        "fixture tree source missing"
    );
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        crate::package::safe_path(&path)?;
        let target = destination.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            ensure!(fs::metadata(&path)?.is_file(), "nonregular fixture file");
            fs::copy(path, target)?;
        }
    }
    Ok(())
}
fn run(kind: &str, state_path: &Path, args: &[String]) -> Result<()> {
    crate::package::safe_path(state_path)?;
    let mut state: Value = serde_json::from_slice(&fs::read(state_path)?)?;
    ensure!(
        state["isolatedFixture"] == true,
        "fake CLI only accepts isolated fixture state"
    );
    let s = |name: &str| {
        state[name]
            .as_str()
            .context(format!("missing fake CLI state: {name}"))
    };
    let root = state_path.parent().context("fixture state parent")?;
    for name in ["source", "codexHome", "logPath"] {
        let path = Path::new(s(name)?);
        crate::package::safe_path(path)?;
        ensure!(
            path.starts_with(root),
            "fake CLI path escaped isolated fixture: {name}"
        );
    }
    let mut log = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(s("logPath")?)?;
    writeln!(
        log,
        "{}",
        json!({"kind":kind,"args":args,"temp":std::env::var("TEMP").ok(),"tmp":std::env::var("TMP").ok(),"tmpdir":std::env::var("TMPDIR").ok()})
    )?;
    let output = |value: Value| -> Result<()> {
        println!("{value}");
        Ok(())
    };
    let fault = state["fault"].as_str().unwrap_or("");
    if kind == "gh" && args.first().map(String::as_str) == Some("api") {
        if args.get(1).map(String::as_str) == Some("--help") {
            println!(
                "gh api ENDPOINT{}",
                if state["r2"]["legacyGh"] == true {
                    ""
                } else {
                    " --allow-escape-sequences"
                }
            );
            return Ok(());
        }
        let mut position = 1;
        let mut hostname = None;
        let mut escape_flag = false;
        while let Some(arg) = args.get(position) {
            match arg.as_str() {
                "--hostname" => {
                    hostname = args.get(position + 1).map(String::as_str);
                    position += 2;
                }
                "--allow-escape-sequences" => {
                    escape_flag = true;
                    position += 1;
                }
                _ => break,
            }
        }
        ensure!(
            hostname.is_none_or(|host| host == "github.com")
                && (state["requireGithubHost"] != true || hostname == Some("github.com")),
            "GitHub API hostname was not pinned"
        );
        ensure!(
            !escape_flag
                || (state["r2"]["legacyGh"] != true && state["r2"]["rejectEscapeFlag"] != true),
            "unknown gh flag"
        );
        let endpoint_arg = args.get(position);
        if endpoint_arg == Some(&format!("repos/{}", s("repository")?)) {
            return output(json!({"id":77,"full_name":s("repository")?}));
        }
        let prefix = format!("repos/{}/", s("repository")?);
        let endpoint = endpoint_arg
            .context("missing API endpoint")?
            .strip_prefix(&prefix)
            .context("unexpected repository")?;
        let sha = s("sha")?;
        let version = s("version")?;
        if let Some(r2) = state.get("r2") {
            if endpoint == format!("contents/.github/remnifold-distribution.json?ref={sha}") {
                return output(
                    json!({"encoding":"base64","content":STANDARD.encode(r2["config"].as_str().context("R2 fixture config")?.as_bytes())}),
                );
            }
            if endpoint == "actions/workflows/ci.yml" {
                return output(json!({"id":42,"path":".github/workflows/ci.yml"}));
            }
            if endpoint.contains("/attempts/") && endpoint.ends_with("/jobs?per_page=100") {
                return output(r2["jobs"].clone());
            }
            if endpoint.starts_with("actions/jobs/") && endpoint.ends_with("/logs") {
                ensure!(
                    escape_flag || r2["legacyGh"] == true,
                    "modern GH log download requires raw ANSI flag"
                );
                ensure!(
                    r2["logUnavailable"] != true,
                    "R2 publication log unavailable"
                );
                std::io::stdout()
                    .write_all(r2["jobLog"].as_str().context("R2 job log")?.as_bytes())?;
                return Ok(());
            }
            if endpoint == format!("actions/runs?head_sha={sha}&event=push&per_page=100") {
                return output(json!({"workflow_runs":[r2["latestRun"]]}));
            }
            if endpoint.starts_with("actions/runs/") && endpoint.contains("/attempts/") {
                return output(r2["publicationRun"].clone());
            }
            if endpoint == format!("commits/{sha}/statuses?per_page=100&page=1") {
                return output(r2["statuses"].clone());
            }
            ensure!(
                !endpoint.contains("/artifacts"),
                "R2 must never fall back to Actions artifacts"
            );
        }
        if state["historyUnavailable"] == true && endpoint.starts_with("actions/runs") {
            return output(json!({"workflow_runs":[],"artifacts":[]}));
        }
        if endpoint == format!("git/ref/heads/release/{version}") {
            return output(
                json!({"ref":format!("refs/heads/release/{version}"),"object":{"type":"commit","sha":if fault=="ref"{"not-a-commit"}else{sha}}}),
            );
        }
        if endpoint == format!("contents/.codex-plugin/plugin.json?ref={sha}") {
            let mut manifest = state["manifest"].clone();
            manifest["version"] = json!(if fault == "version" {
                "999.0.0"
            } else {
                version
            });
            return output(
                json!({"encoding":"base64","content":STANDARD.encode(serde_json::to_vec(&manifest)?)}),
            );
        }
        if endpoint == format!("actions/runs?head_sha={sha}&event=push&per_page=100") {
            return output(
                json!({"workflow_runs":[{"id":100,"run_attempt":1,"name":"CI","head_sha":if fault=="ci-sha"{"ffffffffffffffffffffffffffffffffffffffff"}else{sha},"event":"push","head_branch":format!("release/{version}"),"status":"completed","conclusion":if fault=="ci"{"failure"}else{"success"}}]}),
            );
        }
        if endpoint == "actions/runs/100/artifacts?per_page=100" {
            return output(
                json!({"artifacts":if fault=="artifact"{json!([])}else{json!([{"id":200,"name":state["artifactName"],"digest":if fault=="digest"{json!(format!("sha256:{}","0".repeat(64)))}else{state["artifactDigest"].clone()},"expired":fault=="expired","expires_at":"2999-01-01T00:00:00Z"}])}}),
            );
        }
        if endpoint == "actions/artifacts/200/zip" {
            if fault == "archive" {
                std::io::stdout().write_all(b"invalid zip")?;
            } else {
                std::io::stdout().write_all(&fs::read(s("zipPath")?)?)?;
            }
            return Ok(());
        }
        if endpoint == format!("git/trees/{sha}?recursive=1") {
            let mut tree = state["tree"].clone();
            if fault == "hash" {
                tree[0]["sha"] = json!("0".repeat(40));
            }
            return output(json!({"truncated":fault=="tree","tree":tree}));
        }
        bail!("unexpected GitHub fixture API: {endpoint}");
    }
    if kind == "codex" && args == ["plugin", "list", "--marketplace", "personal", "--json"] {
        return output(
            json!({"installed":[{"pluginId":"remnifold@personal","installed":true,"enabled":true,"version":state["installedVersion"],"source":{"source":"local","path":state["source"]}}]}),
        );
    }
    if kind == "codex" && args == ["plugin", "add", "remnifold@personal", "--json"] {
        ensure!(state["failAdd"] != true, "official install fixture failed");
        let source = PathBuf::from(s("source")?);
        let home = PathBuf::from(s("codexHome")?);
        let manifest: Value =
            serde_json::from_slice(&fs::read(source.join(".codex-plugin/plugin.json"))?)?;
        let version = manifest["version"].as_str().context("fixture version")?;
        copy_tree(
            &source,
            &home.join("plugins/cache/personal/remnifold").join(version),
        )?;
        state["installedVersion"] = json!(version);
        fs::write(state_path, serde_json::to_vec(&state)?)?;
        return output(json!({"installed":true}));
    }
    bail!("unexpected isolated fixture command: {kind} {args:?}")
}
