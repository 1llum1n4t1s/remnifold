//! ログオン時に更新の適用（`remnifold update-apply`）を起動する登録。
//!
//! 多プロセス型の Remnifold は、日中に適用すると他のセッションの MCP を強制終了してしまうため
//! （Velopack の適用は導入先の自分以外の全プロセスを止める）、適用はログオン時だけに行う。
//! その起動点をここで登録・解除する。
//!
//! - Windows: HKCU の Run キーに値を1つ置く。値名は packId に紐づけ（`<packId> Update`）、
//!   別の導入先を指す同名の値は上書きも削除もしない（同じ packId の別の導入や、他の製品の値を壊さない）。
//! - macOS: `~/Library/LaunchAgents/com.kagayoi.<packId>.update.plist` を置く（`RunAtLoad` で
//!   ログイン時に1回だけ `update-apply` を起動する）。Windows と同じく、別の導入先を指す同名の plist は
//!   上書きも削除もしない。登録時に `launchctl bootstrap` はしない（すぐに起動させず、次のログインで
//!   読み込ませるため）。解除時は、読み込み済みなら `launchctl bootout` してから plist を消す。
//! - Linux: XDG autostart（`$XDG_CONFIG_HOME/autostart`、未設定・相対なら `~/.config/autostart`）に
//!   `com.kagayoi.<packId>.update.desktop` を置く。デスクトップ環境がログイン時に1回だけ
//!   `<AppImage> update-apply` を起動する。起動するのは AppImage 本体（マウント先の実行ファイルは起動ごとに
//!   消えるため）。他の OS と同じく、別の導入先を指す同名のファイルは上書きも削除もしない。
use std::path::Path;

/// 登録の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Registration {
    /// 値を新しく作った。
    Created,
    /// 自分の導入先を指す既存の値を、最新の定義へ更新した。
    Updated,
    /// 既に同じ定義だった。
    Unchanged,
    /// 別の導入先を指す同名の値があるため、触らなかった。
    ForeignKept,
}

/// Run キーの値名。packId に紐づけ、他の製品や別の packId の値と衝突しない。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn value_name(app_id: &str) -> String {
    format!("{app_id} Update")
}

/// Run キーに書くコマンド。
///
/// remnifold.exe はコンソール版のため、そのまま起動するとログオンのたびにコンソールが一瞬出る。
/// `conhost.exe --headless` 経由にして、ウィンドウを作らずに起動する。
/// パスは二重引用符で囲む（空白を含む導入先でも1つの引数になる）。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn run_command(executable: &Path) -> String {
    let conhost = std::env::var_os("SystemRoot")
        .map(|root| Path::new(&root).join("System32").join("conhost.exe"))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "conhost.exe".to_string());
    format!(
        "\"{conhost}\" --headless \"{}\" update-apply",
        executable.display()
    )
}

/// 既存の値が、この導入先の実行ファイルを指しているか（引用符で囲んだパスを大小無視で含むか）。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn points_at(command: &str, executable: &Path) -> bool {
    let needle = format!("\"{}\"", executable.display()).to_ascii_lowercase();
    command.to_ascii_lowercase().contains(&needle)
}

/// 登録する。値が無ければ作り、自分を指す値なら更新し、別の場所を指す値は触らない。
pub fn register(app_id: &str, executable: &Path) -> std::io::Result<Registration> {
    #[cfg(windows)]
    {
        windows::register_in(
            &windows::RegKey::current_user(),
            windows::RUN_SUBKEY,
            app_id,
            executable,
        )
    }
    #[cfg(target_os = "macos")]
    {
        macos::register_in(&macos::launch_agents_dir(), app_id, executable)
    }
    #[cfg(target_os = "linux")]
    {
        xdg::register_in(&xdg::autostart_dir(), app_id, executable)
    }
}

/// 登録先の場所（診断・案内の表示用）。Windows は Run キーの値名、macOS は plist、Linux は .desktop のパス。
pub fn describe(app_id: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"HKCU\{}\{}", windows::RUN_SUBKEY, value_name(app_id))
    }
    #[cfg(target_os = "macos")]
    {
        macos::plist_path(&macos::launch_agents_dir(), app_id)
            .display()
            .to_string()
    }
    #[cfg(target_os = "linux")]
    {
        xdg::desktop_path(&xdg::autostart_dir(), app_id)
            .display()
            .to_string()
    }
}

/// 解除する。自分の導入先を指す値だけを消し、消したかを返す。
pub fn unregister(app_id: &str, executable: &Path) -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        windows::unregister_in(
            &windows::RegKey::current_user(),
            windows::RUN_SUBKEY,
            app_id,
            executable,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let dir = macos::launch_agents_dir();
        if macos::points_at_file(&dir, app_id, executable) {
            // 読み込み済みのジョブを外す（未読み込みなら失敗するが、それで構わない）。plist を消すだけでは、
            // ログアウトまで launchd に定義が残るため。
            macos::bootout(app_id);
        }
        macos::unregister_in(&dir, app_id, executable)
    }
    #[cfg(target_os = "linux")]
    {
        // autostart は次のログインで読まれるだけで、常駐の定義は無い（launchctl のような後始末は要らない）。
        xdg::unregister_in(&xdg::autostart_dir(), app_id, executable)
    }
}

/// Linux の XDG autostart（ログイン時に `update-apply` を1回起動する .desktop ファイル）。
///
/// 設計の要点:
/// - ファイル名は `com.kagayoi.<packId>.update.desktop`。packId に紐づけ、テスト用 packId と本番が衝突しない。
/// - `Exec` は Desktop Entry 仕様の引用規則で書く（パスを二重引用符で囲み、`"` `` ` `` `$` `\` を `\` で
///   エスケープしたうえで、文字列値としての `\` をもう一度重ねる。`%` は `%%`）。改行を含むパスは表せないので
///   登録しない（エラー）。
/// - どの導入のものかの判定には、`Exec` の解析ではなく専用のキー `X-Remnifold-Executable=<パス>` を使う
///   （引用の揺れに左右されない）。
/// - 呼び出し元に `REMNIFOLD_HOME`（設定・状態の置き場の上書き）があれば、ログイン時にも同じ場所を使うよう
///   `/usr/bin/env REMNIFOLD_HOME=… ` を前に付ける（macOS の `EnvironmentVariables` と同じ扱い）。
/// - `X-GNOME-Autostart-enabled=true` と `NoDisplay=true`、`Terminal=false`。端末もメニュー項目も出さない。
///
/// パスの組み立てと本文・照合は OS に依存しないため、Windows の単体テストでも確かめられるよう、
/// このモジュールは全 OS でコンパイルする（Linux 以外では呼ばれない）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod xdg {
    use super::*;
    use std::{io, path::PathBuf};

    /// 本番の置き場。`$XDG_CONFIG_HOME` が絶対パスならその配下、無ければ `~/.config`。
    /// 隔離検証では `HOME`・`XDG_CONFIG_HOME` を差し替えると、その配下になる。
    pub fn autostart_dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| crate::common::home_dir().join(".config"))
            .join("autostart")
    }

    /// ファイル名の幹（launchd のラベルと同じ逆 DNS 形式）。
    pub fn entry_name(app_id: &str) -> String {
        format!("com.kagayoi.{app_id}.update")
    }

    pub fn desktop_path(dir: &Path, app_id: &str) -> PathBuf {
        dir.join(format!("{}.desktop", entry_name(app_id)))
    }

    /// Desktop Entry の文字列値のエスケープ（`\` を重ねる）。改行・CR・タブは表せないので None。
    fn string_value(text: &str) -> Option<String> {
        if text.contains(['\n', '\r', '\t']) {
            return None;
        }
        Some(text.replace('\\', "\\\\"))
    }

    /// `Exec` の1引数を引用する（仕様の「予約文字は二重引用符で囲み、`"` `` ` `` `$` `\` を `\` で前置」）。
    /// 文字列値としてのエスケープと `%` の重ねは呼び出し元（[`exec_line`]）で行う。
    fn quote_argument(text: &str) -> String {
        let mut quoted = String::with_capacity(text.len() + 2);
        quoted.push('"');
        for character in text.chars() {
            if matches!(character, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(character);
        }
        quoted.push('"');
        quoted
    }

    /// `Exec=` の値（文字列値のエスケープと `%` の重ねを済ませたもの）。表せないパスなら None。
    pub fn exec_line(executable: &Path, remnifold_home: Option<&Path>) -> Option<String> {
        let mut arguments = Vec::new();
        if let Some(home) = remnifold_home {
            arguments.push("/usr/bin/env".to_string());
            arguments.push(quote_argument(&format!(
                "REMNIFOLD_HOME={}",
                home.to_string_lossy()
            )));
        }
        arguments.push(quote_argument(&executable.to_string_lossy()));
        arguments.push("update-apply".to_string());
        string_value(&arguments.join(" ").replace('%', "%%"))
    }

    /// 書き込む本文。表せないパス（改行を含むなど）なら None。
    pub fn desktop_body(
        app_id: &str,
        executable: &Path,
        remnifold_home: Option<&Path>,
    ) -> Option<String> {
        let exec = exec_line(executable, remnifold_home)?;
        let marker = string_value(&executable.to_string_lossy())?;
        let app = string_value(app_id)?;
        Some(format!(
            "[Desktop Entry]\nType=Application\nName=Remnifold update ({app})\nComment=Applies a downloaded Remnifold update at login when no other Remnifold process is running\nExec={exec}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nX-Remnifold-Executable={marker}\n"
        ))
    }

    /// 本文が、この実行ファイル（AppImage）を起動するものか（専用キーの行が完全に一致するか）。
    pub fn points_at(body: &str, executable: &Path) -> bool {
        let Some(marker) = string_value(&executable.to_string_lossy()) else {
            return false;
        };
        let line = format!("X-Remnifold-Executable={marker}");
        body.lines().any(|candidate| candidate == line)
    }

    /// 既存のファイル（通常ファイル）を読む。無ければ None。リンクなど通常ファイルでないものは、
    /// 他の導入のものとして扱えるよう空文字を返す（上書きも削除もしない側へ倒す）。
    fn read_existing(path: &Path) -> io::Result<Option<String>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(
                String::from_utf8_lossy(&std::fs::read(path)?).into_owned(),
            )),
            Ok(_) => Ok(Some(String::new())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write(path: &Path, body: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::common::atomic_write(path, body.as_bytes(), false).map_err(io::Error::other)
    }

    pub fn register_in(dir: &Path, app_id: &str, executable: &Path) -> io::Result<Registration> {
        let path = desktop_path(dir, app_id);
        let home = std::env::var_os("REMNIFOLD_HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let desired = desktop_body(app_id, executable, home.as_deref()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the AppImage path cannot be written to a desktop entry",
            )
        })?;
        match read_existing(&path)? {
            None => {
                write(&path, &desired)?;
                Ok(Registration::Created)
            }
            Some(existing) if existing == desired => Ok(Registration::Unchanged),
            // 自分の AppImage を指すファイルだけを最新の定義へ書き直す。
            Some(existing) if points_at(&existing, executable) => {
                write(&path, &desired)?;
                Ok(Registration::Updated)
            }
            // 別の場所を指す同名のファイルは、他の導入のものとして触らない。
            Some(_) => Ok(Registration::ForeignKept),
        }
    }

    pub fn unregister_in(dir: &Path, app_id: &str, executable: &Path) -> io::Result<bool> {
        let path = desktop_path(dir, app_id);
        match read_existing(&path)? {
            Some(existing) if points_at(&existing, executable) => {
                std::fs::remove_file(&path)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scratch() -> PathBuf {
            std::env::temp_dir().join(format!("remnifold-autostart-{}", uuid::Uuid::new_v4()))
        }

        #[test]
        fn entry_is_created_updated_and_removed_only_for_the_own_appimage() {
            // 置き場のフォルダーが無くても作る（新しい利用者環境には autostart が無いことがある）。
            let dir = scratch();
            let mine = Path::new("/home/a b/Apps/RemnifoldApp.AppImage");
            let other = Path::new("/opt/RemnifoldApp.AppImage");
            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Created
            );
            // 他の製品のファイルは、登録・解除のどちらでも変わらない。
            std::fs::write(dir.join("other.desktop"), "other").unwrap();
            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Unchanged
            );
            let path = desktop_path(&dir, "TestApp");
            assert!(path.ends_with("com.kagayoi.TestApp.update.desktop"));
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(points_at(&body, mine) && !points_at(&body, other));
            assert!(body.starts_with("[Desktop Entry]\n"));
            for needed in [
                "Type=Application",
                "X-GNOME-Autostart-enabled=true",
                "Terminal=false",
            ] {
                assert!(body.lines().any(|line| line == needed), "{needed}");
            }
            // テストの実行環境に REMNIFOLD_HOME があれば env 経由になるため、末尾だけを確かめる。
            assert!(body.lines().any(|line| line.starts_with("Exec=")
                && line.ends_with("\"/home/a b/Apps/RemnifoldApp.AppImage\" update-apply")));
            // 別の導入先からの登録・解除は、既存のファイルを変えない。
            assert_eq!(
                register_in(&dir, "TestApp", other).unwrap(),
                Registration::ForeignKept
            );
            assert!(!unregister_in(&dir, "TestApp", other).unwrap());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
            // 古い定義でも、自分を指していれば書き直す。
            std::fs::write(
                &path,
                format!(
                    "[Desktop Entry]\nX-Remnifold-Executable={}\n",
                    mine.display()
                ),
            )
            .unwrap();
            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Updated
            );
            assert!(unregister_in(&dir, "TestApp", mine).unwrap());
            assert!(!path.exists());
            assert!(!unregister_in(&dir, "TestApp", mine).unwrap());
            assert_eq!(
                std::fs::read_to_string(dir.join("other.desktop")).unwrap(),
                "other"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn exec_quotes_reserved_characters_and_carries_home() {
            let exe = Path::new(r#"/tmp/a"b$c`d\e%f/R.AppImage"#);
            // 引数の引用（\ " $ ` を \ で前置）→ 文字列値として \ を重ねる → % を重ねる。
            assert_eq!(
                exec_line(exe, None).unwrap(),
                r#""/tmp/a\\"b\\$c\\`d\\\\e%%f/R.AppImage" update-apply"#
            );
            let with_home = exec_line(Path::new("/x/R.AppImage"), Some(Path::new("/h o"))).unwrap();
            assert_eq!(
                with_home,
                r#"/usr/bin/env "REMNIFOLD_HOME=/h o" "/x/R.AppImage" update-apply"#
            );
            // 改行を含むパスは表せないので登録しない。
            assert!(desktop_body("TestApp", Path::new("/tmp/a\nb"), None).is_none());
            assert!(!points_at(
                "X-Remnifold-Executable=/tmp/a",
                Path::new("/tmp/a\nb")
            ));
            // 専用キーの行の完全一致だけを見る（前方一致する別のパスは違う）。
            let body = desktop_body("TestApp", Path::new("/x/R.AppImage"), None).unwrap();
            assert!(points_at(&body, Path::new("/x/R.AppImage")));
            assert!(!points_at(&body, Path::new("/x/R.App")));
        }

        #[cfg(unix)]
        #[test]
        fn a_linked_entry_is_treated_as_foreign() {
            let dir = scratch();
            std::fs::create_dir_all(&dir).unwrap();
            let target = dir.join("target.desktop");
            std::fs::write(&target, "x").unwrap();
            std::os::unix::fs::symlink(&target, desktop_path(&dir, "TestApp")).unwrap();
            let exe = Path::new("/x/R.AppImage");
            assert_eq!(
                register_in(&dir, "TestApp", exe).unwrap(),
                Registration::ForeignKept
            );
            assert!(!unregister_in(&dir, "TestApp", exe).unwrap());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// macOS の LaunchAgent（ログイン時に `update-apply` を1回起動する）。
///
/// 設計の要点:
/// - ラベルは `com.kagayoi.<packId>.update`。packId に紐づけ、テスト用 packId（例 `RemnifoldVelopackTest`）と
///   本番（`RemnifoldApp`）が衝突しない。
/// - `RunAtLoad` だけを立て、`KeepAlive` は付けない（ログインごとに1回走って終わる）。
/// - `AbandonProcessGroup` を立てる。launchd は既定で、ジョブの主プロセスが終わると同じプロセスグループに
///   残った子を終了させる。`update-apply` は自分の終了を待って入れ替える UpdateMac を子として起動するため、
///   これが無いと入れ替えの途中で UpdateMac が止められてしまう。
/// - 呼び出し元に `REMNIFOLD_HOME` があれば（設定・状態の置き場の上書き）、ログイン時にも同じ場所を使うよう
///   `EnvironmentVariables` に引き継ぐ。
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{io, path::PathBuf};

    /// 本番の置き場。`HOME` を差し替えた隔離検証では、その配下になる。
    pub fn launch_agents_dir() -> PathBuf {
        crate::common::home_dir()
            .join("Library")
            .join("LaunchAgents")
    }

    /// launchd のラベル（逆 DNS 形式）。
    pub fn label(app_id: &str) -> String {
        format!("com.kagayoi.{app_id}.update")
    }

    pub fn plist_path(dir: &Path, app_id: &str) -> PathBuf {
        dir.join(format!("{}.plist", label(app_id)))
    }

    /// plist の文字列要素に入れるための XML エスケープ。
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    /// 書き込む plist の本文。`remnifold_home` は呼び出し元の `REMNIFOLD_HOME`（無ければ None）。
    pub fn plist_body(app_id: &str, executable: &Path, remnifold_home: Option<&Path>) -> String {
        let environment = remnifold_home
            .map(|home| {
                format!(
                    "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>REMNIFOLD_HOME</key>\n\t\t<string>{}</string>\n\t</dict>\n",
                    escape(&home.to_string_lossy())
                )
            })
            .unwrap_or_default();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{label}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{exe}</string>\n\t\t<string>update-apply</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>AbandonProcessGroup</key>\n\t<true/>\n\t<key>ProcessType</key>\n\t<string>Background</string>\n{environment}</dict>\n</plist>\n",
            label = escape(&label(app_id)),
            exe = escape(&executable.to_string_lossy()),
        )
    }

    /// plist の本文が、この実行ファイルを起動するものか（エスケープ済みのパスの文字列要素を含むか）。
    pub fn points_at(body: &str, executable: &Path) -> bool {
        body.contains(&format!(
            "<string>{}</string>",
            escape(&executable.to_string_lossy())
        ))
    }

    /// 既存の plist（通常ファイル）を読む。無ければ None。リンクなど通常ファイルでないものは、
    /// 他の導入のものとして扱えるよう空文字を返す（上書きも削除もしない側へ倒す）。
    fn read_existing(path: &Path) -> io::Result<Option<String>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(
                String::from_utf8_lossy(&std::fs::read(path)?).into_owned(),
            )),
            Ok(_) => Ok(Some(String::new())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn points_at_file(dir: &Path, app_id: &str, executable: &Path) -> bool {
        read_existing(&plist_path(dir, app_id))
            .ok()
            .flatten()
            .is_some_and(|body| points_at(&body, executable))
    }

    fn write(path: &Path, body: &str) -> io::Result<()> {
        crate::common::atomic_write(path, body.as_bytes(), false).map_err(io::Error::other)
    }

    pub fn register_in(dir: &Path, app_id: &str, executable: &Path) -> io::Result<Registration> {
        let path = plist_path(dir, app_id);
        let home = std::env::var_os("REMNIFOLD_HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let desired = plist_body(app_id, executable, home.as_deref());
        match read_existing(&path)? {
            None => {
                write(&path, &desired)?;
                Ok(Registration::Created)
            }
            Some(existing) if existing == desired => Ok(Registration::Unchanged),
            // 自分の導入先を指す plist だけを最新の定義へ書き直す。
            Some(existing) if points_at(&existing, executable) => {
                write(&path, &desired)?;
                Ok(Registration::Updated)
            }
            // 別の場所を指す同名の plist は、他の導入のものとして触らない。
            Some(_) => Ok(Registration::ForeignKept),
        }
    }

    pub fn unregister_in(dir: &Path, app_id: &str, executable: &Path) -> io::Result<bool> {
        let path = plist_path(dir, app_id);
        match read_existing(&path)? {
            Some(existing) if points_at(&existing, executable) => {
                std::fs::remove_file(&path)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    unsafe extern "C" {
        fn getuid() -> u32;
    }

    /// ログイン中の利用者の GUI ドメインから、このジョブを外す。失敗（未読み込みなど）は無視する。
    pub fn bootout(app_id: &str) {
        // SAFETY: getuid は引数を取らず、失敗しない。
        let uid = unsafe { getuid() };
        let _ = std::process::Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(format!("gui/{uid}/{}", label(app_id)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scratch() -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("remnifold-agents-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn agent_is_created_updated_and_removed_only_for_the_own_install() {
            let dir = scratch();
            let mine = Path::new("/Users/a b/Apps/Remnifold.app/Contents/MacOS/remnifold");
            let other = Path::new("/Applications/Remnifold.app/Contents/MacOS/remnifold");
            // 他の製品の plist は、登録・解除のどちらでも変わらない。
            std::fs::write(dir.join("com.example.other.plist"), "other").unwrap();

            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Created
            );
            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Unchanged
            );
            let path = plist_path(&dir, "TestApp");
            assert!(path.ends_with("com.kagayoi.TestApp.update.plist"));
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(points_at(&body, mine) && !points_at(&body, other));
            for needed in [
                "<string>update-apply</string>",
                "<key>RunAtLoad</key>",
                "<key>AbandonProcessGroup</key>",
                "<string>com.kagayoi.TestApp.update</string>",
            ] {
                assert!(body.contains(needed), "{needed}");
            }
            assert!(!body.contains("KeepAlive"));
            // 別の導入先からの登録・解除は、既存の plist を変えない。
            assert_eq!(
                register_in(&dir, "TestApp", other).unwrap(),
                Registration::ForeignKept
            );
            assert!(!unregister_in(&dir, "TestApp", other).unwrap());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
            // 古い定義でも、自分を指していれば書き直す。
            std::fs::write(&path, format!("<string>{}</string>", mine.display())).unwrap();
            assert_eq!(
                register_in(&dir, "TestApp", mine).unwrap(),
                Registration::Updated
            );
            assert!(unregister_in(&dir, "TestApp", mine).unwrap());
            assert!(!path.exists());
            assert!(!unregister_in(&dir, "TestApp", mine).unwrap());
            assert_eq!(
                std::fs::read_to_string(dir.join("com.example.other.plist")).unwrap(),
                "other"
            );
        }

        #[test]
        fn special_characters_are_escaped_and_home_is_carried_over() {
            let exe = Path::new("/tmp/a&b<c>/remnifold");
            let body = plist_body("TestApp", exe, Some(Path::new("/tmp/h&ome")));
            assert!(body.contains("<string>/tmp/a&amp;b&lt;c&gt;/remnifold</string>"));
            assert!(body.contains("<key>REMNIFOLD_HOME</key>"));
            assert!(body.contains("<string>/tmp/h&amp;ome</string>"));
            assert!(points_at(&body, exe));
            assert!(!plist_body("TestApp", exe, None).contains("EnvironmentVariables"));
        }

        #[test]
        fn a_linked_plist_is_treated_as_foreign() {
            let dir = scratch();
            let target = dir.join("target.plist");
            std::fs::write(&target, "x").unwrap();
            std::os::unix::fs::symlink(&target, plist_path(&dir, "TestApp")).unwrap();
            let exe = Path::new("/x/remnifold");
            assert_eq!(
                register_in(&dir, "TestApp", exe).unwrap(),
                Registration::ForeignKept
            );
            assert!(!unregister_in(&dir, "TestApp", exe).unwrap());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{io, ptr::null_mut};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
    };

    /// HKCU の Run キー。
    pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const ERROR_SUCCESS: u32 = 0;
    const ERROR_FILE_NOT_FOUND: u32 = 2;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(status: u32) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    /// 根にするキー。本番は HKCU、テストはファイルに閉じた専用のハイブ（実際の Run キーに触れない）。
    pub struct RegKey {
        handle: HKEY,
        owned: bool,
    }

    impl RegKey {
        pub fn current_user() -> Self {
            Self {
                handle: HKEY_CURRENT_USER,
                owned: false,
            }
        }

        /// ファイルに閉じた専用のハイブを開く（RegLoadAppKeyW。このプロセスだけから見える）。
        #[cfg(test)]
        pub fn load_app_hive(file: &Path) -> io::Result<Self> {
            use windows_sys::Win32::System::Registry::{
                KEY_ALL_ACCESS, REG_PROCESS_APPKEY, RegLoadAppKeyW,
            };
            let path = wide(&file.to_string_lossy());
            let mut handle: HKEY = null_mut();
            // SAFETY: path は NUL 終端、handle は書き込み先の変数。
            check(unsafe {
                RegLoadAppKeyW(
                    path.as_ptr(),
                    &mut handle,
                    KEY_ALL_ACCESS,
                    REG_PROCESS_APPKEY,
                    0,
                )
            })?;
            Ok(Self {
                handle,
                owned: true,
            })
        }

        fn create_subkey(&self, subkey: &str) -> io::Result<Self> {
            let name = wide(subkey);
            let mut handle: HKEY = null_mut();
            // SAFETY: name は NUL 終端、handle は書き込み先の変数。
            check(unsafe {
                RegCreateKeyExW(
                    self.handle,
                    name.as_ptr(),
                    0,
                    std::ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_QUERY_VALUE | KEY_SET_VALUE,
                    std::ptr::null(),
                    &mut handle,
                    null_mut(),
                )
            })?;
            Ok(Self {
                handle,
                owned: true,
            })
        }

        /// 文字列の値を読む（無ければ None。文字列型以外の値は空文字として扱い、上書きしない側へ倒す）。
        fn query_string(&self, name: &str) -> io::Result<Option<String>> {
            let name = wide(name);
            let mut kind = 0u32;
            let mut size = 0u32;
            // SAFETY: 大きさだけを問い合わせる（データ領域は null）。
            let status = unsafe {
                RegQueryValueExW(
                    self.handle,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    null_mut(),
                    &mut size,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            let mut buffer = vec![0u8; size as usize + 2];
            let mut length = size;
            // SAFETY: buffer は length バイト以上の書き込み可能な領域。
            check(unsafe {
                RegQueryValueExW(
                    self.handle,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    buffer.as_mut_ptr(),
                    &mut length,
                )
            })?;
            if kind != REG_SZ && kind != 2 {
                return Ok(Some(String::new()));
            }
            let units: Vec<u16> = buffer[..length as usize]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|unit| *unit != 0)
                .collect();
            Ok(Some(String::from_utf16_lossy(&units)))
        }

        fn set_string(&self, name: &str, value: &str) -> io::Result<()> {
            let name = wide(name);
            let data: Vec<u8> = wide(value)
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            // SAFETY: data は NUL 終端を含む UTF-16LE のバイト列。
            check(unsafe {
                RegSetValueExW(
                    self.handle,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr(),
                    data.len() as u32,
                )
            })
        }

        fn delete_value(&self, name: &str) -> io::Result<()> {
            let name = wide(name);
            // SAFETY: name は NUL 終端。
            let status = unsafe { RegDeleteValueW(self.handle, name.as_ptr()) };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            check(status)
        }
    }

    impl Drop for RegKey {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: owned は自分が開いたハンドルだけに立つ。
                unsafe { RegCloseKey(self.handle) };
            }
        }
    }

    pub fn register_in(
        root: &RegKey,
        subkey: &str,
        app_id: &str,
        executable: &Path,
    ) -> io::Result<Registration> {
        let key = root.create_subkey(subkey)?;
        let name = value_name(app_id);
        let desired = run_command(executable);
        match key.query_string(&name)? {
            None => {
                key.set_string(&name, &desired)?;
                Ok(Registration::Created)
            }
            Some(existing) if existing == desired => Ok(Registration::Unchanged),
            // 自分の導入先を指す値だけを最新の定義へ張り直す。
            Some(existing) if points_at(&existing, executable) => {
                key.set_string(&name, &desired)?;
                Ok(Registration::Updated)
            }
            // 別の場所を指す同名の値は、他の導入のものとして触らない。
            Some(_) => Ok(Registration::ForeignKept),
        }
    }

    pub fn unregister_in(
        root: &RegKey,
        subkey: &str,
        app_id: &str,
        executable: &Path,
    ) -> io::Result<bool> {
        let key = root.create_subkey(subkey)?;
        let name = value_name(app_id);
        match key.query_string(&name)? {
            Some(existing) if points_at(&existing, executable) => {
                key.delete_value(&name)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scratch_hive() -> RegKey {
            let dir = std::env::temp_dir().join(format!("remnifold-run-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            RegKey::load_app_hive(&dir.join("hive.dat")).unwrap()
        }

        #[test]
        fn run_value_is_created_updated_and_removed_only_for_the_own_install() {
            let hive = scratch_hive();
            let mine = Path::new(r"C:\Apps\Remnifold\current\remnifold.exe");
            let other = Path::new(r"D:\Elsewhere\current\remnifold.exe");
            // 他の製品の値は、登録・解除のどちらでも変わらない。
            let key = hive.create_subkey(RUN_SUBKEY).unwrap();
            key.set_string("Other", "other.exe").unwrap();

            assert_eq!(
                register_in(&hive, RUN_SUBKEY, "TestApp", mine).unwrap(),
                Registration::Created
            );
            assert_eq!(
                register_in(&hive, RUN_SUBKEY, "TestApp", mine).unwrap(),
                Registration::Unchanged
            );
            // 別の導入先からの登録は、既存の値を上書きしない。
            assert_eq!(
                register_in(&hive, RUN_SUBKEY, "TestApp", other).unwrap(),
                Registration::ForeignKept
            );
            assert!(!unregister_in(&hive, RUN_SUBKEY, "TestApp", other).unwrap());
            let value = key.query_string("TestApp Update").unwrap().unwrap();
            assert!(points_at(&value, mine) && !points_at(&value, other));
            assert!(value.ends_with("update-apply"));
            // 古い定義（コマンドの形が違う）でも、自分を指していれば張り直す。
            key.set_string(
                "TestApp Update",
                &format!("\"{}\" update-apply", mine.display()),
            )
            .unwrap();
            assert_eq!(
                register_in(&hive, RUN_SUBKEY, "TestApp", mine).unwrap(),
                Registration::Updated
            );
            assert!(unregister_in(&hive, RUN_SUBKEY, "TestApp", mine).unwrap());
            assert!(key.query_string("TestApp Update").unwrap().is_none());
            assert_eq!(
                key.query_string("Other").unwrap().as_deref(),
                Some("other.exe")
            );
            // 無い値の解除は何もしない。
            assert!(!unregister_in(&hive, RUN_SUBKEY, "TestApp", mine).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_the_path_and_names_the_subcommand() {
        let exe = Path::new(r"C:\Users\a b\AppData\Local\RemnifoldApp\current\remnifold.exe");
        let command = run_command(exe);
        assert!(command.contains("--headless"));
        assert!(command.ends_with(&format!("\"{}\" update-apply", exe.display())));
        assert!(points_at(&command, exe));
        assert!(!points_at(&command, Path::new(r"C:\Other\remnifold.exe")));
        assert_eq!(value_name("RemnifoldApp"), "RemnifoldApp Update");
    }
}
