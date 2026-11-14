//! 導入先から動いている、自分以外のプロセスの検出。
//!
//! Velopack の適用（`Update.exe apply`）は、導入先から起動している自分以外の全プロセスを強制終了する。
//! 別セッションの `mcp` や、実行中の `hook` を巻き込まないよう、1つでも見つかったら適用しない
//! （強制終了はしない。次のログオンに回す）。
//!
//! # Linux（AppImage）
//! AppImage から起動したプロセスの `/proc/<pid>/exe` は、AppImage 本体ではなく、起動のたびに変わる
//! マウント先（FUSE の `/tmp/.mount_XXXXXX/usr/bin/remnifold`）や展開先（`APPIMAGE_EXTRACT_AND_RUN` の
//! `$TMPDIR/appimage_extracted_<hash>/usr/bin/remnifold`）を指すため、導入先のパスの前方一致では判定できない。
//! そこで Velopack 自身が「自分の AppImage」を知るのと同じ手掛かり、AppImage のランタイムが起動時に設定する
//! 環境変数 `APPIMAGE`（AppImage 本体の絶対パス）を `/proc/<pid>/environ` から読み、自分の AppImage と
//! 一致するプロセスを「同じ導入から動くもの」とみなす（velopack crate 1.2.161 の locator も `$APPIMAGE` で
//! AppImage を特定する）。次のプロセスは数えない。
//! - 自分自身と、自分の祖先（PPid の連鎖）。AppImage の AppRun（vpk が生成するシェルスクリプト）は `exec` で
//!   主実行ファイルへ置き換わるが、別のシェル経由の起動などで `APPIMAGE` を持つ親が残っても、自分の起動の
//!   一部なので除く。
//! - `/proc/<pid>/exe` が AppImage 本体そのものを指すプロセス。これは AppImage のランタイム（FUSE を提供する
//!   子や、展開して実行する場合に子の終了を待つ親）で、アプリの処理は行わない。自分の起動にも必ず付いて来る
//!   ため、数えると常に busy になってしまう。
//!
//! `APPIMAGE` は子プロセスへ引き継がれるため、Remnifold が起動した子（CLI など）も数える。これは「何かが
//! 動いていれば適用しない」安全側への誤検出として受け入れる。他の利用者のプロセスの environ は読めないので
//! 読み飛ばす（AppImage は利用者のものなので、自分の Remnifold は必ず読める）。
//! なお Linux の適用（UpdateNix）は AppImage を `mv -f` で置き換えるだけで、他のプロセスを止めない
//! （velopack リポジトリ 1.2.161 の `apply_linux_impl.rs`）。それでも Windows・macOS と同じく、他のプロセスが
//! 動いている間は適用しない（本人の方針。動作中のプロセスは古い版のまま動き続けるため、版を混ぜない）。
use std::path::Path;
#[cfg(not(target_os = "linux"))]
use std::path::PathBuf;

/// 比較用の正規化（拡張パスの接頭辞を外し、区切りを `/`、小文字にする。ここで扱う導入先は
/// Windows では大小を区別しないため、他の OS でも同じ正規化で十分に安全側へ倒れる）。
/// Linux は AppImage の判定（`APPIMAGE` の一致）を使うため、前方一致の判定は単体テストだけで使う。
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("//?/")
        .unwrap_or(&text)
        .to_ascii_lowercase()
}

/// `candidate` が `root` の配下か（`root` 自身は含めない。`/a/b` と `/a/bc` を取り違えない）。
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn is_under(root: &Path, candidate: &Path) -> bool {
    let mut root = normalize(root);
    if !root.ends_with('/') {
        root.push('/');
    }
    normalize(candidate).starts_with(&root)
}

/// 導入先 `root` の配下の実行ファイルから動いている、`own_pid` 以外のプロセスの ID。
/// 列挙できないとき（権限不足など個々のプロセスの失敗は読み飛ばす）。全体が失敗したら Err。
#[cfg(not(target_os = "linux"))]
pub fn others_in(root: &Path, own_pid: u32) -> std::io::Result<Vec<u32>> {
    // macOS の proc_pidpath はリンクを解決した実体のパスを返す。導入先のパスにリンク（/var → /private/var
    // など）が含まれていると前方一致が外れ、「誰も動いていない」と誤って適用してしまうため、実体へ揃える。
    #[cfg(target_os = "macos")]
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    #[cfg(target_os = "macos")]
    let root = canonical.as_path();
    Ok(list_processes()?
        .into_iter()
        .filter(|(pid, image)| *pid != own_pid && is_under(root, image))
        .map(|(pid, _)| pid)
        .collect())
}

#[cfg(windows)]
fn list_processes() -> std::io::Result<Vec<(u32, PathBuf)>> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
    };
    // SAFETY: Win32 の列挙 API。構造体は dwSize を設定して渡し、ハンドルは必ず閉じる。
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let pid = entry.th32ProcessID;
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if !handle.is_null() {
                let mut buffer = vec![0u16; 32 * 1024];
                let mut length = buffer.len() as u32;
                if QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) != 0 {
                    found.push((
                        pid,
                        PathBuf::from(OsString::from_wide(&buffer[..length as usize])),
                    ));
                }
                CloseHandle(handle);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Ok(found)
    }
}

/// Linux: AppImage `root`（AppImage 本体のパス）から動いている、`own_pid` とその祖先以外のプロセスの ID。
/// 判定の方法はモジュールの説明のとおり（`APPIMAGE` の一致。AppImage 本体を exe とするランタイムは除く）。
/// `/proc` を列挙できなければ Err（呼び出し元は適用しない）。個々のプロセスの読み取りの失敗は読み飛ばす。
#[cfg(target_os = "linux")]
pub fn others_in(root: &Path, own_pid: u32) -> std::io::Result<Vec<u32>> {
    use std::os::unix::ffi::OsStrExt;
    // 比較は実体のパスで行う（AppImage を指すリンク経由で起動されても、ランタイムは実体のパスを設定する）。
    let target = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let excluded = ancestors(own_pid);
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if excluded.contains(&pid) {
            continue;
        }
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        let Some(appimage) = environ_value(&environ, b"APPIMAGE") else {
            continue;
        };
        let appimage = Path::new(std::ffi::OsStr::from_bytes(appimage));
        let appimage = std::fs::canonicalize(appimage).unwrap_or_else(|_| appimage.to_path_buf());
        if appimage != target {
            continue;
        }
        // AppImage のランタイム（exe が AppImage 本体）は数えない。exe が読めないもの（終了の途中など）は、
        // `APPIMAGE` が一致している以上、安全側に数える。
        let runtime = std::fs::read_link(entry.path().join("exe"))
            .is_ok_and(|exe| std::fs::canonicalize(&exe).unwrap_or(exe) == target);
        if !runtime {
            found.push(pid);
        }
    }
    Ok(found)
}

/// `pid` 自身と、その祖先（`/proc/<pid>/stat` の PPid を init の手前まで辿る）の ID。読めなくなった所で止める。
#[cfg(target_os = "linux")]
fn ancestors(pid: u32) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut current = pid;
    // 循環や異常な深さに備えて上限を設ける。
    for _ in 0..64 {
        let Some(parent) = std::fs::read(format!("/proc/{current}/stat"))
            .ok()
            .and_then(|stat| parent_pid(&stat))
        else {
            break;
        };
        if parent <= 1 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

/// `/proc/<pid>/environ`（NUL 区切りの `KEY=VALUE`）から、指定のキーの値を取り出す。空の値は無いものとする。
/// OS に依存しない純関数にして、どの OS の単体テストでも確かめる。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn environ_value<'a>(environ: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    environ.split(|byte| *byte == 0).find_map(|pair| {
        pair.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(b"="))
            .filter(|value| !value.is_empty())
    })
}

/// `/proc/<pid>/stat` から親の PID（4番目の項目）を取り出す。2番目の項目（実行名）は括弧で囲まれ、
/// 空白や括弧を含みうるため、最後の `)` の後ろから数える。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parent_pid(stat: &[u8]) -> Option<u32> {
    let close = stat.iter().rposition(|byte| *byte == b')')?;
    let rest = std::str::from_utf8(&stat[close + 1..]).ok()?;
    // 残りは「 <状態> <PPid> ...」。
    rest.split_ascii_whitespace().nth(1)?.parse().ok()
}

/// macOS は libproc（libSystem に含まれる）で全プロセスの ID と実行ファイルのパスを得る。
/// `ps` の出力解析と違い、空白を含むパスや表示幅の切り詰めに左右されない。
/// 他の利用者のプロセスなどパスを読めないものは読み飛ばす（導入先は利用者のものなので、
/// 自分の Remnifold は必ず読める）。
#[cfg(target_os = "macos")]
fn list_processes() -> std::io::Result<Vec<(u32, PathBuf)>> {
    use std::{ffi::OsStr, os::raw::c_int, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut c_int, buffersize: c_int) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut u8, buffersize: u32) -> c_int;
    }
    /// <sys/proc_info.h> の PROC_PIDPATHINFO_MAXSIZE（4 * MAXPATHLEN）。
    const PATH_MAX_SIZE: usize = 4 * 1024;
    // 数の問い合わせと列挙の間にプロセスが増えても取りこぼさないよう、余裕を持って確保し、
    // 埋まりきったら大きくして取り直す。
    // SAFETY: バッファを null にすると、必要な件数だけが返る。
    let mut capacity = (unsafe { proc_listallpids(std::ptr::null_mut(), 0) }).max(256) as usize * 2;
    let pids = loop {
        let mut buffer = vec![0 as c_int; capacity];
        // SAFETY: buffer は capacity 個の c_int を書き込める領域で、バイト数を渡す。
        let count = unsafe {
            proc_listallpids(
                buffer.as_mut_ptr(),
                (capacity * std::mem::size_of::<c_int>()) as c_int,
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let count = count as usize;
        if count < capacity {
            buffer.truncate(count);
            break buffer;
        }
        capacity *= 2;
    };
    let mut found = Vec::with_capacity(pids.len());
    let mut path = vec![0u8; PATH_MAX_SIZE];
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        // SAFETY: path は PATH_MAX_SIZE バイトの書き込み可能な領域。戻り値は書いたバイト数（0 以下は失敗）。
        let length = unsafe { proc_pidpath(pid, path.as_mut_ptr(), PATH_MAX_SIZE as u32) };
        if length > 0 {
            found.push((
                pid as u32,
                PathBuf::from(OsStr::from_bytes(&path[..length as usize])),
            ));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_matching_is_directory_aware_and_case_insensitive() {
        let root = Path::new(r"C:\Users\a\AppData\Local\RemnifoldApp");
        assert!(is_under(
            root,
            Path::new(r"C:\Users\a\AppData\Local\RemnifoldApp\current\remnifold.exe")
        ));
        assert!(is_under(
            root,
            Path::new(r"c:\users\A\appdata\local\remnifoldapp\Update.exe")
        ));
        assert!(is_under(
            root,
            Path::new(r"\\?\C:\Users\a\AppData\Local\RemnifoldApp\current\x.exe")
        ));
        // 名前が前方一致するだけの別フォルダーと、導入先そのものは対象外。
        assert!(!is_under(
            root,
            Path::new(r"C:\Users\a\AppData\Local\RemnifoldApp2\x.exe")
        ));
        assert!(!is_under(root, root));
    }

    /// 導入先（.app）配下から動いている別プロセスを、リンクを含むパスで指定しても見つけられる。
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_process_inside_the_bundle_is_detected() {
        let base = std::env::temp_dir().join(format!("remnifold-procs-{}", uuid::Uuid::new_v4()));
        let bundle = base.join("Test App.app");
        let bin = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy("/bin/sleep", bin.join("sleep")).unwrap();
        // 導入先へのシンボリックリンク経由で指定しても検出できること（実体のパスへ揃える）。
        let link = base.join("linked.app");
        std::os::unix::fs::symlink(&bundle, &link).unwrap();
        let mut child = std::process::Command::new(bin.join("sleep"))
            .arg("30")
            .spawn()
            .unwrap();
        let found = (0..50).find_map(|_| {
            let pids = others_in(&link, std::process::id()).unwrap();
            if pids.contains(&child.id()) {
                Some(pids)
            } else {
                std::thread::sleep(std::time::Duration::from_millis(100));
                None
            }
        });
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.is_some(), "the child process was not detected");
        // 自分自身は除外される。
        assert!(others_in(&bundle, child.id()).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn environ_and_stat_are_parsed_safely() {
        let environ =
            b"PATH=/usr/bin\0APPIMAGE_EXTRACT_AND_RUN=1\0APPIMAGE=/home/a b/R.AppImage\0EMPTY=\0";
        assert_eq!(
            environ_value(environ, b"APPIMAGE"),
            Some(&b"/home/a b/R.AppImage"[..])
        );
        // 前方一致するだけの別のキーや、空の値は取らない。
        assert_eq!(environ_value(environ, b"APPIMAGE_EXTRACT"), None);
        assert_eq!(environ_value(environ, b"EMPTY"), None);
        assert_eq!(environ_value(b"", b"APPIMAGE"), None);
        // 実行名に空白や括弧を含んでも、最後の `)` の後ろから PPid を読む。
        assert_eq!(parent_pid(b"123 (a) b) c) S 456 123 123 0"), Some(456));
        assert_eq!(parent_pid(b"1 (init) S 0 1 1"), Some(0));
        assert_eq!(parent_pid(b"garbage"), None);
    }

    /// `APPIMAGE` が自分の AppImage と一致する別のプロセスを検出し、別の AppImage のものは数えない。
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_started_from_the_same_appimage_is_detected() {
        let base = std::env::temp_dir().join(format!("remnifold-procs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let mine = base.join("Remnifold.AppImage");
        let other = base.join("Other.AppImage");
        std::fs::write(&mine, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        // AppImage を指すリンクで指定しても、実体のパスで照合する。
        let link = base.join("linked.AppImage");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        let spawn = |appimage: &Path| {
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .env("APPIMAGE", appimage)
                .spawn()
                .unwrap()
        };
        let mut same = spawn(&mine);
        let mut different = spawn(&other);
        let found = (0..50).find_map(|_| {
            let pids = others_in(&link, std::process::id()).unwrap();
            if pids.contains(&same.id()) {
                Some(pids)
            } else {
                std::thread::sleep(std::time::Duration::from_millis(100));
                None
            }
        });
        let found = found.expect("the child process was not detected");
        assert!(!found.contains(&different.id()));
        // 自分自身（と祖先）は除外される。
        assert!(!others_in(&mine, same.id()).unwrap().contains(&same.id()));
        for child in [&mut same, &mut different] {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn own_process_is_excluded_and_unrelated_roots_are_empty() {
        let exe = std::env::current_exe().unwrap();
        let root = exe.parent().unwrap();
        // 自分自身の PID を除けば、この実行ファイルのフォルダーに他のプロセスが居る保証は無いが、
        // 自分が検出に含まれないことは保証できる。
        let others = others_in(root, std::process::id()).unwrap();
        assert!(!others.contains(&std::process::id()));
        assert!(
            others_in(Path::new("/definitely/not/a/running/root"), 0)
                .unwrap()
                .is_empty()
        );
    }
}
