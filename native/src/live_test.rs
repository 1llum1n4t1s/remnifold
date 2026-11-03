//! 明示実行時だけ、接続済みサーバーへ隔離した検証用レコードを作る。
use crate::common::{self, text};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const HELP: &str = "remnifold server-live-test [--keep]\nUses the configured server and API key. Creates isolated semantic/fact test records, verifies enrichment and search, and forgets them afterwards. --keep retains only these probe records. This command is separate from offline validation.";

pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|arg| arg != "--keep") {
        bail!("Unknown live-test option. Use server-live-test --help.");
    }
    let config = common::load_config(None)?;
    let mut request = |path: &str, method: &str, body: Option<&Value>, timeout| {
        common::api(&config, path, method, body, timeout)
    };
    let report = exercise(
        &mut request,
        args.iter().any(|arg| arg == "--keep"),
        std::thread::sleep,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn exercise(
    request: &mut impl FnMut(&str, &str, Option<&Value>, u64) -> Result<Value>,
    keep: bool,
    mut pause: impl FnMut(Duration),
) -> Result<Value> {
    let container = format!("semantic_smoke__{}", uuid::Uuid::new_v4());
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut ids: Vec<String> = Vec::new();
    let mut unknown_save_custom_ids = Vec::new();
    let result = (|| {
        let mut operation = |path: &str, method: &str, body: Option<&Value>| {
            bounded_request(request, deadline, path, method, body)
        };
        let health = operation("/health", "GET", None)?;
        let contents = [
            "CloudflareMemorySemanticSmokeTest の current_color は blue である。current_storage は Cloudflare D1 である。",
            "CloudflareMemorySemanticSmokeTest の current_color は red に変更された。current_color の現在値は red である。",
        ];
        let mut documents = Vec::new();
        for (index, content) in contents.iter().enumerate() {
            ensure!(
                Instant::now() < deadline,
                "Live verification deadline exceeded."
            );
            let custom_id = format!("semantic-smoke-{}", index + 1);
            unknown_save_custom_ids.push(custom_id.clone());
            let saved = operation(
                "/v3/documents",
                "POST",
                Some(&json!({
                    "content":content, "containerTag":container,
                    "customId":custom_id,
                    "metadata":{"sm_scope":"personal","test":true}
                })),
            )?;
            let id = text(&saved["id"]).to_owned();
            ensure!(!id.is_empty(), "Live save returned no document ID.");
            ids.push(id.clone());
            unknown_save_custom_ids.pop();
            let path = format!("/v3/documents/{}", encoded(&id));
            let mut document = Value::Null;
            for attempt in 0..20 {
                ensure!(
                    Instant::now() < deadline,
                    "Live enrichment deadline exceeded."
                );
                document = operation(&path, "GET", None)?;
                if !["embeddingStatus", "factStatus"].iter().any(|key| {
                    matches!(text(&document["enrichment"][key]), "pending" | "processing")
                }) {
                    break;
                }
                if attempt < 19 {
                    bounded_pause(&mut pause, deadline)?;
                }
            }
            ensure!(
                document["enrichment"]["embeddingStatus"] == "done"
                    && document["enrichment"]["factStatus"] == "done",
                "Live embedding/fact enrichment did not complete successfully."
            );
            documents.push(document);
        }
        let mut search = Value::Null;
        for attempt in 0..12 {
            ensure!(
                Instant::now() < deadline,
                "Live semantic search deadline exceeded."
            );
            search = operation(
                "/v4/search",
                "POST",
                Some(&json!({
                    "containerTag":container, "q":"検証対象が利用しているデータベースは何か", "limit":5
                })),
            )?;
            if search["results"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
            {
                break;
            }
            if attempt < 11 {
                bounded_pause(&mut pause, deadline)?;
            }
        }
        let graph = operation(
            "/v4/graph",
            "POST",
            Some(&json!({"containerTag":container,"limit":100})),
        )?;
        let profile = operation(
            "/v4/profile",
            "POST",
            Some(&json!({"containerTag":container})),
        )?;
        ensure!(
            health["semanticSearch"] == "cloudflare-vectorize"
                && health["factGraph"] == "cloudflare-d1-workers-ai",
            "Live server does not expose the required semantic/fact backend."
        );
        ensure!(
            search["results"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == ids[0])),
            "Live semantic search omitted the storage source."
        );
        ensure!(
            graph["facts"]
                .as_array()
                .is_some_and(|rows| rows.len() >= 2),
            "Live fact graph omitted source facts."
        );
        ensure!(
            graph["relations"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "Live fact graph omitted relationships."
        );
        ensure!(
            profile["profile"]["dynamic"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "Live dynamic profile is empty."
        );
        Ok(
            json!({"passed":true,"health":health,"containerTag":container,"retained":keep,
            "first":{"id":ids[0],"enrichment":documents[0]["enrichment"]},
            "second":{"id":ids[1],"enrichment":documents[1]["enrichment"]},
            "semanticSearch":{"resultCount":search["results"].as_array().map_or(0,Vec::len),"ids":search["results"].as_array().unwrap().iter().map(|row|row["id"].clone()).collect::<Vec<_>>()},
            "graph":{"factCount":graph["facts"].as_array().unwrap().len(),"relationCount":graph["relations"].as_array().unwrap().len()},
            "profile":{"dynamicCount":profile["profile"]["dynamic"].as_array().unwrap().len()}}),
        )
    })();
    let mut cleanup_failed = Vec::new();
    let cleanup_deadline = Instant::now() + Duration::from_secs(16);
    if !keep {
        for id in &ids {
            if bounded_request(
                request,
                cleanup_deadline,
                &format!("/v3/documents/{}", encoded(id)),
                "DELETE",
                None,
            )
            .is_err()
            {
                cleanup_failed.push(id.clone());
            }
        }
    }
    let retained = if keep { &ids } else { &cleanup_failed };
    let diagnostic = json!({"containerTag":container,"documentIds":ids,"retainedIds":retained,
        "cleanupFailedIds":cleanup_failed,"unknownSaveCustomIds":unknown_save_custom_ids,"keep":keep});
    let mut report =
        result.map_err(|error| anyhow::anyhow!("{error}; live probe: {diagnostic}"))?;
    ensure!(
        cleanup_failed.is_empty(),
        "Live checks passed, but probe-record cleanup failed; live probe: {diagnostic}"
    );
    report["cleanup"] = diagnostic;
    Ok(report)
}

fn bounded_request(
    request: &mut impl FnMut(&str, &str, Option<&Value>, u64) -> Result<Value>,
    deadline: Instant,
    path: &str,
    method: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(8_000) as u64;
    ensure!(remaining > 0, "Live verification deadline exceeded.");
    request(path, method, body, remaining)
}

fn bounded_pause(pause: &mut impl FnMut(Duration), deadline: Instant) -> Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    ensure!(!remaining.is_zero(), "Live verification deadline exceeded.");
    pause(remaining.min(Duration::from_millis(1_500)));
    Ok(())
}

fn encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(fail_second: bool, keep: bool) -> (Result<Value>, Vec<String>) {
        fixture_cleanup(fail_second, keep, false)
    }
    fn fixture_cleanup(
        fail_second: bool,
        keep: bool,
        fail_cleanup: bool,
    ) -> (Result<Value>, Vec<String>) {
        let mut saved = 0;
        let mut deleted = Vec::new();
        let mut request = |path: &str,
                           method: &str,
                           _body: Option<&Value>,
                           _timeout|
         -> Result<Value> {
            if method == "DELETE" {
                deleted.push(path.to_owned());
                ensure!(!fail_cleanup, "fixture cleanup failure");
                return Ok(Value::Null);
            }
            Ok(match path {
                "/health" => {
                    json!({"semanticSearch":"cloudflare-vectorize","factGraph":"cloudflare-d1-workers-ai"})
                }
                "/v3/documents" => {
                    saved += 1;
                    if fail_second && saved == 2 {
                        bail!("fixture failure");
                    }
                    json!({"id":format!("id-{saved}")})
                }
                "/v3/documents/id-1" | "/v3/documents/id-2" => {
                    json!({"enrichment":{"embeddingStatus":"done","factStatus":"done"}})
                }
                "/v4/search" => json!({"results":[{"id":"id-1","score":1.0}]}),
                "/v4/graph" => json!({"facts":[{},{}],"relations":[{}]}),
                "/v4/profile" => json!({"profile":{"dynamic":[{}]}}),
                _ => bail!("Unexpected fixture request"),
            })
        };
        let result = exercise(&mut request, keep, |_| {
            panic!("completed fixture must not poll")
        });
        (result, deleted)
    }
    #[test]
    fn verifies_semantics_and_forgets_only_created_probe_records() {
        let (result, deleted) = fixture(false, false);
        assert!(result.unwrap()["passed"].as_bool().unwrap());
        assert_eq!(deleted, ["/v3/documents/id-1", "/v3/documents/id-2"]);
    }
    #[test]
    fn partial_failure_still_forgets_successful_probe() {
        let (result, deleted) = fixture(true, false);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("containerTag"));
        assert!(error.contains("\"unknownSaveCustomIds\":[\"semantic-smoke-2\"]"));
        assert_eq!(deleted, ["/v3/documents/id-1"]);
    }
    #[test]
    fn explicit_keep_retains_probe_records() {
        let (result, deleted) = fixture(false, true);
        assert!(result.is_ok());
        assert!(deleted.is_empty());
    }
    #[test]
    fn failed_keep_reports_retained_probe_ids() {
        let (result, deleted) = fixture(true, true);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("\"retainedIds\":[\"id-1\"]"));
        assert!(deleted.is_empty());
    }
    #[test]
    fn cleanup_failure_reports_every_residual_probe_on_success_and_failure() {
        for fail_second in [false, true] {
            let (result, deleted) = fixture_cleanup(fail_second, false, true);
            let error = result.unwrap_err().to_string();
            assert!(error.contains("\"containerTag\":\"semantic_smoke__"));
            assert!(error.contains("\"cleanupFailedIds\":[\"id-1\""));
            assert_eq!(deleted.len(), if fail_second { 1 } else { 2 });
        }
    }
    #[test]
    fn expired_budget_blocks_network_and_polling() {
        let expired = Instant::now();
        let mut request = |_: &str, _: &str, _: Option<&Value>, _: u64| -> Result<Value> {
            panic!("expired network request")
        };
        assert!(bounded_request(&mut request, expired, "/v4/graph", "POST", None).is_err());
        assert!(bounded_pause(&mut |_| panic!("expired poll"), expired).is_err());
        let budget = Instant::now() + Duration::from_millis(100);
        let mut request = |_: &str, _: &str, _: Option<&Value>, timeout| {
            assert!(timeout <= 100);
            Ok(Value::Null)
        };
        bounded_request(&mut request, budget, "/v4/profile", "POST", None).unwrap();
        bounded_pause(
            &mut |delay| assert!(delay <= Duration::from_millis(100)),
            budget,
        )
        .unwrap();
    }
}
