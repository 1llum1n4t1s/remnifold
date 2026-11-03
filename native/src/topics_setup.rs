//! Explicit, finite maintenance of unclassified records through the existing API.
use crate::common;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashSet;

const HELP: &str = "Usage: remnifold topics-setup [--limit 1..50] [--apply]\nDefaults to a read-only plan, limit 10. Uses the existing memory connection.\n--apply queues at most the selected number of eligible unclassified records for\nWorkers AI enrichment, using the existing backend AI/vector/fact pipeline and\nits revision guards and opt-outs. Accepted requests are asynchronous, not completed\nclassifications. This incurs existing backend usage; rerun only after work completes.\n";
#[derive(Debug, PartialEq)]
struct Options {
    apply: bool,
    limit: usize,
    help: bool,
}
fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options {
        apply: false,
        limit: 10,
        help: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => o.apply = true,
            "--help" | "-h" => o.help = true,
            "--limit" => {
                i += 1;
                o.limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .filter(|v| (1..=50).contains(v))
                    .ok_or_else(|| anyhow!("--limit must be an integer from 1 through 50."))?;
            }
            _ => bail!("Unknown topics-setup option. Use topics-setup --help."),
        }
        i += 1;
    }
    Ok(o)
}
fn setup(
    o: &Options,
    mut request: impl FnMut(&str, Option<Value>) -> Result<Value>,
) -> Result<Value> {
    let overview =
        request("/v4/topics", None).map_err(|_| anyhow!("Topic overview request failed."))?;
    let page=request("/v3/documents/list",Some(json!({"topic":"__unclassified__","page":1,"limit":o.limit,"projection":"ids","enrichmentEligible":true}))).map_err(|_|anyhow!("Unclassified document listing failed."))?;
    let documents = page["documents"]
        .as_array()
        .ok_or_else(|| anyhow!("The server returned an invalid classification batch."))?;
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for document in documents {
        let id = document["id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("The server returned an invalid classification batch."))?;
        if seen.insert(id.to_owned()) {
            ids.push(id.to_owned());
        }
    }
    if ids.len() > o.limit {
        bail!("The server returned an invalid classification batch.");
    }
    let mut accepted = Vec::new();
    let mut failed = Vec::new();
    if o.apply {
        // Capture one finite page. No loop over newly eligible records or automatic
        // retries: the server owns enrichment policy, revision guards and opt-outs.
        for id in &ids {
            if request("/v4/enrich", Some(json!({"id":id}))).is_ok() {
                accepted.push(id);
            } else {
                failed.push(id);
            }
        }
    }
    Ok(
        json!({"mode":if o.apply{"apply"}else{"plan"},"unclassifiedCount":overview["unclassifiedCount"],"selected":ids.len(),"accepted":accepted,"failed":failed,"asynchronous":true}),
    )
}
pub fn run(args: &[String]) -> Result<()> {
    let options = parse(args)?;
    if options.help {
        print!("{HELP}");
        return Ok(());
    }
    let config = common::load_config(None)
        .map_err(|_| anyhow!("Memory connection is unavailable. Run remnifold setup first."))?;
    let result = setup(&options, |path, body| {
        common::api(
            &config,
            path,
            if body.is_some() { "POST" } else { "GET" },
            body.as_ref(),
            15_000,
        )
    })?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result["failed"].as_array().is_some_and(|f| !f.is_empty()) {
        bail!(
            "Some enrichment requests failed. Accepted requests may still be running; completion was not verified."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(apply: bool, limit: usize) -> Options {
        Options {
            apply,
            limit,
            help: false,
        }
    }
    #[test]
    fn options_are_bounded_and_explicit() {
        for v in ["0", "51", "1.5", "NaN", ""] {
            assert!(parse(&["--limit".into(), v.into()]).is_err());
        }
        assert!(parse(&["--limit".into()]).is_err());
        assert!(parse(&["--unknown".into()]).is_err());
        assert_eq!(parse(&[]).unwrap(), options(false, 10));
        assert_eq!(
            parse(&["--apply".into(), "--limit".into(), "2".into()]).unwrap(),
            options(true, 2)
        );
    }
    #[test]
    fn plan_has_only_all_space_eligible_index_reads() {
        let mut calls = Vec::new();
        let result = setup(&options(false, 10), |path, body| {
            calls.push((path.to_owned(), body));
            Ok(if path == "/v4/topics" {
                json!({"unclassifiedCount":200})
            } else {
                json!({"documents":[{"id":"legacy-a"}]})
            })
        })
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].1,json!({"topic":"__unclassified__","page":1,"limit":10,"projection":"ids","enrichmentEligible":true}).into());
        assert_eq!(result["selected"], 1);
        assert_eq!(result["accepted"], json!([]));
        assert_eq!(result["mode"], "plan");
    }
    #[test]
    fn finite_snapshot_deduplicates_and_reports_partial_acceptance() {
        let mut queued = Vec::new();
        let result = setup(&options(true, 2), |path, body| match path {
            "/v4/topics" => Ok(json!({"unclassifiedCount":100})),
            "/v3/documents/list" => Ok(json!({"documents":[{"id":"a"},{"id":"a"},{"id":"b"}]})),
            "/v4/enrich" => {
                let id = body.unwrap()["id"].as_str().unwrap().to_owned();
                queued.push(id.clone());
                if id == "a" {
                    bail!("fixture exception with secret");
                }
                Ok(json!({"enrichmentStatus":"pending"}))
            }
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(queued, vec!["a", "b"]);
        assert_eq!(result["accepted"], json!(["b"]));
        assert_eq!(result["failed"], json!(["a"]));
        assert_eq!(result["asynchronous"], true);
        assert!(!result.to_string().contains("secret"));
    }
    #[test]
    fn invalid_batch_is_rejected_before_mutations() {
        for documents in [
            json!([{"id":"a"},{"id":"b"}]),
            json!([{"id":" "}]),
            json!([{"id":4}]),
            json!([{}]),
            Value::Null,
        ] {
            let mut mutations = 0;
            let result = setup(&options(true, 1), |path, _| {
                if path == "/v4/enrich" {
                    mutations += 1;
                }
                Ok(if path == "/v4/topics" {
                    json!({"unclassifiedCount":100})
                } else {
                    json!({"documents":documents})
                })
            });
            assert!(result.is_err());
            assert_eq!(mutations, 0);
        }
    }
    #[test]
    fn empty_snapshot_does_not_enqueue() {
        let mut calls = 0;
        let result = setup(&options(true, 50), |path, _| {
            calls += 1;
            Ok(if path == "/v4/topics" {
                json!({"unclassifiedCount":9})
            } else {
                json!({"documents":[]})
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result["selected"], 0);
    }
    #[test]
    fn real_local_api_uses_owner_auth_without_scope_or_policy_override() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            path::PathBuf,
            time::Duration,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                json!({"unclassifiedCount":1}),
                json!({"documents":[{"id":"eligible-id"}]}),
                json!({"enrichmentStatus":"pending"}),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut bytes = [0u8; 4096];
                loop {
                    let n = stream.read(&mut bytes).unwrap();
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&bytes[..n]);
                    if let Some(end) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                        let h = String::from_utf8_lossy(&raw[..end]);
                        let len = h
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(raw).unwrap());
                let body = response.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            requests
        });
        let config = common::Config {
            base_url: base,
            api_key: "fixture-owner-key".into(),
            raw: json!({}),
            codex_home: PathBuf::new(),
        };
        let result = setup(&options(true, 1), |path, body| {
            common::api(
                &config,
                path,
                if body.is_some() { "POST" } else { "GET" },
                body.as_ref(),
                2_000,
            )
        })
        .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v4/topics "));
        assert!(requests[1].starts_with("POST /v3/documents/list "));
        assert!(requests[2].starts_with("POST /v4/enrich "));
        for request in &requests {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-owner-key")
            );
        }
        let body: Value =
            serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body, json!({"id":"eligible-id"}));
        assert_eq!(result["accepted"], json!(["eligible-id"]));
    }
}
