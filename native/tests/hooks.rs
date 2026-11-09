//! Exercise the shipped hook command against authenticated loopback HTTP.
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

struct Fixture {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    revision: Arc<AtomicUsize>,
    recent_failure: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let revision = Arc::new(AtomicUsize::new(1));
        let recent_failure = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (request_log, rev, fail, stopping) = (
            requests.clone(),
            revision.clone(),
            recent_failure.clone(),
            stop.clone(),
        );
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Windows accepts inherit the listener's nonblocking mode.
                        // Wait for request bytes instead of treating WouldBlock as EOF.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 4096];
                        let parsed = loop {
                            match stream.read(&mut buffer) {
                                Ok(0) | Err(_) => break None,
                                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                            }
                            if bytes.len() > 1024 * 1024 {
                                break None;
                            }
                            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
                                let length = headers
                                    .lines()
                                    .find_map(|line| {
                                        line.split_once(':')
                                            .filter(|(key, _)| {
                                                key.eq_ignore_ascii_case("content-length")
                                            })
                                            .and_then(|(_, value)| {
                                                value.trim().parse::<usize>().ok()
                                            })
                                    })
                                    .unwrap_or(0);
                                if bytes.len() >= end + 4 + length {
                                    break Some((
                                        headers,
                                        serde_json::from_slice::<Value>(
                                            &bytes[end + 4..end + 4 + length],
                                        )
                                        .unwrap_or(Value::Null),
                                    ));
                                }
                            }
                        };
                        let Some((headers, input)) = parsed else {
                            continue;
                        };
                        let path = headers
                            .lines()
                            .next()
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap()
                            .to_owned();
                        let authorized = headers.lines().any(|line| {
                            line.eq_ignore_ascii_case("authorization: Bearer fixture-secret")
                        });
                        request_log
                            .lock()
                            .unwrap()
                            .push((path.clone(), input.clone()));
                        let revision = rev.load(Ordering::Relaxed);
                        let document = |id: &str, title: &str| json!({"id":id,"containerTag":"fixture-space","createdAt":"2026-10-01T00:00:00Z","updatedAt":format!("2026-11-0{revision}T00:00:00Z"),"metadata":{"sessionId":"earlier-session","project":"Fixture","memoryIndex":{"version":1,"title":title,"description":title,"recallable":true,"sections":[]}}});
                        let (status, output) = if !authorized {
                            (401, json!({"error":"unauthorized"}))
                        } else if path == "/v4/search" {
                            (
                                200,
                                json!({"searchScope":"all-containers","searchedContainers":["fixture-space"],"spaceDiscoveryComplete":true,"results":[document("topic-doc", "Repair Unicode transcript boundaries")]}),
                            )
                        } else if path == "/v3/documents/list" && input["recentTasks"] == true {
                            if fail.load(Ordering::Relaxed) {
                                (
                                    503,
                                    json!({"error":"private fixture failure must never be printed"}),
                                )
                            } else {
                                (
                                    200,
                                    json!({"recentTasks":true,"documents":[document("recent-doc", "Review release packaging")]}),
                                )
                            }
                        } else {
                            (404, json!({"error":"unexpected path"}))
                        };
                        let output = serde_json::to_vec(&output).unwrap();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            output.len()
                        );
                        let _ = stream.write_all(&output);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            requests,
            revision,
            recent_failure,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn hook(root: &Path, url: &str, event: &str, input: &[u8]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    support::isolate(&mut command);
    command
        .args(["hook", event])
        .env("CODEX_HOME", root)
        .env("REMNIFOLD_API_URL", url)
        .env("REMNIFOLD_API_KEY", "fixture-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in [
        "KAGAYOI_MEMORY_API_URL",
        "SUPERMEMORY_API_URL",
        "KAGAYOI_MEMORY_API_KEY",
        "SUPERMEMORY_CODEX_API_KEY",
        "CLOUDFLARE_MEMORY_API_KEY",
    ] {
        command.env_remove(name);
    }
    let output = support::run(&mut command, input, Duration::from_secs(15)).unwrap();
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    if output.stdout.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("hooks-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}
fn append(path: &Path, row: Value) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{row}").unwrap();
}

#[test]
fn loopback_fixture_waits_for_request_bytes_after_accept() {
    let fixture = Fixture::new();
    let mut stream = TcpStream::connect(fixture.url.trim_start_matches("http://")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // TCP acceptance can precede the caller writing HTTP headers. In particular,
    // Windows accepted sockets inherit a listener's nonblocking mode.
    thread::sleep(Duration::from_millis(100));
    stream.write_all(b"POST /v4/search HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    assert!(
        output.starts_with("HTTP/1.1 200 "),
        "delayed request response={output:?}; received requests={:?}",
        fixture.requests.lock().unwrap()
    );
}

#[test]
fn continuation_requires_verified_history_but_keeps_recent_task_awareness() {
    let fixture = Fixture::new();
    let root = root();
    let transcript = root.join("session.jsonl");
    let topic = json!({"type":"event_msg","payload":{"type":"user_message","message":"Repair Unicode transcript boundaries"}});
    let payload =
        json!({"session_id":"fixture-session","transcript_path":transcript,"prompt":"続けて"});
    // セッションを確認できない履歴は話題検索へ流さず、独立した recent 取得は保つ。
    for header in [
        String::new(),
        format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":false}})
        ),
        format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":"another-session"}})
        ),
    ] {
        fs::write(&transcript, format!("{header}{topic}\n")).unwrap();
        fixture.requests.lock().unwrap().clear();
        let output = hook(
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        assert!(!output.to_string().contains("topic-doc"));
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(path, _)| path == "/v3/documents/list")
        );
        assert!(!requests.iter().any(|(path, _)| path == "/v4/search"));
    }
    fs::write(
        &transcript,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-session\"}}\n",
    )
    .unwrap();
    append(&transcript, topic);
    fixture.requests.lock().unwrap().clear();
    let output = hook(
        &root,
        &fixture.url,
        "UserPromptSubmit",
        &serde_json::to_vec(&payload).unwrap(),
    );
    assert!(output.to_string().contains("topic-doc"));
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path == "/v4/search")
    );
}

#[test]
fn native_hook_recall_state_recovery_revision_and_partial_failure() {
    let fixture = Fixture::new();
    let root = root();
    let transcript = root.join("session.jsonl");
    fs::write(
        &transcript,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-session\"}}\n",
    )
    .unwrap();
    let mut payload = json!({"session_id":"fixture-session","transcript_path":transcript,"prompt":"Repair Unicode transcript boundaries"});
    let invoke = |event: &str, payload: &Value| {
        hook(
            &root,
            &fixture.url,
            event,
            &serde_json::to_vec(payload).unwrap(),
        )
    };
    assert_eq!(invoke("SessionStart", &payload), json!({}));
    assert!(fixture.requests.lock().unwrap().is_empty());
    let first = invoke("UserPromptSubmit", &payload);
    assert!(
        first["hookSpecificOutput"]["additionalContext"].is_string(),
        "first recall output={first}; received requests={:?}",
        fixture.requests.lock().unwrap()
    );
    let context = first["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("id=topic-doc"));
    assert!(context.contains("id=recent-doc"));
    assert!(context.encode_utf16().count() <= 3000);
    assert_eq!(invoke("UserPromptSubmit", &payload), json!({}));
    append(
        &transcript,
        json!({"type":"event_msg","payload":{"type":"context_compacted"}}),
    );
    assert!(invoke("UserPromptSubmit", &payload)["hookSpecificOutput"].is_object());
    fixture.revision.store(2, Ordering::Relaxed);
    assert!(invoke("UserPromptSubmit", &payload)["hookSpecificOutput"].is_object());
    append(
        &transcript,
        json!({"type":"event_msg","payload":{"type":"user_message","message":"Repair Unicode transcript boundaries"}}),
    );
    payload["prompt"] = json!("続けて");
    invoke("UserPromptSubmit", &payload);
    let requests = fixture.requests.lock().unwrap();
    let (_, search) = requests
        .iter()
        .rev()
        .find(|(path, _)| path == "/v4/search")
        .unwrap();
    assert!(
        search["q"]
            .as_str()
            .unwrap()
            .contains("Repair Unicode transcript boundaries")
    );
    let (_, recent) = requests
        .iter()
        .rev()
        .find(|(path, _)| path == "/v3/documents/list")
        .unwrap();
    assert_eq!(recent["excludeSessionId"], "fixture-session");
    drop(requests);
    fixture.recent_failure.store(true, Ordering::Relaxed);
    let failure = invoke("UserPromptSubmit", &payload);
    assert!(
        failure["systemMessage"]
            .as_str()
            .unwrap()
            .contains("直近の作業を取得できませんでした")
    );
    assert!(!failure.to_string().contains("private fixture"));
    assert_eq!(
        hook(&root, &fixture.url, "UserPromptSubmit", b"[]"),
        json!({})
    );
    assert_eq!(
        hook(&root, &fixture.url, "UserPromptSubmit", b"not json"),
        json!({})
    );
    // 日本語に隣接したパスも、hook入口から端末・アカウント名を送らない。
    for prompt in [
        r"参照C:\Users\Alice\Dev\Repo\src",
        "参照/home/alice/Dev/Repo/src",
    ] {
        payload["prompt"] = json!(prompt);
        let before = fixture.requests.lock().unwrap().len();
        invoke("UserPromptSubmit", &payload);
        let requests = fixture.requests.lock().unwrap();
        let request = requests[before..]
            .iter()
            .find(|(path, _)| path == "/v4/search")
            .expect("hook must submit projected query");
        assert_eq!(request.1["q"], "参照Repo");
        assert!(
            !serde_json::to_string(&request.1)
                .unwrap()
                .to_lowercase()
                .contains("alice")
        );
    }
    // Local lifecycle handling and disabled recall require no API credentials.
    assert_eq!(
        hook(
            &root,
            "",
            "SessionStart",
            &serde_json::to_vec(&payload).unwrap()
        ),
        json!({})
    );
    fs::write(root.join("remnifold.json"), b"{\"recallMode\":\"off\"}").unwrap();
    let count = fixture.requests.lock().unwrap().len();
    assert_eq!(
        hook(
            &root,
            "",
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap()
        ),
        json!({})
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), count);
}
