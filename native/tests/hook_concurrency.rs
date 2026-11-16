//! 複数のフックプロセスが同時に起動しても、想起が D1 過負荷（503）で空にならないことを確かめる。
//! 本番 Worker の D1 は同時処理が増えると 503 + Retry-After: 10 を返すため、フィクスチャは
//! 「同時処理数が上限を超えたら 503 を即返す」スロットルで再現する。実環境の API や設定には触れない。
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// フィクスチャの調整値と観測値（スレッド間で共有する）。
#[derive(Clone, Default)]
struct State {
    /// 同時処理数の上限。超えた要求は処理せず 503 を返す（D1 の過負荷の再現）。0 なら常に 503。
    capacity: Arc<AtomicUsize>,
    /// 1 要求あたりの処理時間（D1 の処理時間の代わり）。
    work_ms: Arc<AtomicUsize>,
    /// 先頭から数えて、この回数までの要求は上限と無関係に 503 を返す（一時的な過負荷）。
    fail_first: Arc<AtomicUsize>,
    /// 503 に付ける Retry-After（秒）。
    retry_after: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
    search_requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}
struct Throttle {
    url: String,
    state: State,
}
impl Throttle {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let state = State {
            capacity: Arc::new(AtomicUsize::new(usize::MAX)),
            retry_after: Arc::new(AtomicUsize::new(10)),
            ..State::default()
        };
        let shared = state.clone();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                let state = shared.clone();
                thread::spawn(move || serve(stream, state));
            }
        });
        Self { url, state }
    }
}
impl Drop for Throttle {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

fn serve(mut stream: TcpStream, s: State) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let (headers, body) = loop {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
        }
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
            let length = headers
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break (
                    headers,
                    serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                        .unwrap_or(Value::Null),
                );
            }
        }
    };
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    if path == "/v4/search" {
        s.search_requests.fetch_add(1, Ordering::SeqCst);
    }
    let now = s.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    s.max_in_flight.fetch_max(now, Ordering::SeqCst);
    let sequence = s.served.fetch_add(1, Ordering::SeqCst);
    let overloaded =
        sequence < s.fail_first.load(Ordering::SeqCst) || now > s.capacity.load(Ordering::SeqCst);
    let document = |id: &str, title: &str| json!({"id":id,"containerTag":"fixture-space","createdAt":"2026-10-01T00:00:00Z","updatedAt":"2026-11-01T00:00:00Z","metadata":{"sessionId":"earlier-session","project":"Fixture","memoryIndex":{"version":1,"title":title,"description":title,"recallable":true,"sections":[]}}});
    let (status, output) = if overloaded {
        (
            503,
            json!({"error":{"message":"Memory database is temporarily overloaded; retry later"}}),
        )
    } else {
        thread::sleep(Duration::from_millis(
            s.work_ms.load(Ordering::SeqCst) as u64
        ));
        if path == "/v4/search" {
            (
                200,
                json!({"searchScope":"all-containers","searchedContainers":["fixture-space"],"spaceDiscoveryComplete":true,"results":[document("topic-doc","Repair Unicode transcript boundaries")]}),
            )
        } else if path == "/v3/documents/list" && body["recentTasks"] == true {
            (
                200,
                json!({"recentTasks":true,"documents":[document("recent-doc","Review release packaging")]}),
            )
        } else {
            (404, json!({"error":"unexpected path"}))
        }
    };
    s.in_flight.fetch_sub(1, Ordering::SeqCst);
    let output = serde_json::to_vec(&output).unwrap();
    let extra = if status == 503 {
        format!("Retry-After: {}\r\n", s.retry_after.load(Ordering::SeqCst))
    } else {
        String::new()
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        output.len()
    );
    let _ = stream.write_all(&output);
}

fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("hook-concurrency-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}
/// 隔離した REMNIFOLD_HOME（全プロセスで共有 = 同じロック置き場）でフックを 1 つ実行する。
fn hook(root: &Path, url: &str, session: usize) -> (Value, Duration) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    support::isolate(&mut command);
    command
        .args(["hook", "UserPromptSubmit"])
        .env_remove("REMNIFOLD_LANGUAGE")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "ja_JP.UTF-8")
        .env("CODEX_HOME", root)
        .env("REMNIFOLD_HOME", root)
        .env("REMNIFOLD_API_URL", url)
        .env("REMNIFOLD_API_KEY", "fixture-secret");
    let input = serde_json::to_vec(&json!({"session_id":format!("concurrent-{session}"),"transcript_path":"","prompt":"Repair Unicode transcript boundaries"})).unwrap();
    let started = Instant::now();
    let output = support::run(&mut command, &input, Duration::from_secs(20)).unwrap();
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = if output.stdout.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&output.stdout).unwrap()
    };
    (value, started.elapsed())
}
fn run_concurrently(root: &Path, url: &str, count: usize) -> Vec<(Value, Duration)> {
    thread::scope(|scope| {
        let handles: Vec<_> = (0..count)
            .map(|n| scope.spawn(move || hook(root, url, n)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

#[test]
fn concurrent_hooks_share_a_bounded_number_of_api_requests() {
    let server = Throttle::new();
    // 同時 2 件までしか受け付けない D1 の再現。6 プロセスが同時に起動しても、
    // プロセス間のスロットで同時数が絞られるため、1 件も 503 に当たらず全員が想起できる。
    server.state.capacity.store(2, Ordering::SeqCst);
    server.state.work_ms.store(250, Ordering::SeqCst);
    let root = root();
    let results = run_concurrently(&root, &server.url, 6);
    for (n, (output, elapsed)) in results.iter().enumerate() {
        assert!(
            output.get("systemMessage").is_none(),
            "hook {n} output={output}"
        );
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("");
        assert!(
            context.contains("id=topic-doc") && context.contains("id=recent-doc"),
            "hook {n} output={output}"
        );
        assert!(
            *elapsed < Duration::from_secs(11),
            "hook {n} took {elapsed:?}"
        );
    }
    assert!(
        server.state.max_in_flight.load(Ordering::SeqCst) <= 2,
        "同時処理数が上限を超えた"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transient_overload_is_retried_within_the_deadline() {
    let server = Throttle::new();
    // 先頭の 2 要求は 503（Retry-After: 1）。Retry-After が締切に収まるため、その間隔で再試行して成功する。
    server.state.fail_first.store(2, Ordering::SeqCst);
    server.state.retry_after.store(1, Ordering::SeqCst);
    let root = root();
    let (output, elapsed) = hook(&root, &server.url, 0);
    assert!(output.get("systemMessage").is_none(), "output={output}");
    assert!(
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("id=topic-doc")
    );
    assert!(
        elapsed >= Duration::from_millis(900),
        "Retry-After を待たずに再送した: {elapsed:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn persistent_overload_fails_open_with_a_notice_and_bounded_retries() {
    let server = Throttle::new();
    // 常に 503 + Retry-After: 10（締切に収まらない固定値）。短いバックオフで有限回だけ試し、通知して続行する。
    server.state.capacity.store(0, Ordering::SeqCst);
    let root = root();
    let (output, elapsed) = hook(&root, &server.url, 0);
    let notice = output["systemMessage"].as_str().unwrap_or("");
    assert!(
        notice.contains("索引を取得できませんでした"),
        "output={output}"
    );
    assert!(output.get("hookSpecificOutput").is_none());
    assert!(
        elapsed < Duration::from_secs(11),
        "締切を超えて待った: {elapsed:?}"
    );
    assert!(
        server.state.search_requests.load(Ordering::SeqCst) <= 3,
        "再試行の回数が上限を超えた"
    );
    // 秘密値（API キー）は通知や出力に現れない。
    assert!(!output.to_string().contains("fixture-secret"));
    let _ = fs::remove_dir_all(&root);
}
