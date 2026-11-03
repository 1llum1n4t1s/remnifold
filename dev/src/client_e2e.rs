use crate::{
    package::safe_path,
    server_tests::{API_KEY, start_fixture},
    tools::Toolchain,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn command(binary: &Path, home: &Path, project: &Path, endpoint: &str) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(project)
        .env("CODEX_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("TEMP", home)
        .env("TMP", home)
        .env("TMPDIR", home)
        .env("REMNIFOLD_API_URL", endpoint)
        .env("REMNIFOLD_API_KEY", API_KEY);
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy().to_ascii_uppercase();
        if key.starts_with("REMNIFOLD_")
            || key.starts_with("KAGAYOI_MEMORY_")
            || key.starts_with("SUPERMEMORY_")
            || key == "CLOUDFLARE_MEMORY_API_KEY"
            || ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"].contains(&key.as_str())
        {
            command.env_remove(&name);
        }
    }
    command
        .env("REMNIFOLD_API_URL", endpoint)
        .env("REMNIFOLD_API_KEY", API_KEY)
        .env("NO_PROXY", "*")
        .env("LOCALAPPDATA", home.join("local-app-data"));
    // PATHをOS付属ツールだけに制限し、成功した子プロセスからNodeを起動できなくする。
    #[cfg(windows)]
    command.env(
        "PATH",
        std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\System32",
    );
    #[cfg(not(windows))]
    command.env("PATH", "");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
fn read_result(reader: impl Read + Send + 'static) -> Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > 4 * 1024 * 1024 {
                    Err(std::io::Error::other(
                        "fixture process output exceeded 4 MiB",
                    ))
                } else {
                    Ok(bytes)
                }
            });
        let _ = tx.send(result);
    });
    rx
}
struct Mcp {
    child: Child,
    input: Option<mpsc::Sender<Value>>,
    writer: Receiver<std::io::Result<()>>,
    replies: Receiver<Result<Value, String>>,
    errors: Receiver<std::io::Result<Vec<u8>>>,
    next: u64,
    project: String,
}
impl Drop for Mcp {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
    }
}
impl Mcp {
    fn start(binary: &Path, home: &Path, project: &Path, endpoint: &str) -> Result<Self> {
        let mut child = command(binary, home, project, endpoint)
            .arg("mcp")
            .spawn()?;
        let mut stdin = child.stdin.take().context("MCP stdin")?;
        let (input, receiver) = mpsc::channel::<Value>();
        let (done, writer) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> std::io::Result<()> {
                for value in receiver {
                    serde_json::to_writer(&mut stdin, &value)?;
                    stdin.write_all(b"\n")?;
                    stdin.flush()?;
                }
                Ok(())
            })();
            drop(stdin);
            let _ = done.send(result);
        });
        let stdout = child.stdout.take().context("MCP stdout")?;
        let errors = read_result(child.stderr.take().context("MCP stderr")?);
        let (tx, replies) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total = 0usize;
            loop {
                let mut bytes = Vec::new();
                let parsed = match (&mut reader)
                    .take(1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n;
                        if n <= 1024 * 1024 && total <= 32 * 1024 * 1024 && bytes.ends_with(b"\n") {
                            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
                        } else {
                            Err("MCP output exceeded frame/total bound or was incomplete".into())
                        }
                    }
                    Err(e) => Err(e.to_string()),
                };
                let failed = parsed.is_err();
                if tx.send(parsed).is_err() {
                    break;
                }
                if failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            writer,
            replies,
            errors,
            next: 0,
            project: url::Url::from_directory_path(project)
                .map_err(|_| anyhow::anyhow!("project URI"))?
                .to_string(),
        })
    }
    fn send(&mut self, value: Value) -> Result<()> {
        self.input
            .as_ref()
            .context("MCP input closed")?
            .send(value)
            .map_err(|_| anyhow::anyhow!("MCP writer closed"))?;
        Ok(())
    }
    fn exchange(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let row = match self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Ok(row)) => row,
                result => {
                    terminate(&mut self.child);
                    bail!("MCP {method} response failed: {result:?}");
                }
            };
            if row["method"] == "roots/list" {
                self.send(json!({"jsonrpc":"2.0","id":row["id"],"result":{"roots":[{"uri":self.project,"name":"native-fixture"}]}}))?;
                continue;
            }
            if row["id"] == id {
                ensure!(row.get("error").is_none(), "MCP error: {row}");
                return Ok(row["result"].clone());
            }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let value = self.exchange("tools/call", json!({"name":name,"arguments":args}))?;
        ensure!(value["isError"] != true, "tool {name}: {value}");
        Ok(value["structuredContent"].clone())
    }
    fn close(&mut self) -> Result<()> {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "MCP exited {status}");
                break;
            }
            if Instant::now() >= deadline {
                terminate(&mut self.child);
                bail!("MCP shutdown timeout");
            }
            thread::sleep(Duration::from_millis(25));
        }
        let errors = match self
            .errors
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(result) => result?,
            Err(_) => {
                terminate(&mut self.child);
                bail!("MCP stderr drain timeout");
            }
        };
        match self
            .writer
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(result) => result?,
            Err(_) => {
                terminate(&mut self.child);
                bail!("MCP stdin drain timeout");
            }
        }
        ensure!(
            errors.is_empty(),
            "MCP stderr: {}",
            String::from_utf8_lossy(&errors)
        );
        Ok(())
    }
}
fn hook(
    binary: &Path,
    home: &Path,
    project: &Path,
    endpoint: &str,
    event: &str,
    payload: Value,
) -> Result<Value> {
    let mut cmd = command(binary, home, project, endpoint);
    cmd.args(["hook", event]);
    let output = process_output(cmd, &serde_json::to_vec(&payload)?)?;
    let bytes = output.stdout;
    let errors = output.stderr;
    let status = output.status;
    ensure!(
        status.success() && errors.is_empty(),
        "hook {event}: {status} {}",
        String::from_utf8_lossy(&errors)
    );
    if bytes.is_empty() {
        Ok(json!({}))
    } else {
        Ok(serde_json::from_slice(&bytes)?)
    }
}
pub(crate) fn process_output(command: Command, input: &[u8]) -> Result<std::process::Output> {
    process_output_with_timeout(command, input, Duration::from_secs(30))
}
fn process_output_with_timeout(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
) -> Result<std::process::Output> {
    let deadline = Instant::now() + timeout;
    let mut child = command.spawn()?;
    let out = read_result(child.stdout.take().context("stdout")?);
    let err = read_result(child.stderr.take().context("stderr")?);
    let mut stdin = child.stdin.take().context("stdin")?;
    let input = input.to_vec();
    let (done, writer) = mpsc::channel();
    thread::spawn(move || {
        let result = stdin.write_all(&input);
        drop(stdin);
        let _ = done.send(result);
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            terminate(&mut child);
            let _ = writer.recv_timeout(Duration::from_secs(1));
            let _ = out.recv_timeout(Duration::from_secs(1));
            let _ = err.recv_timeout(Duration::from_secs(1));
            bail!("native subprocess timeout");
        }
        thread::sleep(Duration::from_millis(25));
    };
    let result = (|| -> Result<_> {
        let stdout = out
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("native stdout drain timeout")??;
        let stderr = err
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("native stderr drain timeout")??;
        let written = writer
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("native stdin drain timeout")?;
        if status.success() {
            written?;
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    })();
    if result.is_err() {
        terminate(&mut child);
        let _ = writer.recv_timeout(Duration::from_secs(1));
        let _ = out.recv_timeout(Duration::from_secs(1));
        let _ = err.recv_timeout(Duration::from_secs(1));
    }
    result
}
fn subprocess_canaries(work: &Path) -> Result<()> {
    let root = work.join("subprocess-canaries");
    fs::create_dir(&root)?;
    let probe = root.join(if cfg!(windows) {
        "remnifold-e2e-probe.exe"
    } else {
        "remnifold-e2e-probe"
    });
    fs::copy(std::env::current_exe()?, &probe)?;
    let mut ignored = command(&probe, &root, &root, "http://127.0.0.1:9");
    ignored.arg("stdin-ignore");
    let started = Instant::now();
    ensure!(
        process_output_with_timeout(
            ignored,
            &vec![b'x'; 1024 * 1024],
            Duration::from_millis(500)
        )
        .is_err()
            && started.elapsed() < Duration::from_secs(3),
        "non-reading stdin was not terminated within its bound"
    );
    let mut flood = command(&probe, &root, &root, "http://127.0.0.1:9");
    flood.arg("flood-before-stdin");
    let output =
        process_output_with_timeout(flood, &vec![b'y'; 128 * 1024], Duration::from_secs(3))?;
    ensure!(
        output.status.success()
            && output.stderr.is_empty()
            && output.stdout.ends_with(b"READ=131072"),
        "stdout-before-stdin canary deadlocked or lost input"
    );
    for _ in 0..32 {
        let mut quick = command(&probe, &root, &root, "http://127.0.0.1:9");
        quick.arg("immediate");
        let result = process_output_with_timeout(quick, b"", Duration::from_secs(3))?;
        ensure!(
            result.status.success() && result.stdout.is_empty() && result.stderr.is_empty(),
            "immediately exiting subprocess failed"
        );
    }
    fs::write(
        work.join("subprocess-validation.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":true,"ignoredStdinDeadlineMs":500,"stdoutBeforeStdinBytes":128*1024,"immediateExits":32,"nodeOnPath":false}),
        )?,
    )?;
    Ok(())
}
fn short_key_capture(
    binary: &Path,
    home: &Path,
    project: &Path,
    worker: &str,
    http: &reqwest::blocking::Client,
    key: &str,
) -> Result<()> {
    use std::net::TcpListener;
    let session = format!("short-key-{}", key.len());
    fs::create_dir_all(home.join("sessions"))?;
    let user = format!("ordinary text {key} with preserved discussion");
    let answer = format!("answer uses {key} and preserved result");
    let transcript = home.join("sessions").join(format!("{session}.jsonl"));
    let rows = [
        json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
        json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":user}]}}),
        json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}}),
    ];
    fs::write(
        &transcript,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"id":session,"thread_name":format!("ordinary {key} title")})
        ),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let client = http.clone();
    let destination = format!("{worker}/v3/documents");
    let expected_key = key.to_owned();
    let proxy = thread::spawn(move || -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(Instant::now() < deadline, "short-key proxy accept timeout");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let body = loop {
            let n = stream.read(&mut buffer)?;
            ensure!(n > 0, "short-key request truncated");
            bytes.extend_from_slice(&buffer[..n]);
            ensure!(bytes.len() < 1024 * 1024, "short-key request oversized");
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                    })
                    .context("short-key content length")?;
                if bytes.len() >= end + 4 + length {
                    ensure!(
                        headers.lines().any(|line| line
                            .split_once(':')
                            .is_some_and(|(name, value)| name
                                .eq_ignore_ascii_case("authorization")
                                && value.trim() == format!("Bearer {expected_key}"))),
                        "CLI ignored configured short key"
                    );
                    break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])?;
                }
            }
        };
        // このローカルfixtureだけで認証を変換し、本文は実Workerへそのまま渡す。
        let response = client
            .post(destination)
            .bearer_auth(API_KEY)
            .json(&body)
            .send()?
            .error_for_status()?
            .bytes()?;
        write!(
            stream,
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )?;
        stream.write_all(&response)?;
        Ok(body)
    });
    let mut cmd = command(binary, home, project, &endpoint);
    cmd.args(["hook", "Stop"]).env("REMNIFOLD_API_KEY", key);
    let output = process_output(
        cmd,
        &serde_json::to_vec(
            &json!({"session_id":session,"transcript_path":transcript,"cwd":project}),
        )?,
    )?;
    let captured = proxy
        .join()
        .map_err(|_| anyhow::anyhow!("short-key proxy thread"))??;
    ensure!(
        output.status.success() && output.stderr.is_empty(),
        "short-key capture failed"
    );
    let content = captured["content"]
        .as_str()
        .context("short-key capture content")?;
    let title = captured["metadata"]["title"]
        .as_str()
        .context("short-key capture title")?;
    ensure!(
        !content.contains(key)
            && !title.contains(key)
            && content.contains("preserved discussion")
            && content.contains("[REDACTED]"),
        "configured short key was not redacted"
    );
    let unsafe_body = format!("### User request\n{user}\n\n### Final assistant response\n{answer}");
    let unsafe_hash = crate::package::hash(unsafe_body.as_bytes());
    ensure!(
        captured["customId"]
            != format!(
                "codex-turn-v2:{session}:1:{unsafe_hash}:1:{}",
                &unsafe_hash[..16]
            ),
        "unsafe pre-redaction identity was retained"
    );
    Ok(())
}
fn disconnected_capture(
    binary: &Path,
    home: &Path,
    project: &Path,
    payload: &Value,
    caller: &str,
) -> Result<()> {
    use std::{
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let stopping = stop.clone();
    let recorded = requests.clone();
    let responder = thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(30);
        while !stopping.load(Ordering::Relaxed) && Instant::now() < end {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                        }
                        if bytes.len() > 2 * 1024 * 1024 {
                            break;
                        }
                        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.split_once(':')
                                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                if let Ok(body) =
                                    serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                {
                                    recorded.lock().unwrap().push(body);
                                }
                                break;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
    });
    let result = (|| -> Result<()> {
        for attempt in 1..=2 {
            let mut cmd = command(binary, home, project, &endpoint);
            if caller == "CLI" {
                cmd.arg("capture");
            } else {
                cmd.args(["hook", "Stop"]);
            }
            let output = process_output(cmd, &serde_json::to_vec(payload)?)?;
            ensure!(output.status.code()==Some(1)&&output.stdout.is_empty()&&output.stderr==b"Remnifold capture failed; unacknowledged records will be retried. [stage=transport; cause=request-failed; action=retry-or-repair]\n","disconnect diagnostic mismatch");
            ensure!(
                requests.lock().unwrap().len() == attempt,
                "network failure retried within one invocation"
            );
        }
        let records = requests.lock().unwrap();
        ensure!(
            records[0]["customId"].is_string() && records[0]["customId"] == records[1]["customId"],
            "disconnected capture changed retry identity"
        );
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = responder.join();
    result
}
pub fn run(repo: &Path, tools: &Toolchain, work: &Path, binary: &Path) -> Result<()> {
    safe_path(work)?;
    safe_path(binary)?;
    fs::create_dir_all(work)?;
    let invocation = work.join(format!("c-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&invocation)?;
    let work = invocation.as_path();
    println!("Native client validation directory: {}", work.display());
    subprocess_canaries(work)?;
    let mut fixture = start_fixture(repo, tools, &work.join("worker"))?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    let home = work.join("codex");
    let project = work.join("project");
    fs::create_dir_all(home.join("sessions"))?;
    fs::create_dir_all(&project)?;
    fs::write(
        home.join("remnifold.json"),
        serde_json::to_vec(
            &json!({"baseUrl":fixture.base_url,"apiKey":API_KEY,"maxRecentTasks":0}),
        )?,
    )?;
    let endpoint = &fixture.base_url;
    let mut checks = Vec::new();
    client
        .post(format!("{endpoint}/__configure"))
        .json(&json!({"AI_ENRICHMENT_MODE":"on"}))
        .send()?
        .error_for_status()?;
    ensure!(
        client
            .post(format!("{endpoint}/v3/documents/list"))
            .json(&json!({}))
            .send()?
            .status()
            == 401,
        "Worker accepted unauthenticated request"
    );
    checks.push("real Worker rejects unauthenticated requests");
    let mut mcp = Mcp::start(binary, &home, &project, endpoint)?;
    let initialized=mcp.exchange("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{"roots":{"listChanged":false}},"clientInfo":{"name":"native-worker-fixture","version":"1"}}))?;
    ensure!(
        initialized["serverInfo"]["name"] == "remnifold",
        "MCP initialization"
    );
    mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    let saved=mcp.tool("add_memory",json!({"content":"OrionNative migration checklist preserves Unicode 😀 and project provenance.","sourceFolder":project}))?;
    ensure!(
        saved["success"] == true && saved["id"].is_string(),
        "save failed: {saved}"
    );
    let id = saved["id"].as_str().unwrap();
    let found = mcp.tool(
        "search_memory",
        json!({"query":"OrionNative migration checklist","sourceFolder":project}),
    )?;
    ensure!(
        found["results"]
            .as_array()
            .context("search results")?
            .iter()
            .any(|v| v["id"] == id),
        "saved document missing from search: {found}"
    );
    let listed = mcp.tool("listDocuments", json!({"limit":10}))?;
    ensure!(
        listed["documents"]
            .as_array()
            .context("list documents")?
            .iter()
            .any(|v| v["id"] == id),
        "saved document missing from list"
    );
    let read = mcp.tool("getDocument", json!({"documentId":id}))?;
    ensure!(
        read.to_string().contains("OrionNative migration checklist"),
        "getDocument projection"
    );
    let provenance: Value = client
        .post(format!("{endpoint}/__sql"))
        .json(&json!({"sql":"SELECT metadata_json FROM memories WHERE id = ?","params":[id]}))
        .send()?
        .error_for_status()?
        .json()?;
    let metadata: Value = serde_json::from_str(
        provenance["results"][0]["metadata_json"]
            .as_str()
            .context("stored provenance")?,
    )?;
    ensure!(
        metadata["sm_scope"] == "project" && metadata["sm_project_id"].is_string(),
        "project provenance missing: {metadata}"
    );
    checks.push("native MCP save/search/list/get use production Worker with project provenance");
    let session = "native-capture-worker-session";
    let transcript = home.join("sessions/rollout-native-capture-worker-session.jsonl");
    let rows = [
        json!({"type":"session_meta","payload":{"id":session,"cwd":project,"timestamp":"2026-01-01T00:00:00Z"}}),
        json!({"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"user_message","message":format!("OrionNative capture request API_KEY='local-test-secret' {API_KEY} 😀\0")}}),
        json!({"type":"response_item","payload":{"role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"progress stays local"}]}}),
        json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"OrionNative finished the worker bridge and verified Unicode 😀."}]}}),
    ];
    fs::write(
        &transcript,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    let payload = json!({"session_id":session,"transcript_path":transcript,"cwd":project});
    let captured = hook(binary, &home, &project, endpoint, "Stop", payload.clone())?;
    ensure!(
        captured["systemMessage"]
            .as_str()
            .unwrap_or("")
            .contains("saved 1 documents; pending 0"),
        "capture failed: {captured}"
    );
    let state: Value = client
        .get(format!("{endpoint}/__state"))
        .send()?
        .error_for_status()?
        .json()?;
    let count_posts = |state: &Value| {
        state["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == "/v3/documents")
            .count()
    };
    let posts = count_posts(&state);
    hook(binary, &home, &project, endpoint, "Stop", payload)?;
    let retried: Value = client
        .get(format!("{endpoint}/__state"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        count_posts(&retried) == posts,
        "acknowledged capture resent"
    );
    let docs = mcp.tool("listDocuments", json!({"limit":50}))?;
    let entries = docs["documents"].as_array().context("captured documents")?;
    let capture = entries
        .iter()
        .find(|v| v["id"] != id)
        .context("capture document missing")?;
    let capture_id = capture["id"].as_str().context("capture id")?;
    let capture_read = mcp.tool("getDocument", json!({"documentId":capture_id}))?;
    let content = capture_read.to_string();
    ensure!(
        content.contains("😀") && content.contains("\\u0000") && content.contains("[REDACTED]"),
        "capture Unicode/NUL/redaction: {capture_read}"
    );
    ensure!(
        !content.contains(API_KEY)
            && !content.contains("local-test-secret")
            && !content.contains("progress stays local"),
        "capture disclosed secrets/commentary"
    );
    checks.push("native Stop captures final-only Unicode/NUL content, sanitizes keys, suppresses acknowledged retries");
    let recall = hook(
        binary,
        &home,
        &project,
        endpoint,
        "UserPromptSubmit",
        json!({"session_id":"native-other-session","cwd":project,"prompt":"OrionNative capture request worker bridge"}),
    )?;
    let context = recall["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("");
    ensure!(
        context.contains(capture_id)
            && context.chars().count() < 4000
            && !context.contains(API_KEY)
            && !context.contains("local-test-secret"),
        "automatic recall: {recall}"
    );
    checks
        .push("native automatic recall finds captured Worker index with bounded sanitized context");
    let forgotten = mcp.tool("add_memory", json!({"action":"forget","documentId":id}))?;
    ensure!(forgotten["success"] == true, "forget failed");
    let after = mcp.tool("listDocuments", json!({"limit":50}))?;
    ensure!(
        !after["documents"]
            .as_array()
            .context("list after forget")?
            .iter()
            .any(|v| v["id"] == id),
        "forgotten document still listed"
    );
    checks.push("native MCP forget removes document from production Worker indexes");
    mcp.close()?;
    client
        .get(format!("{endpoint}/__drain"))
        .send()?
        .error_for_status()?;
    let tombstone: Value = client
        .post(format!("{endpoint}/__sql"))
        .json(&json!({"sql":"SELECT is_forgotten FROM memories WHERE id = ?","params":[id]}))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        tombstone["results"][0]["is_forgotten"] == 1,
        "forget tombstone missing"
    );
    let state: Value = client
        .get(format!("{endpoint}/__state"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        !state["vectors"]
            .as_array()
            .context("fixture vectors")?
            .iter()
            .any(|v| v["id"] == id),
        "forgotten vector retained"
    );
    ensure!(
        state["metrics"]["embeddings"].as_u64().unwrap_or(0) > 0
            && state["metrics"]["vectorQueries"].as_u64().unwrap_or(0) > 0,
        "semantic fixture not exercised"
    );
    ensure!(
        state["requests"]
            .as_array()
            .context("baseline request log")?
            .iter()
            .all(|row| row["status"].as_u64().is_some_and(|status| status < 500)),
        "unexpected Worker HTTP 5xx response in baseline: {state}"
    );
    fs::write(
        work.join("native-client-validation.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":true,"runtime":"native Rust CLI + workerd","nodeOnPath":false,"checks":checks,"binary":binary,"metrics":state["metrics"],"requests":state["requests"]}),
        )?,
    )?;
    println!("Native client contracts: {} passed", checks.len());
    let recall = recall_contracts(binary, &home, &project, endpoint, &client, work);
    let diagnostics = diagnostic_contracts(binary, &project, endpoint, &client, work);
    recall?;
    diagnostics?;
    fixture.cleanup_state()?;
    Ok(())
}

fn diagnostic_contracts(
    binary: &Path,
    project: &Path,
    endpoint: &str,
    http: &reqwest::blocking::Client,
    work: &Path,
) -> Result<()> {
    http.get(format!("{endpoint}/__reset"))
        .send()?
        .error_for_status()?;
    let root = work.join("diagnostics");
    fs::create_dir_all(&root)?;
    let session = "native-diagnostics-session";
    let prepare = |name: &str| -> Result<(std::path::PathBuf, Value)> {
        let home = root.join(name);
        fs::create_dir_all(&home)?;
        fs::write(
            home.join("remnifold.json"),
            serde_json::to_vec(&json!({"baseUrl":endpoint,"apiKey":API_KEY,"maxRecentTasks":0}))?,
        )?;
        let transcript = home.join("fixture-sensitive-marker.jsonl");
        let rows = [
            json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
            json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":"Fixture user turn"}]}}),
            json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Fixture completed turn"}]}}),
        ];
        fs::write(
            &transcript,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
        Ok((
            home,
            json!({"session_id":session,"transcript_path":transcript,"cwd":project}),
        ))
    };
    let (probe, payload) = prepare("probe")?;
    hook(binary, &probe, project, endpoint, "Stop", payload)?;
    let receipt = fs::read_dir(probe.join("remnifold/capture-state"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .context("capture receipt probe")?
        .file_name()
        .unwrap()
        .to_owned();
    let mut checks = Vec::new();
    let mut failures = Vec::new();
    for caller in ["CLI", "Stop"] {
        for (name, stage, cause, action) in [
            (
                "configuration",
                "configuration",
                "invalid",
                "repair-required",
            ),
            ("transcript", "transcript", "unavailable", "repair-required"),
            ("receipt-malformed", "receipt", "invalid", "repair-required"),
            ("receipt-shape", "receipt", "invalid", "repair-required"),
            (
                "legacy-receipt-malformed",
                "receipt",
                "invalid",
                "repair-required",
            ),
            ("receipt-read", "receipt", "read-failed", "repair-required"),
            (
                "receipt-write",
                "receipt",
                "write-failed",
                "repair-required",
            ),
            (
                "http-failure",
                "transport",
                "request-failed",
                "retry-or-repair",
            ),
            (
                "disconnect",
                "transport",
                "request-failed",
                "retry-or-repair",
            ),
            (
                "acknowledgement",
                "transport",
                "invalid-acknowledgement",
                "retry-or-repair",
            ),
            ("success", "", "", ""),
        ] {
            let result = (|| -> Result<()> {
                let (home, mut payload) = prepare(&format!("{caller}-{name}"))?;
                if name == "disconnect" {
                    return disconnected_capture(binary, &home, project, &payload, caller);
                }
                http.post(format!("{endpoint}/__configure")).json(&json!({"captureFault":if ["http-failure","acknowledgement"].contains(&name){name}else{""}})).send()?.error_for_status()?;
                if name == "transcript" {
                    payload["session_id"] = json!("wrong-session");
                }
                if name.starts_with("receipt-") || name == "legacy-receipt-malformed" {
                    let dir = home.join(if name == "legacy-receipt-malformed" {
                        "cloudflare-memory/capture-state"
                    } else {
                        "remnifold/capture-state"
                    });
                    fs::create_dir_all(&dir)?;
                    match name {
                        "receipt-read" => fs::create_dir(dir.join(&receipt))?,
                        "receipt-write" => {
                            fs::create_dir(dir.join("capture-coordination.sqlite3"))?
                        }
                        "receipt-shape" => fs::write(
                            dir.join(&receipt),
                            b"{\"secret\":\"fixture-sensitive-marker\"}",
                        )?,
                        _ => fs::write(dir.join(&receipt), b"broken fixture-sensitive-marker")?,
                    }
                }
                let run = || -> Result<std::process::Output> {
                    let mut cmd = command(binary, &home, project, endpoint);
                    if caller == "CLI" {
                        cmd.arg("capture");
                    } else {
                        cmd.args(["hook", "Stop"]);
                    }
                    if name == "configuration" {
                        cmd.env_remove("REMNIFOLD_API_KEY");
                        let invalid = root.join(format!("{caller}-empty-config"));
                        fs::create_dir_all(&invalid)?;
                        cmd.env("CODEX_HOME", invalid);
                    }
                    process_output(cmd, &serde_json::to_vec(&payload)?)
                };
                let before: Value = http
                    .get(format!("{endpoint}/__state"))
                    .send()?
                    .error_for_status()?
                    .json()?;
                let count = |v: &Value| {
                    v["requests"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| r["path"] == "/v3/documents" && r["method"] == "POST")
                        .count()
                };
                for attempt in 0..2 {
                    let output = run()?;
                    if name == "success" {
                        ensure!(
                            output.status.success() && output.stderr.is_empty(),
                            "successful capture failed"
                        );
                    } else {
                        let expected = format!(
                            "Remnifold capture failed; unacknowledged records will be retried. [stage={stage}; cause={cause}; action={action}]\n"
                        );
                        ensure!(
                            output.status.code() == Some(1)
                                && output.stdout.is_empty()
                                && output.stderr == expected.as_bytes(),
                            "diagnostic mismatch {} {}",
                            output.status,
                            String::from_utf8_lossy(&output.stderr)
                        );
                        for secret in [
                            API_KEY,
                            "fixture-sensitive-marker",
                            home.to_str().unwrap(),
                            endpoint,
                        ] {
                            ensure!(
                                !String::from_utf8_lossy(&output.stderr).contains(secret),
                                "diagnostic leaked sensitive context"
                            );
                        }
                    }
                    let after: Value = http
                        .get(format!("{endpoint}/__state"))
                        .send()?
                        .error_for_status()?
                        .json()?;
                    let sent = count(&after) - count(&before);
                    let expected = if name == "success" {
                        1
                    } else if ["http-failure", "acknowledgement", "receipt-write"].contains(&name) {
                        attempt + 1
                    } else {
                        0
                    };
                    ensure!(
                        sent == expected,
                        "capture retry traffic {sent} != {expected}"
                    );
                }
                Ok(())
            })();
            match result {
                Ok(()) => checks.push(json!({"caller":caller,"scenario":name})),
                Err(error) => failures
                    .push(json!({"caller":caller,"scenario":name,"message":format!("{error:#}")})),
            }
        }
    }
    http.post(format!("{endpoint}/__configure"))
        .json(&json!({"captureFault":""}))
        .send()?
        .error_for_status()?;
    fs::write(
        work.join("capture-diagnostics-validation.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":failures.is_empty(),"runtime":"native capture/Stop + workerd","checks":checks,"failures":failures}),
        )?,
    )?;
    println!(
        "Native capture diagnostics: {} passed, {} failed",
        checks.len(),
        failures.len()
    );
    ensure!(
        failures.is_empty(),
        "native capture diagnostic failures; see capture-diagnostics-validation.json"
    );
    Ok(())
}

struct Recall<'a> {
    binary: &'a Path,
    home: &'a Path,
    project: &'a Path,
    endpoint: &'a str,
    http: &'a reqwest::blocking::Client,
    mcp: Mcp,
    ai: std::cell::Cell<bool>,
}
impl Recall<'_> {
    fn control(&self, path: &str, value: Option<Value>) -> Result<Value> {
        if path == "/__configure" {
            if let Some(mode) = value
                .as_ref()
                .and_then(|v| v["AI_ENRICHMENT_MODE"].as_str())
            {
                self.ai.set(mode == "on");
            }
        }
        let request = if let Some(value) = value {
            self.http
                .post(format!("{}{path}", self.endpoint))
                .json(&value)
        } else {
            self.http.get(format!("{}{path}", self.endpoint))
        };
        let body = request.send()?.error_for_status()?.text()?;
        Ok(serde_json::from_str(&body).unwrap_or(Value::String(body)))
    }
    fn reset(&self) -> Result<()> {
        self.control("/__reset", None)?;
        self.control("/__configure",Some(json!({"AI_ENRICHMENT_MODE":"on","checkpoint":{"title":"Orion checkpoint","overview":"Orion implementation state","verified":["Orion implemented"],"unverified":[],"unresolved":[],"nextActions":[]}})))?;
        self.settings(5, 0)?;
        Ok(())
    }
    fn settings(&self, count: u64, recent: u64) -> Result<()> {
        fs::write(
            self.home.join("remnifold.json"),
            serde_json::to_vec(
                &json!({"baseUrl":self.endpoint,"apiKey":API_KEY,"maxMemories":count,"maxRecentTasks":recent,"minimumSimilarity":0.7}),
            )?,
        )?;
        Ok(())
    }
    fn counts(&self) -> Result<Value> {
        self.control("/__state", None)
    }
    fn clear_counts(&self) -> Result<()> {
        self.control("/__reset_metrics", None)?;
        Ok(())
    }
    fn raw(&self, path: &str, body: Value, method: &str) -> Result<Value> {
        let response = self
            .http
            .request(method.parse()?, format!("{}{path}", self.endpoint))
            .bearer_auth(API_KEY)
            .json(&body)
            .send()?;
        let status = response.status();
        let value: Value = response.json()?;
        ensure!(
            status.is_success(),
            "fixture request {method} {path}: {status} {value}"
        );
        Ok(value)
    }
    fn sql(&self, sql: &str, params: Value) -> Result<Value> {
        self.control("/__sql", Some(json!({"sql":sql,"params":params})))
    }
    fn seed(&self, space: &str, content: &str, metadata: Value) -> Result<String> {
        let enabled = self.ai.get();
        self.control("/__configure", Some(json!({"AI_ENRICHMENT_MODE":"off"})))?;
        let result = self.raw(
            "/v3/documents",
            json!({"containerTag":space,"content":content,"metadata":metadata}),
            "POST",
        )?;
        self.control("/__drain", None)?;
        let id = result["id"].as_str().context("seed id")?.to_owned();
        let mut embedding = vec![0.0; 1024];
        embedding[0] = 1.0;
        self.sql("UPDATE memories SET embedding_json = ?, embedding_status = 'done', vector_status = 'indexed' WHERE id = ?",json!([serde_json::to_string(&embedding)?,id]))?;
        let revision = self.sql(
            "SELECT topic_revision FROM memories WHERE id = ?",
            json!([id]),
        )?;
        self.control("/__vectors",Some(json!({"upsert":[{"id":id,"values":embedding,"namespace":format!("ct_{}",&crate::package::hash(space.as_bytes())[..48]),"metadata":{"topic_revision":revision["results"][0]["topic_revision"]},"score":0.99}]})))?;
        self.control(
            "/__configure",
            Some(json!({"AI_ENRICHMENT_MODE":if enabled{"on"}else{"off"}})),
        )?;
        Ok(id)
    }
    fn index(title: &str, description: &str) -> Value {
        json!({"memoryIndex":{"version":1,"title":title,"description":description,"sections":[],"recallable":true,"sourceKind":"conversation","sourceUpdatedAt":"2025-03-04T12:00:00.000Z"}})
    }
    fn automatic(&self, prompt: &str) -> Result<String> {
        let value = hook(
            self.binary,
            self.home,
            self.project,
            self.endpoint,
            "UserPromptSubmit",
            json!({"session_id":uuid::Uuid::new_v4().to_string(),"cwd":self.project,"prompt":prompt}),
        )?;
        Ok(value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("")
            .to_owned())
    }
    fn search(&mut self, prompt: &str) -> Result<Value> {
        self.mcp.tool("search_memory", json!({"query":prompt}))
    }
    fn checkpoint(&mut self, project: &str, ids: &[String]) -> Result<String> {
        for id in ids {
            self.sql("UPDATE memories SET metadata_json = json_set(metadata_json, '$.sm_project_id', ?) WHERE id = ?",json!([project,id]))?;
        }
        let result = self
            .mcp
            .tool("consolidate_memory", json!({"projectId":project}))?;
        self.control("/__drain", None)?;
        ensure!(
            result["status"] == "consolidated",
            "checkpoint failed: {result}"
        );
        Ok(result["memoryId"]
            .as_str()
            .context("checkpoint id")?
            .to_owned())
    }
    fn traffic(&self, searches: u64) -> Result<()> {
        let state = self.counts()?;
        let requests = state["requests"].as_array().context("requests")?;
        ensure!(
            requests.len() as u64 == searches && requests.iter().all(|v| v["path"] == "/v4/search"),
            "unexpected hydration/discovery requests: {state}"
        );
        ensure!(
            state["metrics"]["embeddings"] == searches
                && state["metrics"]["vectorQueries"] == searches,
            "embedding/vector count: {state}"
        );
        Ok(())
    }
}
fn recall_contracts(
    binary: &Path,
    home: &Path,
    project: &Path,
    endpoint: &str,
    http: &reqwest::blocking::Client,
    work: &Path,
) -> Result<()> {
    let mut mcp = Mcp::start(binary, home, project, endpoint)?;
    mcp.exchange("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{"roots":{}},"clientInfo":{"name":"native-recall-contracts","version":"1"}}))?;
    mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    let mut f = Recall {
        binary,
        home,
        project,
        endpoint,
        http,
        mcp,
        ai: std::cell::Cell::new(true),
    };
    let mut checks = Vec::<String>::new();
    let mut failures = Vec::<Value>::new();
    macro_rules! case {($name:expr,$body:block)=>{{println!("Recall: {}",$name);f.reset()?;let result:Result<()>=(||$body)();match result{Ok(())=>checks.push($name.into()),Err(error)=>{eprintln!("{}: {error:#}",$name);failures.push(json!({"name":$name,"message":format!("{error:#}"),"state":f.counts()?}));}}fs::write(work.join("recall-efficiency-validation.json"),serde_json::to_vec_pretty(&json!({"passed":failures.is_empty(),"checks":checks,"failures":failures}))?)?;}};}
    case!(
        "project paths exclude account prefixes and generic repair noise",
        {
            let wanted = f.seed(
                "other-folder",
                "# Remnifold\n索引の読込",
                Recall::index("Remnifold", "索引の読込"),
            )?;
            let mut noise = Vec::new();
            for (title, request) in [
                ("Kagayoi.Support", r"C:\Users\IMT\dev\Kagayoi.Support"),
                ("請求書", "修正してください"),
                ("Release", "Developer release"),
            ] {
                noise.push(f.seed(
                    "other",
                    &format!("# {title}\n{request}"),
                    Recall::index(title, request),
                )?);
            }
            f.clear_counts()?;
            let text=f.automatic(r#""C:\Users\IMT\dev\remnifold" メモリー機能側で修正で最適化が出来そうな部分が発見できていたら修正してください"#)?;
            ensure!(
                text.contains(&wanted) && noise.iter().all(|id| !text.contains(id)),
                "path matching: {text}"
            );
            f.traffic(1)?;
            let state = f.counts()?;
            let q = state["requests"][0]["body"]["q"].as_str().unwrap_or("");
            ensure!(
                !q.contains("Users")
                    && !q.contains("IMT")
                    && !q.split_whitespace().any(|s| s == "dev"),
                "query leaked path noise: {q}"
            );
            Ok(())
        }
    );
    case!(
        "ASCII fragments and runtime versions retain automatic identifier boundaries",
        {
            let wanted = f.seed(
                "dev",
                "# Dev tools\nDevelopment tools",
                Recall::index("Dev tools", "Dev tools"),
            )?;
            let other = f.seed(
                "release",
                "# Developer release\nRelease history",
                Recall::index("Developer release", "Developer release"),
            )?;
            let text = f.automatic("dev")?;
            ensure!(
                text.contains(&wanted) && !text.contains(&other),
                "fragment match: {text}"
            );
            ensure!(
                f.search("dev")?.to_string().contains(&other),
                "manual semantic search removed"
            );
            let dotnet = f.seed(
                "dotnet",
                "# .NET8 runtime\nRuntime behavior",
                Recall::index(".NET8 runtime", "Runtime behavior"),
            )?;
            let node = f.seed(
                "node",
                "# Node.js24 runtime\nRuntime behavior",
                Recall::index("Node.js24 runtime", "Runtime behavior"),
            )?;
            let wrong = f.seed(
                "wrong",
                "# .NET80 runtime\nDifferent version",
                Recall::index(".NET80 runtime", "Different version"),
            )?;
            ensure!(
                f.automatic(".NET")?.contains(&dotnet) && f.automatic("Node.js")?.contains(&node),
                "runtime boundary missing"
            );
            let text = f.automatic(".NET8")?;
            ensure!(
                text.contains(&dotnet) && !text.contains(&wrong),
                "runtime version boundary"
            );
            Ok(())
        }
    );
    case!(
        "file paths retain project names and discard unrelated README/code names",
        {
            let wanted = f.seed(
                "project",
                "# Remnifold\n索引の読込",
                Recall::index("Remnifold", "索引の読込"),
            )?;
            let other = f.seed(
                "readme",
                "# README.md\nDocumentation changes",
                Recall::index("README.md", "Documentation changes"),
            )?;
            let text = f.automatic(
                r#""C:\Users\IMT\dev\remnifold\client\memory-client.mjs" を修正してください"#,
            )?;
            ensure!(
                text.contains(&wanted) && !text.contains(&other),
                "file path recall"
            );
            let support = f.seed(
                "support",
                "# Kagayoi.Support\nSupport service",
                Recall::index("Kagayoi.Support", "Support service"),
            )?;
            let code = f.seed(
                "code",
                "# Code\nCode samples",
                Recall::index("Code", "Code samples"),
            )?;
            for query in [
                r#""C:\Code\Kagayoi.Support""#,
                r#""C:\Code\Kagayoi.Support\""#,
                r#""/opt/Kagayoi.Support""#,
            ] {
                let text = f.automatic(query)?;
                ensure!(
                    text.contains(&support) && !text.contains(&code),
                    "dotted project path: {query} {text}"
                );
            }
            Ok(())
        }
    );
    case!(
        "legacy hydration selects matching heading and preserves source provenance",
        {
            let wanted=f.seed("legacy","## Gardening\nFlower care\n\n## Remnifold\n索引の読込",json!({"sm_project_id":"legacy-source","sourceTimestamp":"2025-03-04T12:00:00.000Z"}))?;
            f.control("/__configure", Some(json!({"legacy400":true})))?;
            f.clear_counts()?;
            let text = f.automatic("remnifold")?;
            ensure!(
                text.contains(&wanted) && !text.contains("Flower"),
                "legacy selected heading: {text}"
            );
            let state = f.counts()?;
            ensure!(
                state["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["path"] == format!("/v3/documents/{wanted}")),
                "legacy hydration missing"
            );
            Ok(())
        }
    );
    case!(
        "generic repair remains manually searchable and makes no topic request",
        {
            let wanted = f.seed(
                "generic",
                "# 最適化\n最適化",
                Recall::index("最適化", "最適化"),
            )?;
            f.clear_counts()?;
            ensure!(
                f.automatic("修正して最適化してください")?.is_empty(),
                "generic topic was recalled"
            );
            ensure!(
                f.counts()?["requests"].as_array().unwrap().is_empty(),
                "generic topic made request"
            );
            ensure!(
                f.search("最適化")?.to_string().contains(&wanted),
                "generic manual search"
            );
            Ok(())
        }
    );
    case!(
        "legacy settings recover from malformed current config without exposing input",
        {
            let isolated = work.join("legacy-settings");
            fs::create_dir_all(&isolated)?;
            fs::write(
                isolated.join("remnifold.json"),
                b"malformed fixture-sensitive-marker",
            )?;
            fs::write(
                isolated.join("kagayoi-memory.json"),
                serde_json::to_vec(
                    &json!({"baseUrl":endpoint,"apiKey":API_KEY,"recallMode":"off","maxMemories":5,"maxRecentTasks":0}),
                )?,
            )?;
            f.clear_counts()?;
            let result = hook(
                binary,
                &isolated,
                project,
                endpoint,
                "UserPromptSubmit",
                json!({"session_id":"legacy-settings","cwd":project,"prompt":"SettingsNeedle"}),
            )?;
            ensure!(
                result == json!({}) && f.counts()?["requests"].as_array().unwrap().is_empty(),
                "legacy off setting not recovered"
            );
            let mut cmd = command(binary, &isolated, project, endpoint);
            cmd.arg("status");
            let result = process_output(cmd, b"")?;
            ensure!(
                result.status.success()
                    && !String::from_utf8_lossy(&result.stdout).contains(API_KEY)
                    && !String::from_utf8_lossy(&result.stdout)
                        .contains("fixture-sensitive-marker")
                    && result.stderr.is_empty(),
                "legacy status leaked/failed"
            );
            Ok(())
        }
    );
    case!(
        "native setup saves default and explicit recall settings through authenticated Worker",
        {
            for (name, extra, memories, recent) in [
                ("default", Vec::<&str>::new(), 3, 2),
                (
                    "explicit",
                    vec!["--max-memories", "5", "--max-recent-tasks", "3"],
                    5,
                    3,
                ),
            ] {
                let isolated = work.join(format!("setup-{name}"));
                fs::create_dir_all(&isolated)?;
                let mut cmd = command(binary, &isolated, project, endpoint);
                cmd.args(["setup", "--base-url", endpoint, "--codex-home"])
                    .arg(&isolated)
                    .arg("--api-key-stdin")
                    .args(extra);
                let result = process_output(cmd, format!("{API_KEY}\n").as_bytes())?;
                ensure!(
                    result.status.success() && result.stderr.is_empty(),
                    "native setup failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let saved: Value =
                    serde_json::from_slice(&fs::read(isolated.join("remnifold.json"))?)?;
                ensure!(
                    saved["baseUrl"] == endpoint
                        && saved["apiKey"] == API_KEY
                        && saved["maxMemories"] == memories
                        && saved["maxRecentTasks"].as_u64().unwrap_or(2) == recent,
                    "saved setup settings: {name}"
                );
                ensure!(
                    !String::from_utf8_lossy(&result.stdout).contains(API_KEY),
                    "setup exposed key"
                );
            }
            Ok(())
        }
    );
    case!(
        "native permission and unknown hooks require no Node and make no HTTP request",
        {
            f.clear_counts()?;
            let input =
                json!({"tool_name":"mcp__remnifold__getDocument","tool_input":{"id":"fixture"}});
            let allowed = hook(binary, home, project, endpoint, "PreToolUse", input)?;
            ensure!(
                allowed["hookSpecificOutput"]["permissionDecision"] == "allow"
                    && allowed["hookSpecificOutput"]["updatedInput"]["id"] == "fixture",
                "read permission contract"
            );
            ensure!(
                hook(
                    binary,
                    home,
                    project,
                    endpoint,
                    "PreToolUse",
                    json!({"tool_name":"mcp__remnifold__deleteDocument"})
                )? == json!({}),
                "write tool granted automatic permission"
            );
            ensure!(
                hook(binary, home, project, endpoint, "Unknown", json!({}))? == json!({}),
                "unknown hook emitted output"
            );
            ensure!(
                f.counts()?["requests"].as_array().unwrap().is_empty(),
                "permission/unknown hook touched network"
            );
            Ok(())
        }
    );
    case!(
        "59 legacy spaces use one search embedding/vector without discovery or hydration",
        {
            let wanted=f.seed("space-58","# AbortSignal timeout\n\nAbortSignal timeout implementation evidence",json!({"sm_project_id":"legacy-project","sourceTimestamp":"2025-03-04T12:00:00.000Z"}))?;
            for i in 0..58 {
                f.seed(
                    &format!("space-{i}"),
                    &format!("# Gardening {i}\nUnrelated flowers"),
                    json!({}),
                )?;
            }
            f.clear_counts()?;
            ensure!(
                f.automatic("AbortSignal timeout")?.contains(&wanted),
                "legacy across 59 spaces"
            );
            f.traffic(1)?;
            f.clear_counts()?;
            ensure!(
                f.search("AbortSignal timeout")?
                    .to_string()
                    .contains(&wanted),
                "MCP across 59 spaces"
            );
            f.traffic(1)?;
            Ok(())
        }
    );
    case!(
        "101 spaces include oldest tail beyond legacy discovery cap",
        {
            let wanted = f.seed(
                "oldest-tail",
                "# NeedleTail\nNeedleTail historical evidence",
                json!({}),
            )?;
            for i in 0..100 {
                f.seed(
                    &format!("recent-{i}"),
                    &format!("# Other {i}\nUnrelated archive"),
                    json!({}),
                )?;
            }
            f.sql("UPDATE container_tags SET updated_at = '2000-01-01T00:00:00.000Z' WHERE tag = 'oldest-tail'",json!([]))?;
            f.clear_counts()?;
            let result = f.search("NeedleTail")?;
            ensure!(
                result.to_string().contains(&wanted)
                    && result["searchScope"] == "all-containers"
                    && result["spaceDiscoveryComplete"] == true
                    && result["searchedContainers"].as_array().unwrap().len() == 101,
                "global tail: {result}"
            );
            f.traffic(1)?;
            Ok(())
        }
    );
    case!(
        "explicit container retains scoped search and excludes other spaces",
        {
            let wanted = f.seed(
                "chosen",
                "# ScopeNeedle\nScopeNeedle allowed",
                Recall::index("ScopeNeedle", "ScopeNeedle"),
            )?;
            let other = f.seed(
                "elsewhere",
                "# ScopeNeedle\nScopeNeedle elsewhere",
                Recall::index("ScopeNeedle", "ScopeNeedle"),
            )?;
            let result = f.mcp.tool(
                "search_memory",
                json!({"query":"ScopeNeedle","containerTag":"chosen"}),
            )?;
            ensure!(
                result.to_string().contains(&wanted) && !result.to_string().contains(&other),
                "scoped native search: {result}"
            );
            Ok(())
        }
    );
    case!(
        "manual project provenance and recency rank before the candidate hydration cap",
        {
            for i in 0..25 {
                let mut metadata =
                    Recall::index("ReplaceTranslator release", "ReplaceTranslator release");
                metadata["sm_project_id"] = json!("repo_replacetranslator__1234567890abcdef");
                metadata["project"] = json!("ReplaceTranslator");
                f.seed(
                    "memories",
                    &format!("# ReplaceTranslator release {i}\nPublished old release"),
                    metadata,
                )?;
            }
            let mut metadata = Recall::index("依存更新", "依存更新 ESLint Puppeteer");
            metadata["project"] = json!("ReplaceTranslator");
            metadata["sm_project_id"] = json!("repo_replacetranslator__1234567890abcdef");
            metadata["memoryIndex"]["sourceUpdatedAt"] = json!("2026-10-21T00:00:00.000Z");
            let recent = f.seed("memories", "# 依存更新\nESLint Puppeteer", metadata)?;
            f.settings(3, 0)?;
            let manual = f.search("ReplaceTranslator")?;
            ensure!(
                manual["results"][0]["id"] == recent,
                "manual project newest candidate lost before cap: {manual}"
            );
            let combined = f.search("ReplaceTranslator 依存更新 ESLint Puppeteer")?;
            ensure!(
                combined["results"][0]["id"] == recent,
                "combined project/topic rank: {combined}"
            );
            let automatic = f.automatic("ReplaceTranslator")?;
            ensure!(
                !automatic.contains(&recent),
                "manual project provenance contaminated automatic topic recall"
            );
            Ok(())
        }
    );
    case!(
        "manual project chronology compares offsets, stable ties, and saved-date fallback",
        {
            let mut ids = Vec::new();
            for (name, date) in [
                ("old", Some("2026-10-31T09:00:00+09:00")),
                ("new", Some("2026-10-31T01:00:00Z")),
                ("tie", Some("2026-10-31T10:00:00+09:00")),
                ("fallback", None),
            ] {
                let mut metadata = Recall::index("release", "release");
                metadata["project"] = json!("ReplaceTranslator");
                if let Some(date) = date {
                    metadata["memoryIndex"]["sourceUpdatedAt"] = json!(date);
                } else {
                    metadata["memoryIndex"]
                        .as_object_mut()
                        .unwrap()
                        .remove("sourceUpdatedAt");
                }
                let id = f.seed("memories", &format!("# release {name}\nEvidence"), metadata)?;
                if date.is_none() {
                    f.sql(
                        "UPDATE memories SET updated_at = '2026-10-31T00:30:00Z' WHERE id = ?",
                        json!([id]),
                    )?;
                }
                ids.push(id);
            }
            f.settings(5, 0)?;
            let result = f.search("ReplaceTranslator")?;
            let actual = result["results"]
                .as_array()
                .context("project chronology")?
                .iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>();
            ensure!(
                actual.len() == 4
                    && actual[0..2].contains(&ids[1].as_str())
                    && actual[0..2].contains(&ids[2].as_str())
                    && actual[2] == ids[3]
                    && actual[3] == ids[0],
                "project timezone/fallback ordering: {result}"
            );
            Ok(())
        }
    );
    case!(
        "Vectorize failure preserves D1 fallback and AI failure preserves lexical search",
        {
            let wanted = f.seed(
                "failure",
                "# ResilienceNeedle\nResilienceNeedle evidence",
                Recall::index("ResilienceNeedle", "ResilienceNeedle evidence"),
            )?;
            f.control("/__configure", Some(json!({"failVector":true})))?;
            let semantic = f.search("DifferentSemanticQuery")?;
            ensure!(semantic.to_string().contains(&wanted), "D1 vector fallback");
            f.control(
                "/__configure",
                Some(json!({"failVector":false,"failAI":true})),
            )?;
            ensure!(
                f.automatic("ResilienceNeedle")?.contains(&wanted),
                "AI failure lost lexical result"
            );
            Ok(())
        }
    );
    case!(
        "native searches reject late stale vectors for forgotten and revised documents",
        {
            let forgotten = f.seed(
                "memories",
                "# ObsoleteNeedle\nObsoleteNeedle",
                Recall::index("ObsoleteNeedle", "ObsoleteNeedle"),
            )?;
            let revised = f.seed(
                "memories",
                "# ObsoleteNeedle\nObsoleteNeedle",
                Recall::index("ObsoleteNeedle", "ObsoleteNeedle"),
            )?;
            let vectors = f.counts()?["vectors"]
                .as_array()
                .context("stale vector fixtures")?
                .clone();
            let removed = f.mcp.tool(
                "add_memory",
                json!({"action":"forget","documentId":forgotten}),
            )?;
            ensure!(removed["success"] == true, "stale vector forget failed");
            f.raw(&format!("/v3/documents/{revised}"),json!({"content":"# Replacement\nEntirely different evidence","metadata":Recall::index("Replacement","Replacement")}),"PATCH")?;
            f.control("/__drain", None)?;
            let mut opposite = vec![0.0; 1024];
            opposite[0] = -1.0;
            f.sql(
                "UPDATE memories SET embedding_json = ?, vector_status = 'indexed' WHERE id = ?",
                json!([serde_json::to_string(&opposite)?, revised]),
            )?;
            f.control("/__vectors", Some(json!({"upsert":vectors})))?;
            let result = f.search("ObsoleteNeedle")?;
            let rows = result["results"].as_array().context("stale results")?;
            ensure!(
                rows.iter()
                    .all(|row| row["id"] != forgotten && row["id"] != revised),
                "late stale vectors resurrected deleted/revised documents: {result}"
            );
            Ok(())
        }
    );
    case!(
        "body-only evidence supplies bounded original excerpts without unrelated automatic records",
        {
            let wanted=f.seed("answers",&format!("# Request transport\n### User request\nImprove request reliability\n### Final assistant response\n{}Use AbortSignal.timeout to bound fetch execution.","Background detail. ".repeat(50)),Recall::index("Request transport","Improve request reliability"))?;
            let other = f.seed(
                "answers",
                "# Gardening\nFlower arrangement",
                Recall::index("Gardening", "Flower arrangement"),
            )?;
            f.clear_counts()?;
            let text = f.automatic("AbortSignal.timeout")?;
            ensure!(
                text.contains(&wanted)
                    && text.contains("AbortSignal.timeout")
                    && !text.contains(&other)
                    && (text.contains("原文") || text.contains("original")),
                "body excerpt: {text}"
            );
            f.traffic(1)?;
            let manual = f.search("AbortSignal.timeout")?;
            ensure!(
                manual.to_string().contains(&other)
                    && manual.to_string().contains("AbortSignal.timeout"),
                "manual semantic/body excerpt"
            );
            let response = f.mcp.exchange(
                "tools/call",
                json!({"name":"search_memory","arguments":{"query":"AbortSignal.timeout"}}),
            )?;
            let display = response["content"][0]["text"]
                .as_str()
                .context("manual MCP excerpt text")?;
            ensure!(
                display.contains("AbortSignal.timeout")
                    && (display.contains("原文") || display.contains("original")),
                "MCP display lost original query excerpt: {display}"
            );
            Ok(())
        }
    );
    case!(
        "recallable false remains manually discoverable without automatic injection",
        {
            let mut metadata = Recall::index("AcknowledgementNeedle", "AcknowledgementNeedle");
            metadata["memoryIndex"]["recallable"] = json!(false);
            let id = f.seed(
                "ack",
                "# AcknowledgementNeedle\nAcknowledgementNeedle",
                metadata,
            )?;
            ensure!(
                f.search("AcknowledgementNeedle")?.to_string().contains(&id),
                "manual false recallable missing"
            );
            ensure!(
                f.automatic("AcknowledgementNeedle")?.is_empty(),
                "false recallable injected"
            );
            Ok(())
        }
    );
    case!(
        "recent work crosses project boundaries and topic/recent suppression remains independent",
        {
            let mut metadata = Recall::index("Orchard pruning", "Fruit arrangement completed");
            metadata["type"] = json!("conversation");
            metadata["sessionId"] = json!("completed-orchard");
            metadata["captureVersion"] = json!(2);
            metadata["turn"] = json!(1);
            metadata["part"] = json!(1);
            metadata["parts"] = json!(1);
            metadata["sm_project_id"] = json!("other-project");
            metadata["sourceTimestamp"] = json!("2026-10-22T10:00:00Z");
            let id = f.seed(
                "unrelated-space",
                "# Orchard pruning\nFruit arrangement completed",
                metadata,
            )?;
            f.settings(3, 2)?;
            let session = format!("recent-state-{}", uuid::Uuid::new_v4());
            let transcript = home.join("sessions").join(format!("{session}.jsonl"));
            fs::write(
                &transcript,
                format!(
                    "{}\n",
                    json!({"type":"session_meta","payload":{"id":session,"cwd":project}})
                ),
            )?;
            let payload = |prompt: &str| json!({"session_id":session,"transcript_path":transcript,"cwd":project,"prompt":prompt});
            f.clear_counts()?;
            let first = hook(
                binary,
                home,
                project,
                endpoint,
                "UserPromptSubmit",
                payload("修正してください"),
            )?;
            let text = first["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or("");
            ensure!(
                text.contains(&id),
                "topic-less recent work missing: {first}"
            );
            let state = f.counts()?;
            let rows = state["requests"].as_array().unwrap();
            ensure!(
                rows.len() == 1
                    && rows[0]["body"]["recentTasks"] == true
                    && rows[0]["body"]["projection"] == "index"
                    && rows[0]["body"]["excludeSessionId"] == session,
                "recent request shape: {state}"
            );
            let topic = hook(
                binary,
                home,
                project,
                endpoint,
                "UserPromptSubmit",
                payload("Orchard"),
            )?;
            let text = topic["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or("");
            ensure!(
                text.contains(&id) && text.matches(&id).count() == 1,
                "recent visibility suppressed later topic or duplicated record: {topic}"
            );
            ensure!(
                hook(
                    binary,
                    home,
                    project,
                    endpoint,
                    "UserPromptSubmit",
                    payload("Orchard")
                )? == json!({}),
                "seen topic/recent record injected twice"
            );
            let mut file = fs::OpenOptions::new().append(true).open(&transcript)?;
            writeln!(
                file,
                "{}",
                json!({"type":"event_msg","payload":{"type":"context_compacted"}})
            )?;
            drop(file);
            let restored = hook(
                binary,
                home,
                project,
                endpoint,
                "UserPromptSubmit",
                payload("Orchard"),
            )?;
            ensure!(
                restored.to_string().contains(&id),
                "compaction failed to restore recent/topic"
            );
            f.clear_counts()?;
            for prompt in ["", "/help", "/rere"] {
                ensure!(
                    hook(
                        binary,
                        home,
                        project,
                        endpoint,
                        "UserPromptSubmit",
                        payload(prompt)
                    )? == json!({}),
                    "slash/empty prompt received recent context"
                );
            }
            ensure!(
                f.counts()?["requests"].as_array().unwrap().is_empty(),
                "slash/empty prompt requested recent work"
            );
            Ok(())
        }
    );
    case!(
        "legacy and failing recent responses preserve the healthy topic channel without marking unseen work",
        {
            let topic = f.seed(
                "topics",
                "# ChannelNeedle\nChannelNeedle evidence",
                Recall::index("ChannelNeedle", "ChannelNeedle evidence"),
            )?;
            let mut metadata =
                Recall::index("Unrelated completed task", "Completed orchard layout");
            metadata["type"] = json!("conversation");
            metadata["sessionId"] = json!("channel-recent-task");
            metadata["captureVersion"] = json!(2);
            metadata["turn"] = json!(1);
            metadata["part"] = json!(1);
            metadata["parts"] = json!(1);
            metadata["sourceTimestamp"] = json!("2026-10-22T10:00:00Z");
            let recent = f.seed(
                "other-project",
                "# Unrelated completed task\nCompleted orchard layout",
                metadata,
            )?;
            f.settings(3, 2)?;
            for (name, config) in [
                ("legacy", json!({"recentMarkerless":true})),
                ("recent-failure", json!({"recentStatus":500})),
                ("topic-failure", json!({"transportStatus":500})),
            ] {
                let session = format!("channel-{name}-{}", uuid::Uuid::new_v4());
                let transcript = home.join("sessions").join(format!("{session}.jsonl"));
                fs::write(
                    &transcript,
                    format!(
                        "{}\n",
                        json!({"type":"session_meta","payload":{"id":session,"cwd":project}})
                    ),
                )?;
                let payload = json!({"session_id":session,"transcript_path":transcript,"cwd":project,"prompt":"ChannelNeedle"});
                f.control("/__configure", Some(config))?;
                let first = hook(
                    binary,
                    home,
                    project,
                    endpoint,
                    "UserPromptSubmit",
                    payload.clone(),
                )?;
                let text = first["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap_or("");
                if name == "topic-failure" {
                    ensure!(
                        text.contains(&recent) && !text.contains(&topic),
                        "topic failure lost healthy recent channel: {first}"
                    );
                } else {
                    ensure!(
                        text.contains(&topic) && !text.contains(&recent),
                        "legacy/failing recent lost topic or injected unmarked legacy work: {first}"
                    );
                }
                ensure!(
                    !first.to_string().contains(API_KEY)
                        && !first.to_string().contains("injected-"),
                    "partial failure leaked exception/key"
                );
                f.control(
                    "/__configure",
                    Some(json!({"recentMarkerless":false,"recentStatus":0,"transportStatus":0})),
                )?;
                let retry = hook(binary, home, project, endpoint, "UserPromptSubmit", payload)?;
                let text = retry["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap_or("");
                if name == "topic-failure" {
                    ensure!(
                        text.contains(&topic) && !text.contains(&recent),
                        "failed topic was marked seen or healthy recent repeated: {retry}"
                    );
                } else {
                    ensure!(
                        text.contains(&recent) && !text.contains(&topic),
                        "failed/legacy recent was marked seen or healthy topic repeated: {retry}"
                    );
                }
            }
            Ok(())
        }
    );
    case!(
        "actual hook recovers user topics across large tool rows and UTF-8 chunk boundaries",
        {
            let topic = "Realtime 音声翻訳";
            let wanted = f.seed(
                "history",
                "# Realtime 音声翻訳\nTranslation routing",
                Recall::index(topic, topic),
            )?;
            let session = format!("chunk-history-{}", uuid::Uuid::new_v4());
            let transcript = home.join("sessions").join(format!("{session}.jsonl"));
            let user = |message: String| json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":message}]}});
            let rows=[json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),user(format!("{topic}{}"," 語😀".repeat(20000))),json!({"type":"response_item","payload":{"role":"tool","content":"Kubernetes".repeat(130000)}}),user("<skill>\n<name>fixture</name>\n<path>C:/skills/fixture/SKILL.md</path>\nKubernetes\n</skill>".into()),user("[$Kubernetes](thread://other-task)".into()),user("Message Type: MESSAGE\nKubernetes".into()),user("<subagent_notification>Kubernetes</subagent_notification>".into()),user("お願い".into())];
            let mut file = fs::File::create(&transcript)?;
            for row in rows {
                writeln!(file, "{row}")?;
            }
            write!(file, "{}", user("Kubernetes incomplete-tail-canary".into()))?;
            drop(file);
            let mut bytes = fs::read(&transcript)?;
            let straddles = |bytes: &[u8]| {
                (1..=bytes.len() / (128 * 1024))
                    .any(|n| bytes[bytes.len() - n * 128 * 1024] & 0xc0 == 0x80)
            };
            let mut padding = 0;
            while !straddles(&bytes) && padding < 8 {
                bytes.push(b' ');
                padding += 1;
            }
            ensure!(
                straddles(&bytes),
                "UTF-8 fixture does not straddle a real 128KiB read boundary"
            );
            if padding > 0 {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&transcript)?
                    .write_all(&vec![b' '; padding])?;
            }
            let payload = |id: &str| json!({"session_id":id,"transcript_path":transcript,"cwd":project,"prompt":"お願い"});
            f.clear_counts()?;
            let result = hook(
                binary,
                home,
                project,
                endpoint,
                "UserPromptSubmit",
                payload(&session),
            )?;
            ensure!(
                result.to_string().contains(&wanted),
                "large transcript topic recovery failed: {result}"
            );
            let state = f.counts()?;
            let q = state["requests"][0]["body"]["q"]
                .as_str()
                .context("recovered topic query")?;
            ensure!(
                q.contains(topic) && !q.contains("Kubernetes") && !q.contains('\u{fffd}'),
                "foreign/tool/incomplete/UTF8 topic leakage: {q}"
            );
            f.clear_counts()?;
            ensure!(
                hook(
                    binary,
                    home,
                    project,
                    endpoint,
                    "UserPromptSubmit",
                    payload("wrong-session")
                )? == json!({})
                    && f.counts()?["requests"].as_array().unwrap().is_empty(),
                "foreign transcript session searched"
            );
            Ok(())
        }
    );
    case!(
        "actual hook enforces history byte, row, complete-line and user-message bounds",
        {
            f.seed(
                "history",
                "# SnowRunner truck settings\nSuspension",
                Recall::index("SnowRunner truck settings", "Suspension"),
            )?;
            let user = |message: String| json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":message}]}});
            for name in [
                "beyond-byte-bound",
                "oversized-user-row",
                "incomplete-user-row",
                "beyond-user-bound",
            ] {
                let session = format!("{name}-{}", uuid::Uuid::new_v4());
                let transcript = home.join("sessions").join(format!("{session}.jsonl"));
                let mut file = fs::File::create(&transcript)?;
                writeln!(
                    file,
                    "{}",
                    json!({"type":"session_meta","payload":{"id":session,"cwd":project}})
                )?;
                match name {
                    "beyond-byte-bound" => {
                        writeln!(file, "{}", user("SnowRunner truck settings".into()))?;
                        writeln!(
                            file,
                            "{}",
                            json!({"type":"response_item","payload":{"role":"tool","content":"x".repeat(9*1024*1024)}})
                        )?;
                        writeln!(file, "{}", user("お願い".into()))?;
                    }
                    "oversized-user-row" => {
                        writeln!(
                            file,
                            "{}",
                            user(format!("SnowRunner {}", "x".repeat(300 * 1024)))
                        )?;
                        writeln!(file, "{}", user("お願い".into()))?;
                    }
                    "incomplete-user-row" => {
                        writeln!(file, "{}", user("お願い".into()))?;
                        write!(file, "{}", user("SnowRunner truck settings".into()))?;
                    }
                    _ => {
                        writeln!(file, "{}", user("SnowRunner truck settings".into()))?;
                        for _ in 0..17 {
                            writeln!(file, "{}", user("お願い".into()))?;
                        }
                    }
                }
                drop(file);
                f.clear_counts()?;
                let result = hook(
                    binary,
                    home,
                    project,
                    endpoint,
                    "UserPromptSubmit",
                    json!({"session_id":session,"transcript_path":transcript,"cwd":project,"prompt":"お願い"}),
                )?;
                ensure!(
                    result == json!({}) && f.counts()?["requests"].as_array().unwrap().is_empty(),
                    "out-of-bound topic recovered ({name}): {result}"
                );
            }
            Ok(())
        }
    );
    case!(
        "legacy metadata derives matching heading/date/provenance without persisting repairs",
        {
            let content = "# Codex saved memory import: archive\nupdated_at: 2025-03-04T12:00:00.000Z\n## Gardening\nFlower care\n## Needle transport\nNeedle transport query-specific evidence";
            let id = f.seed(
                "legacy",
                content,
                json!({"sm_project_id":"legacy-source","filepath":"rollout_summaries/example.md"}),
            )?;
            let before = f.sql(
                "SELECT metadata_json FROM memories WHERE id = ?",
                json!([id]),
            )?;
            f.clear_counts()?;
            let text = f.automatic("Needle transport")?;
            ensure!(
                text.contains(&id) && text.contains("Needle transport"),
                "legacy matching heading"
            );
            ensure!(
                f.sql(
                    "SELECT metadata_json FROM memories WHERE id = ?",
                    json!([id])
                )? == before,
                "legacy repair persisted"
            );
            let original = f.mcp.tool("getDocument", json!({"documentId":id}))?;
            ensure!(
                original["document"]["content"] == content,
                "legacy original changed"
            );
            Ok(())
        }
    );
    case!(
        "legacy index lists compact authoritative empty topics and original provenance",
        {
            let id=f.seed("legacy-list","# 旧文書\n一覧だけで確認できる要点",json!({"sm_project_id":"legacy-list-source","filepath":"archive/source.md","sourceTimestamp":"2025-03-04T12:00:00.000Z"}))?;
            f.sql("DELETE FROM memory_topics WHERE memory_id = ?", json!([id]))?;
            let result = f
                .mcp
                .tool("listDocuments", json!({"containerTag":"legacy-list"}))?;
            let row = result["documents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .context("listed legacy")?;
            ensure!(
                row["title"] == "旧文書"
                    && row["topics"] == json!([])
                    && row.get("content").is_none(),
                "legacy compact list: {row}"
            );
            let original = f.mcp.tool("getDocument", json!({"documentId":id}))?;
            ensure!(
                original["document"]["provenance"]["projectId"] == "legacy-list-source"
                    && original["document"]["provenance"]["filepath"] == "archive/source.md",
                "legacy original provenance: {original}"
            );
            Ok(())
        }
    );
    case!(
        "brief continuation captures retain deterministic sanitized identities, sections and source dates",
        {
            let session = format!("continuation-{}", uuid::Uuid::new_v4());
            let transcript = home.join("sessions").join(format!("{session}.jsonl"));
            let answer = format!(
                "## timeoutの修正\nAbortSignal.timeoutで取得期限を設定しました。設定値 {API_KEY} は秘匿します。\n## 検証\n実際のAPI経路で期限超過を確認しました。"
            );
            let clean = answer.replace(API_KEY, "[REDACTED]");
            let date = "2025-03-04T12:00:00.000Z";
            let rows = [
                json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
                json!({"type":"event_msg","timestamp":date,"payload":{"type":"user_message","message":"続けて"}}),
                json!({"type":"response_item","timestamp":date,"payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}}),
            ];
            fs::write(
                &transcript,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
            let payload = json!({"session_id":session,"transcript_path":transcript,"cwd":project});
            f.clear_counts()?;
            hook(binary, home, project, endpoint, "Stop", payload.clone())?;
            hook(binary, home, project, endpoint, "Stop", payload)?;
            let state = f.counts()?;
            let captures = state["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["path"] == "/v3/documents" && r["method"] == "POST")
                .collect::<Vec<_>>();
            ensure!(
                captures.len() == 1,
                "continuation capture acknowledgement did not suppress retry"
            );
            let doc = &captures[0]["body"];
            let body = format!("### User request\n続けて\n\n### Final assistant response\n{clean}");
            let digest = crate::package::hash(body.as_bytes());
            ensure!(
                doc["customId"]
                    == format!("codex-turn-v2:{session}:1:{digest}:1:{}", &digest[..16])
                    && doc["metadata"]["memoryIndex"]["recallable"] == true
                    && doc["metadata"]["memoryIndex"]["description"]
                        .as_str()
                        .unwrap_or("")
                        .contains("継続結果")
                    && doc["metadata"]["memoryIndex"]["sections"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("timeoutの修正")),
                "continuation identity/index: {doc}"
            );
            let stored = f.sql(
                "SELECT id FROM memories WHERE custom_id = ?",
                json!([doc["customId"]]),
            )?;
            let id = stored["results"][0]["id"]
                .as_str()
                .context("continuation saved id")?;
            let read = f.mcp.tool("getDocument", json!({"documentId":id}))?;
            ensure!(
                read["document"]["index"]["sourceUpdatedAt"] == date
                    && !read.to_string().contains(API_KEY)
                    && read.to_string().contains("[REDACTED]"),
                "continuation source/date/redaction"
            );
            ensure!(
                f.automatic("timeout")?.contains(id),
                "brief continuation timeout section was not recalled"
            );
            Ok(())
        }
    );
    case!(
        "configured API keys of lengths one through five are redacted in capture content, titles, and identities",
        {
            for key in ["Q", "qX", "qX7", "qX7Z", "qX7Z9"] {
                short_key_capture(
                    binary,
                    &work.join("short-keys").join(key.len().to_string()),
                    project,
                    endpoint,
                    http,
                    key,
                )?;
            }
            let records = f.sql("SELECT content,metadata_json FROM memories", json!([]))?;
            ensure!(
                records["results"]
                    .as_array()
                    .context("short-key saved records")?
                    .len()
                    == 5,
                "short-key captures did not reach actual Worker"
            );
            Ok(())
        }
    );
    case!(
        "history scans every candidate before limits, keeps the largest duplicate, and rejects metadata read failures before upload",
        {
            let isolated = work.join("history-discovery");
            let sessions = isolated.join("sessions");
            let archived = isolated.join("archived_sessions");
            fs::create_dir_all(&sessions)?;
            fs::create_dir_all(&archived)?;
            let write_transcript = |path: &Path,
                                    id: &str,
                                    user: &str,
                                    answer: &str,
                                    extra: Value|
             -> Result<()> {
                let mut meta = json!({"id":id,"cwd":project,"timestamp":"2020-01-01T00:00:00Z"});
                for (key, value) in extra.as_object().context("history metadata overrides")? {
                    meta[key] = value.clone();
                }
                let rows = [
                    json!({"type":"session_meta","payload":meta}),
                    json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":user}]}}),
                    json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}}),
                ];
                fs::write(
                    path,
                    rows.iter()
                        .map(Value::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )?;
                Ok(())
            };
            write_transcript(
                &sessions.join("000-unicode.jsonl"),
                "000-unicode",
                "smaller duplicate",
                "old response",
                json!({}),
            )?;
            write_transcript(
                &archived.join("000-unicode.jsonl"),
                "000-unicode",
                "Unicode 😀 larger duplicate preserved discussion",
                "完了しました 😀𠮷 preserved result",
                json!({}),
            )?;
            for i in 1..=96 {
                write_transcript(
                    &sessions.join(format!("candidate-{i}.jsonl")),
                    &format!("z-{i}"),
                    "ordinary request",
                    "ordinary result",
                    json!({"timestamp":format!("2020-02-{:02}T00:00:00Z",i%28+1)}),
                )?;
            }
            write_transcript(
                &sessions.join("subagent.jsonl"),
                "subagent",
                "subagent request",
                "result",
                json!({"parent_thread_id":"000-unicode"}),
            )?;
            fs::write(sessions.join("malformed.jsonl"), b"not json\n")?;
            let import = || -> Result<std::process::Output> {
                let mut cmd = command(binary, &isolated, project, endpoint);
                cmd.args([
                    "import",
                    "--apply",
                    "--include-recent",
                    "--root-only",
                    "--limit",
                    "1",
                    "--max-document-chars",
                    "10000",
                    "--state-path",
                ])
                .arg(isolated.join("history-state.json"));
                process_output(cmd, b"")
            };
            let result = import()?;
            ensure!(
                result.status.success() && result.stderr.is_empty(),
                "full history import failed"
            );
            let summary: Value = serde_json::from_slice(&result.stdout)?;
            ensure!(
                summary["sourceFiles"] == 100
                    && summary["discoveredSessions"] == 97
                    && summary["duplicateTranscriptFiles"] == 1
                    && summary["selectedSessions"] == 1
                    && summary["created"] == 1,
                "history selection changed: {summary}"
            );
            let docs = f.sql("SELECT content,metadata_json FROM memories", json!([]))?;
            let rows = docs["results"].as_array().context("imported history")?;
            ensure!(
                rows.len() == 1
                    && rows[0]["content"]
                        .as_str()
                        .unwrap()
                        .contains("Unicode 😀 larger duplicate")
                    && !rows[0]["content"]
                        .as_str()
                        .unwrap()
                        .contains("smaller duplicate"),
                "largest earliest root candidate was not selected"
            );
            fs::write(sessions.join("unreadable-metadata.jsonl"), [0xff, b'\n'])?;
            f.clear_counts()?;
            let failed = import()?;
            ensure!(
                !failed.status.success()
                    && failed.stdout.is_empty()
                    && !String::from_utf8_lossy(&failed.stderr).contains(API_KEY)
                    && f.counts()?["requests"].as_array().unwrap().is_empty(),
                "metadata read failure uploaded partial history or leaked key"
            );
            Ok(())
        }
    );
    case!(
        "actual Stop preserves Unicode multipart text and legacy fixed-width v2 identities",
        {
            let session = format!("multipart-{}", uuid::Uuid::new_v4());
            let header = "### User request\n";
            let old_title = "u".repeat(200);
            let width = 120000
                - format!("# {old_title}\n\nSession: {session}\nTurn: 1\n\n")
                    .encode_utf16()
                    .count()
                - 80;
            let user = format!(
                "{}😀 remaining user evidence",
                "u".repeat(width - header.encode_utf16().count() - 1)
            );
            let answer = "Unicode 😀 result verified.";
            let body = format!("{header}{user}\n\n### Final assistant response\n{answer}");
            let digest = crate::package::hash(body.as_bytes());
            let transcript = home.join("sessions").join(format!("{session}.jsonl"));
            let rows = [
                json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":user}}),
                json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}}),
            ];
            fs::write(
                &transcript,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
            let payload = json!({"session_id":session,"transcript_path":transcript,"cwd":project});
            hook(binary, home, project, endpoint, "Stop", payload.clone())?;
            let records=f.sql("SELECT custom_id,content,metadata_json FROM memories WHERE custom_id LIKE 'codex-turn-v2:%' ORDER BY json_extract(metadata_json,'$.part')",json!([]))?;
            let records = records["results"]
                .as_array()
                .context("multipart captures")?;
            let units = body.encode_utf16().collect::<Vec<_>>();
            let chunks = units.chunks(width).collect::<Vec<_>>();
            ensure!(
                records.len() == chunks.len() && records.len() > 1,
                "multipart number mismatch"
            );
            let mut joined = String::new();
            for (i, (record, chunk)) in records.iter().zip(chunks).enumerate() {
                let legacy_part = String::from_utf16_lossy(chunk);
                let part_hash = crate::package::hash(legacy_part.as_bytes());
                let key = format!("{session}:1:{digest}:{}:{}", i + 1, &part_hash[..16]);
                let metadata: Value =
                    serde_json::from_str(record["metadata_json"].as_str().unwrap())?;
                ensure!(
                    record["custom_id"] == format!("codex-turn-v2:{key}")
                        && metadata["captureKey"] == key
                        && metadata["part"] == i + 1
                        && metadata["parts"] == records.len(),
                    "legacy v2 identity mismatch: {metadata}"
                );
                joined.push_str(
                    record["content"]
                        .as_str()
                        .unwrap()
                        .split_once("\n\n")
                        .context("capture title separator")?
                        .1,
                );
            }
            ensure!(joined == body, "multipart Unicode source text changed");
            f.clear_counts()?;
            hook(binary, home, project, endpoint, "Stop", payload)?;
            ensure!(
                f.counts()?["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r["method"] != "POST" || r["path"] != "/v3/documents"),
                "multipart retry resent"
            );
            let mut import = command(binary, home, project, endpoint);
            import
                .args([
                    "import",
                    "--apply",
                    "--include-recent",
                    "--session-id",
                    &session,
                    "--state-path",
                ])
                .arg(work.join("multipart-import-state.json"));
            let imported = process_output(import, b"")?;
            ensure!(
                imported.status.success() && imported.stderr.is_empty(),
                "multipart history import failed"
            );
            let imported: Value = serde_json::from_slice(&imported.stdout)?;
            ensure!(
                imported["created"] == 0 && imported["unchanged"] == records.len(),
                "Stop/import multipart identity differs: {imported}"
            );
            Ok(())
        }
    );
    case!(
        "skill-only completed work preserves ordinary variables and paths through capture and query",
        {
            let session = format!("skills-{}", uuid::Uuid::new_v4());
            let request = "[$snowrunner](C:/skills/snowrunner/SKILL.md)\n<skill>\n<name>snowrunner</name>\n<path>C:/skills/snowrunner/SKILL.md</path>\nskill-only-canary VPS Realtime\n</skill>";
            let answer = "## SnowRunner settings\n$PATH and /tmp remain actual evidence.\n"
                .repeat(8)
                .trim()
                .to_owned();
            let transcript = home.join("sessions").join(format!("{session}.jsonl"));
            let rows = [
                json!({"type":"session_meta","payload":{"id":session,"cwd":project}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":request}}),
                json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}}),
            ];
            fs::write(
                &transcript,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
            hook(
                binary,
                home,
                project,
                endpoint,
                "Stop",
                json!({"session_id":session,"transcript_path":transcript,"cwd":project}),
            )?;
            let records = f.sql(
                "SELECT content,metadata_json FROM memories WHERE custom_id LIKE 'codex-turn-v2:%'",
                json!([]),
            )?;
            let rows = records["results"].as_array().context("skill captures")?;
            ensure!(rows.len() == 1, "skill-only capture count");
            let metadata: Value = serde_json::from_str(rows[0]["metadata_json"].as_str().unwrap())?;
            ensure!(
                metadata["memoryIndex"]["recallable"] == true
                    && rows[0]["content"].as_str().unwrap().contains(&answer)
                    && !rows[0]["content"]
                        .as_str()
                        .unwrap()
                        .contains("skill-only-canary"),
                "skill projection lost evidence"
            );
            f.clear_counts()?;
            f.automatic("Use $PATH in /tmp and /var/cache/widget")?;
            let state = f.counts()?;
            let q = state["requests"][0]["body"]["q"]
                .as_str()
                .context("ordinary variable query")?;
            ensure!(
                q.contains("$PATH") && q.contains("tmp") && q.contains("widget"),
                "ordinary path/variable projection: {q}"
            );
            f.clear_counts()?;
            f.automatic(&format!("{request}\nSnowRunner truck settings"))?;
            let state = f.counts()?;
            let q = state["requests"][0]["body"]["q"].as_str().unwrap_or("");
            ensure!(
                q.contains("SnowRunner")
                    && !q.contains("SKILL.md")
                    && !q.contains("skill-only-canary")
                    && !q.contains("VPS")
                    && !q.contains("Realtime"),
                "skill text entered automatic query: {q}"
            );
            Ok(())
        }
    );
    case!(
        "checkpoints retain uncovered concrete evidence and manual originals",
        {
            let ordinary = f.seed(
                "memories",
                "# Orion implemented\nOrion implemented",
                Recall::index("Orion implemented", "Orion implemented"),
            )?;
            let specific = f.seed(
                "memories",
                "# Orion AbortSignal.timeout\nOrion AbortSignal.timeout implementation detail",
                Recall::index("Orion AbortSignal.timeout", "Orion AbortSignal.timeout"),
            )?;
            let summary = f.checkpoint("orion-project", &[ordinary.clone(), specific.clone()])?;
            let text = f.automatic("Orion AbortSignal.timeout")?;
            ensure!(
                text.contains(&summary) && text.contains(&specific),
                "checkpoint lost concrete detail: {text}"
            );
            let text = f.automatic("Orion implemented")?;
            ensure!(
                text.contains(&summary) && !text.contains(&ordinary),
                "ordinary checkpoint duplicate: {text}"
            );
            ensure!(
                f.search("Orion implemented")?
                    .to_string()
                    .contains(&ordinary),
                "manual originals missing"
            );
            Ok(())
        }
    );
    case!(
        "unselected checkpoint cannot suppress evidence that fills the remaining result budget",
        {
            let mut sources = Vec::new();
            for i in 0..6 {
                let id = f.seed(
                    "memories",
                    &format!("# Orion project{i}\nOrion implementation"),
                    Recall::index(&format!("Orion project{i}"), "Orion implementation"),
                )?;
                f.checkpoint(&format!("budget-project-{i}"), &[id.clone()])?;
                sources.push(id);
            }
            let last = sources.last().unwrap();
            f.sql("UPDATE memories SET metadata_json=json_set(metadata_json,'$.memoryIndex.title','Orion ExactBudgetNeedle','$.memoryIndex.description','Orion ExactBudgetNeedle') WHERE id = ?",json!([last]))?;
            let text = f.automatic("Orion ExactBudgetNeedle")?;
            ensure!(
                text.contains(last),
                "unselected checkpoint suppressed exact evidence: {text}"
            );
            Ok(())
        }
    );
    case!(
        "higher-relevance source survives a lower-relevance unselected checkpoint",
        {
            let a = f.seed(
                "memories",
                "# Orion implemented\nOrion implemented",
                Recall::index("Orion implemented", "Orion implemented"),
            )?;
            let b = f.seed(
                "memories",
                "# Orion implemented\nOrion implemented",
                Recall::index("Orion implemented", "Orion implemented"),
            )?;
            let summary_a = f.checkpoint("choice-a", &[a])?;
            let summary_b = f.checkpoint("choice-b", &[b.clone()])?;
            f.sql("UPDATE memories SET metadata_json=json_set(metadata_json,'$.memoryIndex.title','Orion','$.memoryIndex.description','Orion','$.memoryIndex.sections',json('[]')) WHERE id = ?",json!([summary_b]))?;
            f.settings(2, 0)?;
            let text = f.automatic("Orion implemented")?;
            ensure!(
                text.contains(&summary_a) && text.contains(&b) && !text.contains(&summary_b),
                "checkpoint lower relevance: {text}"
            );
            Ok(())
        }
    );
    case!(
        "global checkpoint candidates retain complete source count with bounded membership",
        {
            let mut ids = Vec::new();
            for i in 0..25 {
                ids.push(f.seed(
                    "memories",
                    &format!("# Orion source {i}\nOrion evidence {i}"),
                    Recall::index(&format!("Orion source {i}"), "Orion evidence"),
                )?);
            }
            let summary = f.checkpoint("membership-project", &ids)?;
            let result = f.raw(
                "/v4/search",
                json!({"allContainers":true,"q":"Orion","indexOnly":true,"limit":3}),
                "POST",
            )?;
            let rows = result["results"].as_array().context("checkpoint results")?;
            let row = rows
                .iter()
                .find(|r| r["id"] == summary)
                .context("summary candidate")?;
            ensure!(
                row["metadata"]["sourceCount"] == 25
                    && row["metadata"]["sourceMemoryIdsTruncated"] == true,
                "checkpoint count/truncation: {row}"
            );
            let candidates = row["metadata"]["sourceMemoryIds"]
                .as_array()
                .context("candidate source ids")?;
            ensure!(
                candidates.len() <= 2
                    && candidates
                        .iter()
                        .all(|id| rows.iter().any(|r| r["id"] == *id)),
                "unbounded checkpoint membership"
            );
            Ok(())
        }
    );
    for (name, config) in [
        ("old-server-400", json!({"legacy400":true})),
        ("markerless-success", json!({"markerless":true})),
    ] {
        case!(name, {
            let id = f.seed(
                "old",
                "# CompatibilityNeedle\nCompatibilityNeedle original",
                json!({}),
            )?;
            f.control("/__configure", Some(config.clone()))?;
            f.clear_counts()?;
            ensure!(
                f.automatic("CompatibilityNeedle")?.contains(&id),
                "legacy automatic result"
            );
            let state = f.counts()?;
            ensure!(
                state["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["path"] == "/v3/container-tags"),
                "legacy compatibility discovery absent: {state}"
            );
            Ok(())
        });
    }
    case!("original excerpts escape markup before hook injection", {
        let id=f.seed("escape","# Transport\nTransport reliability\n### Final assistant response\nUse EscapeNeedle <script>alert('archive')</script> & evidence.",Recall::index("Transport","Transport reliability"))?;
        let text = f.automatic("EscapeNeedle")?;
        ensure!(
            text.contains(&id)
                && text.contains("EscapeNeedle")
                && !text.contains("<script>")
                && text.contains("&amp;"),
            "unsafe original excerpt: {text}"
        );
        Ok(())
    });
    for status in [401, 500] {
        case!(
            format!("HTTP {status} does not fan out to per-space fallback"),
            {
                f.seed(
                    "errors",
                    "# ErrorNeedle\nErrorNeedle",
                    Recall::index("ErrorNeedle", "ErrorNeedle"),
                )?;
                f.control("/__configure", Some(json!({"transportStatus":status})))?;
                f.clear_counts()?;
                let result = f.mcp.exchange(
                    "tools/call",
                    json!({"name":"search_memory","arguments":{"query":"ErrorNeedle"}}),
                )?;
                ensure!(result["isError"] == true, "HTTP failure hidden");
                let state = f.counts()?;
                ensure!(
                    state["requests"].as_array().unwrap().len() == 1
                        && state["requests"][0]["body"]["allContainers"] == true,
                    "HTTP fallback fanout: {state}"
                );
                Ok(())
            }
        );
    }
    case!("index title survives fifty semantic and body-only hits", {
        for i in 0..50 {
            f.seed(
                &format!("body-{i}"),
                &format!("# General request {i}\nBodyCrowdingNeedle implementation detail"),
                Recall::index(&format!("General request {i}"), "General request"),
            )?;
        }
        let id = f.seed(
            "target",
            "# Technical archive\nTransport observations",
            Recall::index("BodyCrowdingNeedle", "BodyCrowdingNeedle"),
        )?;
        let text = f.automatic("BodyCrowdingNeedle")?;
        ensure!(
            text.contains(&id),
            "index crowding evicted actual title match"
        );
        Ok(())
    });
    case!(
        "checkpoint crowding cannot evict the original concrete query evidence",
        {
            for i in 0..41 {
                let id = f.seed(
                    "memories",
                    &format!("# Orion implemented {i}\nOrion implemented"),
                    Recall::index(&format!("Orion implemented {i}"), "Orion implemented"),
                )?;
                f.checkpoint(&format!("crowded-project-{i}"), &[id])?;
            }
            let id = f.seed(
                "specific-original",
                "# Orion ExactNeedle\nOrion ExactNeedle original evidence",
                Recall::index("Orion ExactNeedle", "Orion ExactNeedle"),
            )?;
            f.clear_counts()?;
            ensure!(
                f.automatic("Orion ExactNeedle")?.contains(&id),
                "checkpoint crowding evicted concrete original"
            );
            f.traffic(1)?;
            Ok(())
        }
    );
    for legacy in [false, true] {
        case!(
            format!("concrete index term survives fifty short-term matches (legacy={legacy})"),
            {
                for i in 0..50 {
                    f.seed(
                        &format!("weighted-{i}"),
                        &format!("# General archive {i}\nUnrelated transcript"),
                        Recall::index("Go UI", "Go UI"),
                    )?;
                }
                let metadata = if legacy {
                    json!({"title":"ExactNeedle"})
                } else {
                    Recall::index("ExactNeedle", "ExactNeedle")
                };
                let id = f.seed(
                    "weighted-target",
                    "# Technical archive\nUnrelated original transcript",
                    metadata,
                )?;
                ensure!(
                    f.automatic("go ui ExactNeedle")?.contains(&id),
                    "short-term crowding lost concrete title"
                );
                Ok(())
            }
        );
    }
    for (query, title) in [
        ("école", "ÉCOLE"),
        ("σχολή", "ΣΧΟΛΉ"),
        ("İstanbul", "İSTANBUL"),
        ("kiriha", "KIRIHA"),
        ("ωmega", "ΩMEGA"),
        ("θeta", "ϴETA"),
    ] {
        case!(
            format!("Unicode index folding survives lexical crowding: {query}"),
            {
                f.control("/__configure", Some(json!({"AI_ENRICHMENT_MODE":"off"})))?;
                for i in 0..50 {
                    f.seed(
                        &format!("unicode-{i}"),
                        &format!("# General request {i}\n{query} implementation detail"),
                        Recall::index(&format!("General request {i}"), "General request"),
                    )?;
                }
                let id = f.seed(
                    "unicode-target",
                    "# Technical archive\nTransport observations",
                    Recall::index(title, title),
                )?;
                f.clear_counts()?;
                let text = f.automatic(query)?;
                ensure!(text.contains(&id), "Unicode title lost: {text}");
                let state = f.counts()?;
                ensure!(
                    state["metrics"]["embeddings"] == 0 && state["metrics"]["vectorQueries"] == 0,
                    "AI-off invoked AI/vector"
                );
                Ok(())
            }
        );
    }
    case!("empty prompts and SessionStart make no HTTP/AI requests", {
        f.clear_counts()?;
        for prompt in ["", "   "] {
            ensure!(f.automatic(prompt)?.is_empty(), "empty prompt output");
        }
        hook(
            binary,
            home,
            project,
            endpoint,
            "SessionStart",
            json!({"session_id":"empty-start","cwd":project}),
        )?;
        let state = f.counts()?;
        ensure!(
            state["requests"].as_array().unwrap().is_empty()
                && state["metrics"]["embeddings"] == 0
                && state["metrics"]["vectorQueries"] == 0,
            "empty/start traffic"
        );
        Ok(())
    });
    f.mcp.close()?;
    fs::write(
        work.join("recall-efficiency-validation.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":failures.is_empty(),"runtime":"actual native MCP/hooks + workerd","checks":checks,"failures":failures,"timingComparison":false}),
        )?,
    )?;
    println!(
        "Native recall contracts: {} passed, {} failed",
        checks.len(),
        failures.len()
    );
    ensure!(
        failures.is_empty(),
        "{} native recall contracts failed; see recall-efficiency-validation.json",
        failures.len()
    );
    Ok(())
}
