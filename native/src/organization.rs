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

#[cfg(windows)]
struct PrivateStdin {
    handle: windows_sys::Win32::Foundation::HANDLE,
    original_mode: u32,
    restore_on_drop: bool,
}

#[cfg(windows)]
impl PrivateStdin {
    fn acquire() -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::Console::{
                ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, GetConsoleMode, GetStdHandle,
                STD_INPUT_HANDLE, SetConsoleMode,
            },
        };
        // A real console handle is required: redirected pipes must fail before readiness.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut original_mode = 0;
        if handle.is_null()
            || handle == INVALID_HANDLE_VALUE
            || unsafe { GetConsoleMode(handle, &mut original_mode) } == 0
        {
            bail!("Organization private stdin requires a Windows console.");
        }
        // Rust handles the newline, avoiding the Windows cooked line buffer's size limit.
        let private_mode = original_mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT);
        if unsafe { SetConsoleMode(handle, private_mode) } == 0 {
            bail!("Organization private stdin could not disable console echo.");
        }
        let guard = Self {
            handle,
            original_mode,
            restore_on_drop: true,
        };
        let mut actual_mode = 0;
        if unsafe { GetConsoleMode(handle, &mut actual_mode) } == 0 || actual_mode != private_mode {
            bail!("Organization private stdin could not verify console mode.");
        }
        Ok(guard)
    }

    fn restore(&mut self) -> Result<()> {
        if self.restore_on_drop {
            if unsafe {
                windows_sys::Win32::System::Console::SetConsoleMode(self.handle, self.original_mode)
            } == 0
            {
                bail!("Organization private stdin could not restore console mode.");
            }
            self.restore_on_drop = false;
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for PrivateStdin {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn input(line: bool, private_stdin: bool) -> Result<Value> {
    #[cfg(windows)]
    let mut console = if private_stdin {
        Some(PrivateStdin::acquire()?)
    } else {
        None
    };
    #[cfg(not(windows))]
    if private_stdin {
        bail!("Organization private stdin requires a Windows console.");
    }
    if private_stdin {
        use std::io::Write;
        #[cfg(windows)]
        if let Some(console) = console.as_mut() {
            // Readiness can reach the sender even if flushing subsequently fails.
            // Until a terminator arrives, keep this dedicated PTY's echo disabled.
            console.restore_on_drop = false;
        }
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "REMNIFOLD_STDIN_READY")
            .and_then(|_| stderr.flush())
            .map_err(|_| anyhow!("Organization private stdin readiness could not be written."))?;
    }
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = if private_stdin {
            read_private_line(BufReader::new(io::stdin().take(MAX_INPUT + 1)), &mut bytes)
        } else if line {
            BufReader::new(io::stdin().take(MAX_INPUT + 1))
                .read_until(b'\n', &mut bytes)
                .map(|_| false)
        } else {
            io::stdin()
                .take(MAX_INPUT + 1)
                .read_to_end(&mut bytes)
                .map(|_| false)
        };
        let result = read
            .map(|terminated| (bytes, terminated))
            .map_err(|_| anyhow!("Organization stdin could not be read."));
        let _ = tx.send(result);
    });
    let received = rx.recv_timeout(Duration::from_secs(30));
    #[cfg(windows)]
    if let Some(console) = console.as_mut() {
        if matches!(&received, Ok(Ok((_, true)))) {
            console.restore_on_drop = true;
            console.restore()?;
        }
    }
    let (bytes, _) = received
        .map_err(|_| anyhow!("Organization stdin exceeded its thirty-second deadline."))??;
    parse_input(&bytes, line)
}

fn read_private_line(mut reader: impl BufRead, bytes: &mut Vec<u8>) -> io::Result<bool> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(false);
        }
        // In a Windows console without line editing, Enter arrives as CR.
        let ending = available
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'));
        let count = ending.map_or(available.len(), |index| index + 1);
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if ending.is_some() {
            *bytes.last_mut().expect("line terminator") = b'\n';
            return Ok(true);
        }
    }
}

fn parse_input(bytes: &[u8], line: bool) -> Result<Value> {
    if bytes.len() as u64 > MAX_INPUT {
        bail!("Organization input is too large.");
    }
    if line && bytes.last() != Some(&b'\n') {
        bail!("Organization line input must end with a newline.");
    }
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| anyhow!("Organization stdin must contain one JSON object."))
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
    let mut line = false;
    let mut private_stdin = false;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--line" if !line => line = true,
            "--private-stdin" if !private_stdin => private_stdin = true,
            _ => bail!(
                "Use memory-organize prepare, commit, lookup, or sources. commit/lookup accept optional --line and --private-stdin."
            ),
        }
    }
    if args.is_empty() {
        bail!(
            "Use memory-organize prepare, commit, lookup, or sources. commit/lookup accept optional --line and --private-stdin."
        );
    }
    let action = args[0].as_str();
    if matches!(action, "--help" | "-h") {
        println!(
            "Usage: remnifold memory-organize <prepare|commit|lookup> [--line [--private-stdin]]\n       remnifold memory-organize sources <summary-document-id> [cursor]\nprepare acquires a bounded thirty-minute lease. commit/lookup read JSON from stdin\nwithin thirty seconds (128,000 bytes maximum). --line accepts one compact JSON\nline terminated by a newline without requiring EOF. --private-stdin requires --line\nand a dedicated Windows PTY, never a shared console. Echo is disabled before\nREMNIFOLD_STDIN_READY on stderr. Send one final newline and no bytes afterward.\nUnterminated failure keeps echo disabled; stop sending and close the PTY on failure.\nUses the existing owner connection. No model generation, fallback, remote deployment,\nor plugin installation occurs."
        );
        return Ok(());
    }
    if !matches!(action, "prepare" | "commit" | "lookup") {
        bail!("Unknown organization action.");
    }
    if (line || private_stdin) && action == "prepare" {
        bail!("prepare does not accept --line or --private-stdin.");
    }
    if private_stdin && !line {
        bail!("--private-stdin requires --line.");
    }
    let payload = if action == "prepare" {
        json!({})
    } else {
        input(line, private_stdin)?
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

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a dedicated Windows PTY and the documented synthetic JSON line"]
    fn private_console_round_trip_and_mode_restoration() {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
            SetConsoleMode,
        };
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut original = 0;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut original) }, 0);
        let expected = json!({"content": format!("日本語🌸{}", "x".repeat(126_900))});
        assert_eq!(input(true, true).unwrap(), expected);
        let mut restored = 0;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut restored) }, 0);
        assert_eq!(restored, original);

        let mut guard = PrivateStdin::acquire().unwrap();
        guard.restore_on_drop = false;
        drop(guard);
        let mut retained = 0;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut retained) }, 0);
        assert_eq!(retained & (ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT), 0);
        // This probe sends no further input; restore its own dedicated test console.
        assert_ne!(unsafe { SetConsoleMode(handle, original) }, 0);
    }

    #[test]
    fn organization_input_retains_unicode_and_existing_boundaries() {
        let value = json!({"content":"日本語🌸\n引用と根拠"});
        let mut bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(parse_input(&bytes, false).unwrap(), value);
        assert!(parse_input(&bytes, true).is_err());
        bytes.push(b'\n');
        assert_eq!(parse_input(&bytes, true).unwrap(), value);

        let mut at_limit = br#"{"content":""#.to_vec();
        at_limit.resize(MAX_INPUT as usize - 3, b'x');
        at_limit.extend_from_slice(b"\"}\n");
        assert_eq!(at_limit.len(), MAX_INPUT as usize);
        assert!(parse_input(&at_limit, true).is_ok());
        at_limit.insert(at_limit.len() - 3, b'x');
        assert_eq!(
            parse_input(&at_limit, true).unwrap_err().to_string(),
            "Organization input is too large."
        );
        for invalid in [
            b"[]\n".as_slice(),
            b"{private invalid JSON secret}\n",
            b"{} {}\n",
        ] {
            assert_eq!(
                parse_input(invalid, true).unwrap_err().to_string(),
                "Organization stdin must contain one JSON object."
            );
        }
    }

    #[test]
    fn private_stdin_requires_line_before_reading_or_requesting() {
        for action in ["commit", "lookup"] {
            assert_eq!(
                run(&[action.into(), "--private-stdin".into()])
                    .unwrap_err()
                    .to_string(),
                "--private-stdin requires --line."
            );
        }
        for args in [
            vec!["prepare", "--line", "--private-stdin"],
            vec!["commit", "--line", "--line"],
            vec!["commit", "--private-stdin", "--private-stdin"],
        ] {
            assert!(run(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn private_console_line_accepts_enter_without_cooked_line_editing() {
        for ending in ["\r", "\n", "\r\n"] {
            let value = json!({"content": "日本語🌸".repeat(9_000)});
            let mut serialized = serde_json::to_vec(&value).unwrap();
            serialized.extend_from_slice(ending.as_bytes());
            let mut bytes = Vec::new();
            assert!(
                read_private_line(
                    BufReader::with_capacity(7, serialized.as_slice()),
                    &mut bytes,
                )
                .unwrap()
            );
            assert_eq!(parse_input(&bytes, true).unwrap(), value);
        }
        let mut bytes = Vec::new();
        assert!(!read_private_line(b"{incomplete".as_slice(), &mut bytes).unwrap());
        assert_eq!(bytes, b"{incomplete");
        let mut bytes = Vec::new();
        let oversized = vec![b'x'; MAX_INPUT as usize + 2];
        assert!(!read_private_line(oversized.as_slice().take(MAX_INPUT + 1), &mut bytes).unwrap());
        assert_eq!(bytes.len(), MAX_INPUT as usize + 1);
    }

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
