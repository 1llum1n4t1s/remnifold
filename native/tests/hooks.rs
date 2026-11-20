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
    mixed: Arc<AtomicBool>,
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
        let mixed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (request_log, rev, fail, mix, stopping) = (
            requests.clone(),
            revision.clone(),
            recent_failure.clone(),
            mixed.clone(),
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
                            let mut results = vec![document(
                                "topic-doc",
                                "Repair Unicode transcript boundaries",
                            )];
                            if mix.load(Ordering::Relaxed) {
                                // 他セッションの記録に加え、sessionId の無い記録と今のセッションの記録を混在させる。
                                let mut legacy =
                                    document("legacy-doc", "Repair Unicode transcript boundaries");
                                legacy["metadata"]
                                    .as_object_mut()
                                    .unwrap()
                                    .remove("sessionId");
                                let mut current =
                                    document("current-doc", "Repair Unicode transcript boundaries");
                                current["metadata"]["sessionId"] = json!("fixture-session");
                                // 再開前の同じ会話（旧 ID）の記録と、無関係な別セッションの記録も混在させる。
                                let mut resumed =
                                    document("resumed-doc", "Repair Unicode transcript boundaries");
                                resumed["metadata"]["sessionId"] = json!("resumed-old");
                                let mut stranger =
                                    document("stranger-doc", "Repair Unicode transcript boundaries");
                                stranger["metadata"]["sessionId"] = json!("stranger-session");
                                resumed["metadata"]["memoryIndex"]["sourceUpdatedAt"] =
                                    json!("2026-09-01T10:05:00Z");
                                // フォーク後に親（旧 ID）が保存した記録（上限より新しい）と、時刻の無い旧 ID の記録。
                                let mut late =
                                    document("late-doc", "Repair Unicode transcript boundaries");
                                late["metadata"]["sessionId"] = json!("resumed-old");
                                late["metadata"]["memoryIndex"]["sourceUpdatedAt"] =
                                    json!("2026-09-01T10:05:01Z");
                                let mut untimed =
                                    document("untimed-doc", "Repair Unicode transcript boundaries");
                                untimed["metadata"]["sessionId"] = json!("resumed-old");
                                // 今の ID の記録は、時刻が上限より新しくても通る。
                                current["metadata"]["memoryIndex"]["sourceUpdatedAt"] =
                                    json!("2030-01-01T00:00:00Z");
                                results.extend([legacy, current, resumed, stranger, late, untimed]);
                            }
                            (
                                200,
                                json!({"searchScope":"all-containers","searchedContainers":["fixture-space"],"spaceDiscoveryComplete":true,"results":results}),
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
            mixed,
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
    // 索引の文言は環境の言語で変わるため、既存の検証は日本語環境として固定する。
    hook_in("ja_JP.UTF-8", root, url, event, input)
}
fn hook_in(language: &str, root: &Path, url: &str, event: &str, input: &[u8]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    support::isolate(&mut command);
    command
        .args(["hook", event])
        // 明示の上書きを外し、POSIX の LANG から言語を決める経路を通す。
        .env_remove("REMNIFOLD_LANGUAGE")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", language)
        .env("CODEX_HOME", root)
        // Remnifold 自身の設定・状態も同じフォルダに隔離する（実環境の設定フォルダへ書かない）。
        .env("REMNIFOLD_HOME", root)
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
    // 既定は今のセッションだけの注入になるため、従来の挙動を調べる既存テストは明示して all にする。
    fs::write(root.join("remnifold.json"), br#"{"recallMode":"direct","recallScope":"all"}"#).unwrap();
    root
}
fn root_all() -> PathBuf {
    root()
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

// 旧来の Codex home にだけ接続設定がある利用者は、初回の実行で Remnifold 自身の設定フォルダへ
// 移行される。旧ファイルは残し、以後の状態は新しい場所だけに書く。
#[test]
fn legacy_codex_home_configuration_migrates_to_remnifold_home() {
    let fixture = Fixture::new();
    let root = root();
    let codex = root.join("codex");
    let remnifold = root.join("remnifold-home");
    fs::create_dir_all(&codex).unwrap();
    let legacy = serde_json::to_vec(
        &json!({"baseUrl":fixture.url,"apiKey":"fixture-secret","recallMode":"direct","recallScope":"all"}),
    )
    .unwrap();
    fs::write(codex.join("remnifold.json"), &legacy).unwrap();
    let transcript = root.join("session.jsonl");
    fs::write(
        &transcript,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-session\"}}\n",
    )
    .unwrap();
    let payload = json!({"session_id":"fixture-session","transcript_path":transcript,"prompt":"Repair Unicode transcript boundaries"});
    let mut command = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    support::isolate(&mut command);
    command
        .args(["hook", "UserPromptSubmit"])
        .env("CODEX_HOME", &codex)
        .env("REMNIFOLD_HOME", &remnifold)
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in [
        "REMNIFOLD_API_URL",
        "REMNIFOLD_API_KEY",
        "LC_ALL",
        "LC_MESSAGES",
    ] {
        command.env_remove(name);
    }
    let output = support::run(
        &mut command,
        &serde_json::to_vec(&payload).unwrap(),
        Duration::from_secs(15),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|context| context.contains("id=topic-doc")),
        "{result}"
    );
    assert_eq!(fs::read(remnifold.join("remnifold.json")).unwrap(), legacy);
    assert_eq!(fs::read(codex.join("remnifold.json")).unwrap(), legacy);
    let state = |base: &Path| fs::read_dir(base.join("recall-state")).map_or(0, |dir| dir.count());
    assert_eq!(state(&remnifold.join("remnifold")), 1);
    assert_eq!(state(&codex.join("remnifold")), 0);
}

// 索引の案内文と見出しは、索引を書き込む環境の言語設定で切り替わる。
// 記録の本文（題名・id）は言語によらずそのまま出る。
#[test]
fn recall_index_follows_environment_language() {
    let fixture = Fixture::new();
    for (language, expected, unexpected) in [
        (
            "ja_JP.UTF-8",
            [
                "過去の文書の索引です",
                "直近の作業（話題との一致は未判定）",
                "getDocument は、",
            ],
            ["Historical document indexes", "Recent tasks (not matched"],
        ),
        (
            "en_US.UTF-8",
            [
                "Historical document indexes",
                "Recent tasks (not matched to the current topic)",
                "Use getDocument only",
            ],
            ["過去の文書の索引です", "直近の作業"],
        ),
    ] {
        let root = root();
        let transcript = root.join("session.jsonl");
        fs::write(
            &transcript,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-session\"}}\n",
        )
        .unwrap();
        let payload = json!({"session_id":"fixture-session","transcript_path":transcript,"prompt":"Repair Unicode transcript boundaries"});
        let output = hook_in(
            language,
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("{language}: {output}"));
        assert!(context.starts_with("<remnifold-index>\n"), "{context}");
        assert!(context.ends_with("</remnifold-index>"), "{context}");
        assert!(context.contains("id=topic-doc") && context.contains("id=recent-doc"));
        for text in expected {
            assert!(
                context.contains(text),
                "{language}: missing {text:?} in {context}"
            );
        }
        for text in unexpected {
            assert!(
                !context.contains(text),
                "{language}: unexpected {text:?} in {context}"
            );
        }
    }
}

#[test]
fn session_scope_injects_only_current_session_records_and_skips_recent_tasks() {
    let fixture = Fixture::new();
    fixture.mixed.store(true, Ordering::Relaxed);
    let payload =
        json!({"session_id":" fixture-session ","prompt":"Repair Unicode transcript boundaries"});
    // recallMode 以外は既定のまま・明示の session・未知の値は、いずれも session として扱う。
    for config in [
        Some(r#"{"recallMode":"direct"}"#),
        Some(r#"{"recallMode":"direct","recallScope":"session"}"#),
        Some(r#"{"recallMode":"direct","recallScope":"unknown"}"#),
    ] {
        let root = std::env::temp_dir().join(format!("hooks-e2e-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        if let Some(config) = config {
            fs::write(root.join("remnifold.json"), config).unwrap();
        }
        fixture.requests.lock().unwrap().clear();
        let output = hook(
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("{config:?}: {output}"));
        assert!(context.contains("id=current-doc"), "{context}");
        for other in ["topic-doc", "legacy-doc", "recent-doc"] {
            assert!(!context.contains(other), "{config:?}: {other} in {context}");
        }
        assert!(context.contains("今のセッションの記録"), "{context}");
        let requests = fixture.requests.lock().unwrap();
        assert!(requests.iter().any(|(path, _)| path == "/v4/search"));
        // サーバーにも trim 済みの sessionId を渡し、候補窓を決める前に絞らせる。
        for (_, body) in requests.iter().filter(|(path, _)| path == "/v4/search") {
            assert_eq!(body["sessionId"], "fixture-session", "{config:?}: {body}");
        }
        assert!(
            !requests
                .iter()
                .any(|(path, _)| path == "/v3/documents/list")
        );
    }
    // session_id の無いペイロードは、何も注入せず検索もしない（fail closed）。
    let root = std::env::temp_dir().join(format!("hooks-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fixture.requests.lock().unwrap().clear();
    let output = hook(
        &root,
        &fixture.url,
        "UserPromptSubmit",
        &serde_json::to_vec(&json!({"prompt":"Repair Unicode transcript boundaries"})).unwrap(),
    );
    assert_eq!(output, json!({}));
    assert!(fixture.requests.lock().unwrap().is_empty());
    // all では従来どおり、他セッションの記録と直近の作業も注入する。
    let root = root_all();
    fixture.requests.lock().unwrap().clear();
    let output = hook(
        &root,
        &fixture.url,
        "UserPromptSubmit",
        &serde_json::to_vec(&payload).unwrap(),
    );
    let context = output.to_string();
    for id in ["topic-doc", "legacy-doc", "current-doc", "recent-doc"] {
        assert!(context.contains(id), "{id} in {context}");
    }
    let requests = fixture.requests.lock().unwrap();
    assert!(requests.iter().any(|(path, _)| path == "/v3/documents/list"));
    // all では従来どおり、検索にセッション絞り込みを載せない。
    for (_, body) in requests.iter().filter(|(path, _)| path == "/v4/search") {
        assert!(body.get("sessionId").is_none(), "{body}");
    }
}

// セッション絞り込みは、クライアント種別が transcript から分かるときは (種別, sessionId) の組でサーバーに渡す。
#[test]
fn session_scope_sends_client_source_detected_from_the_transcript() {
    let fixture = Fixture::new();
    let root = std::env::temp_dir().join(format!("hooks-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("remnifold.json"), br#"{"recallMode":"direct"}"#).unwrap();
    let rows = [
        (
            "codex",
            json!({"type":"session_meta","payload":{"id":"fixture-session"}}),
        ),
        (
            "claude-code",
            json!({"type":"user","sessionId":"fixture-session","message":{"role":"user","content":"x"}}),
        ),
    ];
    for (expected, row) in rows {
        let transcript = root.join(format!("{expected}.jsonl"));
        fs::write(&transcript, format!("{row}\n")).unwrap();
        let payload = json!({"session_id":"fixture-session","prompt":"Repair Unicode transcript boundaries","transcript_path":transcript});
        fixture.requests.lock().unwrap().clear();
        hook(
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        let requests = fixture.requests.lock().unwrap();
        let searches: Vec<_> = requests
            .iter()
            .filter(|(path, _)| path == "/v4/search")
            .collect();
        assert!(!searches.is_empty(), "{expected}");
        for (_, body) in searches {
            assert_eq!(body["sessionId"], "fixture-session", "{expected}: {body}");
            assert_eq!(body["sessionSource"], expected, "{expected}: {body}");
        }
    }
    // 種別が分からない（transcript が無い・判定できない）ときは sessionId だけで絞る。
    let unknown = root.join("unknown.jsonl");
    fs::write(&unknown, "{\"type\":\"other\"}\n").unwrap();
    for payload in [
        json!({"session_id":"fixture-session","prompt":"Repair Unicode transcript boundaries"}),
        json!({"session_id":"fixture-session","prompt":"Repair Unicode transcript boundaries","transcript_path":unknown}),
    ] {
        fixture.requests.lock().unwrap().clear();
        hook(
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        let requests = fixture.requests.lock().unwrap();
        for (_, body) in requests.iter().filter(|(path, _)| path == "/v4/search") {
            assert_eq!(body["sessionId"], "fixture-session");
            assert!(body.get("sessionSource").is_none(), "{body}");
        }
    }
}

// Claude Code は再開でコピーされた旧 sessionId も同じ会話として扱い、sessionIds で送って旧 ID の記録も注入する。
// 読み取りは ID の数・行・バイトで抑える。Codex は今の ID のままにする。
#[test]
fn claude_resume_copies_widen_session_scope_with_bounded_ids() {
    let fixture = Fixture::new();
    fixture.mixed.store(true, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("hooks-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("remnifold.json"), br#"{"recallMode":"direct","maxMemories":8}"#).unwrap();
    let row = |kind: &str, id: &str, extra: Value| {
        let mut row = json!({"type":kind,"sessionId":id,"timestamp":"2026-09-01T10:00:00Z","message":{"role":kind,"content":"x"}});
        for (key, value) in extra.as_object().into_iter().flatten() {
            row[key] = value.clone();
        }
        row
    };
    let run = |transcript: &Path| {
        let payload = json!({"session_id":"fixture-session","prompt":"Repair Unicode transcript boundaries","transcript_path":transcript});
        fixture.requests.lock().unwrap().clear();
        let output = hook(
            &root,
            &fixture.url,
            "UserPromptSubmit",
            &serde_json::to_vec(&payload).unwrap(),
        );
        let requests = fixture.requests.lock().unwrap();
        let body = requests
            .iter()
            .find(|(path, _)| path == "/v4/search")
            .map(|(_, body)| body.clone())
            .expect("search request");
        (output.to_string(), body)
    };
    // 旧 ID の過去行（再開でコピーされたもの）に続き、今の ID の行が来る。sidechain・meta・会話以外・不正な ID は使わない。
    let transcript = root.join("resumed.jsonl");
    let rows = [
        row("user", "resumed-old", json!({})),
        row("assistant", "resumed-old", json!({"timestamp":"2026-09-01T10:05:00Z"})),
        row("user", "no-time-old", json!({"timestamp":"not a time"})),
        row("user", "side-only", json!({"isSidechain":true})),
        row("user", "meta-only", json!({"isMeta":true})),
        row("summary", "summary-only", json!({})),
        row("user", "   ", json!({})),
        row("user", &"x".repeat(161), json!({})),
        row("user", "fixture-session", json!({})),
    ];
    let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(&transcript, text).unwrap();
    let (context, body) = run(&transcript);
    assert_eq!(body["sessionIds"], json!(["fixture-session", "resumed-old"]));
    // 旧 ID の時刻の上限は、コピーされた行の timestamp の最大値。今の ID には付かない。
    assert_eq!(body["sessionUntil"], json!({"resumed-old":"2026-09-01T10:05:00Z"}));
    assert!(body.get("sessionId").is_none(), "{body}");
    assert_eq!(body["sessionSource"], "claude-code");
    // 上限ちょうどの旧 ID の記録は入り、上限より後（フォーク後の親の記録）・時刻なしの旧 ID の記録は入らない。
    assert!(context.contains("id=resumed-doc"), "{context}");
    assert!(!context.contains("id=late-doc"), "{context}");
    assert!(!context.contains("id=untimed-doc"), "{context}");
    assert!(context.contains("id=current-doc"), "{context}");
    assert!(!context.contains("stranger-doc"), "{context}");
    // 旧 ID が無い（今の ID だけの）transcript は従来どおり sessionId の 1 件。
    let single = root.join("single.jsonl");
    fs::write(&single, format!("{}\n", row("user", "fixture-session", json!({})))).unwrap();
    let (_, body) = run(&single);
    assert_eq!(body["sessionId"], "fixture-session");
    assert!(body.get("sessionIds").is_none(), "{body}");
    // ID は今の ID を含めて最大 8 件で、transcript の後ろ（今の再開に近い）に現れた旧 ID を優先する。
    let many = root.join("many.jsonl");
    let text: String = (0..12)
        .map(|i| format!("{}\n", row("user", &format!("old-{i:02}"), json!({}))))
        .chain([format!("{}\n", row("user", "fixture-session", json!({})))])
        .collect();
    fs::write(&many, text).unwrap();
    let (_, body) = run(&many);
    let ids: Vec<&str> = body["sessionIds"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(body["sessionUntil"].as_object().unwrap().len(), 7);
    assert_eq!(ids.len(), 8, "{ids:?}");
    assert_eq!(ids[0], "fixture-session");
    assert_eq!(ids[1..], ["old-11", "old-10", "old-09", "old-08", "old-07", "old-06", "old-05"]);
    // 読み取りの上限: 行数を超えた先の旧 ID は読まない（20000 行）。
    let long = root.join("long.jsonl");
    let filler = format!("{}\n", json!({"type":"progress"}));
    let mut text = filler.repeat(20_000);
    text.push_str(&format!("{}\n", row("user", "beyond-limit", json!({}))));
    fs::write(&long, text).unwrap();
    let (_, body) = run(&long);
    assert_eq!(body["sessionId"], "fixture-session");
    assert!(body.get("sessionIds").is_none(), "{body}");
    // Codex の rollout は今の ID のまま（再開時の ID の変化が未確認のため）。
    let codex = root.join("codex.jsonl");
    let text = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"fixture-session"}}),
        row("user", "resumed-old", json!({}))
    );
    fs::write(&codex, text).unwrap();
    let (_, body) = run(&codex);
    assert_eq!(body["sessionId"], "fixture-session");
    assert_eq!(body["sessionSource"], "codex");
    assert!(body.get("sessionIds").is_none(), "{body}");
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
