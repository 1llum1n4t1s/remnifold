//! Trusted-default-branch dependency update policy. Never executes PR contents.
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{fs, time::Duration};

const BOT: u64 = 49_699_333;
const REQUIRED: [&str; 1] = [".github/workflows/ci.yml"];

fn release_identity(version: &Value, reference: &str) -> Result<()> {
    let version = version
        .as_str()
        .filter(|version| !version.is_empty() && !version.contains(['\r', '\n', '\0']))
        .context("Invalid package version")?;
    ensure!(
        reference == format!("refs/heads/release/{version}"),
        "Release branch does not match package version"
    );
    Ok(())
}

pub fn release_branch(repo: &std::path::Path) -> Result<()> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(repo.join(".codex-plugin/plugin.json"))?)?;
    release_identity(&manifest["version"], &std::env::var("GITHUB_REF")?)
}

trait Api {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value>;
    fn pages(&self, path: &str, field: Option<&str>) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        for page in 1..=100 {
            let separator = if path.contains('?') { '&' } else { '?' };
            let value = self.request(
                "GET",
                &format!("{path}{separator}per_page=100&page={page}"),
                None,
            )?;
            let rows = field
                .map_or(&value, |key| &value[key])
                .as_array()
                .context("GitHub pagination response invalid")?;
            all.extend(rows.iter().cloned());
            if rows.len() < 100 {
                return Ok(all);
            }
        }
        bail!("GitHub pagination exceeded safety bound")
    }
}

struct Github {
    client: Client,
    token: String,
    repository: String,
}
impl Api for Github {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        // URLs and response bodies never become errors: credentials may be reflected by servers.
        let url = if path == "/graphql" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("https://api.github.com/repos/{}{path}", self.repository)
        };
        let mut request = self
            .client
            .request(method.parse()?, url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("GitHub request failed"))?;
        let status = response.status();
        if !status.is_success() {
            bail!("GitHub request rejected ({})", status.as_u16());
        }
        response
            .json()
            .map_err(|_| anyhow::anyhow!("GitHub response invalid"))
    }
}

fn eligible(pr: &Value, repository: &Value) -> bool {
    pr["state"] == "open"
        && pr["draft"] == false
        && pr["user"]["id"] == BOT
        && pr["user"]["login"] == "dependabot[bot]"
        && pr["head"]["repo"]["id"] == repository["id"]
        && pr["base"]["ref"] == repository["default_branch"]
}

fn dependency_file(file: &Value) -> bool {
    // This repository has a single Cargo workspace.
    // Unknown ecosystems and code changes fail closed rather than inheriting a broad suffix allowlist.
    matches!(
        file["filename"].as_str(),
        Some("Cargo.toml" | "Cargo.lock" | "native/Cargo.toml" | "dev/Cargo.toml")
    ) && file["status"] == "modified"
        && file.get("previous_filename").is_none()
}

fn verified_commits(commits: &[Value], count: u64, owner: &str) -> bool {
    !commits.is_empty()
        && commits.len() as u64 == count
        && commits.iter().all(|commit| {
            commit["commit"]["verification"]["verified"] == true
                && (commit["author"]["id"] == BOT
                    || (commit["parents"].as_array().is_some_and(|p| p.len() == 2)
                        && commit["author"]["login"]
                            .as_str()
                            .is_some_and(|name| name == "github-actions[bot]" || name == owner)))
        })
}

fn latest_success(runs: &[Value], sha: &str) -> bool {
    runs.iter()
        .filter(|run| {
            run["head_sha"] == sha
                && matches!(
                    run["event"].as_str(),
                    Some("pull_request" | "workflow_dispatch")
                )
        })
        .max_by_key(|run| {
            (
                run["id"].as_u64().unwrap_or(0),
                run["run_attempt"].as_u64().unwrap_or(0),
            )
        })
        .is_some_and(|run| run["status"] == "completed" && run["conclusion"] == "success")
}

fn checks_success(checks: &[Value], own_run: &str) -> bool {
    checks
        .iter()
        .filter(|check| {
            let own_url = format!(
                "https://github.com/{}/actions/runs/{own_run}",
                std::env::var("GITHUB_REPOSITORY").unwrap_or_default()
            );
            !(check["app"]["slug"] == "github-actions"
                && check["details_url"]
                    .as_str()
                    .is_some_and(|url| url == own_url || url.starts_with(&(own_url + "/"))))
        })
        .all(|check| {
            check["status"] == "completed"
                && matches!(
                    check["conclusion"].as_str(),
                    Some("success" | "neutral" | "skipped")
                )
        })
}

fn unchanged(pr: &Value, repository: &Value, sha: &str, current_base: &Value, base: &str) -> bool {
    eligible(pr, repository)
        && pr["head"]["sha"] == sha
        && current_base["commit"]["sha"] == base
        && pr["mergeable"] == true
}

fn verify(
    api: &impl Api,
    pr: &Value,
    repository: &Value,
    workflows: &[Value],
    own_run: &str,
) -> Result<String> {
    ensure!(
        eligible(pr, repository),
        "PR identity or repository changed"
    );
    let number = pr["number"].as_u64().context("Missing PR number")?;
    let sha = pr["head"]["sha"].as_str().context("Missing head")?;
    let files = api.pages(&format!("/pulls/{number}/files"), None)?;
    ensure!(
        !files.is_empty()
            && files.len() as u64 == pr["changed_files"].as_u64().unwrap_or(0)
            && files.iter().all(dependency_file),
        "Non-dependency or incomplete file list"
    );
    let commits = api.pages(&format!("/pulls/{number}/commits"), None)?;
    let owner = repository["owner"]["login"]
        .as_str()
        .context("Missing repository owner")?;
    ensure!(
        verified_commits(&commits, pr["commits"].as_u64().unwrap_or(0), owner),
        "Unverified or unexpected commit author"
    );
    ensure!(
        pr["mergeable"] != false && pr["mergeable_state"] != "dirty",
        "Merge conflict"
    );
    for workflow in workflows {
        let id = workflow["id"].as_u64().context("Missing workflow id")?;
        let runs = api.pages(
            &format!("/actions/workflows/{id}/runs?head_sha={sha}"),
            Some("workflow_runs"),
        )?;
        ensure!(
            latest_success(&runs, sha),
            "Latest exact-head CI has not succeeded"
        );
    }
    let checks = api.pages(
        &format!("/commits/{sha}/check-runs?filter=latest"),
        Some("check_runs"),
    )?;
    ensure!(
        checks_success(&checks, own_run),
        "Other checks pending or failing"
    );
    let statuses = api.request("GET", &format!("/commits/{sha}/status"), None)?;
    ensure!(
        statuses["total_count"] == 0 || statuses["state"] == "success",
        "Commit status pending or failing"
    );
    // GitHub still enforces branch rules at merge time; an explicit review rejection holds earlier.
    let review = api.request("POST", "/graphql", Some(json!({"query":"query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewDecision}}}","variables":{"owner":owner,"name":repository["name"],"number":number}})))?;
    ensure!(
        review.get("errors").is_none() && review["data"]["repository"]["pullRequest"].is_object(),
        "Review policy unavailable"
    );
    ensure!(
        !matches!(
            review["data"]["repository"]["pullRequest"]["reviewDecision"].as_str(),
            Some("CHANGES_REQUESTED" | "REVIEW_REQUIRED")
        ),
        "Review approval required"
    );
    Ok(sha.to_owned())
}

pub fn dependabot(args: &[String]) -> Result<()> {
    ensure!(
        args.is_empty() || args == ["--apply"],
        "Expected dependabot [--apply]"
    );
    let apply = args == ["--apply"];
    ensure!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true"),
        "Dependabot requires GitHub Actions"
    );
    let name = std::env::var("GITHUB_REPOSITORY")?;
    ensure!(
        name.starts_with("1llum1n4t1s/") && name.split('/').count() == 2,
        "Unexpected repository owner"
    );
    let required: Vec<String> = serde_json::from_str(&std::env::var("REQUIRED_WORKFLOWS")?)?;
    ensure!(
        required == REQUIRED,
        "Required workflow policy does not match compiled policy"
    );
    let api = Github {
        client: Client::builder()
            .user_agent("remnifold-ci")
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        token: std::env::var("GH_TOKEN")?,
        repository: name,
    };
    let repository = api.request("GET", "", None)?;
    let branch = repository["default_branch"]
        .as_str()
        .context("Missing default branch")?;
    // The caller checks out the trusted default branch; never accept pull_request code here.
    let reference = std::env::var("GITHUB_REF")?;
    ensure!(
        reference == format!("refs/heads/{branch}"),
        "Policy must execute from the default branch"
    );
    let event: Value = serde_json::from_slice(&fs::read(std::env::var("GITHUB_EVENT_PATH")?)?)?;
    let prs = if std::env::var("GITHUB_EVENT_NAME")? == "pull_request_target" {
        vec![event["pull_request"].clone()]
    } else {
        api.pages("/pulls?state=open", None)?
    };
    let mut workflows = Vec::new();
    for path in required {
        let workflow = api.request(
            "GET",
            &format!("/actions/workflows/{}", path.rsplit('/').next().unwrap()),
            None,
        )?;
        ensure!(
            workflow["path"] == path && workflow["state"] == "active",
            "Required workflow mismatch"
        );
        workflows.push(workflow);
    }
    let own_run = std::env::var("GITHUB_RUN_ID")?;
    let mut report = Vec::new();
    for item in prs.into_iter().filter(|pr| eligible(pr, &repository)) {
        let number = item["number"].as_u64().context("Missing PR number")?;
        let base_path = format!(
            "/branches/{}",
            url::form_urlencoded::byte_serialize(branch.as_bytes()).collect::<String>()
        );
        let base = api.request("GET", &base_path, None)?["commit"]["sha"]
            .as_str()
            .context("Default branch SHA missing")?
            .to_owned();
        let pr = api.request("GET", &format!("/pulls/{number}"), None)?;
        let outcome = (|| -> Result<String> {
            let sha = verify(&api, &pr, &repository, &workflows, &own_run)?;
            let current_base = api.request("GET", &base_path, None)?;
            let current = api.request("GET", &format!("/pulls/{number}"), None)?;
            ensure!(
                unchanged(&current, &repository, &sha, &current_base, &base),
                "Head, base, or mergeability changed"
            );
            if !apply {
                return Ok("eligible (read-only)".into());
            }
            let method = if repository["allow_squash_merge"] == true {
                "squash"
            } else if repository["allow_merge_commit"] == true {
                "merge"
            } else {
                "rebase"
            };
            let result = api.request(
                "PUT",
                &format!("/pulls/{number}/merge"),
                Some(json!({"sha":sha,"merge_method":method})),
            )?;
            ensure!(
                result["merged"] == true,
                "GitHub merge requirements not satisfied"
            );
            Ok("merged after exact-head CI verification".into())
        })();
        // Only our fixed policy messages are emitted; no request/response causes are included.
        let message = match outcome {
            Ok(message) => message,
            Err(error) => format!("held: {error}"),
        };
        println!("#{number}: {message}");
        report.push(format!("- #{number}: {message}"));
    }
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        fs::write(
            path,
            format!(
                "### Dependabot auto-merge\n\n{}\n",
                if report.is_empty() {
                    "No eligible PRs".into()
                } else {
                    report.join("\n")
                }
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowlist_rejects_code_and_removed_manifests() {
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "native/Cargo.toml",
            "dev/Cargo.toml",
        ] {
            assert!(dependency_file(
                &json!({"filename":path,"status":"modified"})
            ));
        }
        for path in [
            "native/src/main.rs",
            "server/package.json",
            ".github/workflows/ci.yml",
            "foo/Cargo.toml",
        ] {
            assert!(!dependency_file(
                &json!({"filename":path,"status":"modified"})
            ));
        }
        assert!(!dependency_file(
            &json!({"filename":"Cargo.toml","status":"removed"})
        ));
    }
    #[test]
    fn exact_latest_ci_must_succeed() {
        let good = json!({"id":1,"head_sha":"head","event":"pull_request","status":"completed","conclusion":"success"});
        let pending = json!({"id":2,"head_sha":"head","event":"pull_request","status":"queued"});
        assert!(latest_success(&[good.clone()], "head"));
        assert!(!latest_success(&[good.clone(), pending], "head"));
        assert!(!latest_success(&[good], "other"));
    }
    #[test]
    fn signed_commits_and_complete_list_required() {
        let signed =
            json!({"commit":{"verification":{"verified":true}},"author":{"id":BOT},"parents":[{}]});
        assert!(verified_commits(&[signed.clone()], 1, "owner"));
        assert!(!verified_commits(&[signed.clone()], 2, "owner"));
        let mut forged = signed;
        forged["author"]["id"] = json!(123);
        assert!(!verified_commits(&[forged], 1, "owner"));
    }
    #[test]
    fn failed_or_pending_checks_hold() {
        assert!(checks_success(
            &[json!({"status":"completed","conclusion":"success"})],
            "1"
        ));
        assert!(!checks_success(&[json!({"status":"in_progress"})], "1"));
        assert!(!checks_success(
            &[json!({"status":"completed","conclusion":"failure"})],
            "1"
        ));
    }
    struct Fixture {
        replies: std::collections::HashMap<String, Value>,
    }
    impl Api for Fixture {
        fn request(&self, method: &str, path: &str, _body: Option<Value>) -> Result<Value> {
            ensure!(method != "PUT", "Tests must never invoke merging");
            self.replies
                .get(path)
                .cloned()
                .with_context(|| format!("Unexpected fixture request {path}"))
        }
    }
    fn fixture() -> (Fixture, Value, Value, Vec<Value>) {
        let pr = json!({"number":1,"state":"open","draft":false,"user":{"id":BOT,"login":"dependabot[bot]"},"head":{"sha":"abc","repo":{"id":42}},"base":{"ref":"main"},"changed_files":1,"commits":1,"mergeable":true});
        let repository =
            json!({"id":42,"default_branch":"main","owner":{"login":"owner"},"name":"repo"});
        let replies = [
            ("/pulls/1/files?per_page=100&page=1", json!([{"filename":"Cargo.lock","status":"modified"}])),
            ("/pulls/1/commits?per_page=100&page=1", json!([{"commit":{"verification":{"verified":true}},"author":{"id":BOT},"parents":[{}]}])),
            ("/actions/workflows/5/runs?head_sha=abc&per_page=100&page=1",json!({"workflow_runs":[{"id":1,"head_sha":"abc","event":"pull_request","status":"completed","conclusion":"success"}]})),
            ("/commits/abc/check-runs?filter=latest&per_page=100&page=1",json!({"check_runs":[{"status":"completed","conclusion":"success"}]})),
            ("/commits/abc/status",json!({"total_count":0,"state":"pending"})),
            ("/graphql", json!({"data":{"repository":{"pullRequest":{"reviewDecision":"APPROVED"}}}})),
        ].into_iter().map(|(key,value)| (key.to_owned(),value)).collect();
        (Fixture { replies }, pr, repository, vec![json!({"id":5})])
    }
    #[test]
    fn trusted_policy_accepts_only_complete_successful_evidence() {
        let (mut api, mut pr, repository, workflows) = fixture();
        assert_eq!(
            verify(&api, &pr, &repository, &workflows, "7").unwrap(),
            "abc"
        );
        pr["head"]["repo"]["id"] = json!(99);
        assert!(verify(&api, &pr, &repository, &workflows, "7").is_err());
        pr["head"]["repo"]["id"] = json!(42);
        pr["changed_files"] = json!(2);
        assert!(verify(&api, &pr, &repository, &workflows, "7").is_err());
        pr["changed_files"] = json!(1);
        api.replies.insert(
            "/commits/abc/status".into(),
            json!({"total_count":1,"state":"pending"}),
        );
        assert!(verify(&api, &pr, &repository, &workflows, "7").is_err());
        api.replies.insert(
            "/commits/abc/status".into(),
            json!({"total_count":1,"state":"success"}),
        );
        api.replies.insert(
            "/graphql".into(),
            json!({"data":{"repository":{"pullRequest":{"reviewDecision":"CHANGES_REQUESTED"}}}}),
        );
        assert!(verify(&api, &pr, &repository, &workflows, "7").is_err());
    }
    #[test]
    fn merge_boundary_rejects_changed_head_base_or_uncertain_mergeability() {
        let (_, mut pr, repository, _) = fixture();
        let base = json!({"commit":{"sha":"base"}});
        assert!(unchanged(&pr, &repository, "abc", &base, "base"));
        assert!(!unchanged(&pr, &repository, "old", &base, "base"));
        assert!(!unchanged(&pr, &repository, "abc", &base, "old"));
        pr["mergeable"] = Value::Null;
        assert!(!unchanged(&pr, &repository, "abc", &base, "base"));
        pr["mergeable"] = json!(true);
        pr["state"] = json!("closed");
        assert!(!unchanged(&pr, &repository, "abc", &base, "base"));
    }
    #[test]
    fn workflow_trigger_and_compiled_required_policy_stay_aligned() {
        let workflow = include_str!("../../.github/workflows/dependabot-auto-merge.yml");
        assert!(workflow.contains("workflows: [\"CI\"]"));
        assert!(workflow.contains("REQUIRED_WORKFLOWS: '[\".github/workflows/ci.yml\"]'"));
        assert!(include_str!("../../.github/workflows/ci.yml").starts_with("name: CI"));
        assert!(!workflow.lines().any(|line| {
            let line = line.trim_start().trim_start_matches("- ");
            line.starts_with("uses:")
        }));
    }

    #[test]
    fn r2_credentials_and_publication_are_scoped_to_trusted_pushes() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        let (matrix, publisher) = workflow.split_once("\n  publish-r2:").unwrap();
        let stage = matrix
            .split_once("      - name: Stage verified native archive")
            .unwrap()
            .1;
        for scope in [stage, publisher] {
            assert!(scope.contains("github.event_name == 'push'"));
            assert!(scope.contains("github.repository == '1llum1n4t1s/remnifold'"));
            assert!(scope.contains("github.workflow_ref == format('{0}/.github/workflows/ci.yml@{1}', github.repository, github.ref)"));
        }
        assert!(stage.contains("github.ref == 'refs/heads/main'"));
        assert!(stage.contains("startsWith(github.ref, 'refs/heads/release/')"));
        assert!(publisher.contains("needs.native-package.result == 'success'"));
        assert!(publisher.contains("needs: native-package"));
        assert!(publisher.contains("name: publish-r2"));
        assert!(publisher.contains("actions: read"));
        assert!(publisher.contains("contents: read"));
        assert!(publisher.contains("startsWith(github.ref, 'refs/heads/release/')"));
        assert!(publisher.contains("cargo xtask ci-release-branch"));
        assert!(!workflow.contains("ref_protected"));
        assert!(matrix.contains("&& 'r2-staging' || 'validation'"));
        assert!(stage.contains("CI_R2_ENVIRONMENT: r2-staging"));
        assert!(publisher.contains("environment: r2-release"));
        assert!(publisher.contains("CI_R2_ENVIRONMENT: r2-release"));
        assert!(!matrix.contains("R2_RELEASE_"));
        assert!(!matrix.contains("statuses: write"));
        assert!(!publisher.contains("R2_CI_"));
        assert_eq!(workflow.matches("statuses: write").count(), 1);
        assert!(
            publisher.contains(
                "REMNIFOLD_R2_STAGE_RECORDS: ${{ toJSON(needs.native-package.outputs) }}"
            )
        );
        assert!(
            publisher.contains("REMNIFOLD_R2_NEEDS_RESULT: ${{ needs.native-package.result }}")
        );
        assert!(stage.contains("cargo xtask r2-stage") && stage.contains("--apply"));
        assert!(publisher.contains("cargo xtask r2-publish --apply"));
    }

    #[test]
    fn release_branch_must_match_manifest_version_exactly() {
        assert!(release_identity(&json!("1.0.16"), "refs/heads/release/1.0.16").is_ok());
        for reference in [
            "refs/heads/main",
            "refs/heads/release/1.0.15",
            "refs/tags/1.0.16",
            "refs/heads/release/1.0.16/suffix",
        ] {
            assert!(release_identity(&json!("1.0.16"), reference).is_err());
        }
        for version in [Value::Null, json!(16), json!(""), json!("1.0.16\n")] {
            assert!(release_identity(&version, "refs/heads/release/1.0.16").is_err());
        }
    }

    #[test]
    fn ci_replaces_only_powershell_with_native_operations() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        assert!(!workflow.contains("pwsh") && !workflow.contains("powershell"));
        assert!(workflow.contains("shell: bash"));
        assert!(workflow.contains("remnifold-ci-paths.exe\" ensure-directory \"$root\""));
        assert!(workflow.contains("remnifold-ci-paths.exe\" reports \"$CI_ROOT/validation\""));
        assert!(workflow.contains("cargo xtask ci-release-branch"));
        assert!(!workflow.contains("shell: 'remnifold_ci.exe"));
        let install = workflow.find("rustup toolchain install 1.99.1").unwrap();
        let helper = workflow
            .find("rustup run 1.99.1 rustc --edition 2024 dev/src/ci_runner.rs")
            .unwrap();
        let configure = workflow
            .find("remnifold-ci-paths.exe\" ensure-directory")
            .unwrap();
        assert!(
            install < helper && helper < configure,
            "CI helper must use the installed pinned toolchain before isolated directory creation"
        );
    }

    #[test]
    fn matrix_output_names_are_unique_and_workflows_have_no_executable_actions() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        for name in [
            "windows_x64",
            "macos_arm64",
            "macos_x64",
            "linux_x64",
            "linux_arm64",
        ] {
            assert_eq!(
                workflow
                    .matches(&format!(
                        "      {name}: ${{{{ steps.stage.outputs.{name} }}}}"
                    ))
                    .count(),
                1
            );
        }
        for workflow in [
            workflow,
            include_str!("../../.github/workflows/dependabot-auto-merge.yml"),
        ] {
            assert!(!workflow.lines().any(|line| {
                line.trim_start()
                    .trim_start_matches("- ")
                    .starts_with("uses:")
            }));
            assert!(!workflow.contains("setup-node") && !workflow.contains("upload-artifact"));
        }
        assert!(workflow.contains("absolute executable paths are not intercepted"));
        assert!(
            !include_str!("../../.github/dependabot.yml")
                .contains("package-ecosystem: github-actions")
        );
    }

    // Literal command inspection complements the PATH guard. It intentionally does
    // not claim to prove the value of dynamically calculated executable paths.
    fn native_commands(source: &str) -> bool {
        let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        compact.split("Command::new(").skip(1).all(|tail| {
            let Some(literal) = tail.strip_prefix('"') else {
                return true;
            };
            let Some((literal, _)) = literal.split_once('"') else {
                return false;
            };
            let filename = literal
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(literal)
                .trim_end_matches(".exe");
            ![
                "node", "nodejs", "npm", "npx", "pnpm", "yarn", "corepack", "wrangler",
            ]
            .contains(&filename)
        })
    }

    #[test]
    fn native_runtime_and_task_runner_have_no_literal_node_commands() -> Result<()> {
        assert!(!native_commands("Command::new(\"node\")"));
        assert!(!native_commands("Command::new(\"C:/tools/node.exe\")"));
        assert!(native_commands("Command::new(\"cargo\")"));
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let mut pending = vec![
            repo.join("dev/src"),
            repo.join("native/src"),
            repo.join("native/build.rs"),
        ];
        while let Some(path) = pending.pop() {
            ensure!(
                !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "Linked Rust source rejected by command audit"
            );
            if path.is_dir() {
                for entry in fs::read_dir(path)? {
                    pending.push(entry?.path());
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                ensure!(
                    native_commands(&fs::read_to_string(&path)?),
                    "Literal JavaScript tool invocation in {}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}
