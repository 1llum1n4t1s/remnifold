//! 公開 R2 の実バイト列は、固定 CI の publish-r2 job log witness と照合する。
use super::*;
use crate::release_manifest as protocol;

const CONFIG: &str = ".github/remnifold-distribution.json";

pub(super) fn config(
    context: &Context,
    tree: &Value,
    commit: &str,
) -> Result<Option<protocol::DistributionConfig>> {
    let entries = tree["tree"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("INCOMPLETE_RELEASE_TREE"))?;
    let matches: Vec<_> = entries.iter().filter(|e| e["path"] == CONFIG).collect();
    if matches.is_empty() {
        return Ok(None);
    }
    ensure!(
        matches.len() == 1 && matches[0]["type"] == "blob" && matches[0]["mode"] == "100644",
        "INVALID_DISTRIBUTION_CONFIG"
    );
    let value = context.api(&format!("contents/{CONFIG}?ref={commit}"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            value["content"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<String>(),
        )
        .map_err(|_| anyhow::anyhow!("INVALID_DISTRIBUTION_CONFIG"))?;
    ensure!(
        blob(&bytes) == matches[0]["sha"].as_str().unwrap_or(""),
        "INVALID_DISTRIBUTION_CONFIG"
    );
    Ok(Some(protocol::parse_updater_config(&bytes).map_err(
        |_| anyhow::anyhow!("INVALID_DISTRIBUTION_CONFIG"),
    )?))
}

fn run_identity(
    run: &Value,
    context: &Context,
    commit: &str,
    version: &str,
    workflow: u64,
    repository: u64,
) -> bool {
    run["name"] == "CI"
        && run["head_sha"] == commit
        && run["event"] == "push"
        && run["head_branch"] == format!("release/{version}")
        && run["path"] == ".github/workflows/ci.yml"
        && run["workflow_id"] == workflow
        && run["repository"]["full_name"] == context.repository
        && run["head_repository"]["full_name"] == context.repository
        && run["repository"]["id"] == repository
        && run["head_repository"]["id"] == repository
}

fn repository_id(context: &Context) -> Result<u64> {
    let repository = context.cli(
        &context.gh,
        &strings(&[
            "api",
            "--hostname",
            "github.com",
            &format!("repos/{}", context.repository),
        ]),
        "GITHUB_QUERY_FAILED",
    )?;
    ensure!(
        repository["full_name"] == context.repository,
        "RELEASE_CI_IDENTITY_MISMATCH"
    );
    repository["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_IDENTITY_MISMATCH"))
}

fn workflow_id(context: &Context) -> Result<u64> {
    let w = context.api("actions/workflows/ci.yml")?;
    ensure!(
        w["path"] == ".github/workflows/ci.yml",
        "RELEASE_CI_IDENTITY_MISMATCH"
    );
    w["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_IDENTITY_MISMATCH"))
}

pub(super) fn latest_ci(context: &Context, commit: &str, version: &str) -> Result<Value> {
    let workflow = workflow_id(context)?;
    let repository = repository_id(context)?;
    let runs = context.api(&format!(
        "actions/runs?head_sha={commit}&event=push&per_page=100"
    ))?;
    let runs = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_NOT_SUCCESSFUL"))?;
    // ページ上限に達した一覧だけでは最新の CI を確定できない。
    ensure!(runs.len() < 100, "RELEASE_CI_DISCOVERY_LIMIT");
    let run = runs
        .iter()
        .filter(|run| run_identity(run, context, commit, version, workflow, repository))
        .max_by_key(|r| {
            (
                r["id"].as_u64().unwrap_or(0),
                r["run_attempt"].as_u64().unwrap_or(0),
            )
        })
        .ok_or_else(|| anyhow::anyhow!("RELEASE_CI_NOT_SUCCESSFUL"))?;
    ensure!(
        run["status"] == "completed"
            && run["conclusion"] == "success"
            && run["id"].as_u64().is_some_and(|id| id > 0)
            && run["run_attempt"]
                .as_u64()
                .is_some_and(|attempt| attempt > 0),
        "RELEASE_CI_NOT_SUCCESSFUL"
    );
    Ok(run.clone())
}

fn witness(
    context: &Context,
    latest: &Value,
    manifest: &protocol::ReleaseManifest,
    digest: &str,
) -> Result<u64> {
    let run = latest["id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("R2_WITNESS_JOB_MISMATCH"))?;
    let attempt = latest["run_attempt"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("R2_WITNESS_JOB_MISMATCH"))?;
    let jobs = context.api(&format!(
        "actions/runs/{run}/attempts/{attempt}/jobs?per_page=100"
    ))?;
    let entries = jobs["jobs"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("R2_WITNESS_JOB_MISMATCH"))?;
    ensure!(
        entries.len() < 100 && jobs["total_count"].as_u64() == Some(entries.len() as u64),
        "R2_WITNESS_JOB_DISCOVERY_LIMIT"
    );
    let matches: Vec<_> = entries
        .iter()
        .filter(|job| job["name"] == "publish-r2")
        .collect();
    ensure!(matches.len() == 1, "R2_WITNESS_JOB_MISMATCH");
    let job = matches[0];
    ensure!(
        job["run_id"] == run
            && job["head_sha"] == manifest.source_commit
            && job["status"] == "completed"
            && job["conclusion"] == "success",
        "R2_WITNESS_JOB_MISMATCH"
    );
    let id = job["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("R2_WITNESS_JOB_MISMATCH"))?;
    let escape = if let Some(supported) = context.gh_log_escape.get() {
        *supported
    } else {
        let help = execute_with_limits(
            &context.gh,
            &strings(&["api", "--help"]),
            &context.work,
            Duration::from_secs(120),
            protocol::MAX_WITNESS_LOG as u64,
        )
        .map_err(|_| anyhow::anyhow!("R2_WITNESS_LOG_UNAVAILABLE"))?;
        let supported = supports_escape_flag(&help)?;
        let _ = context.gh_log_escape.set(supported);
        supported
    };
    let mut args = strings(&["api", "--hostname", "github.com"]);
    if escape {
        args.push("--allow-escape-sequences".into());
    }
    args.push(format!(
        "repos/{}/actions/jobs/{id}/logs",
        context.repository
    ));
    let logs = execute_with_limits(
        &context.gh,
        &args,
        &context.work,
        Duration::from_secs(120),
        protocol::MAX_WITNESS_LOG as u64,
    )
    .map_err(|_| anyhow::anyhow!("R2_WITNESS_LOG_UNAVAILABLE"))?;
    protocol::verify_witness(&logs, manifest, digest)
        .map_err(|_| anyhow::anyhow!("R2_WITNESS_MISMATCH"))?;
    Ok(id)
}

fn supports_escape_flag(help: &[u8]) -> Result<bool> {
    let help =
        std::str::from_utf8(help).map_err(|_| anyhow::anyhow!("R2_WITNESS_LOG_UNAVAILABLE"))?;
    Ok(help
        .split_ascii_whitespace()
        .any(|word| word == "--allow-escape-sequences"))
}

fn publication(context: &Context, index: &protocol::VersionIndex) -> Result<()> {
    let workflow = workflow_id(context)?;
    let repository = repository_id(context)?;
    let run = context.api(&format!(
        "actions/runs/{}/attempts/{}",
        index.run_id, index.run_attempt
    ))?;
    ensure!(
        run_identity(
            &run,
            context,
            &index.source_commit,
            &index.version,
            workflow,
            repository
        ) && run["id"] == index.run_id
            && run["run_attempt"] == index.run_attempt
            && run["status"] == "completed"
            && run["conclusion"] == "success",
        "R2_PUBLICATION_CI_MISMATCH"
    );
    // commit status は新しい順。同じ公開実行の最新状態が失敗なら、古い成功で復活させない。
    let wanted_url = protocol::status_url(index.run_id, index.run_attempt);
    for page in 1..=10 {
        let statuses = context.api(&format!(
            "commits/{}/statuses?per_page=100&page={page}",
            index.source_commit
        ))?;
        let statuses = statuses
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("R2_ANCHOR_MISSING"))?;
        for status in statuses {
            if status["context"] != protocol::STATUS_CONTEXT || status["target_url"] != wanted_url {
                continue;
            }
            ensure!(
                status["state"] == "success"
                    && status["description"] == format!("sha256:{}", index.manifest_sha256)
                    && status["creator"]["login"] == "github-actions[bot]"
                    && status["creator"]["id"] == 41898282u64
                    && status["creator"]["type"] == "Bot",
                "R2_ANCHOR_MISMATCH"
            );
            return Ok(());
        }
        if statuses.len() < 100 {
            bail!("R2_ANCHOR_MISSING");
        }
    }
    bail!("R2_ANCHOR_DISCOVERY_LIMIT")
}

fn download(base: &str, key: &str, limit: u64) -> Result<Vec<u8>> {
    ensure!(safe_name(key), "INVALID_DISTRIBUTION_URL");
    let root = url::Url::parse(base).map_err(|_| anyhow::anyhow!("INVALID_DISTRIBUTION_URL"))?;
    let fixture =
        root.scheme() == "http" && root.host_str() == Some("127.0.0.1") && root.port().is_some();
    ensure!(
        (base == protocol::PUBLIC_BASE_URL || fixture)
            && root.path() == "/"
            && root.username().is_empty()
            && root.password().is_none()
            && root.query().is_none()
            && root.fragment().is_none(),
        "INVALID_DISTRIBUTION_URL"
    );
    let url = url::Url::parse(&format!("{base}/{key}"))
        .map_err(|_| anyhow::anyhow!("INVALID_DISTRIBUTION_URL"))?;
    ensure!(
        (url.scheme() == "https" || fixture)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "INVALID_DISTRIBUTION_URL"
    );
    // GitHub の認証ヘッダー・Cookie・資格情報を渡さず、転送も追従しない。
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| anyhow::anyhow!("R2_DOWNLOAD_FAILED"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|_| anyhow::anyhow!("R2_DOWNLOAD_FAILED"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK
            && response.content_length().is_none_or(|n| n <= limit),
        "R2_DOWNLOAD_FAILED"
    );
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("R2_DOWNLOAD_FAILED"))?;
    ensure!(bytes.len() as u64 <= limit, "R2_DOWNLOAD_TOO_LARGE");
    Ok(bytes)
}

pub(super) struct Published {
    pub archive: Vec<u8>,
    pub receipt: Value,
    pub evidence: Value,
    pub files: Files,
}
pub(super) fn published(
    context: &Context,
    config: &protocol::DistributionConfig,
    commit: &str,
    version: &str,
    target: &str,
    tree: &Value,
) -> Result<Published> {
    let latest = latest_ci(context, commit, version)?;
    let bytes = download(
        &config.public_base_url,
        &protocol::index_key(version),
        protocol::MAX_JSON as u64,
    )?;
    let index = protocol::parse_index(&bytes).map_err(|_| anyhow::anyhow!("INVALID_R2_INDEX"))?;
    ensure!(
        index.source_commit == commit
            && index.version == version
            && index.repository == context.repository,
        "R2_RELEASE_IDENTITY_MISMATCH"
    );
    publication(context, &index)?;
    let bytes = download(
        &config.public_base_url,
        &index.manifest_key,
        protocol::MAX_JSON as u64,
    )?;
    ensure!(
        sha(&bytes) == index.manifest_sha256,
        "R2_MANIFEST_DIGEST_MISMATCH"
    );
    let manifest =
        protocol::parse_manifest(&bytes).map_err(|_| anyhow::anyhow!("INVALID_R2_MANIFEST"))?;
    ensure!(
        manifest.source_commit == commit
            && manifest.version == version
            && manifest.repository == context.repository
            && manifest.run_id == index.run_id
            && manifest.run_attempt == index.run_attempt,
        "R2_RELEASE_IDENTITY_MISMATCH"
    );
    let witness_job = witness(context, &latest, &manifest, &index.manifest_sha256)?;
    let asset = manifest
        .targets
        .get(target)
        .ok_or_else(|| anyhow::anyhow!("R2_TARGET_MISSING"))?;
    let receipt = json!({"schema":2,"transport":"r2","repository":context.repository,"version":version,"sourceCommit":commit,"target":target,"runId":index.run_id,"runAttempt":index.run_attempt,"witnessRunId":latest["id"],"witnessRunAttempt":latest["run_attempt"],"witnessJobId":witness_job,"manifestSha256":index.manifest_sha256,"archiveSha256":asset.sha256,"nativeManifestSha256":asset.native_manifest_sha256,"manifestKey":index.manifest_key,"archiveKey":asset.key,"archiveSize":asset.size});
    let evidence = json!({"version":version,"sha":commit,"transport":"r2","runId":index.run_id,"runAttempt":index.run_attempt,"manifestSha256":index.manifest_sha256,"workRoot":context.work});
    let bytes = download(&config.public_base_url, &asset.key, asset.size.min(LIMIT))?;
    let files = {
        ensure!(
            bytes.len() as u64 == asset.size && sha(&bytes) == asset.sha256,
            "R2_ARCHIVE_DIGEST_MISMATCH"
        );
        let files = parse_tar(&bytes)?;
        ensure!(
            files
                .get("native-build.json")
                .is_some_and(|b| sha(b) == asset.native_manifest_sha256),
            "R2_NATIVE_MANIFEST_DIGEST_MISMATCH"
        );
        verify_native(&files, commit, version, target, tree)?;
        files
    };
    Ok(Published {
        archive: bytes,
        receipt,
        evidence,
        files,
    })
}

pub(super) fn verify_receipt(
    receipt: &Value,
    local: &Files,
    context: &Context,
    version: &str,
    commit: &str,
    target: &str,
) -> Result<()> {
    let native_hash = sha(local
        .get("native-build.json")
        .ok_or_else(|| anyhow::anyhow!("NATIVE_RECEIPT_MISMATCH"))?);
    if receipt["schema"] == 1 {
        ensure!(
            receipt["sourceCommit"] == commit
                && receipt["target"] == target
                && receipt["manifestSha256"] == native_hash,
            "NATIVE_RECEIPT_MISMATCH"
        );
    } else {
        ensure!(
            receipt["schema"] == 2
                && receipt["transport"] == "r2"
                && receipt["repository"] == context.repository
                && receipt["version"] == version
                && receipt["sourceCommit"] == commit
                && receipt["target"] == target
                && receipt["nativeManifestSha256"] == native_hash
                && receipt["manifestSha256"]
                    .as_str()
                    .is_some_and(|h| hex(h, 64))
                && receipt["archiveSha256"]
                    .as_str()
                    .is_some_and(|h| hex(h, 64))
                && receipt["runId"].as_u64().is_some_and(|n| n > 0)
                && receipt["runAttempt"].as_u64().is_some_and(|n| n > 0)
                && receipt["witnessRunId"].as_u64().is_some_and(|n| n > 0)
                && receipt["witnessRunAttempt"].as_u64().is_some_and(|n| n > 0)
                && receipt["witnessJobId"].as_u64().is_some_and(|n| n > 0),
            "NATIVE_RECEIPT_MISMATCH"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            gh: vec![],
            codex: vec![],
            repository: protocol::REPOSITORY.into(),
            work: PathBuf::new(),
            gh_log_escape: std::sync::OnceLock::new(),
        }
    }

    #[test]
    fn detects_escape_flag_without_requiring_it_from_older_github_cli() {
        assert!(
            supports_escape_flag(
                b"FLAGS\n  --allow-escape-sequences  Allow terminal escape sequences\n"
            )
            .unwrap()
        );
        assert!(
            !supports_escape_flag(b"FLAGS\n  --hostname string  The GitHub hostname\n").unwrap()
        );
        assert!(!supports_escape_flag(b"prefix--allow-escape-sequences suffix\n").unwrap());
        assert!(supports_escape_flag(&[0xff]).is_err());
    }

    #[test]
    fn ci_identity_requires_workflow_path_id_and_both_repositories() {
        let context = context();
        let commit = "a".repeat(40);
        let run = json!({"name":"CI","head_sha":commit,"event":"push","head_branch":"release/1.0.15","path":".github/workflows/ci.yml","workflow_id":12,"repository":{"id":34,"full_name":context.repository},"head_repository":{"id":34,"full_name":context.repository}});
        assert!(run_identity(&run, &context, &commit, "1.0.15", 12, 34));
        for field in [
            "path",
            "workflow_id",
            "repository",
            "head_repository",
            "event",
            "head_sha",
            "head_branch",
        ] {
            let mut invalid = run.clone();
            invalid[field] = Value::Null;
            assert!(
                !run_identity(&invalid, &context, &commit, "1.0.15", 12, 34),
                "{field}"
            );
        }
        for field in ["repository", "head_repository"] {
            let mut invalid = run.clone();
            invalid[field]["id"] = json!(35);
            assert!(!run_identity(&invalid, &context, &commit, "1.0.15", 12, 34));
        }
    }

    #[test]
    fn r2_receipt_uses_native_manifest_digest_and_preserves_schema_one() {
        let context = context();
        let commit = "a".repeat(40);
        let local = BTreeMap::from([("native-build.json".into(), b"manifest".to_vec())]);
        let digest = sha(b"manifest");
        let old =
            json!({"schema":1,"sourceCommit":commit,"target":"target","manifestSha256":digest});
        assert!(verify_receipt(&old, &local, &context, "1.0.15", &commit, "target").is_ok());
        let receipt = json!({"schema":2,"transport":"r2","repository":context.repository,"version":"1.0.15","sourceCommit":commit,"target":"target","nativeManifestSha256":digest,"manifestSha256":"b".repeat(64),"archiveSha256":"c".repeat(64),"runId":1,"runAttempt":1,"witnessRunId":2,"witnessRunAttempt":1,"witnessJobId":3});
        assert!(verify_receipt(&receipt, &local, &context, "1.0.15", &commit, "target").is_ok());
        for field in [
            "transport",
            "repository",
            "version",
            "sourceCommit",
            "target",
            "nativeManifestSha256",
            "manifestSha256",
            "archiveSha256",
            "runId",
            "runAttempt",
            "witnessRunId",
            "witnessRunAttempt",
            "witnessJobId",
        ] {
            let mut invalid = receipt.clone();
            invalid[field] = Value::Null;
            assert!(
                verify_receipt(&invalid, &local, &context, "1.0.15", &commit, "target").is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn public_download_rejects_foreign_and_credential_urls_before_connecting() {
        for base in [
            "https://evil.example",
            "http://localhost:1",
            "http://127.0.0.2:1",
            "http://user@127.0.0.1:1",
            "http://127.0.0.1:1/path",
            "http://127.0.0.1:1?x=1",
            "http://127.0.0.1:1#fragment",
            "https://remnifold-updates.kagayoi.com:444",
        ] {
            assert_eq!(
                download(base, "releases/1.0.15/index.json", 100)
                    .unwrap_err()
                    .to_string(),
                "INVALID_DISTRIBUTION_URL"
            );
        }
        assert!(download(protocol::PUBLIC_BASE_URL, "../secret", 100).is_err());
    }
}
