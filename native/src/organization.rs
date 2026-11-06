//! Owner-authenticated Codex organization transport; no model or scheduler runs here.
use crate::common;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Read},
    sync::mpsc,
    thread,
    time::Duration,
};

const MAX_INPUT: u64 = 128_000;

fn input(line: bool) -> Result<Value> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = if line {
            BufReader::new(io::stdin().take(MAX_INPUT + 1)).read_until(b'\n', &mut bytes)
        } else {
            io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes)
        };
        let result = read
            .map_err(|_| anyhow!("Organization stdin could not be read."))
            .and_then(|_| {
                if bytes.len() as u64 > MAX_INPUT {
                    bail!("Organization input is too large.");
                }
                if line && bytes.last() != Some(&b'\n') {
                    bail!("Organization line input must end with a newline.");
                }
                serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .filter(Value::is_object)
                    .ok_or_else(|| anyhow!("Organization stdin must contain one JSON object."))
            });
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(30))
        .map_err(|_| anyhow!("Organization stdin exceeded its thirty-second deadline."))?
}

pub fn run(args: &[String]) -> Result<()> {
    if args.first().is_some_and(|arg| arg == "sources") {
        if !(2..=3).contains(&args.len())
            || args[1].is_empty()
            || args[1].len() > 64
            || args
                .get(2)
                .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 2048)
        {
            bail!("Use memory-organize sources <summary-document-id> [cursor].");
        }
        return request(
            "sources",
            json!({ "documentId": args[1], "cursor": args.get(2) }),
        );
    }
    let line = args.len() == 2 && args[1] == "--line";
    if args.len() != 1 && !line {
        bail!(
            "Use memory-organize prepare, commit, lookup, or sources. commit/lookup accept optional --line."
        );
    }
    let action = args[0].as_str();
    if matches!(action, "--help" | "-h") {
        println!(
            "Usage: remnifold memory-organize <prepare|commit|lookup> [--line]\n       remnifold memory-organize sources <summary-document-id> [cursor]\nprepare acquires a bounded thirty-minute lease. commit/lookup read JSON from stdin\nwithin thirty seconds (128,000 bytes maximum). --line accepts one compact JSON\nline terminated by a newline without requiring EOF. Uses the existing owner connection.\nNo model generation, fallback, remote deployment, or plugin installation occurs."
        );
        return Ok(());
    }
    if !matches!(action, "prepare" | "commit" | "lookup") {
        bail!("Unknown organization action.");
    }
    if line && action == "prepare" {
        bail!("prepare does not accept --line.");
    }
    let payload = if action == "prepare" {
        json!({})
    } else {
        input(line)?
    };
    request(action, payload)
}

fn request(action: &str, payload: Value) -> Result<()> {
    let config = common::load_config(None)
        .map_err(|_| anyhow!("Memory organization connection is unavailable."))?;
    let result = common::api_bounded(
        &config,
        &format!("/v4/organize/{action}"),
        &payload,
        2 * 1024 * 1024,
    )
    .map_err(|error| request_error(action, error))?;
    if result["protocol"] != 1 {
        bail!("Unsupported memory organization response.");
    }
    let output = serde_json::to_string(&result)
        .map_err(|_| anyhow!("Invalid memory organization response."))?;
    if output.len() > 2 * 1024 * 1024 {
        bail!("Memory organization response is too large.");
    }
    println!("{output}");
    Ok(())
}

fn request_error(action: &str, error: anyhow::Error) -> anyhow::Error {
    match error
        .downcast_ref::<common::ApiError>()
        .and_then(|error| error.status)
    {
        Some(404) if action == "sources" => anyhow!(
            "The source listing is unavailable. Check the current summary document ID and server support for source listings; no fallback was attempted."
        ),
        Some(404) => anyhow!(
            "The server does not support memory organization. Deploy the supported server separately; no fallback was attempted."
        ),
        Some(401 | 403) => {
            anyhow!("Memory organization owner authentication failed; no fallback was attempted.")
        }
        Some(409) => anyhow!(
            "Memory organization lease, revision or retry conflict; nothing was substituted."
        ),
        _ => anyhow!("Memory organization request failed; no fallback was attempted."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_source_listing_does_not_claim_the_server_is_unsupported() {
        let missing = || {
            common::ApiError {
                status: Some(404),
                unsupported_global_search: false,
            }
            .into()
        };
        let source_error = request_error("sources", missing()).to_string();
        assert_eq!(
            source_error,
            "The source listing is unavailable. Check the current summary document ID and server support for source listings; no fallback was attempted."
        );
        assert!(
            request_error("lookup", missing())
                .to_string()
                .starts_with("The server does not support memory organization.")
        );
    }
}
