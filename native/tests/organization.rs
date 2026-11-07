use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn invoke(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_remnifold"))
        .arg("memory-organize")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Private mode may close the pipe immediately, before any input is consumed.
    let _ = stdin.write_all(input);
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("organization stdin subprocess did not terminate");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn private_stdin_rejects_redirected_input_before_readiness() {
    for action in ["commit", "lookup"] {
        let result = invoke(
            &[action, "--line", "--private-stdin"],
            b"{private-secret}\n",
        );
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert_eq!(
            String::from_utf8(result.stderr).unwrap().trim(),
            "Organization private stdin requires a Windows console."
        );
    }
}

#[test]
fn existing_line_and_eof_input_keep_redacted_errors() {
    for args in [vec!["commit"], vec!["lookup", "--line"]] {
        let result = invoke(&args, "{非公開🌸private-secret}\n".as_bytes());
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert_eq!(
            String::from_utf8(result.stderr).unwrap().trim(),
            "Organization stdin must contain one JSON object."
        );
    }
    let result = invoke(&["commit", "--line"], b"{}");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8(result.stderr).unwrap().trim(),
        "Organization line input must end with a newline."
    );
}
