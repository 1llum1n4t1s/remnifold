use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
}
impl Mcp {
    fn send(&mut self, value: Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
    }
    fn receive(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(10))
            .expect("bounded MCP response")
    }
    fn exchange(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let response = self.receive();
        assert_eq!(response["id"], id);
        response
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn real_mcp_transport_preserves_protocol_storage_and_roots() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let (requests_tx, requests_rx) = mpsc::channel();
    let fixture = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(25);
        for _ in 0..5 {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(value) => break value,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            let mut authenticated = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    if name.eq_ignore_ascii_case("authorization") {
                        authenticated = value.trim() == "Bearer local-mcp-fixture-key";
                    }
                }
            }
            assert!(authenticated);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&body).unwrap()
            };
            let response = if first.starts_with("POST /v3/documents ") {
                json!({"id":"saved-id","status":"created"})
            } else if first.starts_with("DELETE /v4/memories ") {
                json!({"id":"saved-id","message":"Forgotten"})
            } else if first.starts_with("POST /v4/consolidate ") {
                json!({"status":"consolidated","sourceCount":3})
            } else if first.starts_with("GET /v3/session ") {
                json!({"user":{"id":"owner","name":"Local"},"role":"owner","accessType":"api-key"})
            } else if first.starts_with("GET /v3/container-tags") {
                json!({"spaces":[{"containerTag":"memories","documentCount":1}],"pagination":{"hasMore":false}})
            } else {
                panic!("unexpected fixture route {first}");
            };
            requests_tx.send((first, body)).unwrap();
            let encoded = response.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{encoded}",encoded.len()).unwrap();
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_remnifold"))
        .arg("mcp")
        .env("REMNIFOLD_API_URL", endpoint)
        .env("REMNIFOLD_API_KEY", "local-mcp-fixture-key")
        .env(
            "CODEX_HOME",
            std::env::temp_dir().join("remnifold-mcp-unused-config"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if let Ok(value) = serde_json::from_str(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let mut mcp = Mcp {
        child,
        input,
        output: rx,
    };
    let initialize = mcp.exchange(
        1,
        "initialize",
        json!({"capabilities":{"roots":{}},"protocolVersion":"2099-01-01"}),
    );
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        initialize["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        mcp.exchange(2, "tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    assert_eq!(mcp.exchange(3, "ping", json!({}))["result"], json!({}));
    assert_eq!(
        mcp.exchange(4, "resources/list", json!({}))["result"],
        json!({"resources":[]})
    );
    assert_eq!(
        mcp.exchange(5, "prompts/list", json!({}))["result"],
        json!({"prompts":[]})
    );
    assert_eq!(
        mcp.exchange(6, "unknown", json!({}))["error"]["code"],
        -32601
    );
    writeln!(mcp.input.as_mut().unwrap(), "{{bad-json").unwrap();
    assert_eq!(mcp.receive()["error"]["code"], -32700);
    mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    let source = std::env::current_dir().unwrap().canonicalize().unwrap();
    let save = mcp.exchange(7,"tools/call",json!({"name":"add_memory","arguments":{"content":"日本語の保存内容","sourceFolder":source.to_string_lossy()}}));
    assert_eq!(save["result"]["structuredContent"]["id"], "saved-id");
    let (_, body) = requests_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(body["containerTag"], "memories");
    assert_eq!(body["metadata"]["sm_scope"], "project");
    assert!(
        body["metadata"]["sm_project_id"]
            .as_str()
            .unwrap()
            .starts_with("repo_")
    );
    assert_eq!(
        body["metadata"]["memoryIndex"]["sourceKind"],
        "explicit-memory"
    );
    let forget = mcp.exchange(
        8,
        "tools/call",
        json!({"name":"add_memory","arguments":{"action":"forget","documentId":" saved-id "}}),
    );
    assert_eq!(forget["result"]["structuredContent"]["action"], "forget");
    assert_eq!(
        requests_rx.recv_timeout(Duration::from_secs(5)).unwrap().1,
        json!({"containerTag":"memories","documentId":"saved-id"})
    );
    let consolidation = mcp.exchange(
        9,
        "tools/call",
        json!({"name":"consolidate_memory","arguments":{"projectId":"repo-explicit"}}),
    );
    assert_eq!(
        consolidation["result"]["structuredContent"]["sourceCount"],
        3
    );
    assert_eq!(
        requests_rx.recv_timeout(Duration::from_secs(5)).unwrap().1,
        json!({"projectId":"repo-explicit","force":true})
    );
    mcp.send(json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"whoAmI","arguments":{}}}));
    let roots = mcp.receive();
    assert_eq!(roots["method"], "roots/list");
    mcp.send(json!({"jsonrpc":"2.0","id":roots["id"],"result":{"roots":[{"uri":url::Url::from_directory_path(source).unwrap().as_str()}]}}));
    let identity = mcp.receive();
    assert_eq!(identity["id"], 10);
    assert_eq!(
        identity["result"]["structuredContent"]["workspaceSource"],
        "mcp-roots"
    );
    assert_eq!(
        identity["result"]["structuredContent"]["defaultStorageContainer"],
        "memories"
    );
    fixture.join().unwrap();
    let error = mcp.exchange(11,"tools/call",json!({"name":"add_memory","arguments":{"content":"local-mcp-fixture-key","sourceFolder":"relative/local-mcp-fixture-key"}}));
    assert_eq!(error["result"]["isError"], true);
    assert!(!error.to_string().contains("local-mcp-fixture-key"));
}
