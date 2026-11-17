//! CI release validation and workflow contracts.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::fs;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// ci.yml の `jobs:` 以下を、job ID（2文字インデントの `<id>:` 行）ごとのブロックに切り分ける。
    /// 各ブロックは自分の ID 行から次の job の ID 行の手前まで（直後の job の前置きコメントを含み得る）。
    /// job 単位で切り出すことで、job の追加・並べ替えがあっても検査対象を取り違えないようにする。
    fn job_blocks(workflow: &str) -> Vec<(&str, &str)> {
        let jobs = workflow.split_once("\njobs:\n").unwrap().1;
        let mut starts = Vec::new();
        let mut offset = 0;
        for line in jobs.split_inclusive('\n') {
            let id = line
                .trim_end()
                .strip_prefix("  ")
                .and_then(|rest| rest.strip_suffix(':'))
                .filter(|id| {
                    !id.is_empty()
                        && id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                });
            if let Some(id) = id {
                starts.push((offset, id));
            }
            offset += line.len();
        }
        starts
            .iter()
            .enumerate()
            .map(|(index, &(start, id))| {
                let end = starts.get(index + 1).map_or(jobs.len(), |next| next.0);
                (id, &jobs[start..end])
            })
            .collect()
    }

    fn job<'a>(blocks: &[(&str, &'a str)], id: &str) -> &'a str {
        let matches: Vec<_> = blocks.iter().filter(|block| block.0 == id).collect();
        assert_eq!(matches.len(), 1, "job {id} must exist exactly once");
        matches[0].1
    }

    #[test]
    fn r2_credentials_and_publication_are_scoped_to_trusted_pushes() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        let blocks = job_blocks(workflow);
        let matrix = job(&blocks, "native-package");
        let publisher = job(&blocks, "publish-r2");
        let stage = matrix
            .split_once("      - name: Stage verified native archive")
            .unwrap()
            .1;
        for scope in [stage, publisher] {
            assert!(scope.contains("github.event_name == 'push'"));
            assert!(scope.contains("github.repository == '1llum1n4t1s/remnifold'"));
            assert!(scope.contains("github.workflow_ref == format('{0}/.github/workflows/ci.yml@{1}', github.repository, github.ref)"));
        }
        // r2-staging は release/* だけに許可する（main を許可しない）。main への push は
        // Windows を含む全ターゲットが秘密情報を持たない validation Environment で検証だけを行う。
        // main を名指しするのは rust-cache の保存条件（save-if）だけに限る。Environment の選択・ステージ・
        // 公開の条件に main が入らないことを、行単位で確かめる。
        for line in workflow
            .lines()
            .filter(|line| line.contains("refs/heads/main"))
        {
            assert_eq!(
                line.trim(),
                "save-if: ${{ github.ref == 'refs/heads/main' }}",
                "main may only select cache saving"
            );
        }
        let scope = "startsWith(github.ref, 'refs/heads/release/')";
        assert!(
            matrix.contains(&format!("{scope} && 'r2-staging' || 'validation'")),
            "main jobs must select the secret-free validation environment"
        );
        assert!(
            stage.contains(scope),
            "main jobs must skip distribution calls"
        );
        assert!(!stage.contains("matrix.target == 'x86_64-pc-windows-msvc' &&"));
        assert!(!stage.contains("continue-on-error"));
        assert!(stage.contains("startsWith(github.ref, 'refs/heads/release/')"));
        assert!(publisher.contains("needs.native-package.result == 'success'"));
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

    // 検証 job は並列に走る。公開（publish-r2）は publish-r2 以外の全 job を needs に並べ、if で各 job の
    // result が success であることを確かめる。job を追加しても needs への追加漏れ（= テスト失敗のまま公開）を
    // 汎用に検出できるよう、job ID の一覧から検査する。
    #[test]
    fn publisher_needs_every_job_and_contract_jobs_hold_no_credentials() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        let blocks = job_blocks(workflow);
        let ids: Vec<&str> = blocks.iter().map(|block| block.0).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "job IDs must be unique");
        for id in [
            "native-package",
            "windows-server-contracts",
            "windows-native-contracts",
            "publish-r2",
        ] {
            assert!(ids.contains(&id), "missing job {id}");
        }

        let publisher = job(&blocks, "publish-r2");
        let needs: Vec<&str> = publisher
            .split_once("\n    needs:\n")
            .expect("publish-r2 needs must be a list")
            .1
            .lines()
            .map_while(|line| line.strip_prefix("      - "))
            .collect();
        let others: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| *id != "publish-r2")
            .collect();
        assert_eq!(
            needs.len(),
            others.len(),
            "publish-r2 needs every other job"
        );
        for id in &others {
            assert!(needs.contains(id), "publish-r2 must need {id}");
            assert!(
                publisher.contains(&format!("      needs.{id}.result == 'success' &&\n")),
                "publish-r2 must require {id} success"
            );
        }

        // 新しい検証 job は資格情報・Environment を持たず、名前が公開 witness の job 名と重ならない。
        for id in ["windows-server-contracts", "windows-native-contracts"] {
            let block = job(&blocks, id);
            assert!(block.contains(&format!("\n    name: {id}\n")));
            assert!(block.contains("\n    runs-on: windows-latest\n"));
            assert!(block.contains("\n    timeout-minutes: 90\n"));
            assert!(block.contains("\n    permissions:\n      contents: read\n    defaults:"));
            for forbidden in [
                "environment:",
                "secrets.",
                "vars.",
                "R2_",
                "matrix.",
                "strategy:",
                "statuses: write",
                "actions: read",
                "name: native-package",
                "name: publish-r2",
            ] {
                assert!(
                    !block.contains(forbidden),
                    "{id} must not contain {forbidden}"
                );
            }
            assert!(block.contains("remnifold-ci-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$CI_LABEL"));
            assert!(block.contains("printf 'REMNIFOLD_NODE_GUARD_READY=1\\n' >> \"$GITHUB_ENV\""));
            assert!(block.contains(
                "      - name: Verify no JavaScript runtime was invoked through PATH\n        if: always()\n"
            ));
            assert!(block.contains("remnifold-ci-paths.exe\" reports \"$CI_ROOT/validation\""));
        }
        assert!(job(&blocks, "windows-server-contracts").contains("run: cargo xtask type-check\n"));
        assert!(job(&blocks, "windows-server-contracts").contains("run: cargo xtask server-test "));
        assert!(
            job(&blocks, "windows-native-contracts").contains("run: cargo xtask native-test\n")
        );
        assert!(job(&blocks, "windows-native-contracts").contains("run: cargo xtask client-test "));
        // 各検証コマンドはちょうど1回だけ走る（分割で落としたり重複させたりしない）。
        for command in [
            "cargo xtask type-check",
            "cargo xtask native-test",
            "cargo xtask server-test",
            "cargo xtask client-test",
            "cargo xtask package",
            "cargo xtask verify",
            "cargo xtask updater-test",
        ] {
            assert_eq!(workflow.matches(command).count(), 1, "{command}");
        }
    }

    // Setup（Velopack）の配信ワークフローの契約。release/* への push で起動し、公開バケットへ書くのは
    // 「同じコミットの ci.yml が成功し publish-r2 が成功済み」を gate が確認した後の windows-installer だけ。
    // gate は Secrets・Environment を持たない。ci.yml に署名を持ち込まないこと（署名の失敗が strict 公開の
    // witness を壊さないため）も併せて確かめる。
    #[test]
    fn release_velopack_starts_on_release_pushes_only_after_the_gate() {
        let workflow = include_str!("../../.github/workflows/release-velopack.yml");
        let (header, jobs) = workflow.split_once("\njobs:\n").unwrap();
        let triggers = header.split_once("\non:\n").unwrap().1;
        assert!(triggers.contains("  push:\n    branches:\n      - 'release/**'\n"));
        assert!(triggers.contains("  workflow_dispatch:"));
        for forbidden in ["tags", "workflow_run", "pull_request"] {
            assert!(
                !triggers.contains(forbidden),
                "{forbidden} must not start the release"
            );
        }
        // concurrency はワークフロー直下ではなく windows-installer だけに掛ける（gate の待機で枠を占有しない）。
        assert!(!workflow.contains("\nconcurrency:"));

        let (gate, installer) = jobs.split_once("\n  windows-installer:").unwrap();
        assert!(gate.contains("  gate:\n    name: gate"));
        assert!(gate.contains("runs-on: ubuntu-latest"));
        assert!(gate.contains("actions: read") && gate.contains("contents: read"));
        assert!(gate.contains("scripts/velopack/wait-ci-publication.sh"));
        assert!(gate.contains("startsWith(github.ref, 'refs/heads/release/')"));
        for forbidden in [
            "environment:",
            "secrets.",
            "vars.",
            "r2-release",
            "SIGILUNE",
            "R2_",
            "concurrency:",
            "statuses: write",
            "uses:",
        ] {
            assert!(
                !gate.contains(forbidden),
                "gate must not contain {forbidden}"
            );
        }

        assert!(installer.contains("needs: gate"));
        assert!(installer.contains("environment: r2-release"));
        assert!(installer.contains(
            "    concurrency:\n      group: release-velopack\n      cancel-in-progress: false\n"
        ));
        assert!(installer.contains("github.repository == '1llum1n4t1s/remnifold'"));
        assert!(
            installer.contains(
                "github.event_name == 'push' || github.event_name == 'workflow_dispatch'"
            )
        );
        assert!(installer.contains("startsWith(github.ref, 'refs/heads/release/')"));
        assert!(installer.contains("cargo xtask ci-release-branch"));
        // Sigilune は版とコミット SHA の固定で取得する（リポジトリ変数の版指定は使わない）。
        assert!(installer.contains("$sigiluneVersion") && installer.contains("$sigiluneCommit"));
        assert!(!workflow.contains("vars.SIGILUNE_VERSION"));
        // 署名と R2 の Secrets はこのジョブの専用 step にだけ渡す。
        assert!(installer.contains("secrets.SIGILUNE_EMAIL"));
        assert!(installer.contains("secrets.R2_RELEASE_ACCESS_KEY_ID"));
        assert!(!workflow.lines().any(|line| {
            line.trim_start()
                .trim_start_matches("- ")
                .starts_with("uses:")
        }));
        // 署名は ci.yml へ入れない。
        let ci = include_str!("../../.github/workflows/ci.yml").to_ascii_lowercase();
        assert!(!ci.contains("sigilune") && !ci.contains("authenticode"));
    }

    #[test]
    fn release_gate_script_requires_a_single_successful_publisher() {
        let script = include_str!("../../scripts/velopack/wait-ci-publication.sh");
        assert!(script.contains("publish-r2"));
        assert!(script.contains("attempts/$run_attempt/jobs"));
        assert!(script.contains("workflows/ci.yml/runs"));
        assert!(script.contains("head_sha"));
        // 失敗・取消は待たずに即失敗にする。
        assert!(script.contains("concluded"));
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
        let install = workflow.find("rustup toolchain install 1.100.0").unwrap();
        let helper = workflow
            .find("rustup run 1.100.0 rustc --edition 2024 dev/src/ci_runner.rs")
            .unwrap();
        let configure = workflow
            .find("remnifold-ci-paths.exe\" ensure-directory")
            .unwrap();
        assert!(
            install < helper && helper < configure,
            "CI helper must use the installed pinned toolchain before isolated directory creation"
        );
    }

    /// SHA 固定で使ってよい Action（`owner/repo` と、`# vX.Y.Z` で添える版）。追加するときは、資格情報を持つ
    /// step・job に混ぜないこと、release の run で実行されないことを、下の検査が満たすように置く。
    const ALLOWED_ACTIONS: &[&str] = &["Swatinem/rust-cache"];
    const NOT_RELEASE: &str = "!startsWith(github.ref, 'refs/heads/release/')";

    // ci.yml の Action は許可リストのものだけを 40 桁のコミット SHA で固定して使う。その step は release 以外の
    // run だけで動き（release の run は R2 の資格情報を持つ）、publish-r2 と secrets を参照する step には置かない。
    #[test]
    fn ci_actions_are_allowlisted_sha_pinned_and_kept_out_of_release_credentials() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        let blocks = job_blocks(workflow);
        let mut pinned = 0;
        for (id, block) in &blocks {
            for step in block.split("\n      - name: ").skip(1) {
                let uses: Vec<&str> = step
                    .lines()
                    .filter_map(|line| line.trim_start().strip_prefix("uses: "))
                    .collect();
                if uses.is_empty() {
                    continue;
                }
                assert_ne!(*id, "publish-r2", "publish-r2 must not run Actions");
                assert_eq!(uses.len(), 1, "one Action per step");
                assert!(
                    !step.contains("secrets."),
                    "Actions must not share a step with secrets"
                );
                let (action, version) = uses[0].split_once(" # ").expect("version comment");
                let (name, sha) = action.split_once('@').expect("pinned reference");
                assert!(
                    ALLOWED_ACTIONS.contains(&name),
                    "Action {name} is not allowlisted"
                );
                assert!(
                    sha.len() == 40
                        && sha
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "Action {name} must be pinned to a full commit SHA"
                );
                let semver = version.strip_prefix('v').expect("vX.Y.Z version comment");
                assert!(
                    semver.split('.').count() == 3
                        && semver.split('.').all(
                            |part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                        ),
                    "Action {name} version comment must be vX.Y.Z"
                );
                let condition = step
                    .lines()
                    .find_map(|line| line.trim_start().strip_prefix("if: "))
                    .expect("Action steps must be conditional");
                assert!(
                    condition.contains(NOT_RELEASE),
                    "Action must skip release runs"
                );
                if *id == "native-package" {
                    assert!(condition.contains("matrix.target == 'x86_64-pc-windows-msvc' &&"));
                }
                assert!(step.contains("save-if: ${{ github.ref == 'refs/heads/main' }}"));
                pinned += 1;
            }
        }
        assert_eq!(
            pinned, 3,
            "rust-cache runs in the Windows package and both contract jobs"
        );
        // 固定 target（キャッシュ対象）は release 以外の run だけ。release は run/attempt 別の build を使う。
        assert_eq!(
            workflow
                .matches("if [[ \"$GITHUB_REF\" != refs/heads/release/* ]]; then build=")
                .count(),
            3
        );
        assert!(
            workflow.contains("REMNIFOLD_PACKAGE_BINARY=%s/%s/release/remnifold%s\\n' \"$build\"")
        );
    }

    #[test]
    fn matrix_output_names_are_unique_and_actions_are_tracked_by_dependabot() {
        let workflow = include_str!("../../.github/workflows/ci.yml");
        for name in ["windows_x64", "macos_arm64", "linux_x64", "linux_arm64"] {
            assert_eq!(
                workflow
                    .matches(&format!(
                        "      {name}: ${{{{ steps.stage.outputs.{name} }}}}"
                    ))
                    .count(),
                1
            );
        }
        // Action の許可・固定・配置は ci_actions_are_allowlisted_sha_pinned_and_kept_out_of_release_credentials が検査する。
        assert!(!workflow.contains("setup-node") && !workflow.contains("upload-artifact"));
        // 4 ターゲットは GitHub 提供ランナーがネイティブにビルドする。target と runs-on の対応を厳密に照合する
        // （Linux x64 は glibc の下限を保つため ubuntu-latest ではなく ubuntu-24.04 に固定）。
        for (os, target) in [
            ("windows-latest", "x86_64-pc-windows-msvc"),
            ("macos-latest", "aarch64-apple-darwin"),
            ("ubuntu-24.04", "x86_64-unknown-linux-gnu"),
            ("ubuntu-24.04-arm", "aarch64-unknown-linux-gnu"),
        ] {
            assert_eq!(
                workflow
                    .matches(&format!("          - os: {os}\n            target: {target}\n"))
                    .count(),
                1,
                "{target} must run on {os}"
            );
        }
        assert_eq!(workflow.matches("          - os: ").count(), 4);
        assert!(!workflow.contains("target: x86_64-apple-darwin"));
        // 手元ビルドの local-input 方式は廃止した。
        assert!(!workflow.contains("local-input"));
        let blocks = job_blocks(workflow);
        let matrix = job(&blocks, "native-package");
        // 4 つの step は全ターゲットで無条件に動く（main / PR でも実際にビルドと検証を行う）。
        for name in [
            "Prepare pinned native build tools",
            "Build native consumer archive",
            "Verify final native archive before publication",
            "Exercise native plugin update and recovery",
        ] {
            let step = matrix
                .split_once(&format!("      - name: {name}\n"))
                .unwrap_or_else(|| panic!("missing step {name}"))
                .1
                .split("\n      - name: ")
                .next()
                .unwrap();
            assert!(!step.contains("if:"), "{name} must run for every target");
        }
        // ステージは r2-stage を 1 回だけ、ターゲットで分岐せずに呼ぶ。
        let stage = matrix
            .split_once("      - name: Stage verified native archive")
            .unwrap()
            .1;
        assert_eq!(stage.matches("cargo xtask r2-stage").count(), 1);
        assert!(!stage.contains("$CI_TARGET\" ==") && !stage.contains("if [["));
        // ランナーの取り違えは、toolchain の導入直後（ビルド前）に host と matrix.target の一致で防ぐ。
        let install = matrix
            .find("      - name: Install locked Rust toolchain")
            .unwrap();
        let assert_host = matrix
            .find("test \"$(rustc -vV | sed -n 's/^host: //p')\" = \"$CI_TARGET\"")
            .expect("host assertion must exist");
        let prepare = matrix
            .find("      - name: Prepare pinned native build tools")
            .unwrap();
        assert!(install < assert_host && assert_host < prepare);
        assert!(workflow.contains("absolute executable paths are not intercepted"));
        // SHA 固定の Action は Dependabot（github-actions）が SHA と版コメントを更新する。
        assert!(
            include_str!("../../.github/dependabot.yml")
                .contains("  - package-ecosystem: github-actions\n    directory: /\n")
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
