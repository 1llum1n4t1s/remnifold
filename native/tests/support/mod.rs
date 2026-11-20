// 各 integration binary は必要な共通 fixture だけを利用する。
#![allow(dead_code)]
use std::process::Command;
use std::{
    io::{self, Read, Write},
    process::{Child, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// 非ブロッキングの待受（loopback）から次の接続を受け付け、ブロッキングのソケットで返す。
/// 接続が無い間は `keep_waiting` が true の限り待ち、false になったら None を返す。
/// Windows で accept したソケットは待受の nonblocking を引き継ぎ、読み取りの WouldBlock を EOF と誤る。
/// 明示的に解除して、読み書きの期限を有効にする。
pub fn accept_blocking(
    listener: &std::net::TcpListener,
    mut keep_waiting: impl FnMut() -> bool,
) -> Option<std::net::TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return Some(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if !keep_waiting() {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("loopback accept: {error}"),
        }
    }
}

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
enum Completion {
    Stdout(io::Result<Vec<u8>>),
    Stderr(io::Result<Vec<u8>>),
    Input(io::Result<()>),
}
/// 読取りを先に起動し、stdin・stdout・stderr・終了を同じ期限で待つ。
pub struct BoundedChild {
    child: Option<Child>,
    events: mpsc::Receiver<Completion>,
    deadline: Instant,
}
impl BoundedChild {
    pub fn spawn(command: &mut Command, input: Vec<u8>, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now() + timeout;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let (tx, events) = mpsc::channel();
        let out_tx = tx.clone();
        thread::spawn(move || {
            let result = read_limited(stdout);
            let _ = out_tx.send(Completion::Stdout(result));
        });
        let err_tx = tx.clone();
        thread::spawn(move || {
            let result = read_limited(stderr);
            let _ = err_tx.send(Completion::Stderr(result));
        });
        thread::spawn(move || {
            let result = stdin.write_all(&input);
            drop(stdin);
            let _ = tx.send(Completion::Input(result));
        });
        Ok(Self {
            child: Some(child),
            events,
            deadline,
        })
    }
    pub fn wait(mut self) -> io::Result<Output> {
        let (mut stdout, mut stderr, mut written): (Option<Vec<u8>>, Option<Vec<u8>>, bool) =
            (None, None, false);
        loop {
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native child exceeded its complete I/O deadline",
                ));
            }
            let status = self.child.as_mut().unwrap().try_wait()?;
            if let (Some(status), Some(stdout), Some(stderr), true) =
                (status, stdout.as_ref(), stderr.as_ref(), written)
            {
                let output = Output {
                    status,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                };
                self.child.take();
                return Ok(output);
            }
            let wait = self
                .deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            match self.events.recv_timeout(wait) {
                Ok(Completion::Stdout(result)) => stdout = Some(result?),
                Ok(Completion::Stderr(result)) => stderr = Some(result?),
                Ok(Completion::Input(result)) => {
                    result?;
                    written = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if stdout.is_none() || stderr.is_none() || !written {
                        return Err(io::Error::other("native child I/O worker disconnected"));
                    }
                    thread::sleep(wait);
                }
            }
        }
    }
}
impl Drop for BoundedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
pub fn read_limited(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > OUTPUT_LIMIT {
        return Err(io::Error::other("native child output exceeded 8 MiB limit"));
    }
    Ok(output)
}
pub fn run(command: &mut Command, input: &[u8], timeout: Duration) -> io::Result<Output> {
    BoundedChild::spawn(command, input.to_vec(), timeout)?.wait()
}

/// MCP の送信を有限キューへ渡し、stdin 停止時にも応答期限から子を終了できる。
pub struct QueuedStdin {
    sender: mpsc::SyncSender<Vec<u8>>,
}
impl QueuedStdin {
    pub fn new(mut input: std::process::ChildStdin) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(16);
        thread::spawn(move || {
            while let Ok(bytes) = receiver.recv() {
                if input.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        Self { sender }
    }
    pub fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("MCP fixture message exceeded its limit"));
        }
        self.sender
            .try_send(bytes)
            .map_err(|_| io::Error::other("MCP fixture input queue is full or closed"))
    }
}

pub fn isolate(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if [
            "REMNIFOLD_",
            "KAGAYOI_MEMORY_",
            "SUPERMEMORY_",
            "CLOUDFLARE_MEMORY_",
        ]
        .iter()
        .any(|prefix| upper.starts_with(prefix))
            || ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"].contains(&upper.as_str())
        {
            command.env_remove(key);
        }
    }
    command.env("NO_PROXY", "127.0.0.1,localhost,::1");
    // 人が読む文言は実行環境の言語で変わる。検証は英語に固定し、必要なテストだけが上書きする。
    command.env("REMNIFOLD_LANGUAGE", "en");
    // 既定では Remnifold 自身の設定・状態を毎回新しい一時フォルダへ隔離し、実環境の設定フォルダ
    // （%APPDATA%\Remnifold 等）を読み書きしないようにする。テスト側で必要なら後から上書きする。
    command.env(
        "REMNIFOLD_HOME",
        std::env::temp_dir().join(format!("remnifold-test-home-{}", uuid::Uuid::new_v4())),
    );
}
