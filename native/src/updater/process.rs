//! Windows は実行開始前に Job へ割り当て、子孫全体を同じ寿命へ束縛する。
//! Unix は独立 process group を止める。setsid/setpgid で離脱した子孫までは保証しない。
use anyhow::{Result, bail, ensure};
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
pub(super) struct Tree {
    _job: std::os::windows::io::OwnedHandle,
}
#[cfg(unix)]
pub(super) struct Tree {
    pid: u32,
}

impl Tree {
    pub(super) fn prepare(command: &mut Command) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000004);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
    }
    pub(super) fn attach(child: &mut std::process::Child) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
            use windows_sys::Win32::{
                Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
                System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
            };
            let result = (|| unsafe {
                let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                ensure!(!raw.is_null(), "CLI_JOB_FAILED");
                let job = OwnedHandle::from_raw_handle(raw);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                ensure!(
                    SetInformationJobObject(
                        raw,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32
                    ) != 0
                        && AssignProcessToJobObject(raw, child.as_raw_handle()) != 0,
                    "CLI_JOB_FAILED"
                );
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                ensure!(snapshot != INVALID_HANDLE_VALUE, "CLI_JOB_FAILED");
                let snapshot = OwnedHandle::from_raw_handle(snapshot);
                let mut entry: THREADENTRY32 = std::mem::zeroed();
                entry.dwSize = std::mem::size_of_val(&entry) as u32;
                let mut found = false;
                let mut next = Thread32First(snapshot.as_raw_handle(), &mut entry);
                while next != 0 {
                    if entry.th32OwnerProcessID == child.id() {
                        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                        ensure!(!thread.is_null(), "CLI_JOB_FAILED");
                        let resumed = ResumeThread(thread);
                        CloseHandle(thread);
                        ensure!(resumed != u32::MAX, "CLI_JOB_FAILED");
                        found = true;
                        break;
                    }
                    next = Thread32Next(snapshot.as_raw_handle(), &mut entry);
                }
                ensure!(found, "CLI_JOB_FAILED");
                Ok(Self { _job: job })
            })();
            if result.is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
            result
        }
        #[cfg(unix)]
        {
            Ok(Self { pid: child.id() })
        }
    }
}

#[cfg(unix)]
impl Drop for Tree {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // process_group(0) で作った、この呼出し所有のグループだけを止める。
        unsafe {
            kill(-(self.pid as i32), 9);
        }
    }
}

pub(crate) fn run(mut command: Command, timeout: Duration, limit: u64) -> Result<Vec<u8>> {
    ensure!(limit < u64::MAX, "INVALID_COMMAND_LIMIT");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    Tree::prepare(&mut command);
    let mut child = command.spawn()?;
    let _tree = Tree::attach(&mut child)?;
    fn read_pipe(
        pipe: impl Read + Send + 'static,
        limit: u64,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (sender, reader) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.take(limit + 1).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        reader
    }
    let output = read_pipe(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("CLI_PIPE_FAILED"))?,
        limit,
    );
    let errors = read_pipe(
        child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("CLI_PIPE_FAILED"))?,
        limit,
    );
    let deadline = Instant::now() + timeout;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        for (channel, target) in [(&output, &mut stdout), (&errors, &mut stderr)] {
            if target.is_none() {
                match channel.try_recv() {
                    Ok(result) => {
                        let bytes = result?;
                        if bytes.len() as u64 > limit {
                            let _ = child.kill();
                            let _ = child.wait();
                            bail!("CLI_OUTPUT_TOO_LARGE");
                        }
                        *target = Some(bytes);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(_) => bail!("CLI_PIPE_FAILED"),
                }
            }
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "CLI_FAILED");
            if stdout.is_some() && stderr.is_some() {
                return Ok(stdout.unwrap());
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("CLI_TIMEOUT");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    #[ignore]
    fn output_fixture() {
        use std::io::Write;
        let bytes = vec![b'x'; 32768];
        if std::env::args().any(|arg| arg == "--show-output") {
            std::io::stderr().write_all(&bytes).unwrap();
        } else {
            std::io::stdout().write_all(&bytes).unwrap();
        }
        thread::sleep(Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn tree_fixture() {
        if !env::args().any(|arg| arg == "--show-output") {
            let fixture = format!(
                "{}::tree_fixture",
                module_path!().split_once("::").unwrap().1
            );
            let child = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    &fixture,
                    "--ignored",
                    "--nocapture",
                    "--show-output",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("descendant:{}", child.id());
        }
        thread::sleep(Duration::from_secs(30));
    }

    #[cfg(windows)]
    #[test]
    fn job_close_stops_descendant_not_only_direct_child() {
        use std::io::BufRead;
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, WaitForSingleObject},
        };
        let fixture = format!(
            "{}::tree_fixture",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", &fixture, "--ignored", "--nocapture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Tree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let tree = Tree::attach(&mut child).unwrap();
        let pipe = child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            for line in std::io::BufReader::new(pipe).lines().take(20) {
                if let Ok(line) = line {
                    if let Some(pid) = line.strip_prefix("descendant:") {
                        let _ = send.send(pid.parse::<u32>().unwrap());
                        break;
                    }
                }
            }
        });
        let pid = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        let descendant = unsafe { OpenProcess(0x00100000, 0, pid) };
        assert!(!descendant.is_null());
        drop(tree);
        assert_eq!(unsafe { WaitForSingleObject(descendant, 3000) }, 0);
        unsafe { CloseHandle(descendant) };
        child.wait().unwrap();
    }
    #[test]
    fn both_streams_are_bounded_before_child_exit() {
        let command = vec![env::current_exe().unwrap().to_string_lossy().into_owned()];
        for stderr in [false, true] {
            let fixture = format!(
                "{}::output_fixture",
                module_path!().split_once("::").unwrap().1
            );
            let mut args = strings(&["--exact", &fixture, "--ignored", "--nocapture"]);
            if stderr {
                args.push("--show-output".into());
            }
            let start = Instant::now();
            let mut child = Command::new(&command[0]);
            child.args(&args);
            let error = run(child, Duration::from_secs(3), 1024).unwrap_err();
            assert_eq!(error.to_string(), "CLI_OUTPUT_TOO_LARGE");
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }
}
