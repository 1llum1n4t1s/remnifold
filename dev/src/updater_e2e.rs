use crate::{
    fake_cli::copy_tree,
    package::{hash, safe_path},
    server_tests::{API_KEY, start_fixture},
    tools::Toolchain,
};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
fn files(root: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            safe_path(&path)?;
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                out.insert(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    hash(&fs::read(path)?),
                );
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    if root.exists() {
        walk(root, root, &mut out)?;
    }
    Ok(out)
}
// 旧1.0.15 updaterの公開tree契約を独立oracleとして残す。
fn old_native_source(name: &str) -> bool {
    if name.contains(".test.")
        || name
            .split('/')
            .any(|p| ["node_modules", "test", "tests", "__tests__"].contains(&p))
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
    ) || name.starts_with("skills/") && [".md", ".yaml", ".yml"].iter().any(|s| name.ends_with(s))
        || name.starts_with("server/src/") && name.ends_with(".ts")
        || name.starts_with("server/migrations/") && name.ends_with(".sql")
}
fn checkout_sources(repo: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    fn visit(repo: &Path, path: &Path, result: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
        safe_path(path)?;
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(repo, &entry?.path(), result)?;
            }
        } else {
            let name = path
                .strip_prefix(repo)?
                .to_string_lossy()
                .replace('\\', "/");
            if old_native_source(&name) {
                result.insert(name, fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    for name in [
        ".codex-plugin/plugin.json",
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
        "LICENSE.md",
        "scripts/setup-server.mjs",
        "client/unicode-text.mjs",
        "server/package.json",
        "server/pnpm-lock.yaml",
        "server/pnpm-workspace.yaml",
        "server/tsconfig.json",
        "server/wrangler.example.jsonc",
        "skills",
        "server/src",
        "server/migrations",
    ] {
        let path = repo.join(name);
        if path.exists() {
            visit(repo, &path, &mut result)?;
        }
    }
    Ok(result)
}
fn unpack(archive: &Path, destination: &Path) -> Result<PathBuf> {
    safe_path(archive)?;
    safe_path(destination)?;
    fs::create_dir_all(destination)?;
    let mut seen = BTreeMap::new();
    let mut total = 0;
    for item in tar::Archive::new(GzDecoder::new(fs::File::open(archive)?)).entries()? {
        let mut entry = item?;
        let path = entry.path()?.into_owned();
        ensure!(
            path.starts_with("remnifold")
                && path.components().all(|p| matches!(p, Component::Normal(_))),
            "unsafe fixture archive path"
        );
        if entry.header().entry_type().is_dir() {
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            "fixture archive links rejected"
        );
        ensure!(
            seen.insert(path.clone(), ()).is_none(),
            "duplicate fixture archive path"
        );
        total += entry.size();
        ensure!(total < 512 * 1024 * 1024, "oversized fixture archive");
        let target = destination.join(path);
        safe_path(&target)?;
        fs::create_dir_all(target.parent().unwrap())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        std::io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target, fs::Permissions::from_mode(entry.header().mode()?))?;
        }
    }
    Ok(destination.join("remnifold"))
}
fn fixture_archive(root: &Path, output: &Path) -> Result<()> {
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
        flate2::Compression::default(),
    ));
    for name in files(root)?.keys() {
        let bytes = fs::read(root.join(name))?;
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
    archive.into_inner()?.finish()?;
    Ok(())
}
#[path = "../../native/src/test_links.rs"]
mod test_links;

pub(crate) fn directory_link(target: &Path, link: &Path) -> Result<()> {
    test_links::directory_link(target, link)
}
struct Fixture {
    legacy_temp: bool,
    root: PathBuf,
    source: PathBuf,
    home: PathBuf,
    work: PathBuf,
    state: PathBuf,
    config: PathBuf,
    binary: PathBuf,
    version: String,
    sha: String,
    expected: BTreeMap<String, String>,
}
// umask が 0002 の環境でも、更新領域の祖先を共有書込み可能にしない。
pub(crate) fn create_private_fixture_dir(path: &Path, recursive: bool) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
impl Fixture {
    fn calls(&self) -> Result<Vec<Value>> {
        Ok(fs::read_to_string(self.root.join("calls.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?)
    }
    fn adds(&self) -> Result<usize> {
        Ok(self
            .calls()?
            .iter()
            .filter(|v| v["kind"] == "codex" && v["args"][1] == "add")
            .count())
    }
    fn snapshot(&self) -> Result<Value> {
        Ok(json!({"source":files(&self.source)?,"cache":files(&self.home.join("plugins/cache"))?}))
    }
    fn edit_state(&self, name: &str, value: Value) -> Result<()> {
        let mut state: Value = serde_json::from_slice(&fs::read(&self.state)?)?;
        state[name] = value;
        fs::write(&self.state, serde_json::to_vec(&state)?)?;
        Ok(())
    }
    fn run(&self, mode: &str, work: Option<&Path>, from_source: bool) -> Result<Value> {
        self.run_hook_environment(mode, work, from_source, None)
    }
    fn bootstrap(&self, modes: &[&str], config: Option<&Path>) -> Result<Value> {
        let mut command = Command::new(&self.binary);
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy().to_ascii_uppercase();
            if (key.starts_with("REMNIFOLD_") && key != "REMNIFOLD_KEEP_TEST_STATE")
                || key.starts_with("KAGAYOI_MEMORY_")
                || key.starts_with("SUPERMEMORY_")
                || key == "CLOUDFLARE_MEMORY_API_KEY"
                || ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"].contains(&key.as_str())
            {
                command.env_remove(name);
            }
        }
        command
            .args([
                "plugin-bootstrap",
                "--version",
                &self.version,
                "--work-root",
            ])
            .arg(&self.work)
            .arg("--config")
            .arg(config.unwrap_or(&self.config))
            .args(modes)
            .env("CODEX_HOME", &self.home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("GH_TOKEN", "isolated-updater-token-canary")
            .env("GH_HOST", "hostile.example")
            .env("NO_PROXY", "*")
            .env("TEMP", self.root.join("temp"))
            .env("TMP", self.root.join("temp"))
            .env("TMPDIR", self.root.join("temp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        command.env(
            "PATH",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\System32",
        );
        #[cfg(not(windows))]
        command.env("PATH", "");
        let output = crate::client_e2e::process_output(command, b"")?;
        ensure!(
            output.stderr.is_empty()
                || String::from_utf8_lossy(&output.stderr).trim() == "PLUGIN_BOOTSTRAP_FAILED",
            "bootstrap emitted unstructured diagnostics"
        );
        ensure!(
            !output
                .stdout
                .windows(b"isolated-updater-token-canary".len())
                .any(|w| w == b"isolated-updater-token-canary")
                && !output
                    .stderr
                    .windows(b"isolated-updater-token-canary".len())
                    .any(|w| w == b"isolated-updater-token-canary"),
            "bootstrap diagnostics exposed token"
        );
        let mut result: Value = serde_json::from_slice(&output.stdout)
            .context("bootstrap must return structured JSON")?;
        result["exitCode"] = json!(output.status.code().unwrap_or(-1));
        for call in self.calls()? {
            for key in ["temp", "tmp", "tmpdir"] {
                ensure!(
                    Path::new(call[key].as_str().context("bootstrap child TEMP missing")?)
                        .starts_with(&self.root),
                    "bootstrap child {key} escaped invocation root"
                );
            }
        }
        Ok(result)
    }
    fn run_hook_environment(
        &self,
        mode: &str,
        work: Option<&Path>,
        from_source: bool,
        pathext: Option<Option<&str>>,
    ) -> Result<Value> {
        let work = work.unwrap_or(&self.work);
        let mut command = Command::new(if from_source {
            self.source.join(if cfg!(windows) {
                "bin/remnifold.exe"
            } else {
                "bin/remnifold"
            })
        } else {
            self.binary.clone()
        });
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy().to_ascii_uppercase();
            if key.starts_with("REMNIFOLD_")
                || key.starts_with("KAGAYOI_MEMORY_")
                || key.starts_with("SUPERMEMORY_")
                || key == "CLOUDFLARE_MEMORY_API_KEY"
                || ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"].contains(&key.as_str())
            {
                command.env_remove(name);
            }
        }
        command.env("NO_PROXY", "*");
        command
            .args(["plugin-update", "--version", &self.version, "--work-root"])
            .arg(work)
            .arg("--config")
            .arg(&self.config)
            .arg(mode)
            .env("CODEX_HOME", &self.home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("GH_TOKEN", "isolated-updater-token-canary")
            .env("GH_HOST", "hostile.example")
            .env("TEMP", self.root.join("temp"))
            .env("TMP", self.root.join("temp"))
            .env(
                "TMPDIR",
                if self.legacy_temp {
                    work.to_path_buf()
                } else {
                    self.root.join("temp")
                },
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        command.env(
            "PATH",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\System32",
        );
        #[cfg(not(windows))]
        command.env("PATH", "");
        if let Some(pathext) = pathext {
            if let Some(value) = pathext {
                command.env("PATHEXT", value);
            } else {
                command.env_remove("PATHEXT");
            }
        }
        let mut child = command.spawn()?;
        let mut out = child.stdout.take().unwrap();
        let mut err = child.stderr.take().unwrap();
        let stdout = thread::spawn(move || {
            let mut b = Vec::new();
            out.read_to_end(&mut b).map(|_| b)
        });
        let stderr = thread::spawn(move || {
            let mut b = Vec::new();
            err.read_to_end(&mut b).map(|_| b)
        });
        let deadline = Instant::now() + Duration::from_secs(90);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("updater fixture timeout");
            }
            thread::sleep(Duration::from_millis(25));
        };
        let bytes = stdout
            .join()
            .map_err(|_| anyhow::anyhow!("stdout thread"))??;
        let errors = stderr
            .join()
            .map_err(|_| anyhow::anyhow!("stderr thread"))??;
        ensure!(
            errors.is_empty()
                || String::from_utf8_lossy(&errors).contains("LOCAL_PLUGIN_UPDATE_FAILED"),
            "unstructured updater diagnostics: {}",
            String::from_utf8_lossy(&errors)
        );
        let mut result: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("updater output {}", String::from_utf8_lossy(&bytes)))?;
        result["exitCode"] = json!(status.code().unwrap_or(-1));
        for call in self.calls()? {
            for key in ["temp", "tmp", "tmpdir"] {
                let actual = PathBuf::from(call[key].as_str().context("child TEMP missing")?);
                ensure!(actual == work, "updater child {key} escaped work root");
            }
        }
        Ok(result)
    }
    fn success(&self, mode: &str, status: &str) -> Result<()> {
        let result = self.run(mode, None, false)?;
        ensure!(
            result["exitCode"] == 0
                && result["status"] == status
                && result["version"] == self.version
                && result["sha"] == self.sha,
            "updater {mode}: {result}"
        );
        Ok(())
    }
    fn installed(&self) -> Result<()> {
        let state: Value = serde_json::from_slice(&fs::read(&self.state)?)?;
        ensure!(
            state["installedVersion"] == self.version,
            "installed version"
        );
        ensure!(
            files(&self.source)? == self.expected,
            "updated source mismatch"
        );
        ensure!(
            files(
                &self
                    .home
                    .join("plugins/cache/personal/remnifold")
                    .join(&self.version)
            )? == self.expected,
            "updated cache mismatch"
        );
        Ok(())
    }
}
struct MigrationState {
    project: PathBuf,
    payload: Value,
    files: BTreeMap<PathBuf, Vec<u8>>,
}
// 公開配信fixtureはHTTP境界も実際に通し、認証情報と書込みの漏出を記録する。
struct R2Fixture {
    base: String,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl R2Fixture {
    fn shutdown(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    fn start(objects: BTreeMap<String, Vec<u8>>, redirect: bool, trace: PathBuf) -> Result<Self> {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let base = format!("http://{}", listener.local_addr()?);
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = calls.clone();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while stopped.try_recv().is_err() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Windowsではacceptしたsocketもlistenerのnonblockingを継承する。
                        // 要求到着前のWouldBlockを空のGET/404として扱わない。
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        let mut request = Vec::new();
                        let mut buffer = [0; 2048];
                        while request.len() < 32768 && !request.windows(4).any(|b| b == b"\r\n\r\n")
                        {
                            match stream.read(&mut buffer) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => request.extend_from_slice(&buffer[..n]),
                            }
                        }
                        let request = String::from_utf8_lossy(&request).into_owned();
                        let path = request
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("")
                            .trim_start_matches('/')
                            .to_owned();
                        log.lock().unwrap().push(request);
                        let body = objects.get(&path);
                        let status = if redirect {
                            "302 Found"
                        } else if body.is_some() {
                            "200 OK"
                        } else {
                            "404 Not Found"
                        };
                        let bytes = body.map(Vec::as_slice).unwrap_or(b"missing");
                        let header_result = write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nLocation: http://127.0.0.1:1/forbidden\r\n\r\n",
                            bytes.len()
                        );
                        let body_result = stream.write_all(bytes);
                        if let Ok(mut trace) = fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&trace)
                        {
                            let _ = writeln!(
                                trace,
                                "{}",
                                json!({"path":path,"status":status,"bytes":bytes.len(),"headerWrite":format!("{header_result:?}"),"bodyWrite":format!("{body_result:?}")})
                            );
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            base,
            calls,
            stop,
            worker: Some(worker),
        })
    }
    fn secure_requests(&self) -> Result<()> {
        for request in self.calls.lock().unwrap().iter() {
            let lower = request.to_ascii_lowercase();
            ensure!(
                request.starts_with("GET /")
                    && !lower.contains("authorization:")
                    && !lower.contains("cookie:"),
                "R2 request leaked credentials or mutated data"
            );
        }
        Ok(())
    }
}
impl Drop for R2Fixture {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn bootstrap_case(f: &Fixture, fault: &str, scenarios: &mut Vec<&'static str>) -> Result<()> {
    let before = f.snapshot()?;
    if fault == "success" || fault == "recovery" {
        let config = f.root.join("first-use-config.json");
        let mut settings: Value = serde_json::from_slice(&fs::read(&f.config)?)?;
        settings["commands"]
            .as_object_mut()
            .context("fixture commands")?
            .remove("codex");
        let missing_source = f.root.join("not-installed-plugin");
        settings["localPlugin"]["sourcePath"] = json!(missing_source);
        fs::write(&config, serde_json::to_vec(&settings)?)?;
        let checked = f.bootstrap(&[], Some(&config))?;
        ensure!(
            checked["exitCode"] == 0 && before == f.snapshot()? && f.adds()? == 0,
            "bootstrap default must be read-only: {checked}"
        );
        ensure!(
            !missing_source.exists() && f.calls()?.iter().all(|call| call["kind"] != "codex"),
            "first-use bootstrap default created plugin state or consulted Codex"
        );
        ensure!(
            !files(&f.work)?
                .keys()
                .any(|path| path.starts_with("verified-") || path.contains("/verified-")),
            "bootstrap default created an apply receipt"
        );
        if fault == "recovery" {
            f.edit_state("failAdd", json!(true))?;
            let failed = f.bootstrap(&["--apply"], None)?;
            ensure!(
                failed["exitCode"] == 1 && failed["code"].is_string(),
                "bootstrap ignored official CLI failure: {failed}"
            );
            f.edit_state("failAdd", json!(false))?;
        }
        let applied = f.bootstrap(&["--apply"], None)?;
        ensure!(
            applied["exitCode"] == 0,
            "bootstrap apply failed: {applied}"
        );
        f.installed()?;
        let count = f.adds()?;
        ensure!(
            count == if fault == "recovery" { 2 } else { 1 },
            "bootstrap install retry count changed"
        );
        let snapshot = f.snapshot()?;
        if fault == "success" {
            let verified = f.bootstrap(&["--verify-only"], None)?;
            ensure!(
                verified["exitCode"] == 0
                    && verified["status"] == "verified"
                    && f.adds()? == count
                    && snapshot == f.snapshot()?,
                "bootstrap receipt verification mutated installation: {verified}"
            );
        }
        let repeated = f.bootstrap(&["--apply"], None)?;
        ensure!(
            repeated["exitCode"] == 0 && f.adds()? == count && snapshot == f.snapshot()?,
            "bootstrap retry reinstalled or changed current plugin: {repeated}"
        );
        let invalid = f.bootstrap(&["--apply", "--download-only"], None)?;
        ensure!(
            invalid["exitCode"] == 1 && f.adds()? == count && snapshot == f.snapshot()?,
            "bootstrap accepted conflicting modes"
        );
    } else {
        // Invalid publication fails before download delivery, so all fixture output remains on D:.
        // No Codex command is configured or available through PATH for this first-download path.
        let config = f.root.join("download-only-config.json");
        let mut settings: Value = serde_json::from_slice(&fs::read(&f.config)?)?;
        settings["commands"]
            .as_object_mut()
            .context("fixture commands")?
            .remove("codex");
        fs::write(&config, serde_json::to_vec(&settings)?)?;
        let result = f.bootstrap(&["--download-only"], Some(&config))?;
        let expected = match fault {
            "archive-tamper" => "R2_ARCHIVE_DIGEST_MISMATCH",
            "manifest-tamper" => "R2_MANIFEST_DIGEST_MISMATCH",
            "witness-missing" => "R2_WITNESS_MISMATCH",
            "redirect" => "R2_DOWNLOAD_FAILED",
            _ => anyhow::bail!("unknown bootstrap scenario"),
        };
        ensure!(
            result["exitCode"] == 1
                && result["code"] == expected
                && before == f.snapshot()?
                && f.adds()? == 0,
            "bootstrap invalid-publication contract ({fault}): {result}"
        );
        ensure!(
            f.calls()?.iter().all(|call| call["kind"] != "codex"),
            "download-only resolved or called Codex"
        );
    }
    ensure!(
        !f.calls()?
            .iter()
            .any(|call| call["args"].to_string().contains("artifacts")),
        "bootstrap downgraded from R2 to legacy artifacts"
    );
    scenarios.push(Box::leak(format!("bootstrap-{fault}").into_boxed_str()));
    Ok(())
}

fn r2_cases(
    make: &impl Fn(&str, &Path, bool, &str, &str) -> Result<Fixture>,
    package: &Path,
    archive: &Path,
    target: &str,
    previous: Option<&Path>,
    endpoint: Option<&str>,
    scenarios: &mut Vec<&'static str>,
    bootstrap: bool,
) -> Result<()> {
    const REPO: &str = "1llum1n4t1s/remnifold";
    let archive_bytes = fs::read(archive)?;
    let native_hash = hash(&fs::read(package.join("native-build.json"))?);
    let focus = std::env::var("REMNIFOLD_UPDATER_R2_FOCUS").ok();
    for fault in [
        "success",
        "legacy-success",
        "schema-cross",
        "witness-schema-cross",
        "recovery",
        "same-version",
        "binary-tamper",
        "config-blob",
        "index-missing",
        "anchor-missing",
        "anchor-creator",
        "anchor-hash",
        "manifest-tamper",
        "archive-tamper",
        "archive-missing",
        "native-hash",
        "run-id",
        "run-attempt",
        "run-commit",
        "run-target",
        "run-repository-id",
        "run-head-repository-id",
        "run-path",
        "run-workflow",
        "latest-failed",
        "missing-target",
        "duplicate-target",
        "unsafe-key",
        "size",
        "redirect",
        "index-commit",
        "index-attempt",
        "witness-missing",
        "witness-duplicate",
        "witness-partial",
        "witness-hash",
        "witness-version",
        "witness-commit",
        "witness-run",
        "witness-attempt",
        "witness-log-gone",
        "witness-log-oversized",
        "witness-asset-missing",
        "witness-asset-duplicate",
        "witness-asset-hash",
        "witness-job-name",
        "witness-job-run",
        "witness-job-sha",
        "witness-job-failed",
        "witness-job-duplicate",
        "witness-legacy-gh",
        "witness-gh-unknownflag",
    ] {
        if bootstrap
            && ![
                "success",
                "recovery",
                "archive-tamper",
                "manifest-tamper",
                "witness-missing",
                "redirect",
            ]
            .contains(&fault)
        {
            continue;
        }
        if focus.as_deref().is_some_and(|wanted| wanted != fault) {
            continue;
        }
        let initial = if fault == "success" {
            previous.unwrap_or(package)
        } else {
            package
        };
        let f = make(
            &format!("{}-{fault}", if bootstrap { "bootstrap" } else { "r2" }),
            initial,
            initial != package || fault == "same-version",
            "source",
            "",
        )?;
        if fault == "same-version" {
            fs::write(f.source.join("README.md"), "Changed same-version bytes\n")?;
        }
        let mut archive_bytes = archive_bytes.clone();
        if fault == "binary-tamper" {
            let corrupt = f.root.join("corrupt-package");
            copy_tree(package, &corrupt)?;
            let executable = corrupt.join(if cfg!(windows) {
                "bin/remnifold.exe"
            } else {
                "bin/remnifold"
            });
            let mut bytes = fs::read(&executable)?;
            bytes[0] ^= 1;
            fs::write(executable, bytes)?;
            let corrupt_archive = f.root.join("corrupt.tar.gz");
            fixture_archive(&corrupt, &corrupt_archive)?;
            archive_bytes = fs::read(corrupt_archive)?;
        }
        let migration = if fault == "success" && previous.is_some() {
            let old_binary = initial.join(if cfg!(windows) {
                "bin/remnifold.exe"
            } else {
                "bin/remnifold"
            });
            Some(prepare_migration_state(
                &f,
                &old_binary,
                endpoint.context("R2 migration Worker")?,
            )?)
        } else {
            None
        };
        let asset_hash = hash(&archive_bytes);
        let prefix = format!("releases/{}/{}/100-1", f.version, f.sha);
        let asset_key = format!("{prefix}/{target}/{asset_hash}.tar.gz");
        let manifest_key = format!("{prefix}/manifest.json");
        let mut targets = serde_json::Map::new();
        let release_schema = if fault == "legacy-success" { 1 } else { 2 };
        for t in [
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
        ] {
            if release_schema == 2 && t == "x86_64-apple-darwin" {
                continue;
            }
            targets.insert(t.into(), json!({"key":format!("{prefix}/{t}/{asset_hash}.tar.gz"),"size":archive_bytes.len(),"sha256":asset_hash,"nativeManifestSha256":native_hash}));
        }
        if fault == "missing-target" {
            targets.remove(target);
        }
        if fault == "unsafe-key" {
            targets.get_mut(target).unwrap()["key"] = json!("../archive.tar.gz");
        }
        if fault == "size" {
            targets.get_mut(target).unwrap()["size"] = json!(archive_bytes.len() + 1);
        }
        if fault == "native-hash" {
            targets.get_mut(target).unwrap()["nativeManifestSha256"] = json!("0".repeat(64));
        }
        let manifest = json!({"schema":release_schema,"repository":REPO,"version":f.version,"sourceCommit":f.sha,"runId":100,"runAttempt":1,"targets":targets});
        let mut manifest_bytes = serde_json::to_vec(&manifest)?;
        if fault == "duplicate-target" {
            let text = String::from_utf8(manifest_bytes)?;
            manifest_bytes = text
                .replacen(
                    "\"targets\":{",
                    &format!(
                        "\"targets\":{{\"{target}\":{},",
                        manifest["targets"][target]
                    ),
                    1,
                )
                .into_bytes();
        }
        let manifest_hash = hash(&manifest_bytes);
        let mut index = json!({"schema":if fault == "schema-cross" { 1 } else { release_schema },"repository":REPO,"version":f.version,"sourceCommit":f.sha,"runId":100,"runAttempt":1,"manifestKey":manifest_key,"manifestSha256":manifest_hash});
        if fault == "index-commit" {
            index["sourceCommit"] = json!("f".repeat(40));
        }
        if fault == "index-attempt" {
            index["runAttempt"] = json!(2);
        }
        if fault == "manifest-tamper" {
            manifest_bytes.push(b' ');
        }
        let mut objects = BTreeMap::from([
            (
                format!("releases/{}/index.json", f.version),
                serde_json::to_vec(&index)?,
            ),
            (manifest_key, manifest_bytes),
            (asset_key, archive_bytes.clone()),
        ]);
        if fault == "index-missing" {
            objects.remove(&format!("releases/{}/index.json", f.version));
        }
        if fault == "archive-tamper" {
            objects
                .get_mut(&format!("{prefix}/{target}/{asset_hash}.tar.gz"))
                .unwrap()[0] ^= 1;
        }
        if fault == "archive-missing" {
            objects.remove(&format!("{prefix}/{target}/{asset_hash}.tar.gz"));
        }
        for (key, bytes) in &objects {
            if !key.ends_with(".json") {
                continue;
            }
            let output = f.root.join("http-objects").join(key);
            safe_path(&output)?;
            fs::create_dir_all(output.parent().unwrap())?;
            fs::write(output, bytes)?;
        }
        let mut server = R2Fixture::start(objects, fault == "redirect", f.root.join("http.jsonl"))?;
        let config = serde_json::to_string(
            &json!({"schema":1,"repository":REPO,"stagingBucket":"remnifold-builds","publicBucket":"remnifold-updates","publicBaseUrl":server.base,"statusContext":"remnifold/r2/v1","workflowPath":".github/workflows/ci.yml"}),
        )?;
        let run = json!({"id":100,"run_attempt":1,"name":"CI","head_sha":f.sha,"event":"push","head_branch":format!("release/{}",f.version),"path":".github/workflows/ci.yml","workflow_id":42,"repository":{"id":77,"full_name":REPO},"head_repository":{"id":77,"full_name":REPO},"status":"completed","conclusion":"success"});
        let mut publication = run.clone();
        match fault {
            "run-id" => publication["id"] = json!(101),
            "run-attempt" => publication["run_attempt"] = json!(2),
            "run-commit" => publication["head_sha"] = json!("f".repeat(40)),
            "run-target" => {
                publication["head_repository"]["full_name"] = json!("attacker/remnifold")
            }
            "run-path" => publication["path"] = json!(".github/workflows/attacker.yml"),
            "run-repository-id" => publication["repository"]["id"] = json!(78),
            "run-head-repository-id" => publication["head_repository"]["id"] = json!(78),
            "run-workflow" => publication["workflow_id"] = json!(43),
            _ => (),
        }
        let mut latest = run.clone();
        latest["id"] = json!(101); // immutable indexは以前の成功した公開実行も許可する。
        if fault == "latest-failed" {
            latest["conclusion"] = json!("failure");
        }
        let mut anchor = json!({"context":"remnifold/r2/v1","state":"success","target_url":format!("https://github.com/{REPO}/actions/runs/100/attempts/1"),"description":format!("sha256:{manifest_hash}"),"creator":{"id":41898282,"login":"github-actions[bot]","type":"Bot"}});
        if fault == "anchor-creator" {
            anchor["creator"]["id"] = json!(7);
        }
        if fault == "anchor-hash" {
            anchor["description"] = json!(format!("sha256:{}", "0".repeat(64)));
        }
        let mut marker_fields = vec![
            format!("REMNIFOLD_R2_MANIFEST_V{release_schema}"),
            f.version.clone(),
            f.sha.clone(),
            "100".into(),
            "1".into(),
            manifest_hash.clone(),
        ];
        match fault {
            "witness-version" => marker_fields[1] = "999.0.0".into(),
            "witness-commit" => marker_fields[2] = "f".repeat(40),
            "witness-run" => marker_fields[3] = "101".into(),
            "witness-attempt" => marker_fields[4] = "2".into(),
            "witness-hash" => marker_fields[5] = "0".repeat(64),
            _ => (),
        }
        let marker = marker_fields.join(" ");
        let mut job_log = match fault {
            "witness-missing" => "unrelated job output\n".into(),
            "witness-duplicate" => format!("{marker}\n{marker}\n"),
            "witness-partial" => format!("prefix {marker} suffix\n"),
            _ => format!("2026-11-03T00:00:00.0000000Z {marker}\n"),
        };
        job_log.insert_str(0, "\u{1b}[32mUnrelated CI diagnostic\u{1b}[0m\n");
        for (target, asset) in manifest["targets"].as_object().unwrap() {
            let asset_line = format!(
                "REMNIFOLD_R2_ASSET_V{} {target} {} {} {}\n",
                if fault == "witness-schema-cross" {
                    1
                } else {
                    release_schema
                },
                asset["sha256"].as_str().unwrap(),
                asset["size"],
                asset["key"].as_str().unwrap()
            );
            if fault == "witness-asset-missing" && target == "aarch64-apple-darwin" {
                continue;
            }
            if fault == "witness-asset-duplicate" && target == "aarch64-apple-darwin" {
                job_log.push_str(&asset_line);
            }
            if fault == "witness-asset-hash" && target == "aarch64-apple-darwin" {
                job_log.push_str(&asset_line.replacen(
                    asset["sha256"].as_str().unwrap(),
                    &"0".repeat(64),
                    1,
                ));
            } else {
                job_log.push_str(&asset_line);
            }
        }
        if fault == "witness-log-oversized" {
            job_log.push_str(&"x".repeat(4 * 1024 * 1024 + 1));
        }
        let mut job = json!({"id":1000,"name":"publish-r2","run_id":101,"head_sha":f.sha,"status":"completed","conclusion":"success"});
        match fault {
            "witness-job-name" => job["name"] = json!("attacker-publish"),
            "witness-job-run" => job["run_id"] = json!(100),
            "witness-job-sha" => job["head_sha"] = json!("f".repeat(40)),
            "witness-job-failed" => job["conclusion"] = json!("failure"),
            _ => (),
        }
        let jobs = if fault == "witness-job-duplicate" {
            json!({"total_count":2,"jobs":[job,job]})
        } else {
            json!({"total_count":1,"jobs":[job]})
        };
        let mut state: Value = serde_json::from_slice(&fs::read(&f.state)?)?;
        state["repository"] = json!(REPO);
        let mut blob = Sha1::new();
        blob.update(format!("blob {}\0", config.len()));
        blob.update(config.as_bytes());
        let blob_hash = blob
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        state["tree"].as_array_mut().unwrap().push(json!({"path":".github/remnifold-distribution.json","type":"blob","mode":"100644","sha":if fault=="config-blob"{"0".repeat(40)}else{blob_hash}}));
        state["r2"] = json!({"config":config,"latestRun":latest,"publicationRun":publication,"statuses":if fault=="anchor-missing"{json!([])}else{json!([anchor])},"jobs":jobs,"jobLog":job_log,"logUnavailable":fault=="witness-log-gone","legacyGh":fault=="witness-legacy-gh","rejectEscapeFlag":fault=="witness-gh-unknownflag"});
        fs::write(&f.state, serde_json::to_vec(&state)?)?;
        let mut settings: Value = serde_json::from_slice(&fs::read(&f.config)?)?;
        settings["localPlugin"]["repository"] = json!(REPO);
        fs::write(&f.config, serde_json::to_vec(&settings)?)?;
        if bootstrap {
            bootstrap_case(&f, fault, scenarios)?;
            server.secure_requests()?;
            continue;
        }
        let before = f.snapshot()?;
        if fault == "success"
            || fault == "legacy-success"
            || fault == "recovery"
            || fault == "witness-legacy-gh"
        {
            f.success("--check-published", "published")?;
            ensure!(
                before == f.snapshot()? && f.adds()? == 0,
                "R2 preflight mutated installation"
            );
            if fault == "recovery" {
                f.edit_state("failAdd", json!(true))?;
                let failed = f.run("--apply", None, false)?;
                ensure!(
                    failed["code"] == "PLUGIN_ADD_FAILED",
                    "R2 official failure ignored: {failed}"
                );
                f.edit_state("failAdd", json!(false))?;
            }
            f.success("--apply", "updated")?;
            f.installed()?;
            let receipt: Value = serde_json::from_slice(&fs::read(
                f.work.join(format!("verified-{}-{target}.json", f.sha)),
            )?)?;
            ensure!(
                receipt["schema"] == 2
                    && receipt["transport"] == "r2"
                    && receipt["manifestSha256"] == manifest_hash
                    && receipt["archiveSha256"] == asset_hash
                    && receipt["nativeManifestSha256"] == native_hash
                    && receipt["witnessRunId"] == 101
                    && receipt["witnessRunAttempt"] == 1
                    && receipt["witnessJobId"] == 1000,
                "R2 receipt omitted trusted identity"
            );
            f.success("--apply", "already-current")?;
            let http_count = server.calls.lock().unwrap().len();
            let gh_count = f.calls()?.len();
            f.edit_state("historyUnavailable", json!(true))?;
            f.edit_state("r2", json!({}))?; // config/history inaccessible; receipt must suffice.
            server.shutdown();
            f.success("--verify-only", "verified")?;
            if let Some(migration) = &migration {
                let http = reqwest::blocking::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(30))
                    .build()?;
                verify_migration_state(&f, migration, endpoint.unwrap(), &http)?;
            }
            ensure!(
                server.calls.lock().unwrap().len() == http_count,
                "receipt verification requested R2"
            );
            ensure!(
                !f.calls()?.iter().skip(gh_count).any(|c| c["args"]
                    .to_string()
                    .contains("actions/")
                    || c["args"].to_string().contains("statuses")
                    || c["args"].to_string().contains("distribution.json")),
                "receipt verification requested publication evidence"
            );
        } else {
            let result = f.run("--apply", None, false)?;
            if fault == "witness-gh-unknownflag" {
                ensure!(
                    f.calls()?
                        .iter()
                        .filter(|c| c["args"].to_string().contains("actions/jobs/"))
                        .count()
                        == 1,
                    "unknown GitHub log flag retried with weaker options"
                );
            }
            let expected_code = match fault {
                "config-blob" => "INVALID_DISTRIBUTION_CONFIG",
                "anchor-missing" => "R2_ANCHOR_MISSING",
                "anchor-creator" | "anchor-hash" => "R2_ANCHOR_MISMATCH",
                "manifest-tamper" => "R2_MANIFEST_DIGEST_MISMATCH",
                "archive-tamper" | "size" => "R2_ARCHIVE_DIGEST_MISMATCH",
                "binary-tamper" => "PACKAGE_HASH_MISMATCH",
                "native-hash" => "R2_NATIVE_MANIFEST_DIGEST_MISMATCH",
                "run-id"
                | "run-attempt"
                | "run-commit"
                | "run-target"
                | "run-path"
                | "run-workflow"
                | "run-repository-id"
                | "run-head-repository-id" => "R2_PUBLICATION_CI_MISMATCH",
                "latest-failed" => "RELEASE_CI_NOT_SUCCESSFUL",
                "missing-target" | "duplicate-target" | "unsafe-key" => "INVALID_R2_MANIFEST",
                "index-commit" | "index-attempt" => "INVALID_R2_INDEX",
                "schema-cross" => "R2_RELEASE_IDENTITY_MISMATCH",
                "redirect" | "index-missing" | "archive-missing" => "R2_DOWNLOAD_FAILED",
                "same-version" => "LOCAL_SOURCE_MODIFIED",
                "witness-log-gone" | "witness-log-oversized" => "R2_WITNESS_LOG_UNAVAILABLE",
                "witness-gh-unknownflag" => "R2_WITNESS_LOG_UNAVAILABLE",
                name if name.starts_with("witness-job-") => "R2_WITNESS_JOB_MISMATCH",
                name if name.starts_with("witness-") => "R2_WITNESS_MISMATCH",
                _ => anyhow::bail!("unknown R2 rejection fixture"),
            };
            if ["archive-tamper", "archive-missing", "size"].contains(&fault) {
                let checked = f.run("--check-published", None, false)?;
                ensure!(
                    checked["exitCode"] == 1
                        && checked["code"] == expected_code
                        && before == f.snapshot()?
                        && f.adds()? == 0,
                    "R2 invalid archive accepted by preflight ({fault}): {checked}"
                );
            }
            ensure!(
                result["exitCode"] == 1
                    && result["code"] == expected_code
                    && before == f.snapshot()?
                    && f.adds()? == 0,
                "R2 invalid publication changed installation ({fault}): {result}"
            );
        }
        ensure!(
            !f.calls()?
                .iter()
                .any(|c| c["args"].to_string().contains("artifacts")),
            "R2 downgraded to legacy artifact transport"
        );
        server.secure_requests()?;
        scenarios.push(Box::leak(format!("r2-{fault}").into_boxed_str()));
    }
    Ok(())
}
fn native_state_command(
    binary: &Path,
    home: &Path,
    project: &Path,
    endpoint: &str,
    args: &[&str],
    payload: Option<&Value>,
) -> Result<std::process::Output> {
    let mut command = crate::client_e2e::command(binary, home, project, endpoint);
    command
        .env_remove("REMNIFOLD_API_URL")
        .env_remove("REMNIFOLD_API_KEY")
        .args(args);
    crate::client_e2e::process_output(
        command,
        &payload
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default(),
    )
}
fn prepare_migration_state(
    fixture: &Fixture,
    binary: &Path,
    endpoint: &str,
) -> Result<MigrationState> {
    let project = fixture.root.join("project");
    fs::create_dir(&project)?;
    fs::create_dir_all(fixture.home.join("sessions"))?;
    let session = format!(
        "{}-state",
        fixture.root.file_name().unwrap().to_string_lossy()
    );
    let transcript = fixture
        .home
        .join("sessions")
        .join(format!("rollout-{session}.jsonl"));
    for name in ["remnifold.json", "kagayoi-memory.json", "supermemory.json"] {
        fs::write(
            fixture.home.join(name),
            serde_json::to_vec_pretty(
                &json!({"baseUrl":endpoint,"apiKey":API_KEY,"maxRecentTasks":0,"connectionMarker":format!("既存接続 {name} 😀")}),
            )?,
        )?;
    }
    let rows = [
        json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":"MigrationState preserve completed work 😀"}}),
        json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"MigrationState verified receipt identity and connection preservation."}]}}),
    ];
    fs::write(
        &transcript,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    let payload = json!({"session_id":session,"transcript_path":transcript,"cwd":project});
    let captured = native_state_command(
        binary,
        &fixture.home,
        &project,
        endpoint,
        &["hook", "Stop"],
        Some(&payload),
    )?;
    ensure!(
        captured.status.success() && captured.stderr.is_empty(),
        "initial native migration capture failed: {}",
        String::from_utf8_lossy(&captured.stderr)
    );
    let receipt = fs::read_dir(fixture.home.join("remnifold/capture-state"))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .context("actual capture receipt missing")?;
    let bytes = fs::read(&receipt)?;
    let ids: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        ids.as_array().is_some_and(|ids| ids.len() == 1
            && ids[0]
                .as_str()
                .is_some_and(|id| id.starts_with("codex-turn-v2:"))),
        "invalid actual native receipt"
    );
    for namespace in ["kagayoi-memory", "cloudflare-memory"] {
        let directory = fixture.home.join(namespace).join("capture-state");
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(receipt.file_name().unwrap()), &bytes)?;
    }
    let mut files = BTreeMap::new();
    for name in ["remnifold.json", "kagayoi-memory.json", "supermemory.json"] {
        let path = fixture.home.join(name);
        files.insert(path.clone(), fs::read(path)?);
    }
    for namespace in ["remnifold", "kagayoi-memory", "cloudflare-memory"] {
        let path = fixture
            .home
            .join(namespace)
            .join("capture-state")
            .join(receipt.file_name().unwrap());
        files.insert(path.clone(), fs::read(path)?);
    }
    Ok(MigrationState {
        project,
        payload,
        files,
    })
}
fn verify_migration_state(
    fixture: &Fixture,
    state: &MigrationState,
    endpoint: &str,
    http: &reqwest::blocking::Client,
) -> Result<()> {
    for (path, expected) in &state.files {
        ensure!(
            fs::read(path)? == *expected,
            "updater changed actual connection or capture receipt: {}",
            path.display()
        );
    }
    let binary = fixture.source.join(if cfg!(windows) {
        "bin/remnifold.exe"
    } else {
        "bin/remnifold"
    });
    let count = |state: &Value| {
        state["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == "/v3/documents" && r["method"] == "POST")
            .count()
    };
    let before: Value = http
        .get(format!("{endpoint}/__state"))
        .send()?
        .error_for_status()?
        .json()?;
    let mut homes = vec![fixture.home.clone()];
    for (name, namespace) in [
        ("remnifold.json", "remnifold"),
        ("kagayoi-memory.json", "kagayoi-memory"),
        ("supermemory.json", "cloudflare-memory"),
    ] {
        let home = fixture.home.join(format!("reader-{namespace}"));
        fs::create_dir(&home)?;
        fs::copy(fixture.home.join(name), home.join(name))?;
        for (path, bytes) in &state.files {
            if path.starts_with(fixture.home.join(namespace).join("capture-state")) {
                let dir = home.join(namespace).join("capture-state");
                fs::create_dir_all(&dir)?;
                fs::write(dir.join(path.file_name().unwrap()), bytes)?;
            }
        }
        homes.push(home);
    }
    for home in homes {
        let status =
            native_state_command(&binary, &home, &state.project, endpoint, &["status"], None)?;
        ensure!(
            status.status.success()
                && status.stderr.is_empty()
                && String::from_utf8_lossy(&status.stdout).contains("API reachability: connected")
                && !String::from_utf8_lossy(&status.stdout).contains(API_KEY),
            "updated runtime could not read preserved connection"
        );
        let captured = native_state_command(
            &binary,
            &home,
            &state.project,
            endpoint,
            &["hook", "Stop"],
            Some(&state.payload),
        )?;
        ensure!(
            captured.status.success() && captured.stderr.is_empty(),
            "updated runtime could not read preserved capture receipt"
        );
    }
    let after: Value = http
        .get(format!("{endpoint}/__state"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        count(&before) == count(&after),
        "updated runtime resent acknowledged capture after migration"
    );
    for (path, expected) in &state.files {
        ensure!(
            fs::read(path)? == *expected,
            "preserved connection/receipt changed after read"
        );
    }
    Ok(())
}
fn root_boundary_cases(root: &Path, binary: &Path, archive: &Path) -> Result<()> {
    for (label, variable, ancestor) in [
        ("codex-inside", "CODEX_HOME", false),
        ("codex-ancestor", "CODEX_HOME", true),
        ("plugin-inside", "PLUGIN_ROOT", false),
        ("plugin-ancestor", "PLUGIN_ROOT", true),
    ] {
        let holder = root.join(label);
        let protected = holder.join("protected");
        fs::create_dir_all(&protected)?;
        fs::write(
            protected.join("sentinel.txt"),
            b"preserved unrelated fixture data",
        )?;
        let work = if ancestor {
            holder.clone()
        } else {
            protected.join("must-not-create")
        };
        let before = files(&holder)?;
        let home = root.join(format!("boundary-home-{label}"));
        fs::create_dir(&home)?;
        let mut command = crate::client_e2e::command(
            &std::env::current_exe()?,
            &home,
            &holder,
            "http://127.0.0.1:9",
        );
        command
            .args(["updater-test", "--work-root"])
            .arg(&work)
            .arg("--tool-root")
            .arg(holder.join("must-not-prepare-tools"))
            .arg("--archive")
            .arg(archive)
            .arg("--binary")
            .arg(binary)
            .env(variable, &protected);
        let output = crate::client_e2e::process_output(command, b"")?;
        ensure!(
            output.status.code() == Some(1)
                && String::from_utf8_lossy(&output.stderr)
                    .contains("updater fixture work root overlaps Codex home or plugin source")
                && files(&holder)? == before
                && (ancestor || !work.exists())
                && !holder.join("must-not-prepare-tools").exists(),
            "unsafe {label} was accepted or mutated protected state"
        );
    }
    for (label, profile, plugin) in [
        ("profile-precedence", true, false),
        ("home-fallback", false, false),
        ("default-plugin", true, true),
    ] {
        let holder = root.join(label);
        let preferred = holder.join("preferred-home");
        let other = holder.join("other-home");
        let protected = preferred.join(if plugin {
            "plugins/remnifold"
        } else {
            ".codex"
        });
        fs::create_dir_all(&protected)?;
        fs::create_dir(&other)?;
        fs::write(
            protected.join("sentinel.txt"),
            b"preserved fallback fixture",
        )?;
        let work = protected.join("must-not-create");
        let before = files(&holder)?;
        let mut command = crate::client_e2e::command(
            &std::env::current_exe()?,
            &other,
            &holder,
            "http://127.0.0.1:9",
        );
        command
            .args(["updater-test", "--work-root"])
            .arg(&work)
            .arg("--tool-root")
            .arg(holder.join("must-not-prepare-tools"))
            .arg("--archive")
            .arg(archive)
            .arg("--binary")
            .arg(binary)
            .env_remove("CODEX_HOME")
            .env_remove("PLUGIN_ROOT")
            .env_remove("CLAUDE_PLUGIN_ROOT");
        if profile {
            command.env("USERPROFILE", &preferred).env("HOME", &other);
        } else {
            command.env_remove("USERPROFILE").env("HOME", &preferred);
        }
        let output = crate::client_e2e::process_output(command, b"")?;
        ensure!(
            output.status.code() == Some(1)
                && String::from_utf8_lossy(&output.stderr)
                    .contains("updater fixture work root overlaps Codex home or plugin source")
                && !work.exists()
                && !holder.join("must-not-prepare-tools").exists()
                && files(&holder)? == before,
            "unsafe fallback root accepted: {label}"
        );
    }
    fs::write(
        root.join("root-boundary-validation.json"),
        serde_json::to_vec_pretty(&json!({"passed":true,"cases":7,"writesBeforeRejection":false}))?,
    )?;
    Ok(())
}
pub fn run(
    repo: &Path,
    tools: &Toolchain,
    work: &Path,
    binary: &Path,
    archive: &Path,
    legacy_archive: Option<&Path>,
    previous_archive: Option<&Path>,
) -> Result<()> {
    run_suite(
        repo,
        Some(tools),
        work,
        binary,
        archive,
        legacy_archive,
        previous_archive,
        false,
    )
}
pub fn bootstrap_tests(repo: &Path, work: &Path, binary: &Path, archive: &Path) -> Result<()> {
    run_suite(repo, None, work, binary, archive, None, None, true)
}
fn run_suite(
    repo: &Path,
    tools: Option<&Toolchain>,
    work: &Path,
    binary: &Path,
    archive: &Path,
    legacy_archive: Option<&Path>,
    previous_archive: Option<&Path>,
    bootstrap_only: bool,
) -> Result<()> {
    validate_work_root(repo, work)?;
    safe_path(binary)?;
    ensure!(!work.starts_with(repo), "updater fixtures must be external");
    create_private_fixture_dir(work, true)?;
    let root = work.join(format!("updater-{}", uuid::Uuid::new_v4()));
    create_private_fixture_dir(&root, false)?;
    println!("Updater validation directory: {}", root.display());
    let package = unpack(archive, &root.join("published"))?;
    let input_archive = archive.to_path_buf();
    let input_sha256 = hash(&fs::read(archive)?);
    let original_plugin_bytes = fs::read(package.join(".codex-plugin/plugin.json"))?;
    let original_server_bytes = fs::read(package.join("server/package.json"))?;
    // リポジトリと公開済み原本の版は保ち、隔離fixtureの版だけを変えて実際の更新を検証する。
    let source_build: Value =
        serde_json::from_slice(&fs::read(package.join("native-build.json"))?)?;
    let version_parts = source_build["version"]
        .as_str()
        .context("source version")?
        .split(['-', '+'])
        .next()
        .context("source version core")?
        .split('.')
        .collect::<Vec<_>>();
    ensure!(version_parts.len() == 3, "fixture requires release semver");
    let fixture_version = format!(
        "{}.{}.{}-e2e",
        version_parts[0],
        version_parts[1],
        version_parts[2]
            .parse::<u64>()?
            .checked_add(1)
            .context("fixture patch version overflow")?
    );
    for name in [".codex-plugin/plugin.json", "server/package.json"] {
        let mut value: Value = serde_json::from_slice(&fs::read(package.join(name))?)?;
        value["version"] = json!(fixture_version);
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        fs::write(package.join(name), bytes)?;
    }
    let mut build = source_build.clone();
    build["version"] = json!(fixture_version);
    for name in [".codex-plugin/plugin.json", "server/package.json"] {
        build["files"][name] = json!(hash(&fs::read(package.join(name))?));
    }
    let mut bytes = serde_json::to_vec_pretty(&build)?;
    bytes.push(b'\n');
    fs::write(package.join("native-build.json"), bytes)?;
    let generated_archive = root.join("fixture-publication.tar.gz");
    fixture_archive(&package, &generated_archive)?;
    let archive = generated_archive.as_path();
    let expected = files(&package)?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(package.join(".codex-plugin/plugin.json"))?)?;
    let build: Value = serde_json::from_slice(&fs::read(package.join("native-build.json"))?)?;
    let target = build["target"].as_str().context("native target")?;
    let sha = build["sourceCommit"]
        .as_str()
        .context("native source commit")?;
    let version = manifest["version"].as_str().context("native version")?;
    let mut tree = Vec::new();
    let source_files = checkout_sources(repo)?;
    let packaged_sources = expected
        .keys()
        .filter(|name| old_native_source(name))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        source_files
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            == packaged_sources,
        "checkout source set differs from the old updater release-tree contract; retire obsolete Node provisioning sources before packaging"
    );
    for (name, mut bytes) in source_files {
        if [".codex-plugin/plugin.json", "server/package.json"].contains(&name.as_str()) {
            let mut value: Value = serde_json::from_slice(&bytes)?;
            value["version"] = json!(fixture_version);
            bytes = serde_json::to_vec_pretty(&value)?;
            bytes.push(b'\n');
        }
        ensure!(
            bytes == fs::read(package.join(&name))?,
            "package source differs from checkout release tree: {name}"
        );
        let mut digest = Sha1::new();
        digest.update(format!("blob {}\0", bytes.len()));
        digest.update(bytes);
        tree.push(json!({"path":name,"type":"blob","mode":"100644","sha":digest.finalize().iter().map(|b|format!("{b:02x}")).collect::<String>()}));
    }
    let cargo = fs::read_to_string(repo.join("native/Cargo.toml"))?.replacen(
        &format!(
            "version = \"{}\"",
            source_build["version"].as_str().unwrap()
        ),
        &format!("version = \"{fixture_version}\""),
        1,
    );
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", cargo.len()));
    digest.update(cargo.as_bytes());
    tree.push(json!({"path":"native/Cargo.toml","type":"blob","mode":"100644","sha":digest.finalize().iter().map(|b|format!("{b:02x}")).collect::<String>()}));
    for name in [
        "client/memory-index.mjs",
        "client/memory-redaction.mjs",
        "client/request-projection.mjs",
    ] {
        tree.push(json!({"path":name,"type":"blob","mode":"100644","sha":"a".repeat(40)}));
    }
    let zip_path = root.join("artifact.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path)?);
    zip.start_file(
        format!("remnifold-{target}.tar.gz"),
        zip::write::SimpleFileOptions::default(),
    )?;
    zip.write_all(&fs::read(archive)?)?;
    zip.finish()?;
    let artifact_digest = format!("sha256:{}", hash(&fs::read(&zip_path)?));
    let shim_root = root.join("shims");
    fs::create_dir(&shim_root)?;
    let gh = shim_root.join(if cfg!(windows) { "gh.exe" } else { "gh" });
    let codex = shim_root.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    fs::copy(std::env::current_exe()?, &gh)?;
    fs::copy(std::env::current_exe()?, &codex)?;
    let fixture = |name: &str,
                   initial: &Path,
                   preserve: bool,
                   source_name: &str,
                   fault: &str|
     -> Result<Fixture> {
        let directory = root.join(name);
        create_private_fixture_dir(&directory, false)?;
        fs::create_dir(directory.join("temp"))?;
        let source = directory.join(source_name);
        copy_tree(initial, &source)?;
        let home = directory.join("codex-home");
        let mut initial_manifest: Value =
            serde_json::from_slice(&fs::read(source.join(".codex-plugin/plugin.json"))?)?;
        if !preserve {
            initial_manifest["version"] = json!("0.0.0-e2e");
            fs::write(
                source.join(".codex-plugin/plugin.json"),
                serde_json::to_vec(&initial_manifest)?,
            )?;
        }
        let initial_version = initial_manifest["version"]
            .as_str()
            .context("initial version")?;
        copy_tree(
            &source,
            &home
                .join("plugins/cache/personal/remnifold")
                .join(initial_version),
        )?;
        let state = directory.join("state.json");
        let config = directory.join("config.json");
        let log = directory.join("calls.jsonl");
        fs::write(&log, "")?;
        fs::write(
            &state,
            serde_json::to_vec(
                &json!({"isolatedFixture":true,"requireGithubHost":true,"repository":"fixture/remnifold","version":version,"manifest":manifest,"sha":sha,"tree":tree,"source":source,"codexHome":home,"archive":archive,"zipPath":zip_path,"artifactDigest":artifact_digest,"artifactName":format!("remnifold-{target}"),"installedVersion":initial_version,"logPath":log,"fault":fault}),
            )?,
        )?;
        fs::write(
            &config,
            serde_json::to_vec(
                &json!({"localPlugin":{"id":"remnifold@personal","repository":"fixture/remnifold","workflow":"CI","artifact":"remnifold","sourcePath":source},"commands":{"gh":[gh,&state],"codex":[codex,&state]}}),
            )?,
        )?;
        Ok(Fixture {
            legacy_temp: false,
            root: directory.clone(),
            source,
            home,
            work: directory.join("work"),
            state,
            config,
            binary: binary.to_path_buf(),
            version: version.into(),
            sha: sha.into(),
            expected: expected.clone(),
        })
    };
    let mut scenarios = Vec::new();
    if bootstrap_only {
        r2_cases(
            &fixture,
            &package,
            archive,
            target,
            None,
            None,
            &mut scenarios,
            true,
        )?;
        println!("Native bootstrap contracts: {} passed", scenarios.len());
        return Ok(());
    }
    if std::env::var_os("REMNIFOLD_UPDATER_R2_FOCUS").is_some() {
        r2_cases(
            &fixture,
            &package,
            archive,
            target,
            None,
            None,
            &mut scenarios,
            false,
        )?;
        ensure!(!scenarios.is_empty(), "unknown focused R2 scenario");
        println!(
            "Focused R2 diagnostic: {} passed; full updater contracts were not run",
            scenarios.len()
        );
        return Ok(());
    }
    root_boundary_cases(&root, binary, archive)?;
    scenarios.push("codex-and-plugin-root-overlap-rejected-before-write");
    let updated = fixture("success", &package, false, "source", "")?;
    let before = updated.snapshot()?;
    updated.success("--check-published", "published")?;
    ensure!(
        before == updated.snapshot()? && updated.adds()? == 0,
        "preflight changed installation"
    );
    scenarios.push("published-preflight");
    updated.success("--apply", "updated")?;
    updated.installed()?;
    ensure!(updated.adds()? == 1, "installation count");
    scenarios.push("published-install-source-cache");
    updated.success("--apply", "already-current")?;
    updated.success("--verify-only", "verified")?;
    updated.installed()?;
    ensure!(updated.adds()? == 1, "duplicate install");
    scenarios.push("idempotent-install-and-verify");
    updated.edit_state("historyUnavailable", json!(true))?;
    let calls = updated.calls()?.len();
    let before = updated.snapshot()?;
    updated.success("--verify-only", "verified")?;
    updated.installed()?;
    ensure!(
        before == updated.snapshot()? && updated.adds()? == 1,
        "retained receipt verification mutated installation"
    );
    let calls = updated.calls()?.into_iter().skip(calls).collect::<Vec<_>>();
    ensure!(
        calls.iter().filter(|v| v["kind"] == "gh").count() == 3
            && calls.iter().filter(|v| v["kind"] == "codex").count() == 1,
        "retained verification consulted CI artifacts"
    );
    scenarios.push("verify-without-retained-ci-or-artifact");
    fs::write(
        updated
            .home
            .join("plugins/cache/personal/remnifold")
            .join(version)
            .join("README.md"),
        "Corrupted installed cache\n",
    )?;
    let before = updated.snapshot()?;
    let result = updated.run("--verify-only", None, false)?;
    ensure!(
        result["code"] == "INSTALLED_PLUGIN_CONTENT_MISMATCH"
            && result["exitCode"] == 1
            && before == updated.snapshot()?
            && updated.adds()? == 1,
        "corrupted cache verification repaired or accepted: {result}"
    );
    scenarios.push("verify-rejects-corrupted-cache-without-repair");
    let retry = fixture("official-failure-resume", &package, false, "source", "")?;
    retry.edit_state("failAdd", json!(true))?;
    let before = retry.snapshot()?;
    let result = retry.run("--apply", None, false)?;
    ensure!(
        result["exitCode"] == 1 && result["code"] == "PLUGIN_ADD_FAILED",
        "official CLI failure ignored: {result}"
    );
    ensure!(
        retry.snapshot()?["cache"] == before["cache"] && files(&retry.source)? == expected,
        "failed official install changed cache or lost prepared source"
    );
    retry.edit_state("failAdd", json!(false))?;
    retry.success("--apply", "updated")?;
    retry.installed()?;
    retry.success("--verify-only", "verified")?;
    ensure!(retry.adds()? == 2, "failed install retry count");
    scenarios.push("official-failure-resume");
    #[cfg(windows)]
    for (label, pathext) in [
        ("missing", None),
        ("empty", Some("")),
        ("no-exe", Some(".BAT;.CMD")),
    ] {
        let isolated = fixture(
            &format!("reject-pathext-{label}"),
            &package,
            false,
            "source",
            "",
        )?;
        let before = isolated.snapshot()?;
        let result = isolated.run_hook_environment("--apply", None, false, Some(pathext))?;
        ensure!(
            result["exitCode"] == 1
                && result["code"] == "WINDOWS_PATHEXT_EXE_REQUIRED"
                && isolated.snapshot()? == before
                && isolated.adds()? == 0,
            "PATHEXT {label} changed installation or did not fail closed: {result}"
        );
        let publication =
            isolated.run_hook_environment("--check-published", None, false, Some(pathext))?;
        ensure!(
            publication["status"] == "published" && publication["exitCode"] == 0,
            "publication check incorrectly required hook runtime: {publication}"
        );
        scenarios.push(match label {
            "missing" => "missing-pathext-rejected-before-install",
            "empty" => "empty-pathext-rejected-before-install",
            _ => "non-exe-pathext-rejected-before-install",
        });
    }
    for (fault, code) in [
        ("ref", "RELEASE_REF_MISMATCH"),
        ("version", "RELEASE_VERSION_MISMATCH"),
        ("ci", "RELEASE_CI_NOT_SUCCESSFUL"),
        ("ci-sha", "RELEASE_CI_NOT_SUCCESSFUL"),
        ("artifact", "RELEASE_ARTIFACT_UNAVAILABLE"),
        ("expired", "RELEASE_ARTIFACT_UNAVAILABLE"),
        ("hash", "PACKAGE_HASH_MISMATCH"),
        ("tree", "INCOMPLETE_RELEASE_TREE"),
        ("digest", "ARTIFACT_DIGEST_MISMATCH"),
        ("source-edit", "LOCAL_SOURCE_MODIFIED"),
    ] {
        let isolated = fixture(&format!("reject-{fault}"), &package, false, "source", fault)?;
        if fault == "source-edit" {
            fs::write(
                isolated.source.join("README.md"),
                "User-maintained content that must survive.\n",
            )?;
        }
        let before = isolated.snapshot()?;
        let result = isolated.run("--apply", None, false)?;
        ensure!(
            result["exitCode"] == 1 && result["status"] == "blocked" && result["code"] == code,
            "{fault}: {result}"
        );
        ensure!(
            before == isolated.snapshot()? && isolated.adds()? == 0,
            "{fault} changed installation before validation"
        );
        scenarios.push(Box::leak(
            format!("reject-{fault}-before-mutation").into_boxed_str(),
        ));
    }
    for (name, source_name) in [
        ("reject-work-inside-source", "source"),
        ("reject-unicode-case-alias", "Ä"),
    ] {
        let isolated = fixture(name, &package, false, source_name, "")?;
        let before = isolated.snapshot()?;
        let alias = if cfg!(windows) {
            PathBuf::from(isolated.source.to_string_lossy().to_lowercase())
        } else {
            isolated.source.clone()
        };
        let nested = alias.join("must-not-be-created");
        let result = isolated.run("--apply", Some(&nested), false)?;
        ensure!(
            result["code"] == "INVALID_WORK_ROOT"
                && result["exitCode"] == 1
                && before == isolated.snapshot()?
                && !nested.exists()
                && isolated.calls()?.is_empty(),
            "unsafe work root accepted: {result}"
        );
        scenarios.push(name);
    }
    let self_update = fixture("running-from-source", &package, false, "source", "")?;
    let result = self_update.run("--apply", None, true)?;
    ensure!(
        result["status"] == "updated" && result["exitCode"] == 0,
        "source updater failed: {result}"
    );
    self_update.installed()?;
    scenarios.push("running-from-installed-source");
    let linked = fixture("reject-linked-work-root", &package, false, "source", "")?;
    let real = linked.root.join("real-work");
    fs::create_dir(&real)?;
    let alias = linked.root.join("linked-work");
    directory_link(&real, &alias)?;
    let before = linked.snapshot()?;
    let result = linked.run("--apply", Some(&alias.join("work")), false)?;
    ensure!(
        result["code"] == "LINK_PATH_REJECTED"
            && result["exitCode"] == 1
            && linked.snapshot()? == before
            && linked.calls()?.is_empty()
            && !real.join("work").exists(),
        "linked work ancestor was followed: {result}"
    );
    scenarios.push("linked-work-root-rejected-before-mutation");
    let mut worker = if legacy_archive.is_some() || previous_archive.is_some() {
        Some(start_fixture(
            repo,
            tools.context("updater migration tools missing")?,
            &root.join("worker"),
        )?)
    } else {
        None
    };
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    for (name, input) in [
        ("legacy-js-migration", legacy_archive),
        ("previous-native-migration", previous_archive),
    ] {
        if let Some(input) = input {
            let worker = worker.as_ref().context("migration Worker fixture")?;
            let old = unpack(input, &root.join(format!("{name}-input")))?;
            let original = files(&old)?;
            ensure!(original != expected, "migration fixture must differ");
            let mut migration = fixture(
                name,
                &old,
                name == "previous-native-migration",
                "source",
                "",
            )?;
            if name == "previous-native-migration" {
                migration.legacy_temp = true;
                migration.edit_state("requireGithubHost", json!(false))?;
                migration.binary = old.join(if cfg!(windows) {
                    "bin/remnifold.exe"
                } else {
                    "bin/remnifold"
                });
            }
            let state = prepare_migration_state(&migration, &migration.binary, &worker.base_url)?;
            migration.success("--apply", "updated")?;
            migration.installed()?;
            migration.success("--apply", "already-current")?;
            migration.success("--verify-only", "verified")?;
            verify_migration_state(&migration, &state, &worker.base_url, &http)?;
            ensure!(
                migration.adds()? == 1 && files(&old)? == original,
                "migration changed external state or input"
            );
            scenarios.push(name);
            if name == "previous-native-migration" {
                let mut immutable = fixture(
                    "reject-same-version-different-content",
                    &old,
                    true,
                    "source",
                    "",
                )?;
                let previous_manifest: Value =
                    serde_json::from_slice(&fs::read(old.join(".codex-plugin/plugin.json"))?)?;
                immutable.version = previous_manifest["version"]
                    .as_str()
                    .context("previous version")?
                    .into();
                immutable.edit_state("version", json!(immutable.version))?;
                immutable
                    .edit_state("manifest", serde_json::from_slice(&original_plugin_bytes)?)?;
                let mut original_tree = tree.clone();
                for entry in &mut original_tree {
                    let bytes = match entry["path"].as_str() {
                        Some(".codex-plugin/plugin.json") => Some(&original_plugin_bytes),
                        Some("server/package.json") => Some(&original_server_bytes),
                        _ => None,
                    };
                    if let Some(bytes) = bytes {
                        let mut digest = Sha1::new();
                        digest.update(format!("blob {}\0", bytes.len()));
                        digest.update(bytes);
                        entry["sha"] = json!(
                            digest
                                .finalize()
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>()
                        );
                    }
                }
                immutable.edit_state("tree", json!(original_tree))?;
                let before = immutable.snapshot()?;
                let result = immutable.run("--verify-only", None, false)?;
                ensure!(
                    result["status"] == "blocked"
                        && result["exitCode"] == 1
                        && immutable.snapshot()? == before
                        && immutable.adds()? == 0,
                    "same-version content replacement accepted or mutated: {result}"
                );
                scenarios.push("same-version-content-mismatch-is-immutable");
            }
        }
    }
    let previous = root.join("previous-native-migration-input/remnifold");
    r2_cases(
        &fixture,
        &package,
        archive,
        target,
        previous.is_dir().then_some(previous.as_path()),
        worker.as_ref().map(|w| w.base_url.as_str()),
        &mut scenarios,
        false,
    )?;
    r2_cases(
        &fixture,
        &package,
        archive,
        target,
        None,
        None,
        &mut scenarios,
        true,
    )?;
    fs::write(
        root.join("local-plugin-update-validation.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":true,"runtime":"native CLI fixtures","nodeOnPath":false,"inputArchive":{"path":input_archive,"sha256":input_sha256,"version":source_build["version"],"sourceCommit":source_build["sourceCommit"]},"fixtureArchive":{"path":archive,"sha256":hash(&fs::read(archive)?),"version":version,"sourceCommit":sha,"executableBytesUnchanged":true},"files":expected.len(),"scenarios":scenarios,"workRoot":root}),
        )?,
    )?;
    println!("Updater contracts: {} passed", scenarios.len());
    if let Some(worker) = worker.as_mut() {
        worker.cleanup_state()?;
    }
    Ok(())
}

pub fn validate_work_root(repo: &Path, work: &Path) -> Result<()> {
    #[cfg(windows)]
    fn key(path: &Path) -> String {
        let value = path.to_string_lossy().replace('\\', "/").to_lowercase();
        value.strip_prefix("//?/").unwrap_or(&value).to_owned()
    }
    #[cfg(windows)]
    ensure!(
        work.is_absolute() && key(work).starts_with("d:/codex/remnifold/"),
        "Windows updater fixture root must be under D:/Codex/remnifold"
    );
    safe_path(work)?;
    let normalized = |path: &Path| -> Result<PathBuf> {
        let absolute = std::path::absolute(path)?;
        let mut path = PathBuf::new();
        for part in absolute.components() {
            match part {
                Component::ParentDir => {
                    ensure!(path.pop(), "invalid protected root");
                }
                Component::CurDir => {}
                other => path.push(other),
            }
        }
        let mut missing = Vec::new();
        while !path.exists() {
            missing.push(
                path.file_name()
                    .context("protected root has no existing ancestor")?
                    .to_os_string(),
            );
            ensure!(path.pop(), "protected root has no existing ancestor");
        }
        let mut resolved = fs::canonicalize(path)?;
        for part in missing.into_iter().rev() {
            resolved.push(part);
        }
        Ok(resolved)
    };
    let overlaps = |left: &Path, right: &Path| -> bool {
        #[cfg(windows)]
        {
            let left = key(left);
            let right = key(right);
            left == right
                || left.starts_with(&(right.clone() + "/"))
                || right.starts_with(&(left + "/"))
        }
        #[cfg(not(windows))]
        {
            left.starts_with(right) || right.starts_with(left)
        }
    };
    let work = normalized(work)?;
    ensure!(
        !overlaps(&work, &normalized(repo)?),
        "updater fixture work root overlaps repository"
    );
    let mut protected = Vec::new();
    for name in ["CODEX_HOME", "PLUGIN_ROOT", "CLAUDE_PLUGIN_ROOT"] {
        if let Some(path) = std::env::var_os(name) {
            protected.push(PathBuf::from(path));
        }
    }
    let runtime_home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if std::env::var_os("CODEX_HOME").is_none() {
        if let Some(home) = &runtime_home {
            protected.push(PathBuf::from(home).join(".codex"));
        }
    }
    if let Some(home) = runtime_home {
        protected.push(PathBuf::from(home).join("plugins/remnifold"));
    }
    if let Ok(executable) = std::env::current_exe() {
        for parent in executable.ancestors().skip(1) {
            if parent.join(".codex-plugin/plugin.json").is_file() {
                protected.push(parent.to_path_buf());
                break;
            }
        }
    }
    for path in protected {
        ensure!(
            !overlaps(&work, &normalized(&path)?),
            "updater fixture work root overlaps Codex home or plugin source"
        );
    }
    Ok(())
}
