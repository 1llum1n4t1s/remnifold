use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha512};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
fn track(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut paths: Vec<_> = fs::read_dir(path)
            .expect("read embedded source directory")
            .map(|e| e.expect("source entry").path())
            .collect();
        paths.sort();
        for p in paths {
            track(&p);
        }
    }
}
fn verify_esbuild(
    esbuild: &Path,
    target: &str,
    version: &str,
    entry: &str,
    integrity: &str,
) -> PathBuf {
    assert!(
        esbuild.is_absolute(),
        "REMNIFOLD_ESBUILD must be an absolute prepared-cache entry"
    );
    assert!(
        esbuild
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir)),
        "esbuild path must not contain parent traversal"
    );
    for path in esbuild.ancestors() {
        let metadata =
            fs::symlink_metadata(path).expect("native esbuild missing: run cargo xtask prepare");
        assert!(
            !metadata.file_type().is_symlink(),
            "linked esbuild cache path is not allowed"
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            assert_eq!(
                metadata.file_attributes() & 0x400,
                0,
                "reparse esbuild cache path is not allowed"
            );
        }
    }
    let mut cache = esbuild;
    for component in Path::new(entry).components().rev() {
        assert_eq!(
            cache.file_name(),
            Some(component.as_os_str()),
            "REMNIFOLD_ESBUILD must point to the prepared-cache entry"
        );
        cache = cache.parent().expect("esbuild cache parent");
    }
    assert_eq!(
        cache.file_name().and_then(|x| x.to_str()),
        Some(format!("esbuild-{version}").as_str()),
        "esbuild cache version directory mismatch"
    );
    assert_eq!(
        cache
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|x| x.to_str()),
        Some(target),
        "esbuild cache target directory mismatch"
    );
    let archive_path = cache.join("archive.tgz");
    let metadata = fs::symlink_metadata(&archive_path)
        .expect("prepared esbuild archive missing: run cargo xtask prepare");
    assert!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "esbuild archive must be a regular file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(
            metadata.file_attributes() & 0x400,
            0,
            "esbuild archive must not be a reparse point"
        );
    }
    assert!(
        metadata.len() <= 256 * 1024 * 1024,
        "esbuild archive exceeds size limit"
    );
    println!("cargo:rerun-if-changed={}", archive_path.display());
    let bytes = fs::read(&archive_path).expect("read prepared esbuild archive");
    assert_eq!(
        format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes))),
        integrity,
        "esbuild archive does not match source-pinned integrity"
    );
    let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_slice()));
    let mut expected = None;
    for file in archive.entries().expect("read authenticated esbuild tar") {
        let mut file = file.expect("read authenticated esbuild entry");
        if file.path().expect("esbuild entry path") != Path::new(entry) {
            continue;
        }
        assert!(
            expected.is_none() && file.header().entry_type().is_file(),
            "invalid executable entry in esbuild archive"
        );
        assert!(
            file.size() > 0 && file.size() <= 256 * 1024 * 1024,
            "esbuild entry size limit"
        );
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .expect("read authenticated executable");
        expected = Some(contents);
    }
    let expected = expected.expect("native executable missing from authenticated esbuild archive");
    assert_eq!(
        fs::metadata(esbuild)
            .expect("esbuild executable metadata")
            .len(),
        expected.len() as u64,
        "esbuild executable differs from authenticated archive"
    );
    let actual = fs::read(esbuild).expect("read installed esbuild executable");
    assert!(
        actual == expected,
        "esbuild executable differs from authenticated archive"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(esbuild).unwrap().permissions().mode() & 0o111,
            0,
            "esbuild is not executable"
        );
    }
    esbuild.to_path_buf()
}
fn main() {
    let repo = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = repo.join("server/src");
    let migrations = repo.join("server/migrations");
    track(&source);
    track(&migrations);
    track(&repo.join("server/package.json"));
    track(&repo.join("dev/toolchain.json"));
    println!("cargo:rerun-if-env-changed=REMNIFOLD_ESBUILD");
    let target = env::var("HOST").expect("build host");
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(repo.join("dev/toolchain.json")).expect("read pinned toolchain"),
    )
    .expect("parse pinned toolchain");
    let package = &manifest["targets"][&target]["esbuild"];
    let version = package["version"]
        .as_str()
        .expect("supported build host esbuild version");
    let entry = package["entry"]
        .as_str()
        .expect("supported build host esbuild entry");
    let esbuild = env::var_os("REMNIFOLD_ESBUILD")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo.join(".devtools")
                .join(&target)
                .join(format!("esbuild-{version}"))
                .join(entry)
        });
    println!("cargo:rerun-if-changed={}", esbuild.display());
    let esbuild = verify_esbuild(
        &esbuild,
        &target,
        version,
        entry,
        package["integrity"]
            .as_str()
            .expect("pinned esbuild integrity"),
    );
    let actual_version = Command::new(&esbuild).arg("--version").output().unwrap_or_else(|_| panic!("native esbuild missing: run cargo xtask prepare, or set REMNIFOLD_ESBUILD to the verified native esbuild"));
    assert!(
        actual_version.status.success()
            && String::from_utf8_lossy(&actual_version.stdout).trim() == version,
        "expected native esbuild {version}; run cargo xtask prepare"
    );
    let status = Command::new(&esbuild)
        .arg(source.join("index.ts"))
        .args([
            "--bundle",
            "--format=esm",
            "--platform=browser",
            "--target=es2022",
        ])
        .arg(format!("--outfile={}", out.join("worker.mjs").display()))
        .status()
        .expect("run native esbuild");
    assert!(status.success(), "bundle embedded Worker");
    let mut files: Vec<_> = fs::read_dir(&migrations)
        .expect("read migrations")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    let mut generated = String::from("pub const MIGRATIONS: &[(&str, &str)] = &[\n");
    for file in files {
        generated.push_str(&format!(
            "({:?}, {:?}),\n",
            file.file_name().unwrap().to_str().unwrap(),
            fs::read_to_string(&file).expect("read SQL migration")
        ));
    }
    generated.push_str("];\n");
    fs::write(out.join("migrations.rs"), generated).expect("write embedded migrations");
}
