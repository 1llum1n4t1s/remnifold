use crate::tools::Toolchain;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub const API_KEY: &str = "native-fixture-key";
pub struct WorkerFixture {
    pub base_url: String,
    output_dir: std::path::PathBuf,
    child: Child,
}
impl Drop for WorkerFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl WorkerFixture {
    fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().context("stop completed test Workerd")?;
        }
        self.child
            .wait()
            .context("wait for completed test Workerd")?;
        Ok(())
    }
    pub fn cleanup_state(&mut self) -> Result<()> {
        self.stop()?;
        let state = self.output_dir.join("s");
        if std::env::var_os("REMNIFOLD_KEEP_TEST_STATE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            println!("Test state retained: {}", state.display());
            return Ok(());
        }
        if !state.exists() {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(&state)?;
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let linked = metadata.file_type().is_symlink();
        if linked || !metadata.is_dir() || state.parent() != Some(self.output_dir.as_path()) {
            bail!("refusing cleanup outside owned invocation state")
        }
        fs::remove_dir_all(&state).context("clean completed invocation SQLite state")?;
        // Successful output keeps diagnostics and reports; generated modules are reproducible.
        for name in ["index.js", "observed.ts", "migrations.mjs", "worker.capnp"] {
            let path = self.output_dir.join(name);
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!(
                    "unexpected generated file type during cleanup: {}",
                    path.display()
                )
            }
            fs::remove_file(path)?;
        }
        let temp = self.output_dir.join("tmp");
        if temp.is_dir() {
            fs::remove_dir_all(temp)?;
        }
        Ok(())
    }
}
pub fn start_fixture(repo: &Path, tools: &Toolchain, work: &Path) -> Result<WorkerFixture> {
    if !work.is_absolute() {
        bail!("server test work directory must be absolute")
    }
    for p in work.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(metadata) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let linked = metadata.file_type().is_symlink();
                if linked {
                    bail!("linked test work directory: {}", p.display());
                }
                if !metadata.is_dir() {
                    bail!("test work ancestor is not a directory: {}", p.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::create_dir_all(work)?;
    let output_dir = work.join(format!("w-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&output_dir).context("create exclusive server test invocation")?;
    let work = output_dir.as_path();
    #[cfg(windows)]
    if work
        .join("s/tests")
        .join(format!("{}.sqlite-journal", "0".repeat(64)))
        .to_string_lossy()
        .len()
        >= 260
    {
        bail!("Workerd SQLite journal path exceeds the Windows limit; use a shorter --work-root")
    }
    fs::create_dir(work.join("s"))?;
    fs::create_dir_all(work.join("tmp"))?;
    let mut migration_paths = fs::read_dir(repo.join("server/migrations"))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    migration_paths.retain(|path| path.extension().and_then(|x| x.to_str()) == Some("sql"));
    migration_paths.sort();
    if migration_paths.is_empty() {
        bail!("server migrations are empty")
    }
    let mut migration_module = String::new();
    let mut values = Vec::new();
    for (index, path) in migration_paths.iter().enumerate() {
        let specifier = serde_json::to_string(&path.to_string_lossy().replace('\\', "/"))?;
        migration_module.push_str(&format!("import m{index} from {specifier};\n"));
        values.push(format!(
            "{{name:{},sql:m{index}}}",
            serde_json::to_string(
                &path
                    .file_name()
                    .context("migration name")?
                    .to_string_lossy()
            )?
        ));
    }
    migration_module.push_str(&format!("export const migrations=[{}];\nexport function applyMigrations(db,oauth=true){{for(const m of migrations)if(oauth||!m.name.startsWith('0008_'))db.exec(m.sql)}}\n",values.join(",")));
    fs::write(work.join("migrations.mjs"), migration_module)?;
    let source = fs::read_to_string(repo.join("server/src/index.ts"))?.replace("\r\n", "\n");
    let mut observed = source.replace(
        "from \"./",
        &format!(
            "from \"{}/server/src/",
            repo.to_string_lossy().replace('\\', "/")
        ),
    );
    for name in [
        "excerpt",
        "indexQueryPatterns",
        "queryCoverage",
        "parseMetadata",
    ] {
        let start = observed
            .find(&format!("function {name}( "))
            .or_else(|| observed.find(&format!("function {name}(")))
            .context("observation declaration")?;
        let brace = observed[start..]
            .find("{\n")
            .context("observation function body")?
            + start
            + 2;
        let counter = match name {
            "queryCoverage" => "coverage",
            "parseMetadata" => "metadataParses",
            other => other,
        };
        observed.insert_str(brace, &format!(" fixtureCounts.{counter}++;\n"));
    }
    // 計数の差し込み先が実装から消えると、その計数は常に0のままになり検査が黙って無効化される。
    // 以前は消えた比較ループの計数（rowComparisons）が失敗し得ない検査として残っていたため、
    // 差し込み先が見つからなければハーネスの準備段階で失敗させる。
    for (anchor, instrumented) in [
        (
            "text.toLowerCase()",
            "(fixtureCounts.candidateLower++, text.toLowerCase())",
        ),
        (
            "rowsById.get(vector.id)",
            "(fixtureCounts.rowLookups++, rowsById.get(vector.id))",
        ),
        (
            "ranked.sort((left, right) => right.coverage - left.coverage ||\n    right.indexCoverage - left.indexCoverage || right.checkpoint - left.checkpoint);",
            "ranked.sort((left, right) => { fixtureCounts.comparisons++; return right.coverage - left.coverage ||\n    right.indexCoverage - left.indexCoverage || right.checkpoint - left.checkpoint; });",
        ),
        (
            "const text = truncateUtf16(content.slice(start), 236);",
            "fixtureCounts.excerptWindows++; const text = truncateUtf16(content.slice(start), 236);",
        ),
    ] {
        if !observed.contains(anchor) {
            bail!("server observation anchor is missing from index.ts: {anchor}");
        }
        observed = observed.replace(anchor, instrumented);
    }
    observed = observed.replace(
        "from \"../package.json\"",
        &format!(
            "from \"{}/server/package.json\"",
            repo.to_string_lossy().replace('\\', "/")
        ),
    );
    let prefix = r#"let fixtureSequence=0;
const fixtureCounts={excerpt:0,indexQueryPatterns:0,candidateLower:0,rowLookups:0,digests:0,coverage:0,metadataParses:0,excerptWindows:0,comparisons:0};
export function resetCounts(){for(const key of Object.keys(fixtureCounts))fixtureCounts[key]=0;}
export function counts(){return {...fixtureCounts};}
export function resetFixture(){fixtureSequence=0;resetCounts();}
export function resetFixtureIds(){resetFixture();}
const Date=class extends globalThis.Date { constructor(...args){super(...(args.length?args:["2026-10-21T01:00:00.000Z"]));} static now(){return globalThis.Date.parse("2026-10-21T01:00:00.000Z");} };
const crypto={randomUUID:()=>"00000000-0000-4000-8000-"+String(++fixtureSequence).padStart(12,"0"),subtle:{digest(...args){fixtureCounts.digests++;return globalThis.crypto.subtle.digest(...args)}}};
"#;
    fs::write(
        work.join("observed.ts"),
        format!(
            "{prefix}\n{observed}\nexport {{sortByQueryCoverage as sortCoverage, querySearchExcerpt as searchExcerpt, excerpt as sourceExcerpt, queryCoverage as coverage}};"
        ),
    )?;
    let output = Command::new(&tools.esbuild)
        .current_dir(repo)
        .arg("server/tests/support/entry.mjs")
        .args([
            "--bundle",
            "--format=esm",
            "--platform=browser",
            "--loader:.sql=text",
        ])
        .arg(format!(
            "--alias:remnifold:observed={}",
            work.join("observed.ts").display()
        ))
        .arg(format!(
            "--alias:remnifold:migrations={}",
            work.join("migrations.mjs").display()
        ))
        .arg(format!("--outfile={}", work.join("index.js").display()))
        .output()?;
    if !output.status.success() {
        bail!(
            "server test bundle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let disk = work.join("s").to_string_lossy().replace('\\', "/");
    let namespace = "tests";
    let config = format!(
        r#"using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
 services = [(name = "main", worker = .worker), (name = "do-files", disk = (path = {disk:?}, writable = true))],
 sockets = [(name = "http", address = "127.0.0.1:{port}", http = (), service = "main")]);
const worker :Workerd.Worker = (
 modules = [(name = "index.js", esModule = embed "index.js")], compatibilityDate = "2026-11-02",
 bindings = [(name = "DB", durableObjectNamespace = "TestDatabase")],
 durableObjectNamespaces = [(className = "TestDatabase", uniqueKey = "{namespace}", enableSql = true, preventEviction = true)],
 durableObjectStorage = (localDisk = "do-files"));
"#
    );
    fs::write(work.join("worker.capnp"), config)?;
    let stdout = fs::File::create(work.join("workerd.stdout.log"))?;
    let stderr = fs::File::create(work.join("workerd.stderr.log"))?;
    let mut worker_command = Command::new(&tools.workerd);
    worker_command
        .arg("serve")
        .arg(work.join("worker.capnp"))
        .env("TEMP", work.join("tmp"))
        .env("TMP", work.join("tmp"))
        .env("TMPDIR", work.join("tmp"))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // ローカルfixtureの制御・実行は利用者の外部プロキシを継承しない。
    for (name, _) in std::env::vars_os() {
        if matches!(
            name.to_string_lossy().to_ascii_lowercase().as_str(),
            "http_proxy" | "https_proxy" | "all_proxy" | "no_proxy"
        ) {
            worker_command.env_remove(name);
        }
    }
    let child = worker_command.spawn()?;
    let mut fixture = WorkerFixture {
        base_url: format!("http://127.0.0.1:{port}"),
        output_dir: output_dir.clone(),
        child,
    };
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let start = Instant::now();
    loop {
        if let Some(status) = fixture.child.try_wait()? {
            bail!(
                "workerd exited {status}; see {}",
                work.join("workerd.stderr.log").display()
            )
        }
        if let Ok(r) = client.get(format!("{}/__health", fixture.base_url)).send() {
            if r.status().is_success() {
                return Ok(fixture);
            }
        }
        if start.elapsed() > Duration::from_secs(30) {
            bail!("workerd startup exceeded 30 seconds")
        };
        std::thread::sleep(Duration::from_millis(100));
    }
}
// 各caseのSQLite/WAL/SHMを開いたまま蓄積しない。Macの既定FD上限256でも余裕を残す。
const MAX_CASES_PER_WORKER: usize = 48;
fn finish_batch(
    fixture: &mut WorkerFixture,
    first: usize,
    last: usize,
    failed: usize,
) -> Result<Value> {
    let cleanup = if failed == 0 {
        fixture.cleanup_state()?;
        if std::env::var_os("REMNIFOLD_KEEP_TEST_STATE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            "retained by REMNIFOLD_KEEP_TEST_STATE=1"
        } else {
            "removed after child exit"
        }
    } else {
        fixture.stop()?;
        "retained for failed-case reproduction"
    };
    Ok(
        serde_json::json!({"outputDir":fixture.output_dir,"firstCaseIndex":first,"lastCaseIndex":last,
        "cases":last-first+1,"failed":failed,"stateCleanup":cleanup,"processId":fixture.child.id(),"childStopped":true}),
    )
}
pub fn run(repo: &Path, tools: &Toolchain, work: &Path) -> Result<()> {
    let mut fixture = start_fixture(repo, tools, work)?;
    let report_dir = fixture.output_dir.clone();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(90))
        .build()?;
    let cases: Vec<Value> = client
        .get(format!("{}/__cases", fixture.base_url))
        .send()?
        .error_for_status()?
        .json()?;
    if cases.is_empty() {
        bail!("server contract case list is empty")
    }
    let run = uuid::Uuid::new_v4().to_string();
    let mut results = Vec::new();
    let mut failed = 0;
    let mut batch_failed = 0;
    let mut batch_start = 0;
    let mut batch_states = Vec::new();
    for (position, case) in cases.into_iter().enumerate() {
        if position > 0 && position % MAX_CASES_PER_WORKER == 0 {
            batch_states.push(finish_batch(
                &mut fixture,
                batch_start,
                position - 1,
                batch_failed,
            )?);
            fixture = start_fixture(repo, tools, work)?;
            batch_start = position;
            batch_failed = 0;
        }
        let i = case["index"].as_u64().context("missing test index")?;
        let result: Value = client
            .get(format!("{}/__run?case={i}&run={run}", fixture.base_url))
            .send()
            .with_context(|| format!("server case {}", case["name"]))?
            .error_for_status()?
            .json()?;
        if result["status"] != "passed" {
            failed += 1;
            batch_failed += 1;
            eprintln!("FAILED {}: {}", result["name"], result["error"])
        }
        results.push(result);
    }
    batch_states.push(finish_batch(
        &mut fixture,
        batch_start,
        results.len() - 1,
        batch_failed,
    )?);
    let cleanup = if batch_states
        .iter()
        .all(|batch| batch["stateCleanup"] == batch_states[0]["stateCleanup"])
    {
        batch_states[0]["stateCleanup"].clone()
    } else {
        serde_json::json!("mixed; see batchStates")
    };
    let batch_dirs: Vec<_> = batch_states
        .iter()
        .map(|batch| batch["outputDir"].clone())
        .collect();
    fs::write(
        report_dir.join("server-validation.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"runtime":"workerd","results":results,"failed":failed,"stateCleanup":cleanup,
                "outputDir":report_dir,"maxCasesPerWorker":MAX_CASES_PER_WORKER,"batchOutputDirs":batch_dirs,"batchStates":batch_states}),
        )?,
    )?;
    println!(
        "Server contracts: {} passed, {failed} failed",
        results.len() - failed
    );
    println!(
        "Server validation report: {}",
        report_dir.join("server-validation.json").display()
    );
    if failed > 0 {
        bail!("{failed} server contract cases failed; see server-validation.json")
    };
    Ok(())
}
