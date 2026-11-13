//! Real native CLI + authenticated loopback API checks. Fixtures are retained for inspection.
mod support;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const KEY: &str = "native-capture-fixture-key";
#[derive(Default)]
struct State {
    requests: Vec<Value>,
    documents: Vec<Value>,
    sources: Vec<String>,
}
struct Server {
    endpoint: String,
    state: Arc<Mutex<State>>,
    acknowledge: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    accepted: std::sync::mpsc::Receiver<()>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let acknowledge = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, ack, done) = (state.clone(), acknowledge.clone(), stop.clone());
        let (accepted_tx, accepted) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Windows accepted sockets inherit the listener's nonblocking mode.
                        // Explicit blocking mode makes the bounded read/write timeouts effective.
                        stream.set_nonblocking(false).unwrap();
                        let _ = accepted_tx.send(());
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        serve(&mut stream, &shared, &ack);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            }
        });
        Self {
            endpoint,
            state,
            acknowledge,
            stop,
            worker: Some(worker),
            accepted,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            if let Err(error) = worker.join() {
                // Preserve the fixture failure without a second panic during test unwinding.
                if !thread::panicking() {
                    std::panic::resume_unwind(error);
                }
            }
        }
    }
}

#[test]
fn accepted_socket_waits_for_delayed_headers() {
    let server = Server::new();
    let mut stream = TcpStream::connect(server.endpoint.strip_prefix("http://").unwrap()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    server
        .accepted
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    // The connection is already accepted, but no HTTP bytes are available yet.
    thread::sleep(Duration::from_millis(100));
    stream
        .write_all(b"GET /delayed-fixture HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "delayed headers must reach the fixture"
    );
}
fn serve(stream: &mut TcpStream, state: &Mutex<State>, ack: &AtomicBool) {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "incomplete HTTP header");
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 1024 * 1024, "header bound");
        if let Some(i) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length = header
        .lines()
        .find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length <= 2 * 1024 * 1024, "request bound");
    while bytes.len() < header_end + length {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    let body: Value =
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap_or(Value::Null);
    let authenticated = header.lines().any(|s| {
        s.split_once(':').is_some_and(|(k, v)| {
            k.eq_ignore_ascii_case("authorization") && v.trim() == format!("Bearer {KEY}")
        })
    });
    let path = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let mut state = state.lock().unwrap();
    let source = header
        .lines()
        .find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("x-sm-source"))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_default();
    state.sources.push(source);
    let (status, response) = if !authenticated {
        ("401 Unauthorized", json!({"error":"unauthorized"}))
    } else if path == "/v3/documents" {
        state.requests.push(body.clone());
        if ack.load(Ordering::SeqCst) {
            let id = body["customId"].as_str().unwrap().to_string();
            let mut doc = body;
            doc["id"] = json!(id);
            if let Some(old) = state
                .documents
                .iter_mut()
                .find(|d| d["customId"] == doc["customId"])
            {
                *old = doc
            } else {
                state.documents.push(doc)
            }
            ("200 OK", json!({"id":id}))
        } else {
            ("200 OK", json!({"status":"unknown"}))
        }
    } else if path == "/v3/documents/list" {
        assert_eq!(body["projection"], "capture");
        (
            "200 OK",
            json!({"documents":state.documents,"pagination":{"totalPages":1}}),
        )
    } else {
        ("404 Not Found", json!({"error":"not found"}))
    };
    let body = response.to_string();
    write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
}
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    transcript: PathBuf,
    session: String,
}
impl Fixture {
    fn new(server: &Server) -> Self {
        let base = std::env::var_os("REMNIFOLD_NATIVE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert!(base.is_absolute());
        fs::create_dir_all(&base).unwrap();
        assert!(
            !fs::symlink_metadata(&base)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let root = base.join(format!("remnifold-capture-{}", uuid::Uuid::new_v4()));
        let home = root.join("codex");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        fs::write(
            home.join("remnifold.json"),
            json!({"baseUrl":server.endpoint,"apiKey":KEY}).to_string(),
        )
        .unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        let transcript = home
            .join("sessions")
            .join(format!("rollout-{session}.jsonl"));
        let this = Self {
            root,
            home,
            transcript,
            session,
        };
        this.write_turns(1);
        this
    }
    fn write_turns(&self, count: usize) {
        let mut rows = vec![
            json!({"type":"session_meta","payload":{"id":self.session,"cwd":self.root.join("project"),"timestamp":"2026-01-01T00:00:00Z"}}),
        ];
        for n in 1..=count {
            rows.push(json!({"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"user_message","message":format!("Fix turn {n} API_KEY='assignment-secret' {KEY} 😀\0")}}));
            rows.push(json!({"type":"response_item","payload":{"role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"local-progress-only"}]}}));
            rows.push(json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":format!("Completed {n} <private>private-secret</private>")}]}}));
        }
        fs::write(
            &self.transcript,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        fs::write(
            self.home.join("session_index.jsonl"),
            json!({"id":self.session,"thread_name":format!("Repair API_KEY='title-secret' {KEY}")})
                .to_string(),
        )
        .unwrap();
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_remnifold"));
        support::isolate(&mut command);
        command
            .args(args)
            .env("CODEX_HOME", &self.home)
            .env("REMNIFOLD_HOME", &self.home)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .current_dir(self.root.join("project"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in [
            "REMNIFOLD_API_URL",
            "REMNIFOLD_API_KEY",
            "KAGAYOI_MEMORY_API_URL",
            "KAGAYOI_MEMORY_API_KEY",
            "SUPERMEMORY_API_URL",
            "SUPERMEMORY_API_KEY",
        ] {
            command.env_remove(name);
        }
        command
    }
    fn spawn(&self, args: &[&str]) -> support::BoundedChild {
        let input=json!({"session_id":self.session,"transcript_path":self.transcript,"cwd":self.root.join("project")}).to_string().into_bytes();
        support::BoundedChild::spawn(&mut self.command(args), input, Duration::from_secs(30))
            .unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        finish(self.spawn(args))
    }
    fn import(&self, apply: bool) -> Value {
        let mut command = self.command(&["import", "--include-recent"]);
        command
            .arg("--state-path")
            .arg(self.root.join("import-state.json"));
        if apply {
            command.arg("--apply");
        }
        let output = support::run(&mut command, b"", Duration::from_secs(30)).unwrap();
        assert!(
            output.status.success(),
            "import failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn receipts(&self) -> Vec<String> {
        let files: Vec<_> = fs::read_dir(self.home.join("remnifold/capture-state"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
            .collect();
        assert_eq!(files.len(), 1);
        serde_json::from_slice(&fs::read(files[0].path()).unwrap()).unwrap()
    }
}
fn finish(child: support::BoundedChild) -> Output {
    child.wait().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn real_stop_capture_redaction_stable_retry_and_concurrent_receipts() {
    let server = Server::new();
    let fixture = Fixture::new(&server);
    let first = fixture.run(&["capture"]);
    success(&first);
    assert!(String::from_utf8_lossy(&first.stdout).contains("saved 1 documents"));
    {
        let state = server.state.lock().unwrap();
        assert_eq!(state.requests.len(), 1);
        let doc = &state.requests[0];
        let serialized = doc.to_string();
        for secret in [
            KEY,
            "assignment-secret",
            "title-secret",
            "private-secret",
            "local-progress-only",
        ] {
            assert!(!serialized.contains(secret), "forbidden captured content");
        }
        assert!(doc["content"].as_str().unwrap().contains("😀\0"));
        assert_eq!(doc["metadata"]["sourceTimestamp"], "2026-01-01T00:00:00Z");
        let body = "### User request\nFix turn 1 API_KEY='[REDACTED]' [REDACTED] 😀\0\n\n### Final assistant response\nCompleted 1 [REDACTED]";
        let hash = remnifold::common::sha256(body);
        assert_eq!(
            doc["customId"],
            format!(
                "codex-turn-v2:{}:1:{hash}:1:{}",
                fixture.session,
                &hash[..16]
            )
        );
    }
    success(&fixture.run(&["hook", "Stop"]));
    assert_eq!(server.state.lock().unwrap().requests.len(), 1);
    assert_eq!(fixture.receipts().len(), 1);
    // Model an existing 1.x receipt without a receipt in the renamed state directory.
    let receipt_path = fs::read_dir(fixture.home.join("remnifold/capture-state"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let legacy_directory = fixture.home.join("kagayoi-memory/capture-state");
    fs::create_dir_all(&legacy_directory).unwrap();
    let legacy_path = legacy_directory.join(receipt_path.file_name().unwrap());
    assert!(receipt_path.is_absolute() && legacy_path.starts_with(&fixture.root));
    fs::rename(&receipt_path, &legacy_path).unwrap();
    fixture.write_turns(3);
    let left = fixture.spawn(&["hook", "Stop"]);
    let right = fixture.spawn(&["hook", "Stop"]);
    success(&finish(left));
    success(&finish(right));
    let receipts = fixture.receipts();
    assert_eq!(receipts.len(), 3);
    assert_eq!(receipts.iter().collect::<BTreeSet<_>>().len(), 3);
    let before = server.state.lock().unwrap().requests.len();
    success(&fixture.run(&["capture"]));
    let state = server.state.lock().unwrap();
    assert_eq!(state.requests.len(), before);
    assert_eq!(state.documents.len(), 3);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|document| document["metadata"]["turn"] == 1)
            .count(),
        1,
        "legacy receipts must prevent turn-one retransmission"
    );
    assert!(state.sources.iter().all(|s| s == "codex-history-backfill"));
}

#[test]
fn missing_acknowledgement_retries_same_id_without_receipt_or_secret_diagnostics() {
    let server = Server::new();
    server.acknowledge.store(false, Ordering::SeqCst);
    let fixture = Fixture::new(&server);
    let failed = fixture.run(&["hook", "Stop"]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("cause=invalid-acknowledgement"));
    assert!(!stderr.contains(KEY));
    assert!(!fixture.home.join("remnifold/capture-state").exists());
    server.acknowledge.store(true, Ordering::SeqCst);
    success(&fixture.run(&["capture"]));
    success(&fixture.run(&["capture"]));
    let state = server.state.lock().unwrap();
    assert_eq!(state.requests.len(), 2);
    assert_eq!(state.requests[0]["customId"], state.requests[1]["customId"]);
    assert_eq!(fixture.receipts().len(), 1);
}

#[test]
fn import_plan_apply_and_cross_container_history_deduplication() {
    let server = Server::new();
    let fixture = Fixture::new(&server);
    fixture.write_turns(2);
    let plan = fixture.import(false);
    assert_eq!(plan["mode"], "plan");
    assert_eq!(plan["plannedDocuments"], 2);
    assert_eq!(plan["created"], 0);
    assert!(server.state.lock().unwrap().requests.is_empty());
    assert!(!fixture.root.join("import-state.json").exists());
    // Explicit --codex-home still overrides a different environment default.
    let explicit = support::run(
        fixture
            .command(&["import", "--include-recent", "--codex-home"])
            .arg(&fixture.home)
            .env("CODEX_HOME", fixture.root.join("missing-codex-home")),
        b"",
        Duration::from_secs(30),
    )
    .unwrap();
    success(&explicit);
    let explicit_plan: Value = serde_json::from_slice(&explicit.stdout).unwrap();
    assert_eq!(explicit_plan["plannedDocuments"], 2);
    let apply = fixture.import(true);
    assert_eq!(apply["created"], 2);
    assert!(fixture.root.join("import-state.json").exists());
    // Existing 1.x containers remain visible to the all-space capture projection.
    for doc in &mut server.state.lock().unwrap().documents {
        doc["containerTag"] = json!("legacy-project-space");
    }
    let retry = fixture.import(true);
    assert_eq!(retry["created"], 0);
    assert_eq!(retry["unchanged"], 2);
    let state = server.state.lock().unwrap();
    assert_eq!(state.requests.len(), 2);
    assert!(state.sources.iter().all(|s| s == "codex-history-backfill"));
}
