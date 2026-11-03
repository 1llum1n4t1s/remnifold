use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug)]
pub struct Toolchain {
    pub esbuild: PathBuf,
    pub tsc: PathBuf,
    pub workerd: PathBuf,
    pub cloudflare_types: PathBuf,
}
#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    targets: BTreeMap<String, BTreeMap<String, Package>>,
}
#[derive(Deserialize, Serialize)]
struct Package {
    version: String,
    url: String,
    integrity: String,
    entry: String,
}
#[derive(Deserialize, Serialize)]
struct Receipt {
    target: String,
    package: Package,
    files: BTreeMap<String, String>,
}
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;
fn host() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        other => bail!("unsupported toolchain host: {other:?}"),
    }
}
fn safe_root(root: &Path) -> Result<()> {
    if !root.is_absolute() {
        bail!("tool cache must be an absolute path");
    }
    for p in root.ancestors() {
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.file_type().is_symlink() {
                bail!("linked cache ancestor: {}", p.display());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    bail!("reparse cache ancestor: {}", p.display());
                }
            }
        }
    }
    Ok(())
}
fn digest(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hash = Sha512::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("sha512-{}", STANDARD.encode(hash.finalize())))
}
fn inventory(dir: &Path, base: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        safe_root(&p)?;
        let m = fs::symlink_metadata(&p)?;
        if m.is_dir() {
            inventory(&p, base, files)?;
        } else if m.is_file() {
            let key = p.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
            if key != "receipt.json" && key != "archive.tgz" {
                files.insert(key, digest(&p)?);
            }
        } else {
            bail!("nonregular tool cache file: {}", p.display());
        }
    }
    Ok(())
}
fn native_header(path: &Path, target: &str) -> Result<()> {
    let mut f = fs::File::open(path)?;
    let mut b = [0u8; 4096];
    let n = f.read(&mut b)?;
    let arm = target.starts_with("aarch64");
    let valid = if target.contains("windows") {
        if n < 64 || &b[..2] != b"MZ" {
            false
        } else {
            let offset = u32::from_le_bytes(b[60..64].try_into()?) as usize;
            offset + 6 <= n
                && &b[offset..offset + 4] == b"PE\0\0"
                && u16::from_le_bytes(b[offset + 4..offset + 6].try_into()?) == 0x8664
        }
    } else if target.contains("linux") {
        n >= 20
            && &b[..4] == b"\x7fELF"
            && b[4] == 2
            && b[5] == 1
            && u16::from_le_bytes(b[18..20].try_into()?) == if arm { 183 } else { 62 }
    } else {
        n >= 8
            && b[..4] == [0xcf, 0xfa, 0xed, 0xfe]
            && u32::from_le_bytes(b[4..8].try_into()?) == if arm { 0x0100000c } else { 0x01000007 }
    };
    if !valid {
        bail!(
            "tool executable does not match {target}: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
            bail!("tool is not executable");
        }
    }
    Ok(())
}
fn verify(dir: &Path, target: &str, package: &Package, key: &str) -> Result<PathBuf> {
    safe_root(dir)?;
    safe_root(&dir.join("receipt.json"))?;
    safe_root(&dir.join("archive.tgz"))?;
    let receipt: Receipt = serde_json::from_slice(&fs::read(dir.join("receipt.json"))?)?;
    if receipt.target != target
        || serde_json::to_value(&receipt.package)? != serde_json::to_value(package)?
    {
        bail!("tool cache receipt mismatch");
    }
    let archive_path = dir.join("archive.tgz");
    if fs::metadata(&archive_path)?.len() > MAX_ARCHIVE {
        bail!("cached archive exceeds size limit");
    }
    if digest(&archive_path)? != package.integrity {
        bail!("cached tool archive integrity mismatch");
    }
    let mut trusted = BTreeMap::new();
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(&archive_path)?));
    let mut total = 0u64;
    for e in archive.entries()? {
        let mut e = e?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        total = total
            .checked_add(e.size())
            .context("cache archive size overflow")?;
        if total > MAX_UNPACKED {
            bail!("cached archive exceeds extraction limit");
        }
        let path = e.path()?.to_string_lossy().into_owned();
        let mut h = Sha512::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = e.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        trusted.insert(path, format!("sha512-{}", STANDARD.encode(h.finalize())));
    }
    if trusted != receipt.files {
        bail!("tool cache receipt differs from authenticated archive");
    }
    let mut files = BTreeMap::new();
    inventory(dir, dir, &mut files)?;
    if files != receipt.files || files.is_empty() {
        bail!("tool cache was modified: {}", dir.display());
    }
    let entry = dir.join(&package.entry);
    if key != "cloudflare_types" {
        native_header(&entry, target)?;
        let output = Command::new(&entry)
            .arg("--version")
            .output()
            .context("check native tool version")?;
        let version = String::from_utf8_lossy(&output.stdout);
        let expected = if key == "workerd" {
            format!(
                "{}-{}-{}",
                &package.version[2..6],
                &package.version[6..8],
                &package.version[8..10]
            )
        } else {
            package.version.clone()
        };
        if !output.status.success()
            || !(version.contains(&package.version) || version.contains(&expected))
        {
            bail!("unexpected {key} version: {}", version.trim());
        }
    } else if fs::metadata(&entry)?.len() == 0 {
        bail!("empty Cloudflare declarations");
    }
    Ok(entry)
}
fn install(
    client: &reqwest::blocking::Client,
    root: &Path,
    target: &str,
    key: &str,
    package: &Package,
) -> Result<PathBuf> {
    let dir = root.join(target).join(format!("{key}-{}", package.version));
    if dir.exists() {
        return verify(&dir, target, package, key);
    }
    let url = url::Url::parse(&package.url)?;
    if url.scheme() != "https" || url.host_str() != Some("registry.npmjs.org") {
        bail!("untrusted tool archive URL");
    }
    let mut response = client.get(url).send()?.error_for_status()?;
    if response.content_length().is_some_and(|n| n > MAX_ARCHIVE) {
        bail!("tool archive too large");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut response)
        .take(MAX_ARCHIVE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARCHIVE
        || format!("sha512-{}", STANDARD.encode(Sha512::digest(&bytes))) != package.integrity
    {
        bail!("tool archive integrity mismatch: {key}");
    }
    let parent = dir.parent().context("cache parent")?;
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(".{key}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_slice()));
    let mut total = 0u64;
    let mut count = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !path.components().all(|c| matches!(c, Component::Normal(_)))
            || !path.starts_with("package")
            || path.to_string_lossy().contains(['\\', ':'])
        {
            bail!("unsafe tool archive path");
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("tool archive contains links or special files");
        }
        if kind.is_dir() {
            continue;
        }
        count += 1;
        total = total
            .checked_add(entry.size())
            .context("archive size overflow")?;
        if count > 20000 || total > MAX_UNPACKED || entry.size() > MAX_ARCHIVE {
            bail!("tool archive extraction limit exceeded");
        }
        let dest = stage.join(&path);
        fs::create_dir_all(dest.parent().context("archive parent")?)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)?;
        std::io::copy(&mut entry, &mut file)?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if path == Path::new(&package.entry) && key != "cloudflare_types" {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode))?;
        }
    }
    let mut files = BTreeMap::new();
    inventory(&stage, &stage, &mut files)?;
    let receipt = Receipt {
        target: target.into(),
        package: serde_json::from_value(serde_json::to_value(package)?)?,
        files,
    };
    fs::write(stage.join("archive.tgz"), &bytes)?;
    fs::write(
        stage.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    verify(&stage, target, package, key)?;
    match fs::rename(&stage, &dir) {
        Ok(()) => (),
        Err(e) if dir.exists() => {
            let _ = e;
        }
        Err(e) => return Err(e.into()),
    }
    verify(&dir, target, package, key)
}
pub fn prepare(root: &Path) -> Result<Toolchain> {
    safe_root(root)?;
    fs::create_dir_all(root)?;
    let manifest: Manifest = serde_json::from_str(include_str!("../toolchain.json"))?;
    if manifest.schema != 1 {
        bail!("unsupported toolchain manifest schema");
    }
    let target = host()?;
    let packages = manifest
        .targets
        .get(target)
        .context("missing host toolchain")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut paths = BTreeMap::new();
    for (key, package) in packages {
        eprintln!("Verifying {key} {} ({target})", package.version);
        paths.insert(key.as_str(), install(&client, root, target, key, package)?);
    }
    Ok(Toolchain {
        esbuild: paths.remove("esbuild").context("esbuild")?,
        tsc: paths.remove("tsc").context("tsc")?,
        workerd: paths.remove("workerd").context("workerd")?,
        cloudflare_types: paths
            .remove("cloudflare_types")
            .context("Cloudflare types")?,
    })
}
pub fn check_types(tools: &Toolchain, repo: &Path, work: &Path) -> Result<()> {
    safe_root(work)?;
    fs::create_dir_all(work)?;
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join("server/tsconfig.json"))?)?;
    config["compilerOptions"]["types"] = serde_json::json!([tools.cloudflare_types]);
    config["compilerOptions"]["noEmit"] = true.into();
    config["include"] = serde_json::json!([repo.join("server/src/**/*.ts")]);
    let path = work.join("tsconfig.json");
    fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    let status = Command::new(&tools.tsc)
        .arg("--project")
        .arg(path)
        .status()?;
    if !status.success() {
        bail!("Worker TypeScript validation failed");
    }
    Ok(())
}
