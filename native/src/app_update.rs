//! Setup（Velopack）導入版の更新・登録の処理。
//!
//! # 目的
//! AI 初心者がコマンドを打たずに、Setup で導入し、常に最新版で使えるようにする。共通仕様は
//! `D:\Dev\APP-DISTRIBUTION.md`（単一常駐型と多プロセス型）。
//!
//! # 多プロセス型としての設計
//! Remnifold は、セッションごとに stdio の `mcp` が常駐し、フックが毎プロンプト新しいプロセスで起動する。
//! Velopack の適用（`Update.exe apply`）は導入先の自分以外の全プロセスを強制終了するため、
//! 日中に適用すると別セッションの MCP を巻き込む。そこで次のように分ける。
//! - 起動時の自動適用は全サブコマンドで切る（[`startup`]）。
//! - 確認と取得は、長く動く `mcp` の別スレッドで行う（[`spawn_background`]）。取得だけで適用しない。
//! - 適用はログオン時だけ。`update-apply`（[`update_apply`]）が、導入先に他のプロセスが無いことを確かめ、
//!   署名者を検証してから、再起動なしで適用する。
//! - 登録（Claude Code・Codex）とログオン時の適用の登録は、全 OS で [`registration`] が状態ファイル
//!   （`setup-registration.json`）とあわせて行う。Windows では Velopack のフック（install / updated /
//!   uninstall）からも呼び、いずれも必ず終了コード 0 で終える（フックの失敗でインストール・更新・
//!   アンインストールを止めない）。フックは CLI 1回を3秒で打ち切るため、登録が終わらなかった CLI は状態に
//!   「未完了」と残り、ログオン時の `update-apply`・引数なしの起動・`mcp` の起動で（長めの上限で）やり直す。
//!   macOS ではフックが呼ばれないため、これらの起動だけで登録する。macOS にはアンインストーラーも無いため、
//!   解除は利用者が `uninstall-setup --apply` で行う（[`uninstall_setup`]）。
//! - Linux（AppImage）も macOS と同じく、フック・インストーラー・アンインストーラーが無い（利用者が AppImage を
//!   置いて起動するだけ）。登録・解除は macOS と同じ流れで行い、登録する実行ファイルは AppImage 本体にする
//!   （Velopack の `get_main_exe_path()` は起動のたびに変わるマウント先を指すため）。ログオン時の適用は
//!   XDG autostart（[`logon`]）。
//!
//! # 保存先
//! 設定（`auto_update`）と確認・失敗時刻は、状態フォルダー（`paths::Roots::state`）の
//! `app-update.json` に置く。Velopack の導入先（`current\`）は更新で入れ替わるため使わない。
//! プロセス間の排他は、同じ場所の `app-update.lock` を try-lock して行う。
//!
//! # フィード
//! 更新元の生成は [`update_source`] の1か所に閉じる。
use crate::{connect, paths::Roots};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use velopack::{
    ApplyWaitMode, UpdateCheck, UpdateManager, VelopackApp,
    locator::{self, LocationContext, VelopackLocator},
    sources::UpdateSource,
};

mod logon;
mod processes;
// 登録の要否の判定と状態の読み書き（全 OS 共通）。
mod registration;
mod trust;

/// 本番の更新元。`releases.<channel>.json` と `*-full.nupkg` をここから配信する。
/// 厳格な `install` 用の `releases/<version>/index.json` とは別の階層にしている。
pub const FEED_URL: &str = "https://remnifold-updates.kagayoi.com/velopack";
/// 設定・確認時刻を保存するファイル名（状態フォルダー直下）。
pub const SETTINGS_FILE: &str = "app-update.json";
/// プロセス間の排他に使うファイル名（状態フォルダー直下）。
pub const LOCK_FILE: &str = "app-update.lock";
/// 診断用の小さなログ（フックはコンソールを持たないため、結果の確認はこのファイルで行う）。
pub const LOG_FILE: &str = "app-update.log";
/// 前回の確認（成功・失敗のどちらも）からこの時間が過ぎるまでは、ネットワークへ出ない。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// 常駐中に「確認の時期か」を見直す間隔。壁時計で判定し直すので、スリープ明けや設定変更にも追随する。
pub const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 自動更新の設定と、前回の確認・失敗時刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoUpdateSettings {
    /// 確認・取得を自動で行うか。未設定は `true`（既定ON）。キー名は `auto_update`。
    pub auto_update: bool,
    /// 前回の確認に成功した時刻（UNIX秒）。
    pub last_checked_at: Option<u64>,
    /// 前回の確認に失敗した時刻（UNIX秒）。失敗しても24時間は再試行しない（起動の多い MCP が毎回通信しないため）。
    pub last_failed_at: Option<u64>,
}

impl Default for AutoUpdateSettings {
    fn default() -> Self {
        Self {
            auto_update: true,
            last_checked_at: None,
            last_failed_at: None,
        }
    }
}

/// JSON から設定を作る。壊れた JSON・型の違う項目は項目ごとに既定値へ戻し、決して失敗しない
/// （更新機能の不調で MCP やフックを止めないため）。
pub fn parse_settings(bytes: &[u8]) -> AutoUpdateSettings {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return AutoUpdateSettings::default();
    };
    AutoUpdateSettings {
        auto_update: value
            .get("auto_update")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        last_checked_at: value.get("last_checked_at").and_then(Value::as_u64),
        last_failed_at: value.get("last_failed_at").and_then(Value::as_u64),
    }
}

/// 状態フォルダーから設定を読む。無い・読めない・通常ファイルでない場合は既定値。
pub fn load_settings(state_dir: &Path) -> AutoUpdateSettings {
    let path = state_dir.join(SETTINGS_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => fs::read(&path)
            .map(|bytes| parse_settings(&bytes))
            .unwrap_or_default(),
        _ => AutoUpdateSettings::default(),
    }
}

/// 時刻の1項目だけを書き換える。ファイルにある他のキー（利用者が足した項目を含む）は保つ。
/// 書き込み側は排他ロックの保持者だけなので、読み取り→変更→原子的置換で足りる。
fn record_time(state_dir: &Path, key: &str, now: u64) -> Result<()> {
    let path = state_dir.join(SETTINGS_FILE);
    let mut value = match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
        Ok(_) => anyhow::bail!("The update settings path is not a regular file"),
        Err(_) => json!({}),
    };
    value[key] = json!(now);
    crate::common::atomic_write(
        &path,
        &[serde_json::to_vec_pretty(&value)?, b"\n".to_vec()].concat(),
        false,
    )
}

/// 現在時刻（UNIX秒）。時計が1970年より前の異常値でも 0 として扱う。
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// 前回の時刻から24時間以上たったか。未記録と、未来の時刻（時計の巻き戻し・手編集）は「時期」とみなす
/// （永久に確認しなくなるのを避けるため）。
pub fn is_due(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        Some(last) if last > now => true,
        Some(last) => now - last >= CHECK_INTERVAL.as_secs(),
    }
}

/// 自動で確認・取得をするべきか。ON で、成功時刻と失敗時刻の両方が24時間より前のときだけ真。
pub fn should_check(settings: &AutoUpdateSettings, now: u64) -> bool {
    settings.auto_update
        && is_due(settings.last_checked_at, now)
        && is_due(settings.last_failed_at, now)
}

/// 更新元を作る。公開先を変えるときはこの関数だけを差し替える。
///
/// `update-feed-test` 機能付きのビルドに限り、環境変数 `REMNIFOLD_UPDATE_FEED_DIR` の
/// ローカルフォルダーを更新元にできる（隔離した E2E 検証用。本番ビルドにこの分岐は残らない）。
fn update_source() -> Box<dyn UpdateSource> {
    #[cfg(feature = "update-feed-test")]
    if let Some(directory) = std::env::var_os("REMNIFOLD_UPDATE_FEED_DIR") {
        return Box::new(velopack::sources::FileSource::new(PathBuf::from(directory)));
    }
    Box::new(velopack::sources::HttpSource::new(FEED_URL))
}

/// Velopack 管理下（Setup で導入された `current\` 配下）なら、その配置情報。
/// 開発ビルドや旧 `install` 版では None（その場合、このモジュールは何もしない）。
fn locate() -> Option<VelopackLocator> {
    locator::auto_locate_app_manifest(LocationContext::FromCurrentExe).ok()
}

/// Setup（Velopack）で導入されたビルドか。直接起動の案内（[`welcome`]）を出してよいかの判定に使う。
pub fn is_managed() -> bool {
    locate().is_some()
}

fn manager() -> Result<UpdateManager> {
    UpdateManager::new_boxed(update_source(), None, None)
        .map_err(|error| anyhow::anyhow!("Velopack update manager is unavailable: {error}"))
}

/// 診断ログへ1行足す。失敗は無視し（ログのために本処理を失敗させない）、肥大化したら作り直す。
fn log_line(message: &str) {
    let dir = Roots::current().state;
    let path = dir.join(LOG_FILE);
    let _ = fs::create_dir_all(&dir);
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > 64 * 1024) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {message}", now_unix());
    }
}

// ───────────────────────── 起動時の Velopack 処理 ─────────────────────────

/// 起動直後の結果。
pub struct Startup {
    /// Setup 直後の初回起動か（Velopack の firstrun）。
    pub first_run: bool,
}

/// フックの処理を実行し、成否にかかわらず戻る（panic も飲み込む）。Velopack は処理の後に必ず
/// 終了コード 0 でプロセスを終えるため、ここで失敗を外へ出さない。
fn guarded(label: &str, work: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
        log_line(&format!("{label}: unexpected failure"));
    }
}

/// 登録が使う導入情報（登録する実行ファイル・packId・版）。
struct SetupContext {
    /// Claude Code・Codex とログオン時の適用に登録する実行ファイル。Windows は `current\remnifold.exe`、
    /// macOS は `.app/Contents/MacOS/remnifold`、Linux は AppImage 本体（引数はそのまま主実行ファイルへ渡る）。
    executable: PathBuf,
    app_id: String,
    version: String,
}

impl SetupContext {
    fn detect() -> Option<Self> {
        let locator = locate()?;
        // Linux の主実行ファイルは AppImage のマウント先（/tmp/.mount_XXXXXX/usr/bin/remnifold）にあり、
        // プロセスの終了とともに消える。登録には AppImage 本体のパスを使う。
        #[cfg(target_os = "linux")]
        let executable = locator.get_appimage_path();
        #[cfg(not(target_os = "linux"))]
        let executable = locator.get_main_exe_path();
        Some(Self {
            executable,
            app_id: locator.get_manifest_id(),
            version: locator.get_manifest_version_full_string(),
        })
    }
}

/// インストール・更新の直後の登録（Windows のフック）。`fresh`（インストール）のときは前回の状態を忘れ、
/// Claude Code・Codex とログオン時の適用を全て張り直す。更新では [`registration::plan`] のとおり、
/// ログオン時の適用とフックだけを張り直し、MCP の登録はパスが同じなら CLI を呼ばない（使用中のセッションの
/// 登録を外して付け直さない）。CLI は1回3秒で打ち切り（フックの時間制限のため）、終わらなかった CLI は状態に
/// 未完了と残して、ログオン時の `update-apply` などでやり直す。
#[cfg_attr(not(windows), allow(dead_code))]
fn register_everything(label: &str, fresh: bool, budget: Option<Duration>) {
    let Some(context) = SetupContext::detect() else {
        log_line(&format!("{label}: not a Velopack installation; skipped"));
        return;
    };
    if fresh && let Err(error) = registration::forget(&Roots::current().state) {
        log_line(&format!(
            "{label}: previous registration state not reset: {error}"
        ));
    }
    registration::ensure_within(&context, label, budget);
}

/// アンインストール前の解除。Claude Code・Codex の登録とログオン時の適用を外す。
#[cfg_attr(not(windows), allow(dead_code))]
fn unregister_everything() {
    let Some(context) = SetupContext::detect() else {
        log_line("uninstall: not a Velopack installation; skipped");
        return;
    };
    for line in unregister_context(&context) {
        log_line(&format!("uninstall: {line}"));
    }
}

/// 解除の本体。結果を人が読める行で返す（フックは診断ログへ、`uninstall-setup` は標準出力へ出す）。
fn unregister_context(context: &SetupContext) -> Vec<String> {
    let mut lines = Vec::new();
    // ログオン時の適用の解除（一瞬で終わる）を先に行い、時間がかかりうる CLI の解除を後ろに置く。
    lines.push(
        match logon::unregister(&context.app_id, &context.executable) {
            Ok(removed) => format!("logon apply removed={removed}"),
            Err(error) => format!("logon apply failed: {error}"),
        },
    );
    lines.extend(connect::unregister_for_setup(&context.executable));
    // 次に導入したときに登録し直すよう、登録の状態を忘れる（macOS・Linux の登録の判定に使う）。
    match registration::forget(&Roots::current().state) {
        Ok(_) => {}
        Err(error) => lines.push(format!("registration state not removed: {error}")),
    }
    lines
}

/// Setup 版で、登録が今の自分を指していなければ（または前回終わらなかった CLI があれば）登録する
/// （[`registration`]）。引数なしの起動（.pkg の初回起動・AppImage の直接起動・スタートメニュー）、`mcp` の
/// 背景スレッド、ログオン時の `update-apply` から呼ぶ。Windows ではフックの3秒の打ち切りで終わらなかった
/// 登録をここで完了させる（フック以外なので CLI の上限を長めにする）。普段は状態ファイルを読むだけで終わる。
/// Velopack 管理外のビルドでは何もしない。panic しても呼び出し元（MCP・案内・適用）を止めない。
pub fn ensure_registered() {
    if let Some(context) = SetupContext::detect() {
        connect::set_cli_timeout(connect::RETRY_CLI_TIMEOUT);
        guarded("register", || registration::ensure(&context, "register"));
    }
}

/// Setup 版の解除（`remnifold uninstall-setup [--apply]`）。macOS・Linux には Velopack のアンインストーラーが
/// 無いため、利用者が .app をゴミ箱へ入れる（AppImage を消す）前にこれを実行する。Windows でも、アンインストーラーと同じ解除を
/// 手動で行える。`--apply` が無ければ、何を外すかを示すだけで何も変えない。
pub fn uninstall_setup(args: &[String]) -> Result<Value> {
    let mut apply = false;
    for arg in args {
        match arg.as_str() {
            "--apply" => apply = true,
            "--help" | "-h" => {
                return Ok(json!({
                    "usage": "remnifold uninstall-setup [--apply]",
                    "description": "Removes the Setup installation's Claude Code/Codex registrations and logon update entry that point at this executable. Without --apply, only the plan is shown."
                }));
            }
            _ => anyhow::bail!(
                "Unknown uninstall-setup option. Use remnifold uninstall-setup --help."
            ),
        }
    }
    let Some(context) = SetupContext::detect() else {
        return Ok(
            json!({"status":"unmanaged","reason":"this executable is not a Setup (Velopack) installation"}),
        );
    };
    let mut result = json!({
        "executable": context.executable,
        "appId": context.app_id,
        "logonEntry": logon::describe(&context.app_id),
        "applied": apply,
    });
    if !apply {
        result["status"] = json!("planned");
        result["next"] = json!(
            "Run again with --apply, then move the app to the Trash (macOS), delete the AppImage (Linux) or uninstall it from Settings (Windows)."
        );
        return Ok(result);
    }
    let lines = unregister_context(&context);
    log_line(&format!("uninstall-setup: {}", lines.join("; ")));
    result["status"] = json!("removed");
    result["results"] = json!(lines);
    result["next"] = json!(
        "Move the app to the Trash (macOS), delete the AppImage and /var/tmp/velopack/<packId> (Linux) or uninstall it from Settings (Windows). Your memory server, configuration and state folders are kept."
    );
    Ok(result)
}

/// `main()` の最初（引数の解析より前）に、全サブコマンド共通で呼ぶ。
///
/// - 起動時の自動適用を切る（多プロセス型のため。適用は `update-apply` だけが行う）。
/// - Velopack のフック引数（`--veloapp-*`）は、ここで処理して終了コード 0 で終える。
/// - 管理外のビルドでは、Velopack 側が何もせずに戻る。
pub fn startup() -> Startup {
    let first_run = AtomicBool::new(false);
    let mut app = VelopackApp::build().set_auto_apply_on_startup(false);
    // 高速フックは Windows だけが呼ぶ（crate 側でも登録関数が Windows 限定）。macOS・Linux の登録は
    // 引数なし・`mcp` の起動時に ensure_registered / spawn_background が行う。
    #[cfg(windows)]
    {
        app = app
            .on_after_install_fast_callback(|_| {
                guarded("install", || register_everything("install", true, None))
            })
            .on_after_update_fast_callback(|_| {
                guarded("updated", || {
                    register_everything(
                        "updated",
                        false,
                        Some(registration::UPDATED_HOOK_BUDGET),
                    )
                })
            })
            .on_before_uninstall_fast_callback(|_| guarded("uninstall", unregister_everything));
    }
    app = app.on_first_run(|_| first_run.store(true, Ordering::SeqCst));
    app.run();
    Startup {
        first_run: first_run.load(Ordering::SeqCst),
    }
}

// ───────────────────────── 背景の確認・取得 ─────────────────────────

/// 状態フォルダーのロックを取る。他のプロセスが保持していれば None（その場合は何もしない）。
fn try_lock(state_dir: &Path) -> Option<fs::File> {
    try_lock_named(state_dir, LOCK_FILE)
}

/// 状態フォルダーの指定のロックファイルを try-lock する。保持はファイルを閉じるまで（プロセスの終了を含む）。
fn try_lock_named(state_dir: &Path, name: &str) -> Option<fs::File> {
    fs::create_dir_all(state_dir).ok()?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join(name))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// 確認・取得を1回行う。新版があれば取得だけして、適用はしない。
fn check_and_download() -> Result<String> {
    let manager = manager()?;
    Ok(match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(info) => {
            // 取得済みなら即座に戻る。Velopack が .partial から原子的に確定する。
            manager.download_updates(&info, None)?;
            format!(
                "downloaded {}; it is applied at the next logon",
                info.TargetFullRelease.Version
            )
        }
        UpdateCheck::NoUpdateAvailable => "up to date".to_owned(),
        UpdateCheck::RemoteIsEmpty => "the update feed is empty".to_owned(),
    })
}

/// 時期なら、ロックを取って確認・取得する。結果は設定ファイルに記録する。
fn background_cycle(state_dir: &Path) {
    if !should_check(&load_settings(state_dir), now_unix()) {
        return;
    }
    // 他のセッションが確認・取得中なら何もしない（二重取得しない）。
    let Some(_lock) = try_lock(state_dir) else {
        return;
    };
    // ロックを取る間に、別のプロセスが確認を終えていることがある。読み直して判定し直す。
    if !should_check(&load_settings(state_dir), now_unix()) {
        return;
    }
    match check_and_download() {
        Ok(message) => {
            let _ = record_time(state_dir, "last_checked_at", now_unix());
            log_line(&format!("auto-update: {message}"));
        }
        Err(error) => {
            // 失敗した時刻も記録し、24時間は再試行しない。現行版で動き続ける。
            let _ = record_time(state_dir, "last_failed_at", now_unix());
            log_line(&format!("auto-update failed: {error:#}"));
        }
    }
}

/// `mcp` の起動後に、確認・取得の専用スレッドを始める。Velopack 管理外では何もしない。
///
/// 標準出力は MCP のプロトコル専用のため、このスレッドは何も出力しない（結果は診断ログだけ）。
/// プロセスの終了で一緒に止まるので join しない。呼び出しは `mcp` の起動ごとに1回だけにする。
pub fn spawn_background() {
    if locate().is_none() {
        return;
    }
    let state_dir = Roots::current().state;
    let spawned = std::thread::Builder::new()
        .name("remnifold-auto-update".into())
        .spawn(move || {
            // まず登録が今の自分を指しているか（Windows ではフックで終わらなかった CLI が無いか）を確かめる
            // （普段は状態ファイルを読むだけで終わる）。MCP の応答を遅らせないよう、このスレッドで行う。
            ensure_registered();
            loop {
                // panic しても MCP 本体へは波及させない（このスレッドだけが止まる）。
                if std::panic::catch_unwind(|| background_cycle(&state_dir)).is_err() {
                    log_line("auto-update: unexpected failure");
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        log_line(&format!("auto-update thread could not start: {error}"));
    }
}

// ───────────────────────── ログオン時の適用 ─────────────────────────

/// `update-apply` の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Velopack 管理外（開発ビルド・旧 install 版）。
    Unmanaged,
    /// 設定で自動更新が切られている。
    Disabled,
    /// 保留中の更新が無い。
    NothingPending,
    /// 導入先から他のプロセスが動いているため、適用しなかった（次のログオンに回す）。
    Busy(Vec<u32>),
    /// 署名者などの検証に通らず、適用しなかった。
    Untrusted(String),
    /// 適用を Update.exe に依頼した（このプロセスの終了を待って入れ替わる。再起動しない）。
    Applied(String),
}

impl ApplyOutcome {
    /// 標準出力へ1行で出す JSON。
    pub fn to_json(&self) -> Value {
        match self {
            Self::Unmanaged => json!({"status":"unmanaged"}),
            Self::Disabled => json!({"status":"disabled"}),
            Self::NothingPending => json!({"status":"nothingPending"}),
            Self::Busy(pids) => json!({"status":"busy","processes":pids.len()}),
            Self::Untrusted(reason) => json!({"status":"untrusted","reason":reason}),
            Self::Applied(version) => json!({"status":"applied","version":version}),
        }
    }
}

/// 保留中の更新を、安全な場合だけ適用する。強制終了も再起動もしない。
pub fn update_apply() -> Result<ApplyOutcome> {
    let Some(locator) = locate() else {
        return Ok(ApplyOutcome::Unmanaged);
    };
    // ログオンのたびに、登録が残っているか（フックで終わらなかった CLI が無いか）を確かめて直す。
    // MCP の登録が消えていると Claude Code が `mcp` を起動しないため、ここが確実な再試行の機会になる。
    // 自動更新の設定とは関係なく行う。
    ensure_registered();
    let state_dir = Roots::current().state;
    if !load_settings(&state_dir).auto_update {
        return Ok(ApplyOutcome::Disabled);
    }
    let manager = manager()?;
    let Some(pending) = manager.get_update_pending_restart() else {
        return Ok(ApplyOutcome::NothingPending);
    };
    // 導入先から動いている自分以外のプロセスが1つでもあれば、適用しない。適用は全て強制終了してしまう。
    #[cfg(not(target_os = "linux"))]
    let root = locator.get_root_dir();
    #[cfg(target_os = "linux")]
    let root = locator.get_appimage_path();
    let others = processes::others_in(&root, std::process::id())?;
    if !others.is_empty() {
        log_line(&format!(
            "update-apply: skipped, {} other process(es) are running from the install folder",
            others.len()
        ));
        return Ok(ApplyOutcome::Busy(others));
    }
    let package = locator.get_packages_dir().join(&pending.FileName);
    ensure!(package.is_file(), "The pending update package is missing.");
    if let Err(error) = trust::verify_package(&package) {
        log_line(&format!("update-apply: refused: {error}"));
        return Ok(ApplyOutcome::Untrusted(error.to_string()));
    }
    // このプロセスの終了を待って適用し、再起動しない。Update.exe は別プロセスとして続行する。
    manager.unsafe_apply_updates(
        &pending,
        true,
        ApplyWaitMode::WaitPid(std::process::id()),
        false,
        Vec::<String>::new(),
    )?;
    log_line(&format!("update-apply: applying {}", pending.Version));
    Ok(ApplyOutcome::Applied(pending.Version.clone()))
}

// ───────────────────────── 案内（初回起動・引数なしの対話起動） ─────────────────────────

/// 利用者が直接起動したとき（Setup 直後の初回起動、スタートメニューからの起動）の案内。
/// `mcp` として stdin を待たず、導入済み・登録済みであることと次の手順を示す。
/// 端末が対話的で接続が未設定なら、そのまま `setup` を始める。
pub fn welcome(first_run: bool) -> Result<()> {
    use crate::locale::tr;
    use std::io::{BufRead, IsTerminal};
    println!("{}", tr("welcome.installed"));
    println!("{}", tr("welcome.updates"));
    let roots = Roots::current();
    let configured = crate::paths::config_file(&roots, &crate::common::codex_home())
        .ok()
        .flatten()
        .is_some();
    if configured {
        println!("{}", tr("welcome.configured"));
        return Ok(());
    }
    println!("{}", tr("welcome.next"));
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !interactive {
        println!("{}", tr("welcome.run_setup"));
        return Ok(());
    }
    print!("{}", tr("welcome.prompt_url"));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let url = line.trim();
    if url.is_empty() {
        println!("{}", tr("welcome.later"));
    } else {
        crate::local::setup(&["--base-url".to_string(), url.to_string()])?;
    }
    if first_run {
        // 初回起動のコンソールは、閉じると結果が読めなくなる。確認してから閉じる。
        print!("{}", tr("welcome.press_enter"));
        std::io::stdout().flush()?;
        let mut ignored = String::new();
        let _ = std::io::stdin().lock().read_line(&mut ignored);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("app-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn defaults_are_on_and_unchecked() {
        let settings = AutoUpdateSettings::default();
        assert!(settings.auto_update);
        assert_eq!(settings.last_checked_at, None);
        assert_eq!(settings.last_failed_at, None);
        assert_eq!(load_settings(&scratch()), settings);
    }

    #[test]
    fn broken_or_mistyped_json_falls_back_per_field() {
        let default = AutoUpdateSettings::default();
        for broken in [&b""[..], b"{", b"not json", b"[1,2]", b"null", b"\xff\xfe"] {
            assert_eq!(parse_settings(broken), default);
        }
        let parsed =
            parse_settings(br#"{"auto_update":"no","last_checked_at":123,"last_failed_at":"x"}"#);
        assert!(parsed.auto_update);
        assert_eq!(parsed.last_checked_at, Some(123));
        assert_eq!(parsed.last_failed_at, None);
        assert!(!parse_settings(br#"{"auto_update":false}"#).auto_update);
    }

    #[test]
    fn recording_keeps_other_keys_and_each_time_separately() {
        let dir = scratch();
        fs::write(
            dir.join(SETTINGS_FILE),
            br#"{"auto_update":false,"note":"mine"}"#,
        )
        .unwrap();
        record_time(&dir, "last_checked_at", 1_000).unwrap();
        record_time(&dir, "last_failed_at", 2_000).unwrap();
        let settings = load_settings(&dir);
        assert!(!settings.auto_update);
        assert_eq!(settings.last_checked_at, Some(1_000));
        assert_eq!(settings.last_failed_at, Some(2_000));
        let value: Value =
            serde_json::from_slice(&fs::read(dir.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(value["note"], "mine");
        // 一時ファイルを残さない。
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // 壊れたファイルは作り直す。
        fs::write(dir.join(SETTINGS_FILE), b"{ broken").unwrap();
        record_time(&dir, "last_checked_at", 5).unwrap();
        assert_eq!(load_settings(&dir).last_checked_at, Some(5));
    }

    #[test]
    fn throttle_counts_both_success_and_failure_times() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(is_due(None, 10));
        assert!(!is_due(Some(1_000), 1_000 + day - 1));
        assert!(is_due(Some(1_000), 1_000 + day));
        // 時計の巻き戻しで時刻が未来になっても、永久に確認しない状態にならない。
        assert!(is_due(Some(5_000), 1_000));
        let now = 1_000_000;
        let due = AutoUpdateSettings {
            auto_update: true,
            last_checked_at: Some(now - day),
            last_failed_at: Some(now - day),
        };
        assert!(should_check(&due, now));
        // 失敗が新しければ、成功が古くても確認しない。逆も同じ。
        assert!(!should_check(
            &AutoUpdateSettings {
                last_failed_at: Some(now - 60),
                ..due
            },
            now
        ));
        assert!(!should_check(
            &AutoUpdateSettings {
                last_checked_at: Some(now - 60),
                ..due
            },
            now
        ));
        assert!(!should_check(
            &AutoUpdateSettings {
                auto_update: false,
                ..due
            },
            now
        ));
    }

    #[test]
    fn lock_is_exclusive_between_holders() {
        let dir = scratch();
        let first = try_lock(&dir).expect("first holder");
        assert!(try_lock(&dir).is_none(), "second holder must not run");
        drop(first);
        assert!(try_lock(&dir).is_some());
    }

    #[test]
    fn unmanaged_builds_do_nothing() {
        // テストの実行ファイルは Velopack の導入先ではない。
        assert!(locate().is_none());
        assert_eq!(update_apply().unwrap(), ApplyOutcome::Unmanaged);
        spawn_background();
    }

    #[test]
    fn outcomes_report_stable_status_codes() {
        assert_eq!(
            ApplyOutcome::NothingPending.to_json()["status"],
            "nothingPending"
        );
        assert_eq!(ApplyOutcome::Busy(vec![1, 2]).to_json()["processes"], 2);
        assert_eq!(
            ApplyOutcome::Applied("9.0.1".into()).to_json()["version"],
            "9.0.1"
        );
    }
}
