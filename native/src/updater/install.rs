//! 単独アプリとしての導入・更新。
//!
//! Codex のプラグイン CLI には依存せず、従来の updater と同じ公開検証（リリース commit・CI の発行者ログ・
//! R2 manifest・アーカイブ digest・パッケージの出所）を通した配布物だけを、利用者ごとの導入先
//! （crate::paths::app_root）へ展開する。
//!
//! 導入先の配置:
//!   versions/<version>/   検証済みパッケージの展開（版ごとに不変。内容が違う同名版は置き換えない）
//!   bin/remnifold(.exe)   MCP・フックの登録が指す固定の実行ファイル（現在の版の複製）
//!   receipts/verified-<commit>-<target>.json  CI の保持期限後もオフライン検証に使う受領証
//!   current.json          現在の版・commit・対象
//!
//! Windows では起動中の実行ファイルを上書きできないが、名前の変更はできる。そのため固定の実行ファイルは、
//! 旧ファイルを退避名へ移してから新ファイルを置き、退避名は削除できるときに片付ける。
//! 起動中のプロセス（MCP サーバー等）は再起動するまで旧版のまま動くため、置き換えたときは restartRequired を返す。
use super::*;

pub fn run(argv: &[String]) -> Result<()> {
    if argv == ["--help"] || argv == ["-h"] {
        println!(
            "Usage: remnifold install --version <version> [--work-root <absolute-path>] [--check-published | --apply | --verify-only]\nDefault: --check-published (verify the publication without installing).\n--apply installs or updates the standalone app under {} and points {} at it.\n--verify-only re-checks the installed version against its retained receipt.",
            crate::paths::app_root().display(),
            crate::paths::app_executable(&crate::paths::app_root()).display()
        );
        return Ok(());
    }
    match install(argv) {
        Ok(value) => {
            println!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
        Err(error) => {
            let text = error.to_string();
            let code = if text
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                text.as_str()
            } else {
                "INSTALL_FAILED"
            };
            println!("{}", json!({"status":"blocked","code":code}));
            bail!("INSTALL_FAILED")
        }
    }
}

/// 廃止した Codex プラグインの導入・更新コマンド。固定の失敗コードで install を案内する。
pub fn retired(name: &str) -> Result<()> {
    println!(
        "{}",
        json!({"status":"blocked","code":"USE_REMNIFOLD_INSTALL","message":format!("{name} was retired. Use: remnifold install --version <version> --apply")})
    );
    bail!("USE_REMNIFOLD_INSTALL")
}

struct Args {
    version: String,
    work: Option<String>,
    config: Option<String>,
    mode: &'static str,
}

fn arguments(argv: &[String]) -> Result<Args> {
    let mut version = None;
    let mut work = None;
    let mut config = None;
    let mut mode = None;
    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        match flag {
            "--version" | "--work-root" | "--config" => {
                index += 1;
                let value = argv
                    .get(index)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("INVALID_ARGUMENTS"))?
                    .clone();
                let slot = match flag {
                    "--version" => &mut version,
                    "--work-root" => &mut work,
                    _ => &mut config,
                };
                ensure!(slot.is_none(), "INVALID_ARGUMENTS");
                *slot = Some(value);
            }
            "--check-published" | "--apply" | "--verify-only" => {
                ensure!(mode.is_none(), "INVALID_ARGUMENTS");
                mode = Some(match flag {
                    "--check-published" => "check",
                    "--apply" => "apply",
                    _ => "verify",
                });
            }
            _ => bail!("INVALID_ARGUMENTS"),
        }
        index += 1;
    }
    let version = version.ok_or_else(|| anyhow::anyhow!("INVALID_VERSION"))?;
    ensure!(valid_version(&version), "INVALID_VERSION");
    Ok(Args {
        version,
        work,
        config,
        mode: mode.unwrap_or("check"),
    })
}

fn install(argv: &[String]) -> Result<Value> {
    let args = arguments(argv)?;
    let version = args.version.as_str();
    // --config は検証用 fixture のための入力（リポジトリ名と gh コマンドの差し替え）。
    let config: Value = match &args.config {
        Some(path) => serde_json::from_slice(&fs::read(absolute(path)?)?)?,
        None => json!({}),
    };
    let repository = config["repository"]
        .as_str()
        .or_else(|| config["localPlugin"]["repository"].as_str())
        .unwrap_or("1llum1n4t1s/remnifold");
    ensure!(
        regex::Regex::new(r"^[\w.-]+/[\w.-]+$")?.is_match(repository),
        "INVALID_CONFIG"
    );
    let app = crate::paths::app_root();
    ensure!(app.is_absolute(), "INVALID_INSTALL_ROOT");
    let work = match &args.work {
        Some(path) => absolute(path)?,
        None => default_work_parent()?.join(format!("install-{version}")),
    };
    // 作業ルートは導入先・設定/状態フォルダ・Git 作業ツリー・実行中の実行ファイルと重ねない。
    ensure!(
        !within(&app, &work)
            && !within(&work, &app)
            && !work.ancestors().any(|p| p.join(".git").exists()),
        "INVALID_WORK_ROOT"
    );
    no_links(&work, true)?;
    no_links(&app, true)?;
    isolated_from_remnifold_roots(&work)?;
    let current = physical_path(&env::current_exe()?)?;
    ensure!(
        !within(&physical_path(&work)?, &current),
        "INVALID_WORK_ROOT"
    );
    ensure!(
        !within(&physical_path(&app)?, &physical_path(&work)?)
            && !within(&physical_path(&work)?, &physical_path(&app)?),
        "INVALID_WORK_ROOT"
    );
    let _guard = private_work::PrivateWorkGuard::open(&work)?;
    let context = Context {
        gh: command_config(
            &config,
            "gh",
            strings(&[if cfg!(windows) { "gh.exe" } else { "gh" }]),
        )?,
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
    let tree = context.api(&format!("git/trees/{commit}?recursive=1"))?;
    ensure!(
        tree["truncated"] == false && tree["tree"].is_array(),
        "INCOMPLETE_RELEASE_TREE"
    );
    let target = target()?;
    let version_dir = app.join("versions").join(version);
    let receipt_path = app
        .join("receipts")
        .join(format!("verified-{commit}-{target}.json"));
    let executable_path = crate::paths::app_executable(&app);
    if args.mode == "verify" {
        let local = files_at(&version_dir).map_err(|_| anyhow::anyhow!("NOT_INSTALLED"))?;
        ensure!(
            receipt_path.parent().unwrap().is_dir(),
            "INSTALL_RECEIPT_MISSING"
        );
        let _receipts = private_work::PrivateWorkGuard::open(receipt_path.parent().unwrap())?;
        verify_native(&local, commit, version, target, &tree)
            .map_err(|_| anyhow::anyhow!("INSTALLED_CONTENT_MISMATCH"))?;
        no_links(&receipt_path, false)?;
        private_work::validate_receipt(&receipt_path)?;
        let receipt: Value = serde_json::from_slice(
            &fs::read(&receipt_path).map_err(|_| anyhow::anyhow!("INSTALL_RECEIPT_MISSING"))?,
        )?;
        verify_receipt_schema(&receipt, &tree)?;
        r2::verify_receipt(&receipt, &local, &context, version, commit, target)?;
        let current: Value = serde_json::from_slice(
            &fs::read(app.join("current.json")).map_err(|_| anyhow::anyhow!("NOT_INSTALLED"))?,
        )?;
        ensure!(
            current["version"] == version
                && local.get(executable(target)).map(|b| sha(b))
                    == fs::read(&executable_path).ok().map(|b| sha(&b)),
            "INSTALLED_CONTENT_MISMATCH"
        );
        return Ok(json!({
            "status":"verified","version":version,"sha":commit,"files":local.len(),
            "installRoot":app,"executable":executable_path
        }));
    }
    let distribution = r2::config(&context, &tree, commit)?
        .ok_or_else(|| anyhow::anyhow!("R2_DISTRIBUTION_REQUIRED"))?;
    let published = r2::published(&context, &distribution, commit, version, target, &tree)?;
    if args.mode == "check" {
        let mut result = published.evidence;
        result["status"] = json!("published");
        return Ok(result);
    }
    let wanted = hashes(&published.files);
    // 版ごとの展開は不変。既にある同名版の内容が違えば、置き換えずに止める。
    let fresh = if version_dir.exists() {
        no_links(&version_dir, false)?;
        ensure!(
            hashes_at(&version_dir)? == wanted,
            "INSTALLED_VERSION_CONTENT_MISMATCH"
        );
        false
    } else {
        let incoming = app
            .join("versions")
            .join(format!(".incoming-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&incoming)?;
        for (name, bytes) in &published.files {
            let destination = incoming.join(name);
            fs::create_dir_all(destination.parent().unwrap())?;
            fs::write(&destination, bytes)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                incoming.join(executable(target)),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        ensure!(
            hashes_at(&incoming)? == wanted,
            "INSTALL_COPY_VERIFICATION_FAILED"
        );
        fs::rename(&incoming, &version_dir)?;
        true
    };
    // CI の保持期限後もオフライン検証できるよう、導入した実物に結び付く最小の受領証を残す。
    // 受領証フォルダは作業ルートと同じく利用者だけが書ける私用フォルダにし、差し替えを防ぐ。
    let _receipts = private_work::PrivateWorkGuard::open(receipt_path.parent().unwrap())?;
    no_links(&receipt_path, true)?;
    private_work::validate_receipt(&receipt_path)?;
    write_private(
        &receipt_path,
        &serde_json::to_vec_pretty(&published.receipt)?,
    )?;
    let replaced = place_executable(
        &executable_path,
        published
            .files
            .get(executable(target))
            .ok_or_else(|| anyhow::anyhow!("PACKAGE_EXECUTABLE_MISSING"))?,
    )?;
    let current = json!({"version":version,"sourceCommit":commit,"target":target});
    write_private(
        &app.join("current.json"),
        &serde_json::to_vec_pretty(&current)?,
    )?;
    let mut result = published.evidence;
    result["status"] = json!(if fresh || replaced {
        "installed"
    } else {
        "already-current"
    });
    result["files"] = json!(wanted.len());
    result["installRoot"] = json!(app);
    result["executable"] = json!(executable_path);
    result["restartRequired"] = json!(replaced);
    Ok(result)
}

/// 所有者だけが読める（Unix）ファイルを、書き終えてから置き換える。
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let temporary =
        path.with_file_name(format!(".remnifold-{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

/// 固定の実行ファイルを、検証済みの内容へ置き換える。同じ内容なら何もしない（false）。
fn place_executable(target: &Path, bytes: &[u8]) -> Result<bool> {
    let directory = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("INVALID_INSTALL_ROOT"))?;
    fs::create_dir_all(directory)?;
    no_links(target, true)?;
    if fs::read(target).is_ok_and(|current| current == bytes) {
        cleanup_retired(directory);
        return Ok(false);
    }
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let incoming = directory.join(format!(".remnifold-{suffix}.new"));
    {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&incoming)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&incoming, fs::Permissions::from_mode(0o755))?;
    }
    ensure!(
        fs::read(&incoming)? == bytes,
        "INSTALL_COPY_VERIFICATION_FAILED"
    );
    if target.exists() {
        // 起動中でも名前の変更はできる。退避名へ移してから新しいファイルを置く。
        let retired = directory.join(format!(".remnifold-{suffix}.old"));
        fs::rename(target, &retired)?;
        if let Err(error) = fs::rename(&incoming, target) {
            let _ = fs::rename(&retired, target);
            return Err(error.into());
        }
    } else {
        fs::rename(&incoming, target)?;
    }
    cleanup_retired(directory);
    Ok(true)
}

/// 退避した旧実行ファイルを、使われていないものだけ片付ける（起動中なら次回に回す）。
fn cleanup_retired(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".remnifold-") && (name.ends_with(".old") || name.ends_with(".new")) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_one_mode_and_valid_version() {
        let parse =
            |items: &[&str]| arguments(&items.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(parse(&["--version", "1.0.24"]).unwrap().mode, "check");
        assert_eq!(
            parse(&["--version", "1.0.24", "--apply"]).unwrap().mode,
            "apply"
        );
        assert!(parse(&["--version", "1.0.24", "--apply", "--verify-only"]).is_err());
        assert!(parse(&["--version", "latest"]).is_err());
        assert!(parse(&["--apply"]).is_err());
        assert!(parse(&["--version", "1.0.24", "--unknown"]).is_err());
    }

    #[test]
    fn executable_placement_is_idempotent_and_replaces_by_rename() {
        let root = env::temp_dir().join(format!("remnifold-install-{}", uuid::Uuid::new_v4()));
        let target = root.join("bin").join("remnifold.bin");
        assert!(place_executable(&target, b"one").unwrap());
        assert!(!place_executable(&target, b"one").unwrap());
        // 置き換え中のファイルを開いたままでも（Windows で起動中の実行ファイルと同じく）置き換えられる。
        let held = fs::File::open(&target).unwrap();
        assert!(place_executable(&target, b"two").unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"two");
        drop(held);
        cleanup_retired(&root.join("bin"));
        let names: Vec<_> = fs::read_dir(root.join("bin"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["remnifold.bin".to_string()]);
        if env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            fs::remove_dir_all(root).unwrap();
        }
    }
}
