use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
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
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn client(home: &Fixture) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_remnifold"));
    cmd.env("CODEX_HOME", &home.0);
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
    let mut child = client(&home)
        .args(["setup", "--base-url", &endpoint, "--api-key-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let key = "fixture-owner-private-key";
    child
        .stdin
        .take()
        .unwrap()
        .write_all(key.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
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
    let again = client(&home)
        .args(["setup", "--base-url", &endpoint, "--api-key-stdin"])
        .stdin(Stdio::null())
        .output()
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
    let bad = client(&home)
        .args([
            "setup",
            "--base-url",
            "https://user:do-not-log@example.com?api_key=do-not-log",
            "--api-key-stdin",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(!String::from_utf8_lossy(&bad.stderr).contains("do-not-log"));
    assert!(!home.0.join("remnifold.json").exists());
    let mut child = client(&home)
        .arg("capture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"private\":\"do-not-log\"")
        .unwrap();
    let output = child.wait_with_output().unwrap();
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
    let help = client(&home)
        .env("PATH", &home.0)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let mut child = client(&home)
        .env("PATH", &home.0)
        .args(["hook", "SessionStart"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}
