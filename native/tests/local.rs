mod support;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("remnifold-local-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn client(home: &Fixture) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    support::isolate(&mut cmd);
    cmd.env("CODEX_HOME", &home.0);
    // Remnifold 自身の設定も同じフォルダに隔離する（実環境の設定フォルダへ書かない）。
    cmd.env("REMNIFOLD_HOME", &home.0);
    for name in [
        "REMNIFOLD_API_URL",
        "REMNIFOLD_API_KEY",
        "KAGAYOI_MEMORY_API_URL",
        "KAGAYOI_MEMORY_API_KEY",
        "SUPERMEMORY_API_URL",
        "SUPERMEMORY_CODEX_API_KEY",
        "CLOUDFLARE_MEMORY_API_KEY",
    ] {
        cmd.env_remove(name);
    }
    cmd
}
fn once_server() -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(start.elapsed().as_secs() < 10, "client did not connect");
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let count = socket.read(&mut buf).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..count]);
            if bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                break;
            }
        }
        let body = r#"{"user":{"id":"owner"},"role":"owner"}"#;
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (endpoint, server)
}

#[test]
fn setup_legacy_state_and_secret_safe_output() {
    let home = Fixture::new();
    let (endpoint, server) = once_server();
    let secret = "fixture-private-key-日本語";
    let prior = json!({"baseUrl":endpoint,"apiKey":secret,"maxMemories":4,"maxRecentTasks":0,"readContainerTags":["legacy-project"],"unknownSetting":{"retained":true}});
    fs::write(
        home.0.join("supermemory.json"),
        serde_json::to_vec(&prior).unwrap(),
    )
    .unwrap();
    // HTTP header credentials are ASCII. The old value remains intact until verification succeeds.
    let key = "fixture-owner-private-key";
    let output = support::run(
        client(&home).args(["setup", "--base-url", &endpoint, "--api-key-stdin"]),
        key.as_bytes(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /v3/session "));
    assert!(
        request
            .to_lowercase()
            .contains(&format!("authorization: bearer {key}"))
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(key));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(key));
    let saved: Value =
        serde_json::from_slice(&fs::read(home.0.join("remnifold.json")).unwrap()).unwrap();
    assert_eq!(saved["apiKey"], key);
    assert_eq!(saved["maxMemories"], 4);
    assert_eq!(saved["maxRecentTasks"], 0);
    assert_eq!(saved["unknownSetting"]["retained"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(home.0.join("supermemory.json")).unwrap())
            .unwrap(),
        prior
    );
    let again = support::run(
        client(&home).args(["setup", "--base-url", &endpoint, "--api-key-stdin"]),
        b"",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(!again.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(home.0.join("remnifold.json")).unwrap()).unwrap(),
        saved
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.0.join("remnifold.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_endpoint_and_capture_payload_do_not_print_secrets() {
    let home = Fixture::new();
    let bad = support::run(
        client(&home).args([
            "setup",
            "--base-url",
            "https://user:do-not-log@example.com?api_key=do-not-log",
            "--api-key-stdin",
        ]),
        b"",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(!bad.status.success());
    assert!(!String::from_utf8_lossy(&bad.stderr).contains("do-not-log"));
    assert!(!home.0.join("remnifold.json").exists());
    let output = support::run(
        client(&home).arg("capture"),
        b"{\"private\":\"do-not-log\"",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Remnifold capture failed; unacknowledged records will be retried. [stage=payload; cause=invalid; action=repair-required]\n"
    );
}

#[test]
fn native_help_and_no_config_hook_require_no_node() {
    let home = Fixture::new();
    let help = support::run(
        client(&home).env("PATH", &home.0).arg("--help"),
        b"",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(help.status.success());
    let output = support::run(
        client(&home)
            .env("PATH", &home.0)
            .args(["hook", "SessionStart"]),
        b"{}",
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}
#[test]
fn blocked_stdin_and_full_output_pipes_cannot_escape_the_operation_deadline() {
    const FIXTURE: &str = "REMNIFOLD_BLOCKED_IO_FIXTURE";
    if std::env::var(FIXTURE).as_deref() == Ok("1") {
        std::io::stdout()
            .write_all(&vec![b'x'; 128 * 1024])
            .unwrap();
        std::io::stderr()
            .write_all(&vec![b'y'; 128 * 1024])
            .unwrap();
        thread::sleep(Duration::from_secs(30));
        return;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    support::isolate(&mut command);
    command
        .args([
            "--exact",
            "blocked_stdin_and_full_output_pipes_cannot_escape_the_operation_deadline",
            "--nocapture",
        ])
        .env(FIXTURE, "1");
    let started = Instant::now();
    let error = support::run(
        &mut command,
        &vec![b'x'; 16 * 1024 * 1024],
        Duration::from_millis(500),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}
