//! Node を使わない R2 発行。最後の index 作成だけが公開の可視性を切り替える。
use crate::release_manifest::{self as manifest, ReleaseAsset, ReleaseManifest, VersionIndex};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Method, StatusCode, blocking::Client};
use ring::hmac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const STAGING: &str = "remnifold-builds";
const PUBLIC: &str = "remnifold-updates";
fn hash(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}
fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("Missing {name}"))
}
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()?)
}
fn body(response: reqwest::blocking::Response, max: u64) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= max),
        "Oversized HTTP response"
    );
    let mut b = Vec::new();
    response
        .take(max + 1)
        .read_to_end(&mut b)
        .context("HTTP body failed")?;
    ensure!(b.len() as u64 <= max, "Oversized HTTP response");
    Ok(b)
}
fn mac(key: &[u8], value: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), value.as_bytes())
        .as_ref()
        .to_vec()
}
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let key = mac(format!("AWS4{secret}").as_bytes(), date);
    let key = mac(&key, region);
    let key = mac(&key, service);
    mac(&key, "aws4_request")
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}
// UTC civil date conversion (the request clock does not depend on an external program).
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    let s = seconds % 86400;
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}
fn authorization(
    method: &str,
    path: &str,
    host: &str,
    stamp: &str,
    payload: &str,
    key: &str,
    secret: &str,
) -> String {
    let date = &stamp[..8];
    let scope = format!("{date}/auto/s3/aws4_request");
    let canonical = format!(
        "{method}\n{path}\n\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{stamp}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}"
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hash(canonical.as_bytes())
    );
    let k = signing_key(secret, date, "auto", "s3");
    format!(
        "AWS4-HMAC-SHA256 Credential={key}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={}",
        hex(&mac(&k, &to_sign))
    )
}
struct Store {
    http: Client,
    host: String,
    origin: String,
    key: String,
    secret: String,
}
impl Store {
    fn new(prefix: &str) -> Result<Self> {
        let account = env("CLOUDFLARE_ACCOUNT_ID")?;
        ensure!(
            manifest::hex(&account, 32),
            "Invalid Cloudflare account identity"
        );
        Ok(Self {
            http: client()?,
            host: format!("{account}.r2.cloudflarestorage.com"),
            origin: format!("https://{account}.r2.cloudflarestorage.com"),
            key: env(&format!("{prefix}_ACCESS_KEY_ID"))?,
            secret: env(&format!("{prefix}_SECRET_ACCESS_KEY"))?,
        })
    }
    fn request(
        &self,
        method: Method,
        bucket: &str,
        key: &str,
        bytes: Option<&[u8]>,
        immutable: bool,
    ) -> Result<reqwest::blocking::Response> {
        self.request_condition(method, bucket, key, bytes, immutable, None)
    }
    fn request_condition(
        &self,
        method: Method,
        bucket: &str,
        key: &str,
        bytes: Option<&[u8]>,
        immutable: bool,
        etag: Option<&str>,
    ) -> Result<reqwest::blocking::Response> {
        ensure!(
            [STAGING, PUBLIC].contains(&bucket)
                && key.split('/').all(|p| !p.is_empty()
                    && p != "."
                    && p != ".."
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))),
            "Unsafe R2 key"
        );
        let path = format!("/{bucket}/{key}").replace('+', "%2B");
        let stamp = timestamp();
        let payload = hash(bytes.unwrap_or_default());
        let auth = authorization(
            method.as_str(),
            &path,
            &self.host,
            &stamp,
            &payload,
            &self.key,
            &self.secret,
        );
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.origin))
            .header("x-amz-date", stamp)
            .header("x-amz-content-sha256", payload)
            .header("Authorization", auth);
        if let Some(bytes) = bytes {
            request = request
                .header(
                    "Cache-Control",
                    if immutable {
                        "public,max-age=31536000,immutable"
                    } else {
                        "no-store"
                    },
                )
                .body(bytes.to_vec());
            request = if let Some(etag) = etag {
                request.header("If-Match", etag)
            } else {
                request.header("If-None-Match", "*")
            };
        }
        request
            .send()
            .map_err(|_| anyhow::anyhow!("R2 request failed"))
    }
    fn get(&self, bucket: &str, key: &str, max: u64) -> Result<Option<Vec<u8>>> {
        let response = self.request(Method::GET, bucket, key, None, false)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "R2 read rejected ({})",
            response.status().as_u16()
        );
        Ok(Some(body(response, max)?))
    }
    fn get_index(&self, key: &str) -> Result<Option<(Vec<u8>, String)>> {
        let response = self.request(Method::GET, PUBLIC, key, None, false)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(response.status().is_success(), "R2 index read rejected");
        let etag = response
            .headers()
            .get("ETag")
            .context("R2 index ETag missing")?
            .to_str()
            .context("Invalid R2 ETag")?
            .to_owned();
        ensure!(
            etag.len() <= 256 && etag.starts_with('"') && etag.ends_with('"'),
            "Invalid R2 index ETag"
        );
        Ok(Some((body(response, manifest::MAX_JSON as u64)?, etag)))
    }
    fn replace_provisional_index(&self, key: &str, bytes: &[u8], etag: &str) -> Result<()> {
        let response =
            self.request_condition(Method::PUT, PUBLIC, key, Some(bytes), false, Some(etag))?;
        if [StatusCode::PRECONDITION_FAILED, StatusCode::CONFLICT].contains(&response.status()) {
            ensure!(
                self.get(PUBLIC, key, manifest::MAX_JSON as u64)?.as_deref() == Some(bytes),
                "Provisional publication CAS conflict"
            );
            return Ok(());
        }
        ensure!(
            response.status().is_success(),
            "Provisional index write rejected"
        );
        ensure!(
            self.get(PUBLIC, key, manifest::MAX_JSON as u64)?.as_deref() == Some(bytes),
            "Provisional index verification failed"
        );
        Ok(())
    }
    fn put(&self, bucket: &str, key: &str, bytes: &[u8], immutable: bool) -> Result<()> {
        let r = self.request(Method::PUT, bucket, key, Some(bytes), immutable)?;
        if [StatusCode::PRECONDITION_FAILED, StatusCode::CONFLICT].contains(&r.status()) {
            ensure!(
                self.get(bucket, key, bytes.len() as u64)?.as_deref() == Some(bytes),
                "R2 immutable object conflict"
            );
            return Ok(());
        }
        ensure!(
            r.status().is_success(),
            "R2 write rejected ({})",
            r.status().as_u16()
        );
        ensure!(
            self.get(bucket, key, bytes.len() as u64)?.as_deref() == Some(bytes),
            "R2 post-write verification failed"
        );
        Ok(())
    }
    fn remove_owned(&self, bucket: &str, key: &str, bytes: &[u8]) -> Result<()> {
        ensure!(
            key.starts_with("validation/") && key.ends_with(".json"),
            "Invalid validation cleanup key"
        );
        let Some(actual) = self.get(bucket, key, bytes.len() as u64)? else {
            return Ok(());
        };
        ensure!(actual == bytes, "Validation cleanup ownership mismatch");
        let response = self.request(Method::DELETE, bucket, key, None, false)?;
        ensure!(
            response.status().is_success(),
            "Validation cleanup rejected"
        );
        ensure!(
            self.get(bucket, key, bytes.len() as u64)?.is_none(),
            "Validation cleanup verification failed"
        );
        Ok(())
    }
}

/// 配布済み version/ref に触れず、UUID 所有の一時オブジェクトだけで実基盤を検証する。
pub fn smoke(args: &[String]) -> Result<()> {
    let (apply, _) = options(args, &[])?;
    if !apply {
        println!("R2 smoke requires --apply; validation objects will be uploaded and removed");
        return Ok(());
    }
    let root = PathBuf::from(env("REMNIFOLD_R2_WORK_ROOT")?);
    crate::package::safe_path(&root)?;
    ensure!(
        !root.starts_with(repo()),
        "Smoke work root must be external"
    );
    fs::create_dir_all(&root)?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let key = format!("validation/{nonce}+build.json");
    let bytes = serde_json::to_vec(
        &serde_json::json!({"schema":1,"purpose":"remnifold-r2-smoke","nonce":nonce}),
    )?;
    let evidence = root.join(format!("r2-smoke-{nonce}.json"));
    let ci = Store::new("R2_CI");
    let release = Store::new("R2_RELEASE");
    let mut checks = serde_json::Map::new();
    let result = (|| -> Result<()> {
        let ci = ci
            .as_ref()
            .map_err(|_| anyhow::anyhow!("R2_SMOKE_CI_CONFIGURATION"))?;
        let release = release
            .as_ref()
            .map_err(|_| anyhow::anyhow!("R2_SMOKE_RELEASE_CONFIGURATION"))?;
        ci.put(STAGING, &key, &bytes, false)?;
        checks.insert("privateCiPutGet".into(), true.into());
        let denied = ci.request(Method::PUT, PUBLIC, &key, Some(&bytes), false)?;
        ensure!(
            denied.status() == StatusCode::FORBIDDEN,
            "R2_SMOKE_CI_PUBLIC_PERMISSION"
        );
        checks.insert("ciPublicPutDenied".into(), true.into());
        ensure!(
            release.get(STAGING, &key, bytes.len() as u64)?.as_deref() == Some(bytes.as_slice()),
            "R2_SMOKE_RELEASE_PRIVATE_READ"
        );
        checks.insert("releasePrivateGet".into(), true.into());
        release.put(PUBLIC, &key, &bytes, false)?;
        checks.insert("releasePublicPutGet".into(), true.into());
        let different = serde_json::to_vec(&serde_json::json!({"nonce":nonce,"changed":true}))?;
        let conflict = release.request(Method::PUT, PUBLIC, &key, Some(&different), false)?;
        ensure!(
            conflict.status() == StatusCode::PRECONDITION_FAILED,
            "R2_SMOKE_CONDITIONAL_PUT"
        );
        ensure!(
            release.get(PUBLIC, &key, bytes.len() as u64)?.as_deref() == Some(bytes.as_slice()),
            "R2_SMOKE_CONFLICT_CHANGED_BYTES"
        );
        checks.insert("conditionalConflictPreservedBytes".into(), true.into());
        let response = client()?
            .get(format!("{}/{key}", manifest::PUBLIC_BASE_URL))
            .header("Cache-Control", "no-cache")
            .send()
            .map_err(|_| anyhow::anyhow!("R2_SMOKE_PUBLIC_REQUEST"))?;
        ensure!(
            response.status().is_success() && body(response, bytes.len() as u64)? == bytes,
            "R2_SMOKE_PUBLIC_BYTES"
        );
        checks.insert("publicHttpsBytes".into(), true.into());
        Ok(())
    })();
    // 読めない／内容の異なるものは削除しない。失敗経路でも自分の nonce を照合する。
    let private_cleanup = ci
        .as_ref()
        .is_ok_and(|s| s.remove_owned(STAGING, &key, &bytes).is_ok());
    let public_cleanup = release
        .as_ref()
        .is_ok_and(|s| s.remove_owned(PUBLIC, &key, &bytes).is_ok());
    let success = result.is_ok() && private_cleanup && public_cleanup;
    let report = serde_json::json!({"schema":1,"success":success,"objectKey":key,"objectSha256":hash(&bytes),"checks":checks,"privateCleanup":private_cleanup,"publicCleanup":public_cleanup,"failureCode":if success{serde_json::Value::Null}else{serde_json::json!("R2_SMOKE_FAILED")}});
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&evidence)?
        .write_all(&serde_json::to_vec_pretty(&report)?)?;
    println!("R2 smoke evidence: {}", evidence.display());
    ensure!(success, "R2_SMOKE_FAILED");
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StageRecord {
    target: String,
    version: String,
    source_commit: String,
    run_id: u64,
    run_attempt: u64,
    key: String,
    size: u64,
    sha256: String,
    native_manifest_sha256: String,
}
/// Owner-trusted build input; CI binds it to its own run only after verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalInputRecord {
    schema: u32,
    repository: String,
    version: String,
    source_commit: String,
    target: String,
    key: String,
    size: u64,
    sha256: String,
    native_manifest_sha256: String,
}
impl LocalInputRecord {
    fn validate(&self, version: &str, commit: &str, target: &str) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.repository == manifest::REPOSITORY
                && self.version == version
                && manifest::version(version)
                && self.source_commit == commit
                && manifest::hex(commit, 40)
                && self.target == target
                && manifest::TARGETS.contains(&target)
                && !target.contains("windows")
                && self.size > 0
                && self.size <= manifest::MAX_ARCHIVE
                && manifest::hex(&self.sha256, 64)
                && manifest::hex(&self.native_manifest_sha256, 64)
                && self.key == format!("local-inputs/{commit}/{target}/{}.tar.gz", self.sha256),
            "Local input identity mismatch"
        );
        Ok(())
    }
    fn verify_bytes(&self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() as u64 == self.size && hash(bytes) == self.sha256,
            "Local input archive size or digest mismatch"
        );
        let (_, native) = native_parts(bytes, &self.target)?;
        ensure!(
            hash(&native) == self.native_manifest_sha256,
            "Local input native manifest mismatch"
        );
        Ok(())
    }
}
fn local_record_key(commit: &str, target: &str) -> String {
    format!("local-inputs/{commit}/{target}/record.json")
}
fn require_local_import_origin(
    event: &str,
    repository: &str,
    workflow: &str,
    reference: &str,
    version: &str,
) -> Result<()> {
    ensure!(
        event == "push"
            && repository == manifest::REPOSITORY
            && reference == format!("refs/heads/release/{version}")
            && workflow
                == format!(
                    "{}/.github/workflows/ci.yml@{reference}",
                    manifest::REPOSITORY
                ),
        "Local input import requires a trusted release push"
    );
    Ok(())
}
fn download_local_input(
    store: &Store,
    id: &Identity,
    target: &str,
) -> Result<(LocalInputRecord, Vec<u8>)> {
    let input = store.get(STAGING, &local_record_key(&id.commit, target), manifest::MAX_JSON as u64)?
        .context("Local input missing for GITHUB_SHA and target; stage the verified local build before pushing")?;
    let record: LocalInputRecord = manifest::parse_strict(&input)?;
    record.validate(&id.version, &id.commit, target)?;
    let bytes = store
        .get(STAGING, &record.key, record.size)?
        .context("Local input archive missing")?;
    record.verify_bytes(&bytes)?;
    Ok((record, bytes))
}
fn upload_local_input(store: &Store, record: &LocalInputRecord, bytes: &[u8]) -> Result<()> {
    record.verify_bytes(bytes)?;
    let key = local_record_key(&record.source_commit, &record.target);
    if let Some(existing) = store.get(STAGING, &key, manifest::MAX_JSON as u64)? {
        let previous: LocalInputRecord = manifest::parse_strict(&existing)?;
        ensure!(previous == *record, "Conflicting local input record");
        let archive = store
            .get(STAGING, &record.key, record.size)?
            .context("Existing local input archive missing")?;
        ensure!(archive == bytes, "Existing local input archive changed");
        return Ok(());
    }
    store.put(STAGING, &record.key, bytes, true)?;
    store.put(STAGING, &key, &serde_json::to_vec(record)?, true)
}
pub fn local_input(args: &[String]) -> Result<()> {
    let action = args
        .first()
        .context("local-input requires stage or import")?;
    ensure!(
        matches!(action.as_str(), "stage" | "import"),
        "Unknown local-input action"
    );
    let allowed: &[&str] = if action == "stage" {
        &["--target", "--archive", "--binary", "--source-commit"]
    } else {
        &["--target", "--work-root"]
    };
    let (apply, o) = options(&args[1..], allowed)?;
    let get = |key: &str| o.get(key).context("Missing local-input option");
    let target = get("--target")?;
    ensure!(
        manifest::TARGETS.contains(&target.as_str()) && !target.contains("windows"),
        "Local input requires a supported Unix target"
    );
    if action == "stage" {
        let commit = get("--source-commit")?;
        crate::source::verify_checkout(&repo(), commit)?;
        let archive = Path::new(get("--archive")?);
        let binary = Path::new(get("--binary")?);
        crate::package::verify(&repo(), target, binary, archive, commit)?;
        crate::source::verify(&repo(), archive, commit)?;
        ensure!(
            fs::metadata(archive)?.len() <= manifest::MAX_ARCHIVE,
            "Oversized local input archive"
        );
        let bytes = fs::read(archive)?;
        let (_, native) = native_parts(&bytes, target)?;
        let version = serde_json::from_slice::<serde_json::Value>(&fs::read(
            repo().join("package.json"),
        )?)?["version"]
            .as_str()
            .context("Missing version")?
            .to_owned();
        let digest = hash(&bytes);
        let record = LocalInputRecord {
            schema: 1,
            repository: manifest::REPOSITORY.into(),
            version,
            source_commit: commit.clone(),
            target: target.clone(),
            key: format!("local-inputs/{commit}/{target}/{digest}.tar.gz"),
            size: bytes.len() as u64,
            sha256: digest,
            native_manifest_sha256: hash(&native),
        };
        record.validate(&record.version, commit, target)?;
        if apply {
            upload_local_input(&Store::new("R2_CI")?, &record, &bytes)?;
        }
        println!(
            "Local input verified{}",
            if apply {
                " and staged"
            } else {
                "; --apply required to upload"
            }
        );
        return Ok(());
    }
    let id = identity(false)?;
    require_local_import_origin(
        &env("GITHUB_EVENT_NAME")?,
        &env("GITHUB_REPOSITORY")?,
        &env("GITHUB_WORKFLOW_REF")?,
        &env("GITHUB_REF")?,
        &id.version,
    )?;
    let root = PathBuf::from(get("--work-root")?);
    crate::updater_e2e::validate_work_root(&repo(), &root)?;
    fs::create_dir_all(&root)?;
    let store = Store::new("R2_CI")?;
    let (record, bytes) = download_local_input(&store, &id, target)?;
    LocalVerifier { repo: repo(), root }.verify(
        target,
        &bytes,
        &record.native_manifest_sha256,
        &id.commit,
    )?;
    if !apply {
        println!("Local input verified; --apply required to bind and stage for CI");
        return Ok(());
    }
    let staged = StageRecord {
        target: target.clone(),
        version: id.version,
        source_commit: id.commit,
        run_id: id.run,
        run_attempt: id.attempt,
        key: format!(
            "ci/{}/{}/{target}/{}.tar.gz",
            id.run, id.attempt, record.sha256
        ),
        size: record.size,
        sha256: record.sha256,
        native_manifest_sha256: record.native_manifest_sha256,
    };
    store.put(STAGING, &staged.key, &bytes, true)?;
    stage_output(&staged)
}
fn stage_output(record: &StageRecord) -> Result<()> {
    let output = PathBuf::from(env("GITHUB_OUTPUT")?);
    crate::package::safe_path(&output)?;
    writeln!(
        fs::OpenOptions::new().append(true).open(output)?,
        "{}={}",
        output_key(&record.target)?,
        serde_json::to_string(record)?
    )?;
    Ok(())
}
fn output_key(target: &str) -> Result<&'static str> {
    Ok(match target {
        "x86_64-pc-windows-msvc" => "windows_x64",
        "aarch64-apple-darwin" => "macos_arm64",
        "x86_64-unknown-linux-gnu" => "linux_x64",
        "aarch64-unknown-linux-gnu" => "linux_arm64",
        _ => bail!("Invalid target"),
    })
}
struct Identity {
    version: String,
    commit: String,
    run: u64,
    attempt: u64,
}
fn identity(publishing: bool) -> Result<Identity> {
    manifest::parse_config(&fs::read(
        repo().join(".github/remnifold-distribution.json"),
    )?)?;
    let version = serde_json::from_slice::<serde_json::Value>(&fs::read(
        repo().join("package.json"),
    )?)?["version"]
        .as_str()
        .context("Missing version")?
        .to_owned();
    let commit = env("GITHUB_SHA")?;
    let run = env("GITHUB_RUN_ID")?.parse()?;
    let attempt = env("GITHUB_RUN_ATTEMPT")?.parse()?;
    let reference = env("GITHUB_REF")?;
    ensure!(
        manifest::version(&version)
            && manifest::hex(&commit, 40)
            && run > 0
            && attempt > 0
            && env("GITHUB_REPOSITORY")? == manifest::REPOSITORY
            && env("GITHUB_EVENT_NAME")? == "push"
            && env("GITHUB_JOB")?
                == if publishing {
                    "publish-r2"
                } else {
                    "native-package"
                }
            && env("CI_R2_ENVIRONMENT")?
                == if publishing {
                    "r2-release"
                } else {
                    "r2-staging"
                }
            && (reference == format!("refs/heads/release/{version}")
                || (!publishing && reference == "refs/heads/main"))
            && env("GITHUB_WORKFLOW_REF")?
                == format!(
                    "{}/.github/workflows/ci.yml@{reference}",
                    manifest::REPOSITORY
                ),
        "Untrusted publication identity"
    );
    crate::source::verify_checkout(&repo(), &commit)?;
    Ok(Identity {
        version,
        commit,
        run,
        attempt,
    })
}
fn options(args: &[String], allowed: &[&str]) -> Result<(bool, BTreeMap<String, String>)> {
    let mut out = BTreeMap::new();
    let mut apply = false;
    let mut it = args.iter();
    while let Some(k) = it.next() {
        if k == "--apply" {
            ensure!(!apply, "Duplicate --apply");
            apply = true;
        } else {
            ensure!(allowed.contains(&k.as_str()), "Unknown R2 option");
            ensure!(
                out.insert(
                    k.clone(),
                    it.next().context("Missing option value")?.clone()
                )
                .is_none(),
                "Duplicate R2 option"
            );
        }
    }
    Ok((apply, out))
}
fn native_parts(bytes: &[u8], target: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut binary = None;
    let mut metadata = None;
    let binary_name = format!("remnifold/{}", crate::package::executable(target)?);
    let mut expanded = 0u64;
    let mut count = 0usize;
    let decoded = crate::package::decode_archive(bytes)?;
    for entry in tar::Archive::new(decoded.as_slice()).entries()?.raw(true) {
        let e = entry?;
        count += 1;
        ensure!(count <= 10_000, "Package entry limit");
        expanded = expanded
            .checked_add(e.size())
            .context("Package expansion overflow")?;
        ensure!(
            expanded <= manifest::MAX_EXPANDED,
            "Package expansion limit"
        );
        let name = String::from_utf8(e.path_bytes().into_owned())?;
        if name == binary_name || name == "remnifold/native-build.json" {
            ensure!(
                e.header().entry_type().is_file() && e.size() <= manifest::MAX_ARCHIVE,
                "Invalid native package entry"
            );
            let mut b = Vec::new();
            e.take(manifest::MAX_ARCHIVE + 1).read_to_end(&mut b)?;
            let slot = if name == binary_name {
                &mut binary
            } else {
                &mut metadata
            };
            ensure!(slot.replace(b).is_none(), "Duplicate package entry");
        }
    }
    Ok((
        binary.context("Missing package executable")?,
        metadata.context("Missing native manifest")?,
    ))
}
pub fn stage(args: &[String]) -> Result<()> {
    let (apply, o) = options(
        args,
        &["--target", "--archive", "--binary", "--source-commit"],
    )?;
    let id = identity(false)?;
    let get = |key: &str| o.get(key).context("Missing R2 stage option");
    let target = get("--target")?;
    ensure!(
        manifest::TARGETS.contains(&target.as_str()),
        "Unsupported current stage target"
    );
    ensure!(
        get("--source-commit")? == &id.commit,
        "Stage source mismatch"
    );
    let archive = Path::new(get("--archive")?);
    let binary = Path::new(get("--binary")?);
    crate::package::verify(&repo(), target, binary, archive, &id.commit)?;
    crate::source::verify(&repo(), archive, &id.commit)?;
    let bytes = fs::read(archive)?;
    let (_, native) = native_parts(&bytes, target)?;
    let digest = hash(&bytes);
    let key = format!("ci/{}/{}/{target}/{digest}.tar.gz", id.run, id.attempt);
    let record = StageRecord {
        target: target.clone(),
        version: id.version,
        source_commit: id.commit,
        run_id: id.run,
        run_attempt: id.attempt,
        key,
        size: bytes.len() as u64,
        sha256: digest,
        native_manifest_sha256: hash(&native),
    };
    if !apply {
        println!("R2 stage validated; --apply required to upload");
        return Ok(());
    }
    Store::new("R2_CI")?.put(STAGING, &record.key, &bytes, true)?;
    let output = PathBuf::from(env("GITHUB_OUTPUT")?);
    crate::package::safe_path(&output)?;
    writeln!(
        fs::OpenOptions::new().append(true).open(output)?,
        "{}={}",
        output_key(target)?,
        serde_json::to_string(&record)?
    )?;
    Ok(())
}
struct GitHub {
    http: Client,
    origin: String,
    token: String,
    repository_id: u64,
    public_origin: String,
}
impl GitHub {
    fn new() -> Result<Self> {
        Ok(Self {
            http: client()?,
            origin: "https://api.github.com".into(),
            token: env("GH_TOKEN")?,
            repository_id: env("GITHUB_REPOSITORY_ID")?.parse()?,
            public_origin: manifest::PUBLIC_BASE_URL.into(),
        })
    }
    fn request(&self, method: Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let repository = format!("{}/repos/{}", self.origin, manifest::REPOSITORY);
        let url = if path.is_empty() {
            repository
        } else {
            format!("{repository}/{path}")
        };
        self.http
            .request(method, url)
            .bearer_auth(&self.token)
            .header("User-Agent", "remnifold-native-publisher")
    }
    fn job_log(&self, id: u64) -> Result<Vec<u8>> {
        let response = self
            .request(Method::GET, &format!("actions/jobs/{id}/logs"))
            .send()
            .map_err(|_| anyhow::anyhow!("GitHub log request failed"))?;
        if response.status() == StatusCode::FOUND {
            let location = response
                .headers()
                .get("Location")
                .context("Missing GitHub log location")?
                .to_str()
                .context("Invalid GitHub log location")?;
            let url = url::Url::parse(location)
                .map_err(|_| anyhow::anyhow!("Invalid GitHub log location"))?;
            let host = url.host_str().context("Missing GitHub log host")?;
            ensure!(
                url.scheme() == "https"
                    && url.port_or_known_default() == Some(443)
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
                    && [
                        ".blob.core.windows.net",
                        ".actions.githubusercontent.com",
                        ".githubusercontent.com"
                    ]
                    .iter()
                    .any(|suffix| host.ends_with(suffix)),
                "Untrusted GitHub log location"
            );
            // GitHub の一度限りの storage redirect に token を転送しない。
            let response = self
                .http
                .get(url)
                .send()
                .map_err(|_| anyhow::anyhow!("GitHub log download failed"))?;
            ensure!(
                response.status().is_success(),
                "GitHub log download rejected"
            );
            return body(response, manifest::MAX_WITNESS_LOG as u64);
        }
        ensure!(response.status().is_success(), "GitHub log unavailable");
        body(response, manifest::MAX_WITNESS_LOG as u64)
    }
}
fn github(gh: &GitHub, path: &str) -> Result<serde_json::Value> {
    let r = gh
        .request(Method::GET, path)
        .header("User-Agent", "remnifold-native-publisher")
        .send()
        .map_err(|_| anyhow::anyhow!("GitHub request failed"))?;
    ensure!(r.status().is_success(), "GitHub read rejected");
    Ok(serde_json::from_slice(&body(r, 1024 * 1024)?)?)
}
fn verify_public(gh: &GitHub, key: &str, expected: &[u8]) -> Result<()> {
    let response = gh
        .http
        .get(format!("{}/{key}", gh.public_origin))
        .header("Cache-Control", "no-cache")
        .send()
        .map_err(|_| anyhow::anyhow!("Public R2 verification request failed"))?;
    ensure!(
        response.status().is_success(),
        "Public R2 object unavailable"
    );
    ensure!(
        body(response, expected.len() as u64)? == expected,
        "Public R2 object bytes mismatch"
    );
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunState {
    Accepted,
    Provisional,
    Busy,
}
fn validate_run(http: &GitHub, id: &Identity, completed: bool) -> Result<RunState> {
    let v = github(
        http,
        &format!("actions/runs/{}/attempts/{}", id.run, id.attempt),
    )?;
    ensure!(
        v["id"].as_u64() == Some(id.run)
            && v["run_attempt"].as_u64() == Some(id.attempt)
            && v["head_sha"] == id.commit
            && v["head_branch"] == format!("release/{}", id.version)
            && v["event"] == "push"
            && v["path"] == ".github/workflows/ci.yml"
            && v["repository"]["full_name"] == manifest::REPOSITORY
            && v["head_repository"]["full_name"] == manifest::REPOSITORY
            && v["repository"]["id"].as_u64() == Some(http.repository_id),
        "GitHub workflow identity mismatch"
    );
    let workflow = github(http, "actions/workflows/ci.yml")?;
    ensure!(
        workflow["id"] == v["workflow_id"] && workflow["path"] == ".github/workflows/ci.yml",
        "GitHub workflow mismatch"
    );
    let state = if v["status"] == "completed" {
        if v["conclusion"] == "success" {
            RunState::Accepted
        } else {
            ensure!(
                v["conclusion"] == "failure"
                    || v["conclusion"] == "cancelled"
                    || v["conclusion"] == "timed_out",
                "Unsupported publication conclusion"
            );
            RunState::Provisional
        }
    } else {
        RunState::Busy
    };
    ensure!(
        !completed || state != RunState::Busy,
        "Original publication still running"
    );
    Ok(state)
}
fn run_jobs(http: &GitHub, id: &Identity) -> Result<Vec<serde_json::Value>> {
    let response = github(
        http,
        &format!(
            "actions/runs/{}/attempts/{}/jobs?per_page=100",
            id.run, id.attempt
        ),
    )?;
    let jobs = response["jobs"]
        .as_array()
        .context("Missing workflow jobs")?;
    ensure!(
        jobs.len() < 100 && response["total_count"].as_u64() == Some(jobs.len() as u64),
        "Workflow job discovery limit"
    );
    Ok(jobs.clone())
}
fn checked_job<'a>(
    jobs: &'a [serde_json::Value],
    id: &Identity,
    name: &str,
    success: bool,
) -> Result<&'a serde_json::Value> {
    let matches: Vec<_> = jobs.iter().filter(|job| job["name"] == name).collect();
    ensure!(matches.len() == 1, "Missing or duplicate origin job");
    let job = matches[0];
    ensure!(
        job["run_id"].as_u64() == Some(id.run)
            && job["head_sha"] == id.commit
            && job["status"] == "completed"
            && (!success || job["conclusion"] == "success")
            && job["id"].as_u64().is_some_and(|n| n > 0),
        "Origin job identity or conclusion mismatch"
    );
    Ok(job)
}
fn validate_stage_origins(http: &GitHub, id: &Identity, records: &[StageRecord]) -> Result<()> {
    ensure!(
        records.len() == manifest::TARGETS.len(),
        "Incomplete stage origins"
    );
    let mut origins = BTreeMap::new();
    for (position, r) in records.iter().enumerate() {
        ensure!(
            r.target == manifest::TARGETS[position]
                && r.run_id == id.run
                && r.run_attempt > 0
                && r.run_attempt <= id.attempt
                && r.source_commit == id.commit
                && r.version == id.version
                && r.size > 0
                && r.size <= manifest::MAX_ARCHIVE
                && manifest::hex(&r.sha256, 64)
                && manifest::hex(&r.native_manifest_sha256, 64)
                && r.key
                    == format!(
                        "ci/{}/{}/{}/{}.tar.gz",
                        r.run_id, r.run_attempt, r.target, r.sha256
                    ),
            "Stage origin mismatch"
        );
        if !origins.contains_key(&r.run_attempt) {
            let origin = Identity {
                version: id.version.clone(),
                commit: id.commit.clone(),
                run: id.run,
                attempt: r.run_attempt,
            };
            validate_run(http, &origin, false)?;
            origins.insert(r.run_attempt, run_jobs(http, &origin)?);
        }
        let origin = Identity {
            version: id.version.clone(),
            commit: id.commit.clone(),
            run: id.run,
            attempt: r.run_attempt,
        };
        checked_job(
            &origins[&r.run_attempt],
            &origin,
            &format!("native-package ({})", r.target),
            true,
        )?;
    }
    Ok(())
}
fn publication_log(http: &GitHub, id: &Identity, success: bool) -> Result<Vec<u8>> {
    let jobs = run_jobs(http, id)?;
    let job = checked_job(&jobs, id, "publish-r2", success)?;
    http.job_log(
        job["id"]
            .as_u64()
            .context("Missing publisher job identity")?,
    )
}
/// 公開済み Windows archive を GitHub 生ログと commit の両方で検証する。
/// 全体が成功した release CI と publisher job の証拠だけを認める。
pub(crate) fn bootstrap_binary(
    version: &str,
    commit: &str,
    root: &Path,
    token: &str,
) -> Result<Vec<u8>> {
    let mut http = GitHub {
        http: client()?,
        origin: "https://api.github.com".into(),
        token: token.into(),
        repository_id: 0,
        public_origin: manifest::PUBLIC_BASE_URL.into(),
    };
    let response = http
        .request(Method::GET, "")
        .send()
        .map_err(|_| anyhow::anyhow!("GitHub repository request failed"))?;
    ensure!(
        response.status().is_success(),
        "GitHub repository unavailable"
    );
    let repository: serde_json::Value = manifest::parse_strict(&body(response, 1024 * 1024)?)?;
    ensure!(
        repository["full_name"] == manifest::REPOSITORY,
        "GitHub repository mismatch"
    );
    http.repository_id = repository["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Missing GitHub repository ID")?;
    bootstrap_binary_verified(&http, version, commit, root)
}
fn bootstrap_binary_verified(
    http: &GitHub,
    version: &str,
    commit: &str,
    root: &Path,
) -> Result<Vec<u8>> {
    let public = |key: &str, max| -> Result<Vec<u8>> {
        let response = http
            .http
            .get(format!("{}/{key}", http.public_origin))
            .send()
            .map_err(|_| anyhow::anyhow!("Public bootstrap archive request failed"))?;
        ensure!(
            response.status().is_success(),
            "Public bootstrap archive unavailable"
        );
        body(response, max)
    };
    let index = manifest::parse_index(&public(
        &manifest::index_key(version),
        manifest::MAX_JSON as u64,
    )?)?;
    ensure!(
        index.version == version && index.source_commit == commit,
        "Bootstrap publication identity mismatch"
    );
    verify_existing_anchor(http, &index)?;
    let identity = Identity {
        version: version.into(),
        commit: commit.into(),
        run: index.run_id,
        attempt: index.run_attempt,
    };
    ensure!(
        validate_run(http, &identity, true)? == RunState::Accepted,
        "Bootstrap publication CI not successful"
    );
    let bytes = public(&index.manifest_key, manifest::MAX_JSON as u64)?;
    ensure!(
        hash(&bytes) == index.manifest_sha256,
        "Bootstrap manifest digest mismatch"
    );
    let release = manifest::parse_manifest(&bytes)?;
    ensure!(
        release.schema == index.schema
            && release.source_commit == commit
            && release.version == version
            && release.run_id == index.run_id
            && release.run_attempt == index.run_attempt,
        "Bootstrap manifest identity mismatch"
    );
    let workflow = github(http, "actions/workflows/ci.yml")?;
    let runs = github(
        http,
        &format!("actions/runs?head_sha={commit}&event=push&per_page=100"),
    )?;
    let runs = runs["workflow_runs"]
        .as_array()
        .context("Missing bootstrap workflow runs")?;
    ensure!(runs.len() < 100, "Bootstrap CI discovery limit");
    let latest = runs
        .iter()
        .filter(|run| {
            run["head_sha"] == commit
                && run["head_branch"] == format!("release/{version}")
                && run["event"] == "push"
                && run["workflow_id"] == workflow["id"]
                && run["path"] == ".github/workflows/ci.yml"
                && run["repository"]["id"].as_u64() == Some(http.repository_id)
                && run["repository"]["full_name"] == manifest::REPOSITORY
                && run["head_repository"]["full_name"] == manifest::REPOSITORY
        })
        .max_by_key(|run| {
            (
                run["id"].as_u64().unwrap_or(0),
                run["run_attempt"].as_u64().unwrap_or(0),
            )
        })
        .context("Latest release CI unavailable")?;
    ensure!(
        latest["status"] == "completed" && latest["conclusion"] == "success",
        "Latest release CI not successful"
    );
    let latest_identity = Identity {
        version: version.into(),
        commit: commit.into(),
        run: latest["id"]
            .as_u64()
            .filter(|v| *v > 0)
            .context("Missing latest CI ID")?,
        attempt: latest["run_attempt"]
            .as_u64()
            .filter(|v| *v > 0)
            .context("Missing latest CI attempt")?,
    };
    ensure!(
        validate_run(http, &latest_identity, true)? == RunState::Accepted,
        "Latest release CI not accepted"
    );
    manifest::verify_witness(
        &publication_log(http, &latest_identity, true)?,
        &release,
        &index.manifest_sha256,
    )?;
    let target = "x86_64-pc-windows-msvc";
    let asset = &release.targets[target];
    let archive = public(&asset.key, asset.size)?;
    ensure!(
        archive.len() as u64 == asset.size && hash(&archive) == asset.sha256,
        "Bootstrap archive digest mismatch"
    );
    LocalVerifier {
        repo: repo(),
        root: root.into(),
    }
    .verify(target, &archive, &asset.native_manifest_sha256, commit)?;
    let (binary, native) = native_parts(&archive, target)?;
    let native: serde_json::Value = manifest::parse_strict(&native)?;
    ensure!(
        native["capabilities"]
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v == "bootstrap-v1")),
        "Published native executable does not support bootstrap-v1; publish a capable release first"
    );
    Ok(binary)
}
fn verify_existing_anchor(http: &GitHub, index: &VersionIndex) -> Result<()> {
    let wanted = manifest::status_url(index.run_id, index.run_attempt);
    for page in 1..=10 {
        let list = github(
            http,
            &format!(
                "commits/{}/statuses?per_page=100&page={page}",
                index.source_commit
            ),
        )?;
        let list = list.as_array().context("Invalid GitHub status list")?;
        for status in list {
            if status["context"] == manifest::STATUS_CONTEXT && status["target_url"] == wanted {
                ensure!(
                    status["state"] == "success"
                        && status["description"] == format!("sha256:{}", index.manifest_sha256)
                        && status["creator"]["login"] == "github-actions[bot]"
                        && status["creator"]["id"] == 41898282u64
                        && status["creator"]["type"] == "Bot",
                    "Existing publication anchor mismatch"
                );
                return Ok(());
            }
        }
        if list.len() < 100 {
            break;
        }
    }
    bail!("Existing publication anchor missing")
}
fn emit_witness(release: &ReleaseManifest, digest: &str) -> Result<()> {
    for line in manifest::witness_lines(release, digest)? {
        println!("{line}");
    }
    Ok(())
}
pub fn publish(args: &[String]) -> Result<()> {
    let (apply, _) = options(args, &[])?;
    let id = identity(true)?;
    ensure!(
        env("REMNIFOLD_R2_NEEDS_RESULT")? == "success",
        "Matrix did not succeed"
    );
    let input = env("REMNIFOLD_R2_STAGE_RECORDS")?;
    ensure!(input.len() <= manifest::MAX_JSON, "Oversized stage records");
    let records: BTreeMap<String, String> = manifest::parse_strict(input.as_bytes())?;
    ensure!(
        records.len() == manifest::TARGETS.len(),
        "Incomplete matrix records"
    );
    let mut targets = BTreeMap::new();
    let mut staged = Vec::new();
    for target in manifest::TARGETS {
        let r: StageRecord = manifest::parse_strict(
            records
                .get(output_key(target)?)
                .context("Missing matrix target")?
                .as_bytes(),
        )?;
        ensure!(
            r.target == target
                && r.version == id.version
                && r.source_commit == id.commit
                && r.run_id == id.run
                && r.run_attempt > 0
                && r.run_attempt <= id.attempt
                && r.size > 0
                && r.size <= manifest::MAX_ARCHIVE
                && manifest::hex(&r.sha256, 64)
                && manifest::hex(&r.native_manifest_sha256, 64)
                && r.key
                    == format!(
                        "ci/{}/{}/{target}/{}.tar.gz",
                        r.run_id, r.run_attempt, r.sha256
                    ),
            "Stage record mismatch"
        );
        targets.insert(
            target.into(),
            ReleaseAsset {
                key: manifest::asset_key(
                    &id.version,
                    &id.commit,
                    id.run,
                    id.attempt,
                    target,
                    &r.sha256,
                ),
                size: r.size,
                sha256: r.sha256.clone(),
                native_manifest_sha256: r.native_manifest_sha256.clone(),
            },
        );
        staged.push(r);
    }
    let release = ReleaseManifest {
        schema: manifest::CURRENT_RELEASE_SCHEMA,
        repository: manifest::REPOSITORY.into(),
        version: id.version.clone(),
        source_commit: id.commit.clone(),
        run_id: id.run,
        run_attempt: id.attempt,
        targets,
    };
    release.validate()?;
    if !apply {
        println!("R2 publication plan validated; --apply required to publish");
        return Ok(());
    }
    let http = GitHub::new()?;
    validate_run(&http, &id, false)?;
    validate_stage_origins(&http, &id, &staged)?;
    let store = Store::new("R2_RELEASE")?;
    let root = PathBuf::from(env("REMNIFOLD_R2_WORK_ROOT")?);
    crate::package::safe_path(&root)?;
    ensure!(
        !root.starts_with(repo()),
        "Publication work root must be external"
    );
    fs::create_dir_all(&root)?;
    let invocation = root.join(format!("publisher-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&invocation)?;
    let mut archives = Vec::new();
    if store
        .get_index(&manifest::index_key(&id.version))?
        .is_none()
    {
        for r in &staged {
            let bytes = store
                .get(STAGING, &r.key, r.size)?
                .context("Missing staged archive")?;
            ensure!(
                bytes.len() as u64 == r.size && hash(&bytes) == r.sha256,
                "Staging archive digest mismatch"
            );
            archives.push(bytes);
        }
    }
    let verifier = LocalVerifier {
        repo: repo(),
        root: invocation,
    };
    let proof = publish_verified(&store, &http, &id, &release, &staged, &archives, &verifier)?;
    emit_witness(&proof.release, &proof.digest)
}
struct PublicationProof {
    release: ReleaseManifest,
    digest: String,
}
trait ArchiveVerifier {
    fn verify(&self, target: &str, bytes: &[u8], native_hash: &str, commit: &str) -> Result<()>;
}
struct LocalVerifier {
    repo: PathBuf,
    root: PathBuf,
}
impl ArchiveVerifier for LocalVerifier {
    fn verify(&self, target: &str, bytes: &[u8], native_hash: &str, commit: &str) -> Result<()> {
        ensure!(
            bytes.len() as u64 <= manifest::MAX_ARCHIVE,
            "Oversized verified archive"
        );
        let (binary, native) = native_parts(bytes, target)?;
        ensure!(
            hash(&native) == native_hash,
            "Native manifest digest mismatch"
        );
        let name = format!("{}-{target}", uuid::Uuid::new_v4());
        let archive = self.root.join(format!("{name}.tar.gz"));
        let executable = self.root.join(format!("{name}-binary"));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)?
            .write_all(bytes)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)?
            .write_all(&binary)?;
        crate::package::verify(&self.repo, target, &executable, &archive, commit)?;
        crate::source::verify(&self.repo, &archive, commit)
    }
}
fn publish_verified(
    store: &Store,
    http: &GitHub,
    id: &Identity,
    release: &ReleaseManifest,
    staged: &[StageRecord],
    archives: &[Vec<u8>],
    verifier: &dyn ArchiveVerifier,
) -> Result<PublicationProof> {
    release.validate()?;
    ensure!(
        release.schema == manifest::CURRENT_RELEASE_SCHEMA,
        "New publication requires current release schema"
    );
    ensure!(
        staged.len() == manifest::TARGETS.len(),
        "Incomplete verified publication"
    );
    for (position, r) in staged.iter().enumerate() {
        ensure!(
            r.target == manifest::TARGETS[position],
            "Unexpected stage target order"
        );
    }
    let mut selected = release.clone();
    let mut selected_archives = std::borrow::Cow::Borrowed(archives);
    let mut selected_verified = false;
    let index_key = manifest::index_key(&id.version);
    let mut provisional_etag = None;
    // 成功 CI が受理した index は不変。失敗済みの未受理 index だけを CAS で回復する。
    if let Some((bytes, etag)) = store.get_index(&index_key)? {
        let index = manifest::parse_index(&bytes)?;
        ensure!(
            index.source_commit == id.commit && index.version == id.version,
            "Version already published from another commit"
        );
        verify_existing_anchor(&http, &index)?;
        let old_identity = Identity {
            version: index.version.clone(),
            commit: index.source_commit.clone(),
            run: index.run_id,
            attempt: index.run_attempt,
        };
        let state = validate_run(http, &old_identity, true)?;
        let old_bytes = store
            .get(PUBLIC, &index.manifest_key, manifest::MAX_JSON as u64)?
            .context("Missing published manifest")?;
        ensure!(
            hash(&old_bytes) == index.manifest_sha256,
            "Published manifest digest mismatch"
        );
        let old = manifest::parse_manifest(&old_bytes)?;
        ensure!(
            old.schema == index.schema
                && old.source_commit == id.commit
                && old.version == id.version
                && old.run_id == index.run_id
                && old.run_attempt == index.run_attempt,
            "Published identity mismatch"
        );
        let log = publication_log(http, &old_identity, state == RunState::Accepted)?;
        if state == RunState::Accepted {
            manifest::verify_witness(&log, &old, &index.manifest_sha256)?;
        } else {
            manifest::verify_candidate(&log, &old, &index.manifest_sha256)?;
        }
        let mut original_archives = Vec::new();
        for target in manifest::targets_for_schema(old.schema)? {
            let a = &old.targets[*target];
            let actual = store
                .get(PUBLIC, &a.key, a.size)?
                .context("Missing published archive")?;
            ensure!(
                actual.len() as u64 == a.size && hash(&actual) == a.sha256,
                "Published archive changed"
            );
            verifier.verify(target, &actual, &a.native_manifest_sha256, &id.commit)?;
            verify_public(http, &a.key, &actual)?;
            original_archives.push(actual);
        }
        if state == RunState::Accepted {
            verify_public(http, &index.manifest_key, &old_bytes)?;
            verify_public(http, &index_key, &bytes)?;
            return Ok(PublicationProof {
                release: old,
                digest: index.manifest_sha256,
            });
        }
        ensure!(
            index.run_id != id.run || index.run_attempt != id.attempt,
            "Failed publication requires a new CI attempt"
        );
        provisional_etag = Some(etag);
        selected = old;
        selected.run_id = id.run;
        selected.run_attempt = id.attempt;
        for target in manifest::targets_for_schema(selected.schema)? {
            let asset = selected.targets.get_mut(*target).unwrap();
            asset.key = manifest::asset_key(
                &id.version,
                &id.commit,
                id.run,
                id.attempt,
                target,
                &asset.sha256,
            );
        }
        selected.validate()?;
        selected_archives = std::borrow::Cow::Owned(original_archives);
        selected_verified = true;
    }
    ensure!(
        selected_archives.len() == manifest::targets_for_schema(selected.schema)?.len(),
        "Incomplete verified publication archives"
    );
    for (target, bytes) in manifest::targets_for_schema(selected.schema)?
        .iter()
        .zip(selected_archives.iter())
    {
        let asset = &selected.targets[*target];
        ensure!(
            asset.size == bytes.len() as u64 && asset.sha256 == hash(bytes),
            "Verified archive changed before publication"
        );
        if !selected_verified {
            verifier.verify(target, bytes, &asset.native_manifest_sha256, &id.commit)?;
        }
    }
    for (target, bytes) in manifest::targets_for_schema(selected.schema)?
        .iter()
        .zip(selected_archives.iter())
    {
        store.put(PUBLIC, &selected.targets[*target].key, bytes, true)?;
        verify_public(http, &selected.targets[*target].key, bytes)?;
    }
    let bytes = serde_json::to_vec(&selected)?;
    let manifest_key = manifest::manifest_key(&id.version, &id.commit, id.run, id.attempt);
    let digest = hash(&bytes);
    store.put(PUBLIC, &manifest_key, &bytes, true)?;
    verify_public(&http, &manifest_key, &bytes)?;
    println!("{}", manifest::candidate_line(&selected, &digest)?);
    let response=http.request(Method::POST,&format!("statuses/{}",id.commit)).json(&serde_json::json!({"state":"success","context":manifest::STATUS_CONTEXT,"description":format!("sha256:{digest}"),"target_url":manifest::status_url(id.run,id.attempt)})).send().map_err(|_|anyhow::anyhow!("GitHub status request failed"))?;
    ensure!(response.status().is_success(), "GitHub status rejected");
    let index = VersionIndex {
        schema: selected.schema,
        repository: manifest::REPOSITORY.into(),
        version: id.version.clone(),
        source_commit: id.commit.clone(),
        run_id: id.run,
        run_attempt: id.attempt,
        manifest_key,
        manifest_sha256: digest,
    };
    let index_bytes = serde_json::to_vec(&index)?;
    if let Some(etag) = provisional_etag {
        store.replace_provisional_index(&index_key, &index_bytes, &etag)?;
    } else {
        store.put(PUBLIC, &index_key, &index_bytes, false)?;
    }
    verify_public(http, &index_key, &index_bytes)?;
    Ok(PublicationProof {
        release: selected,
        digest: index.manifest_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn github_repository_endpoint_has_no_trailing_slash() {
        let github = GitHub {
            http: client().unwrap(),
            origin: "https://api.github.com".into(),
            token: "fixture-token".into(),
            repository_id: 1,
            public_origin: manifest::PUBLIC_BASE_URL.into(),
        };
        for path in ["", "actions/runs/1"] {
            let request = github.request(Method::GET, path).build().unwrap();
            let expected = format!(
                "https://api.github.com/repos/{}{}",
                manifest::REPOSITORY,
                if path.is_empty() {
                    String::new()
                } else {
                    format!("/{path}")
                }
            );
            assert_eq!(request.url().as_str(), expected);
        }
    }
    struct FixtureVerifier;
    impl ArchiveVerifier for FixtureVerifier {
        fn verify(
            &self,
            target: &str,
            bytes: &[u8],
            native_hash: &str,
            commit: &str,
        ) -> Result<()> {
            ensure!(
                commit == "a".repeat(40)
                    && native_hash == "c".repeat(64)
                    && std::str::from_utf8(bytes)?.ends_with(target),
                "Fixture package/source mismatch"
            );
            Ok(())
        }
    }
    fn publish_objects(
        store: &Store,
        http: &GitHub,
        id: &Identity,
        release: &ReleaseManifest,
        staged: &[StageRecord],
        archives: &[Vec<u8>],
    ) -> Result<PublicationProof> {
        validate_stage_origins(http, id, staged)?;
        publish_verified(store, http, id, release, staged, archives, &FixtureVerifier)
    }
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Default)]
    struct State {
        files: BTreeMap<String, Vec<u8>>,
        statuses: Vec<serde_json::Value>,
        writes: Vec<String>,
        fail_status: bool,
        fail_index: bool,
        cas: bool,
        fail_matrix: bool,
        foreign_run: bool,
        failed_attempt: Option<(u64, &'static str)>,
        fail_public_index: bool,
        cas_conflict: bool,
        logs: BTreeMap<u64, Vec<u8>>,
        origin_targets: BTreeMap<u64, Vec<String>>,
    }
    struct PublicationFixture {
        store: Store,
        github: GitHub,
        state: Arc<Mutex<State>>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for PublicationFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(&self.store.host);
            self.worker.take().unwrap().join().unwrap();
        }
    }
    fn publication_fixture() -> PublicationFixture {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let origin = format!("http://{host}");
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut received = Vec::new();
                let mut one = [0];
                while !received.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut one).unwrap();
                    received.push(one[0]);
                    assert!(received.len() < 16384)
                }
                let header = String::from_utf8(received).unwrap();
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut payload = vec![0; length];
                stream.read_exact(&mut payload).unwrap();
                let mut first = header.lines().next().unwrap().split_whitespace();
                let method = first.next().unwrap();
                let path = first.next().unwrap();
                let mut s = shared.lock().unwrap();
                let mut status = 200u16;
                let response: Vec<u8> = if let Some(key) = path
                    .strip_prefix("/remnifold-updates/")
                    .or_else(|| path.strip_prefix("/remnifold-builds/"))
                {
                    if method == "PUT" {
                        s.writes.push(format!("PUT {key}"));
                        let match_etag = header
                            .lines()
                            .find_map(|line| line.strip_prefix("if-match: "));
                        assert!(
                            match_etag.is_some()
                                || header.to_ascii_lowercase().contains("if-none-match: *\r\n")
                        );
                        if key.ends_with("/index.json") && s.fail_index {
                            status = 500;
                        } else if let Some(etag) = match_etag {
                            if s.cas_conflict {
                                status = 412;
                            } else if s
                                .files
                                .get(key)
                                .is_some_and(|v| format!("\"{}\"", hash(v)) == etag)
                            {
                                s.files.insert(key.into(), payload);
                            } else {
                                status = 412;
                            }
                        } else if s.files.contains_key(key) {
                            status = 412;
                        } else if key.ends_with("/index.json") && s.cas {
                            s.files.insert(key.into(), payload.clone());
                            status = 412;
                        } else {
                            s.files.insert(key.into(), payload);
                        }
                        Vec::new()
                    } else {
                        match s.files.get(key) {
                            Some(v) => v.clone(),
                            None => {
                                status = 404;
                                Vec::new()
                            }
                        }
                    }
                } else if let Some(key) = path.strip_prefix("/public/") {
                    if s.fail_public_index && key.ends_with("/index.json") {
                        status = 503;
                        Vec::new()
                    } else {
                        match s.files.get(key).cloned() {
                            Some(v) => {
                                if key.ends_with("/index.json") {
                                    let index = manifest::parse_index(&v).unwrap();
                                    let m = manifest::parse_manifest(&s.files[&index.manifest_key])
                                        .unwrap();
                                    let log = s.logs.entry(9000 + index.run_attempt).or_default();
                                    if !log
                                        .windows(manifest::MANIFEST_MARKER.len())
                                        .any(|w| w == manifest::MANIFEST_MARKER.as_bytes())
                                    {
                                        log.extend_from_slice(
                                            manifest::witness_lines(&m, &index.manifest_sha256)
                                                .unwrap()
                                                .join("\n")
                                                .as_bytes(),
                                        );
                                        log.push(b'\n');
                                    }
                                }
                                v
                            }
                            None => {
                                status = 404;
                                Vec::new()
                            }
                        }
                    }
                } else if path.contains("/statuses/") && method == "POST" {
                    s.writes.push("STATUS".into());
                    if s.fail_status {
                        status = 500;
                        Vec::new()
                    } else {
                        let mut v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                        let digest = v["description"]
                            .as_str()
                            .unwrap()
                            .strip_prefix("sha256:")
                            .unwrap();
                        let m = s
                            .files
                            .iter()
                            .find(|(key, bytes)| {
                                key.ends_with("manifest.json") && hash(bytes) == digest
                            })
                            .map(|(_, bytes)| manifest::parse_manifest(bytes).unwrap())
                            .unwrap();
                        let log = s.logs.entry(9000 + m.run_attempt).or_default();
                        if !log
                            .windows(manifest::CANDIDATE_MARKER.len())
                            .any(|w| w == manifest::CANDIDATE_MARKER.as_bytes())
                        {
                            log.extend_from_slice(
                                manifest::candidate_line(&m, digest).unwrap().as_bytes(),
                            );
                            log.push(b'\n');
                        }
                        v["creator"] = serde_json::json!({"login":"github-actions[bot]","id":41898282u64,"type":"Bot"});
                        s.statuses.insert(0, v);
                        status = 201;
                        b"{}".to_vec()
                    }
                } else if path.contains("/commits/") && path.contains("/statuses?") {
                    serde_json::to_vec(&s.statuses).unwrap()
                } else if path.contains("/actions/jobs/") && path.ends_with("/logs") {
                    let id = path
                        .split("/actions/jobs/")
                        .nth(1)
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap();
                    match s.logs.get(&id) {
                        Some(log) => log.clone(),
                        None => {
                            status = 404;
                            Vec::new()
                        }
                    }
                } else if path.contains("/actions/workflows/") {
                    serde_json::to_vec(
                        &serde_json::json!({"id":99,"path":".github/workflows/ci.yml"}),
                    )
                    .unwrap()
                } else if path.contains("/jobs?") {
                    let attempt = path
                        .split("/attempts/")
                        .nth(1)
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap();
                    let mut jobs:Vec<_>=manifest::TARGETS.iter().enumerate().filter(|(_,t)|s.origin_targets.get(&attempt).is_none_or(|targets|targets.iter().any(|v|v==**t))).map(|(n,t)|serde_json::json!({"id":1000+100*attempt+n as u64,"run_id":42,"head_sha":"a".repeat(40),"name":format!("native-package ({t})"),"status":"completed","conclusion":if s.fail_matrix {"failure"}else{"success"}})).collect();
                    jobs.push(serde_json::json!({"id":9000+attempt,"run_id":42,"head_sha":"a".repeat(40),"name":"publish-r2","status":"completed","conclusion":s.failed_attempt.filter(|(a,_)|*a==attempt).map(|(_,v)|v).unwrap_or("success")}));
                    serde_json::to_vec(&serde_json::json!({"total_count":jobs.len(),"jobs":jobs}))
                        .unwrap()
                } else if path.contains("/actions/runs/") {
                    let attempt = path
                        .split("/attempts/")
                        .nth(1)
                        .unwrap()
                        .parse::<u64>()
                        .unwrap();
                    let conclusion = s
                        .failed_attempt
                        .filter(|(a, _)| *a == attempt)
                        .map(|(_, v)| v)
                        .unwrap_or("success");
                    serde_json::to_vec(&serde_json::json!({"id":42,"run_attempt":attempt,"head_sha":"a".repeat(40),"head_branch":"release/1.0.15","event":if s.foreign_run{"pull_request"}else{"push"},"path":".github/workflows/ci.yml","repository":{"full_name":manifest::REPOSITORY,"id":123},"head_repository":{"full_name":manifest::REPOSITORY},"workflow_id":99,"status":if conclusion=="in_progress"{"in_progress"}else{"completed"},"conclusion":conclusion})).unwrap()
                } else {
                    status = 404;
                    Vec::new()
                };
                drop(s);
                write!(
                    stream,
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nETag: \"{}\"\r\nConnection: close\r\n\r\n",
                    response.len(),hash(&response)
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        PublicationFixture {
            store: Store {
                http: client().unwrap(),
                host,
                origin: origin.clone(),
                key: "fixture".into(),
                secret: "fixture".into(),
            },
            github: GitHub {
                http: client().unwrap(),
                origin: origin.clone(),
                token: "fixture".into(),
                repository_id: 123,
                public_origin: format!("{origin}/public"),
            },
            state,
            stop,
            worker: Some(worker),
        }
    }
    fn publication() -> (Identity, ReleaseManifest, Vec<StageRecord>, Vec<Vec<u8>>) {
        publication_attempt(1)
    }
    fn publication_attempt(
        attempt: u64,
    ) -> (Identity, ReleaseManifest, Vec<StageRecord>, Vec<Vec<u8>>) {
        let id = Identity {
            version: "1.0.15".into(),
            commit: "a".repeat(40),
            run: 42,
            attempt,
        };
        let mut release = ReleaseManifest {
            schema: manifest::CURRENT_RELEASE_SCHEMA,
            repository: manifest::REPOSITORY.into(),
            version: id.version.clone(),
            source_commit: id.commit.clone(),
            run_id: id.run,
            run_attempt: id.attempt,
            targets: BTreeMap::new(),
        };
        let mut staged = Vec::new();
        let mut archives = Vec::new();
        for target in manifest::TARGETS {
            let bytes = format!("verified fixture {target}").into_bytes();
            let digest = hash(&bytes);
            let asset = ReleaseAsset {
                key: manifest::asset_key(
                    &id.version,
                    &id.commit,
                    id.run,
                    id.attempt,
                    target,
                    &digest,
                ),
                size: bytes.len() as u64,
                sha256: digest.clone(),
                native_manifest_sha256: "c".repeat(64),
            };
            release.targets.insert(target.into(), asset.clone());
            staged.push(StageRecord {
                target: target.into(),
                version: id.version.clone(),
                source_commit: id.commit.clone(),
                run_id: id.run,
                run_attempt: id.attempt,
                key: format!("ci/{}/{}/{target}/{digest}.tar.gz", id.run, id.attempt),
                size: asset.size,
                sha256: digest,
                native_manifest_sha256: asset.native_manifest_sha256,
            });
            archives.push(bytes);
        }
        (id, release, staged, archives)
    }
    fn rebuilt_publication(
        attempt: u64,
    ) -> (Identity, ReleaseManifest, Vec<StageRecord>, Vec<Vec<u8>>) {
        let (id, mut release, mut staged, mut archives) = publication_attempt(attempt);
        for (position, target) in manifest::TARGETS.iter().enumerate() {
            archives[position] = format!("rebuilt fixture {target}").into_bytes();
            let digest = hash(&archives[position]);
            let asset = release.targets.get_mut(*target).unwrap();
            asset.sha256 = digest.clone();
            asset.size = archives[position].len() as u64;
            asset.key =
                manifest::asset_key(&id.version, &id.commit, id.run, id.attempt, target, &digest);
            let r = &mut staged[position];
            r.sha256 = digest.clone();
            r.size = asset.size;
            r.key = format!("ci/{}/{}/{target}/{digest}.tar.gz", id.run, id.attempt);
        }
        (id, release, staged, archives)
    }
    #[test]
    fn publication_real_http_index_last_and_retry_reuses_anchor() {
        let f = publication_fixture();
        let (id, release, staged, archives) = publication();
        validate_run(&f.github, &id, false).unwrap();
        let first =
            publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).unwrap();
        let first_lines = manifest::witness_lines(&first.release, &first.digest).unwrap();
        assert_eq!(first_lines.len(), manifest::TARGETS.len() + 1);
        manifest::verify_witness(first_lines.join("\n").as_bytes(), &release, &first.digest)
            .unwrap();
        let writes = f.state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), manifest::TARGETS.len() + 3);
        assert_eq!(writes[manifest::TARGETS.len() + 1], "STATUS");
        assert_eq!(
            writes[manifest::TARGETS.len() + 2],
            "PUT releases/1.0.15/index.json"
        );
        let (next, next_release, next_staged, next_archives) = rebuilt_publication(2);
        let reused = publish_objects(
            &f.store,
            &f.github,
            &next,
            &next_release,
            &next_staged,
            &next_archives,
        )
        .unwrap();
        assert_eq!(
            manifest::witness_lines(&reused.release, &reused.digest).unwrap(),
            first_lines
        );
        assert_eq!(reused.release.run_attempt, 1);
        assert_eq!(f.state.lock().unwrap().writes, writes);
        let mut incomplete = release.clone();
        incomplete.targets.remove(manifest::TARGETS[0]);
        assert!(
            publish_objects(&f.store, &f.github, &id, &incomplete, &staged, &archives).is_err()
        );
        assert_eq!(f.state.lock().unwrap().writes, writes);
        let mut changed = archives.clone();
        changed[0].push(0);
        let fresh = publication_fixture();
        assert!(
            publish_objects(
                &fresh.store,
                &fresh.github,
                &id,
                &release,
                &staged,
                &changed
            )
            .is_err()
        );
        assert!(fresh.state.lock().unwrap().writes.is_empty());
        assert_eq!(f.state.lock().unwrap().writes, writes);
    }
    #[test]
    fn publication_real_http_anchor_and_index_failure_are_retryable() {
        for fail_anchor in [true, false] {
            let f = publication_fixture();
            {
                let mut s = f.state.lock().unwrap();
                s.fail_status = fail_anchor;
                s.fail_index = !fail_anchor;
            }
            let (id, release, staged, archives) = publication();
            assert!(
                publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).is_err()
            );
            assert!(
                !f.state
                    .lock()
                    .unwrap()
                    .files
                    .contains_key("releases/1.0.15/index.json")
            );
            {
                let mut s = f.state.lock().unwrap();
                s.fail_status = false;
                s.fail_index = false;
            }
            publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).unwrap();
            assert!(
                f.state
                    .lock()
                    .unwrap()
                    .files
                    .contains_key("releases/1.0.15/index.json")
            );
        }
    }
    #[test]
    fn publication_real_http_cas_and_immutable_conflicts() {
        let f = publication_fixture();
        f.state.lock().unwrap().cas = true;
        let (id, release, staged, archives) = publication();
        publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).unwrap();
        let target = manifest::TARGETS[0];
        f.state
            .lock()
            .unwrap()
            .files
            .insert(release.targets[target].key.clone(), b"tampered".to_vec());
        assert!(publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).is_err());
        let f = publication_fixture();
        f.state
            .lock()
            .unwrap()
            .files
            .insert(release.targets[target].key.clone(), b"conflict".to_vec());
        assert!(publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).is_err());
        assert!(f.state.lock().unwrap().statuses.is_empty());
        assert!(
            !f.state
                .lock()
                .unwrap()
                .files
                .contains_key("releases/1.0.15/index.json")
        );
    }
    #[test]
    fn publication_recovers_only_failed_provisional_index_with_etag_cas() {
        for conclusion in ["failure", "cancelled", "timed_out"] {
            let f = publication_fixture();
            f.state.lock().unwrap().fail_public_index = true;
            let (id, release, staged, archives) = publication();
            assert!(
                publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).is_err()
            );
            let original = f.state.lock().unwrap().files["releases/1.0.15/index.json"].clone();
            {
                let mut s = f.state.lock().unwrap();
                s.fail_public_index = false;
                s.failed_attempt = Some((1, conclusion));
                s.cas_conflict = true;
            }
            let (next, release, staged, _) = rebuilt_publication(2);
            let archives = Vec::new(); // private staging の期限切れでも元 public bytes を選ぶ。
            assert!(
                publish_objects(&f.store, &f.github, &next, &release, &staged, &archives).is_err()
            );
            assert_eq!(
                f.state.lock().unwrap().files["releases/1.0.15/index.json"],
                original
            );
            f.state.lock().unwrap().cas_conflict = false;
            publish_objects(&f.store, &f.github, &next, &release, &staged, &archives).unwrap();
            let index =
                manifest::parse_index(&f.state.lock().unwrap().files["releases/1.0.15/index.json"])
                    .unwrap();
            assert_eq!(index.run_attempt, 2);
            let writes = f.state.lock().unwrap().writes.clone();
            let (last, release, staged, archives) = publication_attempt(3);
            publish_objects(&f.store, &f.github, &last, &release, &staged, &archives).unwrap();
            assert_eq!(f.state.lock().unwrap().writes, writes);
        }
    }
    #[test]
    fn publication_cannot_take_over_running_attempt_or_untrusted_anchor() {
        let f = publication_fixture();
        let (id, release, staged, archives) = publication();
        publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).unwrap();
        let writes = f.state.lock().unwrap().writes.clone();
        f.state.lock().unwrap().failed_attempt = Some((1, "in_progress"));
        let (next, release, staged, archives) = publication_attempt(2);
        assert!(publish_objects(&f.store, &f.github, &next, &release, &staged, &archives).is_err());
        assert_eq!(f.state.lock().unwrap().writes, writes);
        {
            let mut s = f.state.lock().unwrap();
            s.failed_attempt = Some((1, "failure"));
            s.statuses[0]["creator"]["id"] = serde_json::json!(99);
        }
        assert!(publish_objects(&f.store, &f.github, &next, &release, &staged, &archives).is_err());
        assert_eq!(f.state.lock().unwrap().writes, writes);
    }
    #[test]
    fn provisional_recovery_rejects_missing_fake_or_duplicate_candidate_logs() {
        for mode in 0..3 {
            let f = publication_fixture();
            f.state.lock().unwrap().fail_public_index = true;
            let (id, release, staged, archives) = publication();
            assert!(
                publish_objects(&f.store, &f.github, &id, &release, &staged, &archives).is_err()
            );
            {
                let mut s = f.state.lock().unwrap();
                s.fail_public_index = false;
                s.failed_attempt = Some((1, "failure"));
                let log = s.logs.get_mut(&9001).unwrap();
                match mode {
                    0 => log.clear(),
                    1 => {
                        let duplicate = log.clone();
                        log.extend(duplicate)
                    }
                    _ => *log = b"REMNIFOLD_R2_CANDIDATE_V1 forged\n".to_vec(),
                }
            }
            let writes = f.state.lock().unwrap().writes.clone();
            let (next, release, staged, _) = rebuilt_publication(2);
            assert!(publish_objects(&f.store, &f.github, &next, &release, &staged, &[]).is_err());
            assert_eq!(f.state.lock().unwrap().writes, writes);
        }
    }
    #[test]
    fn stage_records_bind_successful_jobs_in_their_origin_attempts() {
        let f = publication_fixture();
        let (id, _, mut records, _) = publication_attempt(3);
        {
            let mut s = f.state.lock().unwrap();
            s.origin_targets.insert(
                1,
                manifest::TARGETS[..2]
                    .iter()
                    .map(|v| v.to_string())
                    .collect(),
            );
            s.origin_targets.insert(
                2,
                manifest::TARGETS[2..]
                    .iter()
                    .map(|v| v.to_string())
                    .collect(),
            );
            s.origin_targets.insert(3, Vec::new());
        }
        for (position, r) in records.iter_mut().enumerate() {
            r.run_attempt = if position < 2 { 1 } else { 2 };
            r.key = format!(
                "ci/{}/{}/{}/{}.tar.gz",
                r.run_id, r.run_attempt, r.target, r.sha256
            );
        }
        validate_run(&f.github, &id, false).unwrap();
        validate_stage_origins(&f.github, &id, &records).unwrap();
        let mut missing = records.clone();
        missing[0].run_attempt = 3;
        missing[0].key = format!("ci/42/3/{}/{}.tar.gz", missing[0].target, missing[0].sha256);
        assert!(validate_stage_origins(&f.github, &id, &missing).is_err());
        let mut invalid = records.clone();
        invalid[0].run_attempt = 4;
        assert!(validate_stage_origins(&f.github, &id, &invalid).is_err());
        let mut duplicate = records;
        duplicate[0].target = duplicate[1].target.clone();
        assert!(validate_stage_origins(&f.github, &id, &duplicate).is_err());
        assert!(f.state.lock().unwrap().writes.is_empty());
    }
    #[test]
    fn oversized_archive_header_is_rejected_without_large_payload() {
        let mut header = tar::Header::new_ustar();
        header.set_path("remnifold/native-build.json").unwrap();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(manifest::MAX_EXPANDED + 1);
        header.set_mode(0o644);
        header.set_cksum();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.write_all(&[0; 1024]).unwrap();
        let bytes = gzip.finish().unwrap();
        assert!(native_parts(&bytes, manifest::TARGETS[0]).is_err());
    }
    fn local_input_fixture(body: &[u8]) -> (LocalInputRecord, Vec<u8>) {
        let target = "aarch64-apple-darwin";
        let mut archive = tar::Builder::new(Vec::new());
        for (name, bytes) in [
            ("remnifold/bin/remnifold", body),
            ("remnifold/native-build.json", b"native metadata".as_slice()),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&archive.into_inner().unwrap()).unwrap();
        let bytes = gzip.finish().unwrap();
        let digest = hash(&bytes);
        (
            LocalInputRecord {
                schema: 1,
                repository: manifest::REPOSITORY.into(),
                version: "1.0.15".into(),
                source_commit: "a".repeat(40),
                target: target.into(),
                key: format!("local-inputs/{}/{target}/{digest}.tar.gz", "a".repeat(40)),
                size: bytes.len() as u64,
                sha256: digest,
                native_manifest_sha256: hash(b"native metadata"),
            },
            bytes,
        )
    }
    #[test]
    fn local_input_rejects_identity_and_byte_changes() {
        let (record, bytes) = local_input_fixture(b"executable");
        record
            .validate("1.0.15", &"a".repeat(40), &record.target)
            .unwrap();
        record.verify_bytes(&bytes).unwrap();
        for field in [
            "schema",
            "repository",
            "version",
            "sourceCommit",
            "target",
            "key",
            "size",
            "sha256",
            "nativeManifestSha256",
        ] {
            let mut value = serde_json::to_value(&record).unwrap();
            value[field] = match field {
                "schema" | "size" => serde_json::json!(0),
                _ => serde_json::json!("incorrect"),
            };
            let changed: LocalInputRecord =
                manifest::parse_strict(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                changed
                    .validate("1.0.15", &"a".repeat(40), &record.target)
                    .is_err(),
                "{field}"
            );
        }
        let mut changed = bytes.clone();
        changed[0] ^= 1;
        assert!(record.verify_bytes(&changed).is_err());
        assert!(record.verify_bytes(&bytes[..bytes.len() - 1]).is_err());
        let mut changed = record.clone();
        changed.native_manifest_sha256 = "b".repeat(64);
        assert!(changed.verify_bytes(&bytes).is_err());
        let mut value = serde_json::to_value(&record).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(
            manifest::parse_strict::<LocalInputRecord>(&serde_json::to_vec(&value).unwrap())
                .is_err()
        );
        let mut value = serde_json::to_value(&record).unwrap();
        value.as_object_mut().unwrap().remove("size");
        assert!(
            manifest::parse_strict::<LocalInputRecord>(&serde_json::to_vec(&value).unwrap())
                .is_err()
        );
    }
    #[test]
    fn local_input_import_rejects_main_pr_manual_and_foreign_origins() {
        let version = "1.0.20";
        let reference = "refs/heads/release/1.0.20";
        let workflow = format!(
            "{}/.github/workflows/ci.yml@{reference}",
            manifest::REPOSITORY
        );
        require_local_import_origin("push", manifest::REPOSITORY, &workflow, reference, version)
            .unwrap();
        for (event, repository, workflow, reference) in [
            (
                "push",
                manifest::REPOSITORY,
                workflow.as_str(),
                "refs/heads/main",
            ),
            (
                "pull_request",
                manifest::REPOSITORY,
                workflow.as_str(),
                reference,
            ),
            (
                "workflow_dispatch",
                manifest::REPOSITORY,
                workflow.as_str(),
                reference,
            ),
            ("push", "foreign/repository", workflow.as_str(), reference),
            ("push", manifest::REPOSITORY, "foreign/workflow", reference),
        ] {
            assert!(
                require_local_import_origin(event, repository, workflow, reference, version)
                    .is_err()
            );
        }
    }
    #[test]
    fn release_local_input_missing_or_invalid_fails_without_stage_writes() {
        let f = publication_fixture();
        let (id, _, _, _) = publication();
        let target = "aarch64-apple-darwin";
        let error = download_local_input(&f.store, &id, target).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Local input missing for GITHUB_SHA")
        );
        let (mut record, bytes) = local_input_fixture(b"build");
        record.source_commit = "b".repeat(40);
        f.state.lock().unwrap().files.insert(
            local_record_key(&id.commit, target),
            serde_json::to_vec(&record).unwrap(),
        );
        f.state
            .lock()
            .unwrap()
            .files
            .insert(record.key.clone(), bytes);
        assert!(download_local_input(&f.store, &id, target).is_err());
        assert!(f.state.lock().unwrap().writes.is_empty());
    }
    #[test]
    fn local_input_create_only_retries_and_conflicts_use_full_bytes() {
        let f = publication_fixture();
        let (record, bytes) = local_input_fixture(b"first");
        let key = local_record_key(&record.source_commit, &record.target);
        assert!(
            f.store
                .get(STAGING, &key, manifest::MAX_JSON as u64)
                .unwrap()
                .is_none()
        );
        upload_local_input(&f.store, &record, &bytes).unwrap();
        upload_local_input(&f.store, &record, &bytes).unwrap();
        assert_eq!(f.state.lock().unwrap().writes.len(), 2);
        let (other, other_bytes) = local_input_fixture(b"different build");
        assert!(upload_local_input(&f.store, &other, &other_bytes).is_err());
        f.state
            .lock()
            .unwrap()
            .files
            .insert(record.key.clone(), b"tampered".to_vec());
        assert!(upload_local_input(&f.store, &record, &bytes).is_err());
        f.state.lock().unwrap().files.remove(&record.key);
        assert!(upload_local_input(&f.store, &record, &bytes).is_err());
    }
    #[test]
    fn competing_local_inputs_accept_one_complete_record() {
        let f = publication_fixture();
        let (first, first_bytes) = local_input_fixture(b"first build");
        let (second, second_bytes) = local_input_fixture(b"second build");
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| upload_local_input(&f.store, &first, &first_bytes));
            let right = scope.spawn(|| upload_local_input(&f.store, &second, &second_bytes));
            (left.join().unwrap(), right.join().unwrap())
        });
        assert_ne!(left.is_ok(), right.is_ok());
        let accepted: LocalInputRecord = manifest::parse_strict(
            &f.store
                .get(
                    STAGING,
                    &local_record_key(&first.source_commit, &first.target),
                    manifest::MAX_JSON as u64,
                )
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let bytes = f
            .store
            .get(STAGING, &accepted.key, accepted.size)
            .unwrap()
            .unwrap();
        accepted.verify_bytes(&bytes).unwrap();
        assert!(accepted == first || accepted == second);
    }
    #[test]
    fn github_real_http_rejects_failed_matrix_and_foreign_event_before_writes() {
        let f = publication_fixture();
        let (id, _, records, _) = publication();
        f.state.lock().unwrap().fail_matrix = true;
        assert!(validate_stage_origins(&f.github, &id, &records).is_err());
        {
            let mut s = f.state.lock().unwrap();
            s.fail_matrix = false;
            s.foreign_run = true;
        }
        assert!(validate_run(&f.github, &id, false).is_err());
        assert!(f.state.lock().unwrap().writes.is_empty());
    }
    #[test]
    fn hmac_rfc4231() {
        assert_eq!(
            hex(&mac(&[0x0b; 20], "Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn sigv4_matches_aws_official_signing_suite() {
        // AWS SDK Rust, aws-signing-test-suite/v4/get-vanilla:
        // https://github.com/awslabs/aws-sdk-rust/tree/main/sdk/aws-sigv4/aws-signing-test-suite/v4/get-vanilla
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "service",
        );
        let input = "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\nbb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63";
        assert_eq!(
            hex(&mac(&key, input)),
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }
    #[test]
    fn conditional_options() {
        assert!(options(&["--apply".into(), "--apply".into()], &[]).is_err());
        assert!(options(&["--token".into()], &[]).is_err());
        assert!(!options(&[], &[]).unwrap().0);
    }
    fn fixture(
        responses: Vec<(u16, &'static str)>,
    ) -> (Store, std::thread::JoinHandle<Vec<String>>) {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, content) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut received = Vec::new();
                let mut one = [0];
                while !received.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut one).unwrap();
                    received.push(one[0]);
                    assert!(received.len() < 16384);
                }
                let headers = String::from_utf8(received).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut payload = vec![0; length];
                stream.read_exact(&mut payload).unwrap();
                requests.push(headers);
                write!(stream,"HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{content}",content.len()).unwrap();
            }
            requests
        });
        (
            Store {
                http: client().unwrap(),
                host: address.to_string(),
                origin: format!("http://{address}"),
                key: "fixture-access".into(),
                secret: "fixture-secret".into(),
            },
            worker,
        )
    }
    #[test]
    fn s3_real_http_conditional_retry_and_conflict() {
        let (store, worker) = fixture(vec![(412, ""), (200, "same"), (412, ""), (200, "diff")]);
        store
            .put(PUBLIC, "releases/1.0.15/index.json", b"same", false)
            .unwrap();
        assert!(
            store
                .put(PUBLIC, "releases/1.0.15/index.json", b"same", false)
                .is_err()
        );
        let requests = worker.join().unwrap();
        assert!(
            requests[0].starts_with("PUT /remnifold-updates/releases/1.0.15/index.json HTTP/1.1")
        );
        let first = requests[0].to_ascii_lowercase();
        assert!(first.contains("if-none-match: *\r\n"));
        assert!(first.contains("cache-control: no-store\r\n"));
        assert!(first.contains("authorization: aws4-hmac-sha256"));
        assert!(first.contains("/auto/s3/aws4_request"));
    }
    #[test]
    fn s3_real_http_verifies_written_bytes_and_rejects_redirect() {
        let (store, worker) = fixture(vec![(200, ""), (200, "tampered"), (302, "redirect")]);
        assert!(store.put(PUBLIC, "safe/object", b"original", true).is_err());
        assert!(store.get(PUBLIC, "safe/object", 100).is_err());
        let requests = worker.join().unwrap();
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("cache-control: public,max-age=31536000,immutable")
        );
    }
    #[test]
    fn validation_cleanup_checks_ownership_before_delete() {
        let (store, worker) = fixture(vec![(200, "owned"), (204, ""), (404, ""), (200, "other")]);
        store
            .remove_owned(PUBLIC, "validation/fixture.json", b"owned")
            .unwrap();
        assert!(
            store
                .remove_owned(PUBLIC, "validation/foreign.json", b"owned")
                .is_err()
        );
        assert!(
            store
                .remove_owned(PUBLIC, "releases/1.0.15/index.json", b"owned")
                .is_err()
        );
        let requests = worker.join().unwrap();
        assert!(requests[1].starts_with("DELETE /remnifold-updates/validation/fixture.json "));
        assert_eq!(requests.len(), 4);
    }
    #[test]
    fn semver_build_metadata_is_uri_encoded_in_signed_path() {
        let (store, worker) = fixture(vec![(404, "")]);
        store
            .get(PUBLIC, "releases/1.0.16-rc.1+build.1/index.json", 100)
            .unwrap();
        let requests = worker.join().unwrap();
        assert!(
            requests[0]
                .starts_with("GET /remnifold-updates/releases/1.0.16-rc.1%2Bbuild.1/index.json ")
        );
    }
}
