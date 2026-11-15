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
        // r2-staging は release/* だけに許可する（main を許可しない）。main への push は
        // Windows を含む全ターゲットが秘密情報を持たない validation Environment で検証だけを行う。
        assert!(!workflow.contains("refs/heads/main"));
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

    #[test]
    fn matrix_output_names_are_unique_and_workflows_have_no_executable_actions() {
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
        assert!(!workflow.lines().any(|line| {
            line.trim_start()
                .trim_start_matches("- ")
                .starts_with("uses:")
        }));
        assert!(!workflow.contains("setup-node") && !workflow.contains("upload-artifact"));
        assert_eq!(
            workflow.matches("          - os: windows-latest").count(),
            4
        );
        assert!(!workflow.contains("target: x86_64-apple-darwin"));
        assert!(workflow.contains("cargo xtask local-input import --target"));
        for name in [
            "Build native consumer archive",
            "Verify final native archive before publication",
            "Exercise native plugin update and recovery",
        ] {
            assert!(workflow.contains(&format!(
                "      - name: {name}\n        if: matrix.target == 'x86_64-pc-windows-msvc'"
            )));
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
