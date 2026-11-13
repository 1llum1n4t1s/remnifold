//! 初回取得も updater と同じ公開証跡・アーカイブ検証を使う。
use super::*;

pub fn run(argv: &[String]) -> Result<()> {
    if argv == ["--help"] || argv == ["-h"] {
        println!(
            "Usage: remnifold plugin-bootstrap --version <version> [--work-root <absolute-path>] [--download-only | --apply | --verify-only]\nDefault: verify publication without installing. DownloadOnly requires no Codex/plugin installation. Apply migrates an existing installation and verifies it; retain the work-root receipt."
        );
        return Ok(());
    }
    match bootstrap(argv) {
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
                "PLUGIN_BOOTSTRAP_FAILED"
            };
            println!("{}", json!({"status":"blocked","code":code}));
            bail!("PLUGIN_BOOTSTRAP_FAILED")
        }
    }
}

fn arguments(argv: &[String]) -> Result<BTreeMap<String, String>> {
    let mut args = BTreeMap::new();
    let mut index = 0;
    while index < argv.len() {
        let raw = argv[index].as_str();
        let flag = [
            ("-Version", "--version"),
            ("-WorkRoot", "--work-root"),
            ("-Apply", "--apply"),
            ("-DownloadOnly", "--download-only"),
        ]
        .into_iter()
        .find_map(|(alias, canonical)| raw.eq_ignore_ascii_case(alias).then_some(canonical))
        .unwrap_or(raw);
        ensure!(!args.contains_key(flag), "INVALID_ARGUMENTS");
        let value = match flag {
            "--version" | "--work-root" | "--config" => {
                index += 1;
                argv.get(index)
                    .filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("INVALID_ARGUMENTS"))?
                    .clone()
            }
            "--download-only" | "--apply" | "--verify-only" => String::new(),
            _ => bail!("INVALID_ARGUMENTS"),
        };
        args.insert(flag.to_owned(), value);
        index += 1;
    }
    ensure!(
        args.get("--version").is_some_and(|v| valid_version(v)),
        "INVALID_VERSION"
    );
    ensure!(
        ["--apply", "--download-only", "--verify-only"]
            .iter()
            .filter(|f| args.contains_key(**f))
            .count()
            <= 1,
        "INVALID_ARGUMENTS"
    );
    Ok(args)
}

fn default_work_root() -> Result<PathBuf> {
    #[cfg(windows)]
    let path = PathBuf::from("D:/Claude/remnifold/bootstrap");
    #[cfg(not(windows))]
    let path = fs::canonicalize(env::temp_dir())?.join("remnifold-bootstrap");
    ensure!(path.is_absolute(), "INVALID_WORK_ROOT");
    Ok(path)
}

fn bootstrap(argv: &[String]) -> Result<Value> {
    let mut args = arguments(argv)?;
    if !args.contains_key("--work-root") {
        args.insert(
            "--work-root".into(),
            default_work_root()?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("INVALID_WORK_ROOT"))?
                .to_owned(),
        );
    }
    if args.contains_key("--apply") || args.contains_key("--verify-only") {
        let mut update_args: Vec<String> = args
            .iter()
            .filter(|(k, _)| !["--apply", "--verify-only"].contains(&k.as_str()))
            .flat_map(|(k, v)| [k.clone(), v.clone()])
            .collect();
        update_args.push(
            if args.contains_key("--apply") {
                "--apply"
            } else {
                "--verify-only"
            }
            .into(),
        );
        let result = update(&update_args)?;
        if args.contains_key("--apply") {
            *update_args.last_mut().unwrap() = "--verify-only".into();
            update(&update_args)?;
        }
        return Ok(result);
    }
    let version = &args["--version"];
    let work = absolute(&args["--work-root"])?;
    #[cfg(windows)]
    ensure!(
        within(&absolute("D:/Claude/remnifold")?, &work),
        "INVALID_WORK_ROOT"
    );
    let config: Value = if let Some(path) = args.get("--config") {
        serde_json::from_slice(&fs::read(absolute(path)?)?)?
    } else {
        json!({"localPlugin":{"repository":"1llum1n4t1s/remnifold","sourcePath":"~/plugins/remnifold"}})
    };
    let source = absolute(
        config["localPlugin"]["sourcePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("INVALID_CONFIG"))?,
    )?;
    let codex = absolute(&env::var("CODEX_HOME").unwrap_or_else(|_| "~/.codex".into()))?;
    isolated_physical_roots(&source, &work, &codex)?;
    ensure!(
        !work
            .ancestors()
            .any(|p| p.join(".git").exists() || p.join(".codex-plugin/plugin.json").exists()),
        "INVALID_WORK_ROOT"
    );
    let binary = env::current_exe()?;
    ensure!(
        !within(&work, &physical_path(&binary)?),
        "INVALID_WORK_ROOT"
    );
    let _guard = private_work::PrivateWorkGuard::open(&work)?;
    isolated_physical_roots(&source, &work, &codex)?;
    let repository = config["localPlugin"]["repository"].as_str().unwrap_or("");
    ensure!(
        regex::Regex::new(r"^[\w.-]+/[\w.-]+$")?.is_match(repository),
        "INVALID_CONFIG"
    );
    let context = Context {
        gh: command_config(
            &config,
            "gh",
            strings(&[if cfg!(windows) { "gh.exe" } else { "gh" }]),
        )?,
        codex: Vec::new(),
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
    let distribution = r2::config(&context, &tree, commit)?
        .ok_or_else(|| anyhow::anyhow!("R2_DISTRIBUTION_REQUIRED"))?;
    let published = r2::published(&context, &distribution, commit, version, target()?, &tree)?;
    if !args.contains_key("--download-only") {
        let mut evidence = published.evidence;
        evidence["status"] = json!("published");
        return Ok(evidence);
    }
    let delivery =
        delivery_root().map_err(|_| anyhow::anyhow!("BOOTSTRAP_PROFILE_RESOLUTION_FAILED"))?;
    isolated_physical_roots(&source, &delivery, &codex)?;
    separate_delivery_work(&work, &delivery)?;
    let profile = delivery
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("INVALID_DELIVERY_ROOT"))?;
    let expected = published.receipt["archiveSha256"].as_str().unwrap_or("");
    let path = stage_io(
        deliver(&delivery, profile, &published.archive, expected, &work),
        "BOOTSTRAP_DELIVERY_IO_FAILED",
    )?;
    Ok(
        json!({"status":"downloaded","archivePath":path,"version":version,"sha256":expected,"size":published.archive.len(),"workRoot":work}),
    )
}

fn stage_io<T>(result: Result<T>, code: &'static str) -> Result<T> {
    result.map_err(|error| {
        if error.downcast_ref::<std::io::Error>().is_some() {
            anyhow::anyhow!(code)
        } else {
            error
        }
    })
}

fn separate_delivery_work(work: &Path, delivery: &Path) -> Result<()> {
    let work = physical_path(work)?;
    let delivery = physical_path(delivery)?;
    ensure!(
        !within(&work, &delivery) && !within(&delivery, &work),
        "DELIVERY_WORK_OVERLAP"
    );
    Ok(())
}

fn deliver(
    delivery: &Path,
    profile: &Path,
    archive: &[u8],
    expected: &str,
    work: &Path,
) -> Result<PathBuf> {
    ensure!(
        hex(expected, 64) && sha(archive) == expected,
        "DELIVERY_DIGEST_MISMATCH"
    );
    let guard = stage_io(
        private_work::PrivateWorkGuard::open_delivery(delivery, profile),
        "BOOTSTRAP_DELIVERY_SECURITY_IO_FAILED",
    )?;
    separate_delivery_work(work, delivery)?;
    deliver_bytes(delivery, archive, expected, &guard)
}

fn deliver_bytes(
    delivery: &Path,
    archive: &[u8],
    expected: &str,
    _guard: &private_work::PrivateWorkGuard,
) -> Result<PathBuf> {
    let path = delivery.join("remnifold.tar.gz");
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(archive)?;
    file.sync_all()?;
    drop(file);
    ensure!(
        fs::metadata(&path)?.len() == archive.len() as u64
            && hash_reader(fs::File::open(&path)?)? == expected,
        "DELIVERY_DIGEST_MISMATCH"
    );
    Ok(path)
}

#[cfg(windows)]
fn delivery_root() -> Result<PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath},
    };
    let mut raw = std::ptr::null_mut();
    ensure!(
        unsafe { SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut raw) } == 0
            && !raw.is_null(),
        "PROFILE_LOOKUP_FAILED"
    );
    let result = (|| {
        let mut n = 0;
        while n < 32768 && unsafe { *raw.add(n) } != 0 {
            n += 1;
        }
        ensure!(n < 32768, "PROFILE_LOOKUP_FAILED");
        Ok(PathBuf::from(String::from_utf16(unsafe {
            std::slice::from_raw_parts(raw, n)
        })?))
    })();
    unsafe { CoTaskMemFree(raw.cast()) };
    let profile: PathBuf = result?;
    no_links(&profile, false)?;
    Ok(physical_path(&profile)?
        .join("Remnifold/Downloads")
        .join(format!("bootstrap-{}", uuid::Uuid::new_v4().simple())))
}
#[cfg(not(windows))]
fn delivery_root() -> Result<PathBuf> {
    bail!("WINDOWS_BOOTSTRAP_REQUIRED")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_io_diagnostics_hide_raw_paths_and_keep_fixed_policy_codes() {
        let error = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private-path-and-diagnostic",
        ));
        assert_eq!(
            stage_io::<()>(Err(error), "BOOTSTRAP_DELIVERY_IO_FAILED")
                .unwrap_err()
                .to_string(),
            "BOOTSTRAP_DELIVERY_IO_FAILED"
        );
        assert_eq!(
            stage_io::<()>(
                Err(anyhow::anyhow!("PRIVATE_WORK_ACL_UNSAFE")),
                "BOOTSTRAP_DELIVERY_IO_FAILED"
            )
            .unwrap_err()
            .to_string(),
            "PRIVATE_WORK_ACL_UNSAFE"
        );
    }
    #[cfg(windows)]
    #[test]
    fn bootstrap_canonical_profile_prefix_is_checked_after_drive_root() {
        let root = env::temp_dir().join(format!(
            "bootstrap-canonical-profile-{}",
            uuid::Uuid::new_v4()
        ));
        let _guard = private_work::PrivateWorkGuard::open(&root).unwrap();
        let canonical = fs::canonicalize(&root).unwrap();
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
        no_links(&canonical, false).unwrap();
        let _canonical_guard = private_work::PrivateWorkGuard::open(&canonical).unwrap();
        let alias = root.join("linked");
        test_links::directory_link(&root, &alias).unwrap();
        assert_eq!(
            no_links(&canonical.join("linked"), false)
                .unwrap_err()
                .to_string(),
            "LINK_PATH_REJECTED"
        );
        for incomplete in [r"C:", r"\\?\C:", r"\\?\C:relative"] {
            assert_eq!(
                no_links(Path::new(incomplete), false)
                    .unwrap_err()
                    .to_string(),
                "INVALID_PATH"
            );
        }
    }
    #[test]
    fn bootstrap_default_work_root_is_absolute_and_platform_specific() {
        let path = default_work_root().unwrap();
        assert!(path.is_absolute());
        #[cfg(windows)]
        assert_eq!(path, PathBuf::from("D:/Claude/remnifold/bootstrap"));
        #[cfg(not(windows))]
        {
            assert_eq!(
                path,
                fs::canonicalize(env::temp_dir())
                    .unwrap()
                    .join("remnifold-bootstrap")
            );
            assert_eq!(
                path.parent().unwrap(),
                fs::canonicalize(path.parent().unwrap()).unwrap()
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn bootstrap_delivery_overlap_resolves_existing_short_name_prefix() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let known = windows_program_files().unwrap();
        let wide: Vec<u16> = known.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut short = vec![0u16; 32768];
        let count =
            unsafe { GetShortPathNameW(wide.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
        if count == 0 || count as usize >= short.len() {
            eprintln!("8.3 alias unavailable; actual short-name comparison unverified");
            return;
        }
        let short = PathBuf::from(String::from_utf16(&short[..count as usize]).unwrap());
        if path_equal(&known, &short) {
            eprintln!("8.3 alias unavailable; actual short-name comparison unverified");
            return;
        }
        assert!(path_equal(
            &physical_path(&known).unwrap(),
            &physical_path(&short).unwrap()
        ));
        assert_eq!(
            separate_delivery_work(
                &short,
                &known.join("nonexistent-bootstrap-delivery-fixture")
            )
            .unwrap_err()
            .to_string(),
            "DELIVERY_WORK_OVERLAP"
        );
        eprintln!(
            "verified existing 8.3 prefix without writes: {}",
            short.display()
        );
    }
    #[test]
    fn bootstrap_formal_delivery_stays_outside_processing_work() {
        let profile = env::temp_dir().join(format!(
            "bootstrap-overlap-profile-{}",
            uuid::Uuid::new_v4()
        ));
        let _guard = private_work::PrivateWorkGuard::open(&profile).unwrap();
        let downloads = profile.join("Remnifold/Downloads");
        let delivery = downloads.join("bootstrap-fixture");
        for work in [&profile, &downloads, &delivery, &delivery.join("child")] {
            assert_eq!(
                separate_delivery_work(work, &delivery)
                    .unwrap_err()
                    .to_string(),
                "DELIVERY_WORK_OVERLAP"
            );
        }
        separate_delivery_work(&profile.join("processing"), &delivery).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn bootstrap_delivery_rejects_unsafe_persistent_ancestors() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{Authorization::*, *},
            Storage::FileSystem::*,
        };

        struct LocalMemory(*mut std::ffi::c_void);
        impl Drop for LocalMemory {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        LocalFree(self.0);
                    }
                }
            }
        }
        fn security(path: &Path, add_delete_child: bool) -> Vec<u16> {
            let file = fs::OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC | FILE_LIST_DIRECTORY)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .unwrap();
            unsafe {
                let mut descriptor = std::ptr::null_mut();
                let mut dacl = std::ptr::null_mut();
                assert_eq!(
                    GetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut dacl,
                        std::ptr::null_mut(),
                        &mut descriptor
                    ),
                    0
                );
                let _descriptor = LocalMemory(descriptor);
                if add_delete_child {
                    let sid_text: Vec<u16> = "S-1-5-11".encode_utf16().chain(Some(0)).collect();
                    let mut sid = std::ptr::null_mut();
                    assert_ne!(ConvertStringSidToSidW(sid_text.as_ptr(), &mut sid), 0);
                    let _sid = LocalMemory(sid);
                    let access = EXPLICIT_ACCESS_W {
                        grfAccessPermissions: FILE_DELETE_CHILD,
                        grfAccessMode: GRANT_ACCESS,
                        grfInheritance: NO_INHERITANCE,
                        Trustee: TRUSTEE_W {
                            pMultipleTrustee: std::ptr::null_mut(),
                            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                            TrusteeForm: TRUSTEE_IS_SID,
                            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                            ptstrName: sid.cast(),
                        },
                    };
                    let mut changed = std::ptr::null_mut();
                    assert_eq!(SetEntriesInAclW(1, &access, dacl, &mut changed), 0);
                    let _changed = LocalMemory(changed.cast());
                    assert_eq!(
                        SetSecurityInfo(
                            file.as_raw_handle(),
                            SE_FILE_OBJECT,
                            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            changed,
                            std::ptr::null()
                        ),
                        0
                    );
                    return security(path, false);
                }
                let mut text = std::ptr::null_mut();
                let mut length = 0;
                assert_ne!(
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor,
                        SDDL_REVISION_1,
                        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                        &mut text,
                        &mut length
                    ),
                    0
                );
                let _text = LocalMemory(text.cast());
                std::slice::from_raw_parts(text, length as usize).to_vec()
            }
        }

        let root = env::temp_dir().join(format!("bootstrap-delivery-{}", uuid::Uuid::new_v4()));
        let _root = private_work::PrivateWorkGuard::open(&root).unwrap();
        let delivery = root.join("Remnifold/Downloads/bootstrap-fixture");
        let bytes = b"formal authenticated archive fixture";
        let input = root.join("input.tar.gz");
        fs::write(&input, bytes).unwrap();
        // 外側ドライブの owner/ACL に依存せず、fixture 自身を子の置換が可能な祖先にする。
        let unsafe_security = security(&root, true);
        let input_security = security(&input, false);
        assert_eq!(
            private_work::PrivateWorkGuard::open(&root)
                .err()
                .unwrap()
                .to_string(),
            "PRIVATE_WORK_ACL_UNSAFE"
        );
        assert!(
            security(&root, false) == unsafe_security,
            "fixture ancestor security changed"
        );
        assert_eq!(fs::read(&input).unwrap(), bytes);
        assert!(
            security(&input, false) == input_security,
            "fixture input security changed"
        );
        // 全祖先の検証は、さらに外側の未信頼 owner を先に検出する環境もある。
        let error = deliver(
            &delivery,
            &root,
            bytes,
            &sha(bytes),
            &root.join("processing"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            matches!(
                error.as_str(),
                "PRIVATE_WORK_OWNER_MISMATCH" | "PRIVATE_WORK_ACL_UNSAFE"
            ),
            "unexpected delivery error: {error}"
        );
        assert!(
            security(&root, false) == unsafe_security,
            "fixture ancestor security changed"
        );
        assert_eq!(fs::read(&input).unwrap(), bytes);
        assert!(
            security(&input, false) == input_security,
            "fixture input security changed"
        );
        assert!(!delivery.exists());
        assert!(!delivery.join("remnifold.tar.gz").exists());
        assert!(
            deliver(
                &root.join("other"),
                &root,
                bytes,
                &"0".repeat(64),
                &root.join("processing")
            )
            .is_err()
        );
        assert!(!root.join("other").exists());
    }
    #[cfg(windows)]
    #[test]
    fn bootstrap_private_delivery_rehashes_and_keeps_directory_locks() {
        let profile = env::temp_dir().join(format!(
            "bootstrap-private-profile-{}",
            uuid::Uuid::new_v4()
        ));
        let root_guard = private_work::PrivateWorkGuard::open(&profile).unwrap();
        let delivery = profile.join("Remnifold/Downloads/bootstrap-fixture");
        let guard = private_work::PrivateWorkGuard::open(&delivery).unwrap();
        let bytes = b"verified formal archive";
        let path = deliver_bytes(&delivery, bytes, &sha(bytes), &guard).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        private_work::validate_receipt(&path).unwrap();
        assert!(fs::rename(&profile, profile.with_extension("moved")).is_err());
        assert!(deliver_bytes(&delivery, bytes, &sha(bytes), &guard).is_err());
        drop(guard);
        drop(root_guard);
    }
    #[test]
    fn bootstrap_arguments_preserve_explicit_action_boundary() {
        for argv in [
            vec!["--version", "1.0.17"],
            vec!["-Version", "1.0.17", "-DownloadOnly"],
            vec![
                "-version",
                "1.0.17",
                "-wOrKrOoT",
                "D:/example",
                "-downloadonly",
            ],
            vec!["-VERSION", "1.0.17", "-aPpLy"],
        ] {
            assert!(arguments(&strings(&argv)).is_ok());
        }
        for argv in [
            vec!["--version", "1.0.17", "--apply", "--download-only"],
            vec!["--version", "1.0.17", "-Version", "1.0.17"],
            vec!["-version", "1.0.17", "-VERSION", "1.0.17"],
            vec!["-version", "1.0.17", "-apply", "-APPLY"],
            vec!["--Version", "1.0.17"],
            vec!["--version", "01.0.17"],
        ] {
            assert!(arguments(&strings(&argv)).is_err());
        }
    }
}
