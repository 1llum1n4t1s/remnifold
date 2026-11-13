// Remnifold 自身の設定・状態の保存場所を決める。
//
// 以前は Codex の home（~/.codex）に接続設定・capture/recall 状態・サーバー設定を置いていたが、
// Remnifold は Claude Code からも使い、Codex が無い環境もあるため、Codex に依存しない
// OS 標準の場所へ移した。Codex の会話記録（sessions）を読む場所は従来どおり Codex home を使う。
//
// 解決順:
// 1. REMNIFOLD_HOME（明示の上書き。テストの隔離や、利用者が別の場所を使いたい場合）
//    設定: <REMNIFOLD_HOME>/remnifold.json、状態: <REMNIFOLD_HOME>/remnifold/...
//    （旧来の Codex home と同じ配置なので、同じフォルダを指せば従来のファイルをそのまま使える）
// 2. OS 標準
//    Windows: %APPDATA%\Remnifold（設定と状態を同じ場所に置く）
//    macOS:   ~/Library/Application Support/Remnifold（同上）
//    Linux 等: 設定 $XDG_CONFIG_HOME/remnifold（既定 ~/.config/remnifold）、
//              状態 $XDG_STATE_HOME/remnifold（既定 ~/.local/state/remnifold）
//
// 旧来の Codex home にだけ設定がある場合は、初回に新しい場所へコピーして以後は新しい場所を使う
// （旧ファイルは消さない）。capture/recall の状態は新旧を合わせて読み、書くのは新しい場所だけにする。
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// 接続設定として読むファイル名（先頭が現行名、後ろは旧製品名の互換読取り）。
pub const CONFIG_NAMES: [&str; 3] = ["remnifold.json", "kagayoi-memory.json", "supermemory.json"];
/// 状態フォルダの旧名（Codex home 配下の互換読取り）。先頭が現行名。
pub const LEGACY_STATE_NAMESPACES: [&str; 3] = ["remnifold", "kagayoi-memory", "cloudflare-memory"];

/// Remnifold 自身の設定と状態の置き場所。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    /// remnifold.json と server/ を置くフォルダ。
    pub config: PathBuf,
    /// capture-state/ と recall-state/ を置くフォルダ。
    pub state: PathBuf,
}

impl Roots {
    /// 現在のプロセス環境から決める。
    pub fn current() -> Self {
        resolve(|name| std::env::var_os(name), std::env::consts::OS)
    }
    /// 新しい接続設定ファイルのパス。
    pub fn config_file(&self) -> PathBuf {
        self.config.join("remnifold.json")
    }
    /// サーバー設定（server-setup の再開用状態）の既定パス。
    pub fn server_state(&self) -> PathBuf {
        self.config.join("server").join("config.json")
    }
    /// 書込み先の状態フォルダ（capture-state / recall-state）。
    pub fn state_dir(&self, kind: &str) -> PathBuf {
        self.state.join(kind)
    }
    /// 読み取り候補の状態フォルダ。先頭が書込み先で、続いて旧来の Codex home 配下を旧名順に並べる。
    pub fn state_dirs(&self, codex_home: &Path, kind: &str) -> Vec<PathBuf> {
        let mut dirs = vec![self.state_dir(kind)];
        for name in LEGACY_STATE_NAMESPACES {
            let legacy = codex_home.join(name).join(kind);
            if !dirs.contains(&legacy) {
                dirs.push(legacy);
            }
        }
        dirs
    }
}

/// 環境変数の取得関数と OS 名から置き場所を決める（テストで環境を差し替えられるよう分けている）。
pub fn resolve(get: impl Fn(&str) -> Option<OsString>, os: &str) -> Roots {
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(root) = absolute("REMNIFOLD_HOME") {
        return Roots {
            state: root.join("remnifold"),
            config: root,
        };
    }
    let home = get(if os == "windows" {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .unwrap_or_default();
    match os {
        "windows" => {
            let root = absolute("APPDATA")
                .unwrap_or_else(|| home.join("AppData").join("Roaming"))
                .join("Remnifold");
            Roots {
                config: root.clone(),
                state: root,
            }
        }
        "macos" => {
            let root = home
                .join("Library")
                .join("Application Support")
                .join("Remnifold");
            Roots {
                config: root.clone(),
                state: root,
            }
        }
        _ => Roots {
            config: absolute("XDG_CONFIG_HOME")
                .unwrap_or_else(|| home.join(".config"))
                .join("remnifold"),
            state: absolute("XDG_STATE_HOME")
                .unwrap_or_else(|| home.join(".local").join("state"))
                .join("remnifold"),
        },
    }
}

/// 単独アプリとしての導入先（実行ファイル・版ごとの展開・受領証）。現在の環境から決める。
pub fn app_root() -> PathBuf {
    resolve_app(|name| std::env::var_os(name), std::env::consts::OS)
}

/// 導入先の解決。管理者権限が要らない利用者ごとの場所にする。
/// REMNIFOLD_HOME 指定時は <REMNIFOLD_HOME>/app、Windows は %LOCALAPPDATA%\Programs\Remnifold、
/// macOS は ~/Library/Application Support/Remnifold/app、Linux 等は $XDG_DATA_HOME/remnifold
/// （既定 ~/.local/share/remnifold）。
pub fn resolve_app(get: impl Fn(&str) -> Option<OsString>, os: &str) -> PathBuf {
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(root) = absolute("REMNIFOLD_HOME") {
        return root.join("app");
    }
    let home = get(if os == "windows" {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .unwrap_or_default();
    match os {
        "windows" => absolute("LOCALAPPDATA")
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("Programs")
            .join("Remnifold"),
        "macos" => home
            .join("Library")
            .join("Application Support")
            .join("Remnifold")
            .join("app"),
        _ => absolute("XDG_DATA_HOME")
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("remnifold"),
    }
}

/// 導入先の固定の実行ファイル（MCP・フックの登録はここを指す）。
pub fn app_executable(app: &Path) -> PathBuf {
    app.join("bin").join(if cfg!(windows) {
        "remnifold.exe"
    } else {
        "remnifold"
    })
}

/// 読むべき接続設定ファイルを返す。
///
/// 新しい場所に設定があればそれを使う。無ければ旧来の Codex home を旧名順に探し、
/// 有効な JSON オブジェクトなら新しい場所へ一度だけコピーしてそのコピーを返す。
/// 旧ファイルが壊れている場合はコピーせず旧ファイルのパスを返し、従来どおりの扱い
/// （環境変数で接続できれば続行、無ければ失敗）に任せる。旧ファイルは消さない。
pub fn config_file(roots: &Roots, codex_home: &Path) -> Result<Option<PathBuf>> {
    if let Some(path) = CONFIG_NAMES
        .iter()
        .map(|name| roots.config.join(name))
        .find(|path| path.exists())
    {
        return Ok(Some(path));
    }
    let Some(legacy) = CONFIG_NAMES
        .iter()
        .map(|name| codex_home.join(name))
        .find(|path| path.exists())
    else {
        return Ok(None);
    };
    let bytes = fs::read(&legacy).context("Existing configuration could not be read.")?;
    if !serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|value| value.is_object()) {
        return Ok(Some(legacy));
    }
    let target = roots.config_file();
    // 移行先へ書けない環境（読み取り専用のプロファイル等）では、旧ファイルをそのまま使い続ける。
    match copy_new(&target, &bytes) {
        Ok(()) => Ok(Some(target)),
        Err(_) => Ok(Some(legacy)),
    }
}

/// サーバー設定の既定パスを返す。
/// 新しい場所に無く旧来の Codex home にある場合、`migrate`（--apply）のときだけ一度コピーし、
/// 読み取り専用のプランでは何も書かずに旧ファイルを読む。
pub fn server_state_file(roots: &Roots, codex_home: &Path, migrate: bool) -> Result<PathBuf> {
    let target = roots.server_state();
    let legacy = codex_home
        .join("remnifold")
        .join("server")
        .join("config.json");
    if !target.exists() && legacy != target && legacy.is_file() {
        if !migrate {
            return Ok(legacy);
        }
        let bytes = fs::read(&legacy).context("Existing server state could not be read.")?;
        copy_new(&target, &bytes)?;
    }
    Ok(target)
}

/// まだ無い場合だけ、内容を書き終えたファイルを置く。
/// フックと MCP サーバーが同時に移行しても、片方が書きかけのファイルを読まないよう、
/// 一時ファイルに書いてからハードリンクで置く（既にあれば何もしない）。
/// 接続設定は API キーを含むため、Unix では所有者だけが読める権限にする。
fn copy_new(target: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = target.parent().context("Missing configuration directory")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".remnifold-migrate-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    let linked = fs::hard_link(&temporary, target);
    let _ = fs::remove_file(&temporary);
    match linked {
        Ok(()) => Ok(()),
        // 別プロセスが先に移行した場合は、その結果をそのまま使う。
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn resolves_override_and_os_defaults() {
        let base = std::env::temp_dir().join("remnifold-paths");
        let over = base.join("override");
        let roots = resolve(
            env(&[("REMNIFOLD_HOME", over.to_str().unwrap())]),
            "windows",
        );
        assert_eq!(
            roots,
            Roots {
                config: over.clone(),
                state: over.join("remnifold")
            }
        );
        // 相対パスの上書きは無視して既定へ戻る。
        let home = base.join("home");
        let roots = resolve(
            env(&[
                ("REMNIFOLD_HOME", "relative"),
                ("HOME", home.to_str().unwrap()),
            ]),
            "linux",
        );
        assert_eq!(roots.config, home.join(".config/remnifold"));
        assert_eq!(roots.state, home.join(".local/state/remnifold"));
        let xdg = base.join("xdg");
        let roots = resolve(
            env(&[
                ("HOME", home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", xdg.join("c").to_str().unwrap()),
                ("XDG_STATE_HOME", xdg.join("s").to_str().unwrap()),
            ]),
            "linux",
        );
        assert_eq!(roots.config, xdg.join("c/remnifold"));
        assert_eq!(roots.state, xdg.join("s/remnifold"));
        let roots = resolve(env(&[("HOME", home.to_str().unwrap())]), "macos");
        assert_eq!(
            roots.config,
            home.join("Library/Application Support/Remnifold")
        );
        assert_eq!(roots.state, roots.config);
        let appdata = base.join("roaming");
        let roots = resolve(env(&[("APPDATA", appdata.to_str().unwrap())]), "windows");
        assert_eq!(roots.config, appdata.join("Remnifold"));
        assert_eq!(roots.state, roots.config);
        let roots = resolve(env(&[("USERPROFILE", home.to_str().unwrap())]), "windows");
        assert_eq!(roots.config, home.join("AppData/Roaming/Remnifold"));
        // 導入先
        assert_eq!(
            resolve_app(env(&[("REMNIFOLD_HOME", over.to_str().unwrap())]), "linux"),
            over.join("app")
        );
        let local = base.join("local");
        assert_eq!(
            resolve_app(env(&[("LOCALAPPDATA", local.to_str().unwrap())]), "windows"),
            local.join("Programs/Remnifold")
        );
        assert_eq!(
            resolve_app(env(&[("USERPROFILE", home.to_str().unwrap())]), "windows"),
            home.join("AppData/Local/Programs/Remnifold")
        );
        assert_eq!(
            resolve_app(env(&[("HOME", home.to_str().unwrap())]), "macos"),
            home.join("Library/Application Support/Remnifold/app")
        );
        assert_eq!(
            resolve_app(env(&[("HOME", home.to_str().unwrap())]), "linux"),
            home.join(".local/share/remnifold")
        );
    }

    #[test]
    fn migrates_legacy_configuration_once_and_prefers_new_location() {
        let base = std::env::temp_dir().join(format!("remnifold-migrate-{}", uuid::Uuid::new_v4()));
        let codex = base.join("codex");
        let roots = Roots {
            config: base.join("new"),
            state: base.join("new"),
        };
        fs::create_dir_all(&codex).unwrap();
        // 新旧どちらにも無い: 設定なし。
        assert_eq!(config_file(&roots, &codex).unwrap(), None);
        // 旧名の設定だけがある: 新しい場所へコピーし、旧ファイルは残す。
        fs::write(
            codex.join("kagayoi-memory.json"),
            br#"{"baseUrl":"https://a.example","apiKey":"k"}"#,
        )
        .unwrap();
        let path = config_file(&roots, &codex).unwrap().unwrap();
        assert_eq!(path, roots.config_file());
        assert_eq!(
            fs::read(&path).unwrap(),
            fs::read(codex.join("kagayoi-memory.json")).unwrap()
        );
        assert!(codex.join("kagayoi-memory.json").exists());
        // 新しい場所が優先され、旧ファイルが変わっても上書きしない。
        fs::write(
            codex.join("kagayoi-memory.json"),
            br#"{"baseUrl":"https://b.example","apiKey":"k"}"#,
        )
        .unwrap();
        assert_eq!(
            config_file(&roots, &codex).unwrap().unwrap(),
            roots.config_file()
        );
        assert!(
            fs::read_to_string(roots.config_file())
                .unwrap()
                .contains("a.example")
        );
        // 壊れた旧設定はコピーせず、旧ファイルの扱いに任せる。
        let other = Roots {
            config: base.join("other"),
            state: base.join("other"),
        };
        let broken = base.join("broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("remnifold.json"), b"unfinished").unwrap();
        assert_eq!(
            config_file(&other, &broken).unwrap().unwrap(),
            broken.join("remnifold.json")
        );
        assert!(!other.config_file().exists());
        // 状態フォルダは新しい場所を先頭に、旧名を後ろに並べる。
        let dirs = roots.state_dirs(&codex, "capture-state");
        assert_eq!(dirs[0], roots.state.join("capture-state"));
        assert_eq!(dirs[1], codex.join("remnifold/capture-state"));
        assert_eq!(dirs.len(), 4);
        // 上書き指定で旧来と同じ配置を指すと、同じフォルダを重複させない。
        let same = Roots {
            config: codex.clone(),
            state: codex.join("remnifold"),
        };
        assert_eq!(same.state_dirs(&codex, "recall-state").len(), 3);
        // サーバー設定も一度だけ移す。
        fs::create_dir_all(codex.join("remnifold/server")).unwrap();
        fs::write(codex.join("remnifold/server/config.json"), b"{}").unwrap();
        // 読み取り専用のプランでは移さず旧ファイルを読む。
        assert_eq!(
            server_state_file(&roots, &codex, false).unwrap(),
            codex.join("remnifold/server/config.json")
        );
        assert!(!roots.server_state().exists());
        let server = server_state_file(&roots, &codex, true).unwrap();
        assert_eq!(server, roots.server_state());
        assert!(server.exists());
        if std::env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            fs::remove_dir_all(base).unwrap();
        }
    }
}
