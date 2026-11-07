mod bootstrap_release;
mod ci;
#[cfg(test)]
#[allow(dead_code)]
#[path = "ci_runner.rs"]
mod ci_runner_contract;
mod client_e2e;
mod fake_cli;
mod package;
mod r2;
#[path = "../../native/src/release_manifest.rs"]
mod release_manifest;
mod server_tests;
mod source;
mod tools;
mod updater_e2e;

use anyhow::{Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    if let Some(result) = fake_cli::dispatch() {
        return result;
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    if command == "local-input" {
        return r2::local_input(&args.collect::<Vec<_>>());
    }
    if command == "bootstrap-release" {
        return bootstrap_release::run(&repo, &args.collect::<Vec<_>>());
    }
    if command == "r2-stage" {
        return r2::stage(&args.collect::<Vec<_>>());
    }
    if command == "r2-publish" {
        return r2::publish(&args.collect::<Vec<_>>());
    }
    if command == "r2-smoke" {
        return r2::smoke(&args.collect::<Vec<_>>());
    }
    if command == "ci-release-branch" {
        if args.next().is_some() {
            bail!("ci-release-branch does not accept arguments");
        }
        return ci::release_branch(&repo);
    }
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        println!(
            "cargo xtask <prepare|type-check|native-test|server-test|client-test|updater-test|bootstrap-test|package|verify|check>\n\nOptions: --tool-root <absolute-cache> --work-root <absolute-output>\nPackage: --target <Rust target> --binary <executable> --output <archive>\nBootstrap release: bootstrap-release --source-commit <SHA> --work-root <absolute-output> [--apply]\nSource: --source-mode commit|checkout (package/verify default: commit)\nVerify/update tests: --archive <archive> [--legacy-archive <old-JS-package>] [--previous-archive <old-native-package>]"
        );
        return Ok(());
    }
    if ![
        "prepare",
        "type-check",
        "native-test",
        "server-test",
        "client-test",
        "updater-test",
        "bootstrap-test",
        "package",
        "verify",
        "check",
    ]
    .contains(&command.as_str())
    {
        bail!("Unknown command: {command}");
    }
    let mut options = BTreeMap::new();
    while let Some(option) = args.next() {
        if ![
            "--tool-root",
            "--work-root",
            "--target",
            "--binary",
            "--output",
            "--archive",
            "--source-commit",
            "--source-mode",
            "--legacy-archive",
            "--previous-archive",
        ]
        .contains(&option.as_str())
        {
            bail!("Unknown option: {option}");
        }
        let value = args
            .next()
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| anyhow::anyhow!("{option} needs a value"))?;
        if options.insert(option.clone(), value).is_some() {
            bail!("Duplicate option: {option}");
        }
    }
    let target = options
        .get("--target")
        .map(String::as_str)
        .unwrap_or(host_target());
    package::executable(target)?;
    anyhow::ensure!(
        package::TARGETS.contains(&target),
        "Unsupported current target"
    );
    let source_mode = options
        .get("--source-mode")
        .map(String::as_str)
        .unwrap_or("commit");
    if !matches!(source_mode, "commit" | "checkout") {
        bail!("Source mode must be commit or checkout");
    }
    let root = options
        .get("--tool-root")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("REMNIFOLD_TOOL_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| repo.join(".devtools"));
    let work = options
        .get("--work-root")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("REMNIFOLD_VALIDATION_DIR").map(PathBuf::from))
        .map(Ok)
        .unwrap_or_else(default_work)?;
    if !root.is_absolute() || !work.is_absolute() {
        bail!("Tool and validation roots must be absolute");
    }
    package::safe_path(&root)?;
    package::safe_path(&work)?;
    if matches!(
        command.as_str(),
        "check" | "updater-test" | "bootstrap-test"
    ) {
        updater_e2e::validate_work_root(&repo, &work)?;
        updater_e2e::create_private_fixture_dir(&work, true)?;
    } else {
        fs::create_dir_all(&work)?;
    }
    let binary = options
        .get("--binary")
        .map(PathBuf::from)
        .unwrap_or_else(|| binary_path(&repo, false, None));
    if command == "bootstrap-test" {
        return updater_e2e::bootstrap_tests(
            &repo,
            &work,
            &binary,
            &required_path(&options, "--archive")?,
        );
    }
    let configured = tools::prepare(&root)?;
    match command.as_str() {
        "prepare" => {
            println!("esbuild: {}", configured.esbuild.display());
            println!("TypeScript: {}", configured.tsc.display());
            println!("workerd: {}", configured.workerd.display());
        }
        "type-check" => tools::check_types(&configured, &repo, &work)?,
        "native-test" => native_tests(&repo, &configured, &work)?,
        "server-test" => server_tests::run(&repo, &configured, &work)?,
        "client-test" => {
            if !options.contains_key("--binary") {
                build_native(&repo, &configured, None, false)?;
            }
            client_e2e::run(&repo, &configured, &work, &binary)?;
        }
        "updater-test" => {
            let archive = required_path(&options, "--archive")?;
            let legacy = options.get("--legacy-archive").map(PathBuf::from);
            let previous = options.get("--previous-archive").map(PathBuf::from);
            updater_e2e::run(
                &repo,
                &configured,
                &work,
                &binary,
                &archive,
                legacy.as_deref(),
                previous.as_deref(),
            )?;
        }
        "package" => {
            let commit = source_commit(&repo, &options)?;
            if source_mode == "commit" {
                source::verify_checkout(&repo, &commit)?;
            }
            let binary = match options.get("--binary") {
                Some(path) => PathBuf::from(path),
                None => {
                    build_native(&repo, &configured, Some(target), true)?;
                    binary_path(&repo, true, Some(target))
                }
            };
            let output = options
                .get("--output")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.join("dist").join(format!("remnifold-{target}.tar.gz")));
            let archive = package::build(&repo, target, &binary, &output, &commit)?;
            verify_source(&repo, &archive, &commit, source_mode)?;
            println!("{}", archive.display());
        }
        "verify" => {
            let archive = required_path(&options, "--archive")?;
            let commit = source_commit(&repo, &options)?;
            package::verify(&repo, target, &binary, &archive, &commit)?;
            verify_source(&repo, &archive, &commit, source_mode)?;
        }
        "check" => {
            tools::check_types(&configured, &repo, &work)?;
            native_tests(&repo, &configured, &work)?;
            server_tests::run(&repo, &configured, &work.join("server"))?;
            build_native(&repo, &configured, None, false)?;
            let native = options
                .get("--binary")
                .map(PathBuf::from)
                .unwrap_or_else(|| binary_path(&repo, false, None));
            client_e2e::run(&repo, &configured, &work.join("client"), &native)?;
            let output = work
                .join(format!("package-{}", uuid::Uuid::new_v4()))
                .join(format!("remnifold-{target}.tar.gz"));
            let commit = source_commit(&repo, &options)?;
            let archive = package::build(&repo, target, &native, &output, &commit)?;
            let legacy = options.get("--legacy-archive").map(PathBuf::from);
            let previous = options.get("--previous-archive").map(PathBuf::from);
            updater_e2e::run(
                &repo,
                &configured,
                &work.join("updater"),
                &native,
                &archive,
                legacy.as_deref(),
                previous.as_deref(),
            )?;
            run_command(
                Command::new("git")
                    .args(["diff", "--check"])
                    .current_dir(&repo),
                "diff check",
            )?;
            println!(
                "Local Node-free validation passed; the test archive is verified against this checkout, without release commit verification"
            );
        }
        _ => bail!("Unknown command: {command}"),
    }
    Ok(())
}

fn verify_source(repo: &Path, archive: &Path, commit: &str, mode: &str) -> Result<()> {
    if mode == "commit" {
        source::verify(repo, archive, commit)?;
        println!("Archive and release commit verified");
    } else {
        println!(
            "Archive verified against this local checkout; release commit identity is unverified"
        );
    }
    Ok(())
}

fn required_path(options: &BTreeMap<String, String>, name: &str) -> Result<PathBuf> {
    options
        .get(name)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("{name} is required"))
}
fn host_target() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        panic!("Intel macOS is not supported")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
fn binary_path(repo: &Path, release: bool, target: Option<&str>) -> PathBuf {
    let mut root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("target"));
    if !root.is_absolute() {
        root = repo.join(root);
    }
    if let Some(target) = target {
        root.push(target);
    }
    root.push(if release { "release" } else { "debug" });
    root.join(if target.unwrap_or(host_target()).contains("windows") {
        "remnifold.exe"
    } else {
        "remnifold"
    })
}
fn run_command(command: &mut Command, stage: &str) -> Result<()> {
    if !command.status()?.success() {
        bail!("{stage} failed");
    }
    Ok(())
}
fn build_native(
    repo: &Path,
    tools: &tools::Toolchain,
    target: Option<&str>,
    release: bool,
) -> Result<()> {
    let mut command = Command::new(env!("CARGO"));
    command
        .args(["build", "--locked", "--package", "remnifold"])
        .env("REMNIFOLD_ESBUILD", &tools.esbuild)
        .current_dir(repo);
    if release {
        command.arg("--release");
    }
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    run_command(&mut command, "native build")
}
fn default_work() -> Result<PathBuf> {
    Ok(if cfg!(windows) {
        PathBuf::from("D:/Codex/remnifold/validation")
    } else {
        std::env::temp_dir()
            .canonicalize()?
            .join("remnifold-validation")
    })
}
fn native_tests(repo: &Path, tools: &tools::Toolchain, work: &Path) -> Result<()> {
    let isolated = work.join(format!("n-{}", uuid::Uuid::new_v4().simple()));
    let temporary = isolated.join("t");
    fs::create_dir_all(&temporary)?;
    let mut command = Command::new(env!("CARGO"));
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if ([
            "REMNIFOLD_",
            "KAGAYOI_MEMORY_",
            "SUPERMEMORY_",
            "CLOUDFLARE_MEMORY_",
        ]
        .iter()
        .any(|prefix| upper.starts_with(prefix))
            && upper != "REMNIFOLD_KEEP_TEST_STATE")
            || ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"].contains(&upper.as_str())
        {
            command.env_remove(key);
        }
    }
    command
        .args(["test", "--locked", "--workspace"])
        .env("REMNIFOLD_ESBUILD", &tools.esbuild)
        .env("REMNIFOLD_VALIDATION_DIR", &isolated)
        .env("CODEX_HOME", isolated.join("codex"))
        .env("NO_PROXY", "127.0.0.1,localhost,::1")
        .env("TEMP", &temporary)
        .env("TMP", &temporary)
        .env("TMPDIR", &temporary)
        .current_dir(repo);
    run_command(&mut command, "Rust tests")
}
fn source_commit(repo: &Path, options: &BTreeMap<String, String>) -> Result<String> {
    if let Some(commit) = options.get("--source-commit") {
        return Ok(commit.clone());
    }
    let result = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()?;
    if !result.status.success() {
        bail!("Cannot resolve source commit; use --source-commit");
    }
    Ok(String::from_utf8(result.stdout)?.trim().to_string())
}
