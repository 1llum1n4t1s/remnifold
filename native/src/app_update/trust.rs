//! 取得した更新パッケージの署名者の検証（適用の直前に行う）。
//!
//! R2 からの HTTPS と Velopack のフィードのハッシュは、配信元の改ざんと転送の破損を防ぐが、
//! 「配信元に誰かが別のビルドを置いた」場合は防げない。そこで Windows では、適用前に
//! パッケージ内の `remnifold.exe` を取り出し、Authenticode の署名が有効で、署名者の CN が
//! 期待どおり（本人の Certum 証明書）であることを確かめる。満たさなければ適用しない。
//!
//! 検査の対象は、主実行ファイルだけでなく、パッケージ内の全ての PE（.exe・.dll）とする。
//! 後で実行される Update.exe（パッケージ内では `Squirrel.exe`）を差し替えられると、主実行ファイルの
//! 署名だけでは防げないため。Squirrel.exe と主実行ファイルは、ちょうど1つずつあることも確かめる。
//!
//! 限界（意図して範囲を絞っている）:
//! - 検査から適用までの間に利用者権限で nupkg を差し替えられる競合は防げない（同じ権限なら導入先の
//!   実行ファイルそのものも書き換えられるため、この検査の脅威モデルの外）。
//! - 失効確認（OCSP/CRL）は行わない（ログオン時にネットワークが無くても適用できるようにするため）。
//!
//! # Linux（AppImage）の信頼モデル（本人の判断、2026-11-15）
//! AppImage にはコード署名の標準が無いため、Linux は「R2 からの HTTPS」と「Velopack のフィードのハッシュ照合」
//! で信頼する。Velopack は取得時に、nupkg の SHA-256（無ければ SHA-1）をフィード `releases.linux-*.json` の値と
//! 照合してから確定する（velopack crate 1.2.161 `UpdateManager::download_updates` / `verify_package_checksum`）。
//! ここでは、それに加えて署名の検証はしない。代わりに、適用の直前に次の「置き場の健全性」だけを確かめる。
//! - Velopack の Linux の取得先は `/var/tmp/velopack/<packId>/packages` に固定されている（HOME に従わない。
//!   crate の locator）。`/var/tmp` は他の利用者も書けるため、別の利用者が先にこのフォルダーを作っておくと、
//!   ハッシュ照合の後で nupkg を差し替えられる（`get_update_pending_restart` は置き場の nupkg を版だけで選び、
//!   適用時にハッシュを照合し直さない）。そこで、nupkg と packages フォルダーと `<packId>` フォルダーが
//!   いずれも自分の所有で、他人（other）が書けず、グループが書ける場合はそのグループが自分の主グループである
//!   こと（Ubuntu の既定 umask 002 の利用者別グループを許すため）、nupkg がリンクでない通常ファイルであることを
//!   要求する。満たさなければ適用しない（untrusted）。
//! - nupkg の中身は、vpk 1.2.161 の Linux 版の形（`lib/app/` 直下に AppImage がちょうど1つ）であることと、
//!   曖昧な名前（`\`・`:`・`..`・絶対パス）が無いことを確かめる。
//!
//! 限界: R2 の配信元に別のビルドを置かれた場合（フィードごと差し替えられた場合）は防げない。将来、独自の署名
//! （例: ed25519 の公開鍵をアプリに埋め込み、nupkg の署名を配信物に添える）を足す余地があるが、今は実装しない。
//! 検査から適用までの間の差し替え（TOCTOU）も、同じ利用者の権限なら AppImage そのものを書き換えられるため
//! 範囲外とする。
//!
//! macOS では、UpdateMac が導入先へ展開するのと同じ関数（velopack の `extract_lib_contents_to_path`）で
//! nupkg を私的な一時フォルダーへ展開し、その .app を `codesign --verify --deep --strict` と、
//! 「Apple が発行した Developer ID Application 証明書で、チーム ID が本人のもの」という要件（-R）で検証する。
//! `--deep` で、バンドル内の全ての Mach-O（主実行ファイルと UpdateMac）の署名と、封印された全ファイルの
//! ハッシュを確かめる（封印外のファイルの追加も `--strict` で拒否される）。
//! 続けて公証を `spctl --assess --type execute` で確かめる（`source=Notarized Developer ID` であること）。
//! vpk は .app を公証してチケットを貼ってから nupkg を作るため、チケットは nupkg の中にある（実機で確認）。
//! そのため、ログイン時にネットワークが無くても判定できる見込み（オフラインでの判定は未検証）。
//! 失効確認は Windows と同じく行わない。
use anyhow::{Result, bail};
use std::path::Path;

/// 期待する署名者の CN（本人の共通のコード署名証明書。thumbprint は年次更新で変わるため CN で照合する）。
#[cfg_attr(not(windows), allow(dead_code))]
pub const EXPECTED_SIGNER_COMMON_NAME: &str = "Open Source Developer Yuichiro Shinozaki";
/// パッケージ内の主実行ファイルのパス（Velopack の nupkg は `lib/app/` 配下に導入物を持つ）。
const MAIN_EXE_ENTRY: &str = "lib/app/remnifold.exe";
/// Velopack の Update.exe の、パッケージ内での名前。
const UPDATER_ENTRY: &str = "lib/app/Squirrel.exe";
/// 期待する Apple Developer のチーム ID（Developer ID Application 証明書の OU）。公開情報で秘密ではない。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const EXPECTED_TEAM_ID: &str = "SL228Y8UUR";
/// macOS のパッケージ内の主実行ファイル（nupkg の `lib/app/` が .app の中身そのもの）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAC_MAIN_EXE_ENTRY: &str = "lib/app/Contents/MacOS/remnifold";
/// macOS の更新器（UpdateMac）の、パッケージ内での名前。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAC_UPDATER_ENTRY: &str = "lib/app/Contents/MacOS/UpdateMac";

/// 署名の検証を省いてよいか。テスト用の feature を付けたビルドで、環境変数が明示されたときだけ真。
/// 本番ビルドでは常に偽で、この分岐自体がコンパイルされない。
fn unsigned_allowed_for_test() -> bool {
    #[cfg(feature = "update-feed-test")]
    {
        std::env::var_os("REMNIFOLD_UPDATE_ALLOW_UNSIGNED").is_some_and(|value| value == "1")
    }
    #[cfg(not(feature = "update-feed-test"))]
    {
        false
    }
}

/// nupkg の中の主実行ファイルを検証する。通れば Ok、満たさなければ理由つきの Err。
pub fn verify_package(package: &Path) -> Result<()> {
    if unsigned_allowed_for_test() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        windows::verify_package(package, EXPECTED_SIGNER_COMMON_NAME)
    }
    #[cfg(target_os = "macos")]
    {
        macos::verify_package(package, EXPECTED_TEAM_ID)
    }
    #[cfg(target_os = "linux")]
    {
        // 署名は検証しない（モジュールの説明の信頼モデル）。置き場の健全性と中身の形だけを確かめる。
        linux::verify_package(package)
    }
}

/// Linux の nupkg の名前を確かめる。曖昧な名前はパッケージごと拒否し、`lib/app/` 直下の AppImage が
/// ちょうど1つあることを要求する（vpk 1.2.161 の Linux の nupkg の形。UpdateNix はこれを取り出して置き換える）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_package_names(names: &[String]) -> Result<()> {
    let mut appimages = 0;
    for name in names {
        if name.contains(['\\', ':'])
            || name.starts_with('/')
            || name.split('/').any(|part| part == "..")
        {
            bail!("The update package has an entry name that is not allowed.");
        }
        if let Some(file) = name.strip_prefix("lib/app/")
            && !file.contains('/')
            && file.to_ascii_lowercase().ends_with(".appimage")
        {
            appimages += 1;
        }
    }
    if appimages != 1 {
        bail!("The update package must contain exactly one AppImage.");
    }
    Ok(())
}

/// 置き場の所有者・権限が安全か（自分の所有で、other が書けず、group が書けるならそのグループが自分の主グループ）。
/// 純関数にして、どの OS の単体テストでも確かめる。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn owned_and_private(owner: u32, group: u32, mode: u32, uid: u32, gid: u32) -> bool {
    owner == uid && mode & 0o002 == 0 && (mode & 0o020 == 0 || group == gid)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    unsafe extern "C" {
        fn getuid() -> u32;
        fn getgid() -> u32;
    }

    pub fn verify_package(package: &Path) -> Result<()> {
        // SAFETY: getuid / getgid は引数を取らず、失敗しない。
        let (uid, gid) = unsafe { (getuid(), getgid()) };
        let file = std::fs::symlink_metadata(package)?;
        if !file.file_type().is_file() {
            bail!("The pending update package is not a regular file.");
        }
        let mut checked = vec![(package.to_path_buf(), file)];
        // packages フォルダーと、その親（`<packId>` フォルダー）。`/var/tmp/velopack` 自体は最初に使った
        // 利用者のものなので確かめない（`<packId>` を差し替えられても、ここで所有者の違いとして気づく）。
        for directory in package.ancestors().skip(1).take(2) {
            checked.push((
                directory.to_path_buf(),
                std::fs::symlink_metadata(directory)?,
            ));
        }
        for (path, meta) in &checked {
            if !owned_and_private(meta.uid(), meta.gid(), meta.mode(), uid, gid) {
                bail!(
                    "The update package location is not private to this user: {}",
                    path.display()
                );
            }
        }
        let mut archive = zip::ZipArchive::new(std::fs::File::open(package)?)
            .map_err(|_| anyhow::anyhow!("The update package is not a readable archive."))?;
        let mut names = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            names.push(archive.by_index_raw(index)?.name()?.into_owned());
        }
        linux_package_names(&names)
    }
}

/// macOS の nupkg の名前を確かめる。区切りの `\`、ドライブの `:`、`..`、絶対パスを含む名前はパッケージごと
/// 拒否し、主実行ファイルと UpdateMac が大小を無視してちょうど1つずつあることを要求する。
/// 戻り値は展開後の合計サイズ（zip 爆弾の検出に使う）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_package_names(entries: &[(String, u64)]) -> Result<u64> {
    let (mut main, mut updater, mut total) = (0, 0, 0u64);
    for (name, size) in entries {
        if name.contains(['\\', ':'])
            || name.starts_with('/')
            || name.split('/').any(|part| part == "..")
        {
            bail!("The update package has an entry name that is not allowed.");
        }
        main += usize::from(name.eq_ignore_ascii_case(MAC_MAIN_EXE_ENTRY));
        updater += usize::from(name.eq_ignore_ascii_case(MAC_UPDATER_ENTRY));
        total = total.saturating_add(*size);
    }
    if main != 1 || updater != 1 {
        bail!("The update package must contain exactly one main executable and one updater.");
    }
    Ok(total)
}

/// codesign に渡す要件。Apple のルートに連なり（anchor apple generic）、中間が Developer ID の CA
/// （1.2.840.113635.100.6.2.6）、葉が Developer ID Application（1.2.840.113635.100.6.1.13）で、
/// 葉の OU（チーム ID）が期待どおりであること。自己署名の証明書に同じ OU を書いても通らない。
/// codesign には `-R=<要件>` の1引数で渡す（getopt が `=<要件>` を値として受け取り、先頭の `=` が
/// 「ファイルではなく要件の文字列」の印になる。要件の側にも `=` を付けると構文エラーになる。実機で確認）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn developer_id_requirement(team_id: &str) -> String {
    format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{team_id}\""
    )
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{os::unix::fs::DirBuilderExt, path::PathBuf, process::Command};

    /// 展開後の合計サイズの上限（Remnifold の .app は数十 MB。これを大きく超えるものは異常として拒否する）。
    const MAX_EXTRACTED: u64 = 1024 * 1024 * 1024;

    pub fn verify_package(package: &Path, team_id: &str) -> Result<()> {
        // 名前とサイズを先に確かめる（展開前に、曖昧な名前と巨大な展開を拒否する）。
        let mut archive = zip::ZipArchive::new(std::fs::File::open(package)?)
            .map_err(|_| anyhow::anyhow!("The update package is not a readable archive."))?;
        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index)?;
            entries.push((entry.name()?.into_owned(), entry.size()));
        }
        drop(archive);
        if mac_package_names(&entries)? > MAX_EXTRACTED {
            bail!("The update package is too large.");
        }
        // 検査は私的な一時フォルダー（0700）で行い、結果にかかわらず消す。
        let work =
            std::env::temp_dir().join(format!("remnifold-update-verify-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&work)?;
        let result = verify_in(package, &work, team_id);
        let _ = std::fs::remove_dir_all(&work);
        result
    }

    fn verify_in(package: &Path, work: &Path, team_id: &str) -> Result<()> {
        // .app としての名前で展開する（codesign はバンドルとして扱う）。展開は UpdateMac と同じ関数で行い、
        // 実際に導入先へ入れ替わる中身（.__symlink の復元や実行権限の付与を含む）を検証する。
        let app: PathBuf = work.join("Remnifold.app");
        let bundle = velopack::bundle::load_bundle_from_file(package).map_err(|_| {
            anyhow::anyhow!("The update package is not a readable Velopack package.")
        })?;
        bundle
            .extract_lib_contents_to_path(&app, |_| {})
            .map_err(|_| {
                anyhow::anyhow!("The update package could not be extracted for verification.")
            })?;
        verify_app(&app, team_id)
    }

    /// .app の署名が、バンドル全体で有効かつ期待の Developer ID のものであることを確かめる。
    pub fn verify_app(app: &Path, team_id: &str) -> Result<()> {
        let status = Command::new("/usr/bin/codesign")
            .arg("--verify")
            .arg("--deep")
            .arg("--strict")
            .arg(format!("-R={}", developer_id_requirement(team_id)))
            .arg(app)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| anyhow::anyhow!("codesign could not be started."))?;
        if !status.success() {
            bail!("The update package is not signed by the expected Developer ID.");
        }
        if !notarized(app) {
            bail!("The update package is not notarized.");
        }
        Ok(())
    }

    /// 公証の確認。vpk は .app を公証してチケットを .app に貼ってから nupkg を作るため（実機で確認）、
    /// 展開した .app には `Contents/CodeResources` としてチケットがあり、`spctl` はそれで判定できる
    /// （ネットワーク無しでの判定は見込みで、未検証）。
    /// 利用者が Gatekeeper の判定自体を切っている（`spctl --status` が disabled）場合は、この確認を省く
    /// （利用者が公証の保証を自ら外しているため。Developer ID の署名の確認は常に行う）。
    fn notarized(app: &Path) -> bool {
        let assessed = Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute", "-vv"])
            .arg(app)
            .stdin(std::process::Stdio::null())
            .output();
        if let Ok(output) = assessed {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if output.status.success() && text.contains("source=Notarized Developer ID") {
                return true;
            }
        }
        Command::new("/usr/sbin/spctl")
            .arg("--status")
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| {
                String::from_utf8_lossy(&output.stdout).contains("assessments disabled")
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 未署名・ad-hoc 署名のバンドルは拒否される。
        #[test]
        fn unsigned_and_ad_hoc_bundles_are_rejected() {
            let base =
                std::env::temp_dir().join(format!("remnifold-trust-{}", uuid::Uuid::new_v4()));
            let app = base.join("Test.app");
            let bin = app.join("Contents/MacOS");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(
                app.join("Contents/Info.plist"),
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<plist version=\"1.0\"><dict>",
                    "<key>CFBundleExecutable</key><string>tool</string>",
                    "<key>CFBundleIdentifier</key><string>com.example.remnifold-test</string>",
                    "</dict></plist>\n"
                ),
            )
            .unwrap();
            std::fs::copy("/bin/sleep", bin.join("tool")).unwrap();
            assert!(verify_app(&app, EXPECTED_TEAM_ID).is_err());
            let signed = Command::new("/usr/bin/codesign")
                .args(["--force", "--deep", "--sign", "-"])
                .arg(&app)
                .output()
                .unwrap();
            assert!(signed.status.success());
            // ad-hoc 署名は署名としては有効だが、Developer ID の要件を満たさない。
            assert!(
                Command::new("/usr/bin/codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(&app)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(verify_app(&app, EXPECTED_TEAM_ID).is_err());
            let _ = std::fs::remove_dir_all(&base);
        }

        /// 署名・公証済みの .app がある環境でだけ、要件の照合を確かめる。
        /// `REMNIFOLD_TEST_SIGNED_APP` を指定したときだけ走る（期待のチーム ID で通り、別の ID で拒否される）。
        #[test]
        fn signed_app_probe() {
            let Some(app) = std::env::var_os("REMNIFOLD_TEST_SIGNED_APP") else {
                return;
            };
            let app = PathBuf::from(app);
            verify_app(&app, EXPECTED_TEAM_ID).unwrap();
            assert!(verify_app(&app, "AAAAAAAAAA").is_err());
            // 署名は正しいが公証されていない .app（`REMNIFOLD_TEST_UNNOTARIZED_APP`）は拒否される。
            if let Some(unnotarized) = std::env::var_os("REMNIFOLD_TEST_UNNOTARIZED_APP") {
                assert!(verify_app(&PathBuf::from(unnotarized), EXPECTED_TEAM_ID).is_err());
            }
        }
    }
}

/// nupkg（zip）の中の名前が、書き出し先で曖昧にならないかを確かめ、署名を検査すべき PE（.exe・.dll）の
/// 位置を返す。区切りの `\`、ドライブの `:`、`..`、絶対パスを含む名前は、正規のパッケージには現れない
/// ためパッケージごと拒否する。主実行ファイルと Update.exe は、大小を無視してちょうど1つずつあること。
#[cfg_attr(not(windows), allow(dead_code))]
fn package_pe_entries(names: &[String]) -> Result<Vec<usize>> {
    let (mut main, mut updater) = (0, 0);
    let mut entries = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if name.contains(['\\', ':'])
            || name.starts_with('/')
            || name.split('/').any(|part| part == "..")
        {
            bail!("The update package has an entry name that is not allowed.");
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".dll") {
            entries.push(index);
        }
        main += usize::from(name.eq_ignore_ascii_case(MAIN_EXE_ENTRY));
        updater += usize::from(name.eq_ignore_ascii_case(UPDATER_ENTRY));
    }
    if main != 1 || updater != 1 {
        bail!("The update package must contain exactly one main executable and one updater.");
    }
    Ok(entries)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        io::{Read, Write},
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
    };
    use windows_sys::{
        Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            Security::{
                Cryptography::{
                    CERT_CONTEXT, CERT_FIND_SUBJECT_CERT, CERT_INFO, CERT_NAME_ATTR_TYPE,
                    CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED, CERT_QUERY_FORMAT_FLAG_BINARY,
                    CERT_QUERY_OBJECT_FILE, CMSG_SIGNER_CERT_INFO_PARAM, CertCloseStore,
                    CertFindCertificateInStore, CertFreeCertificateContext, CertGetNameStringW,
                    CryptMsgClose, CryptMsgGetParam, CryptQueryObject, PKCS_7_ASN_ENCODING,
                    X509_ASN_ENCODING, szOID_COMMON_NAME,
                },
                WinTrust::{
                    WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_NONE,
                    WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
                },
            },
        },
        core::GUID,
    };

    /// WINTRUST_ACTION_GENERIC_VERIFY_V2 {00AAC56B-CD44-11d0-8CC2-00C04FC295EE}。
    const GENERIC_VERIFY_V2: GUID = GUID {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };

    pub fn verify_package(package: &Path, expected_common_name: &str) -> Result<()> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(package)?)
            .map_err(|_| anyhow::anyhow!("The update package is not a readable archive."))?;
        let mut names = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            names.push(archive.by_index_raw(index)?.name()?.into_owned());
        }
        let entries = package_pe_entries(&names)?;
        // 検査する実体は、検査のために取り出した私的な一時ファイル。検査後に必ず消す。
        for index in entries {
            let temporary = std::env::temp_dir().join(format!(
                "remnifold-update-verify-{}.bin",
                uuid::Uuid::new_v4()
            ));
            let result = (|| -> Result<()> {
                let mut entry = archive.by_index(index)?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                // 展開サイズを制限する（zip 爆弾で一時領域を使い切られないため）。
                let copied =
                    std::io::copy(&mut entry.by_ref().take(512 * 1024 * 1024 + 1), &mut file)?;
                if copied > 512 * 1024 * 1024 {
                    bail!("An executable in the update package is too large.");
                }
                file.flush()?;
                drop(file);
                verify_file(&temporary, expected_common_name)
            })();
            let _ = std::fs::remove_file(&temporary);
            result?;
        }
        Ok(())
    }

    /// ファイルの Authenticode 署名が有効で、署名者の CN が期待と一致することを確かめる。
    pub fn verify_file(path: &Path, expected_common_name: &str) -> Result<()> {
        if !signature_is_valid(path) {
            bail!("An executable in the update package has no valid Authenticode signature.");
        }
        let actual = signer_common_name(path).ok_or_else(|| {
            anyhow::anyhow!("The signer of the update package could not be read.")
        })?;
        if actual != expected_common_name {
            bail!("The update package is signed by an unexpected publisher.");
        }
        Ok(())
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn signature_is_valid(path: &Path) -> bool {
        let path = wide(path);
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: path.as_ptr(),
            hFile: null_mut(),
            pgKnownSubject: null_mut(),
        };
        // SAFETY: ゼロ初期化した構造体に必要な項目を設定してから渡す。state は VERIFY の後に CLOSE で解放する。
        unsafe {
            let mut data: WINTRUST_DATA = std::mem::zeroed();
            data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
            data.dwUIChoice = WTD_UI_NONE;
            data.fdwRevocationChecks = WTD_REVOKE_NONE;
            data.dwUnionChoice = WTD_CHOICE_FILE;
            data.Anonymous.pFile = &mut file;
            data.dwStateAction = WTD_STATEACTION_VERIFY;
            let mut action = GENERIC_VERIFY_V2;
            let status = WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                &mut data as *mut WINTRUST_DATA as *mut c_void,
            );
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                &mut data as *mut WINTRUST_DATA as *mut c_void,
            );
            status == 0
        }
    }

    /// 埋め込み署名の最初の署名者の証明書から、CN を読む（CryptQueryObject の定番の手順）。
    fn signer_common_name(path: &Path) -> Option<String> {
        let path = wide(path);
        let mut store = null_mut();
        let mut message: *mut c_void = null_mut();
        // SAFETY: 出力はいずれも自分の変数。得たハンドル・コンテキストは末尾で必ず解放する。
        unsafe {
            if CryptQueryObject(
                CERT_QUERY_OBJECT_FILE,
                path.as_ptr() as *const c_void,
                CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
                CERT_QUERY_FORMAT_FLAG_BINARY,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut store,
                &mut message,
                null_mut(),
            ) == 0
            {
                return None;
            }
            let name = (|| {
                let mut size = 0u32;
                if CryptMsgGetParam(
                    message,
                    CMSG_SIGNER_CERT_INFO_PARAM,
                    0,
                    null_mut(),
                    &mut size,
                ) == 0
                {
                    return None;
                }
                // CERT_INFO を指すポインタを含むため、8 バイト境界の領域に取る。
                let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
                if CryptMsgGetParam(
                    message,
                    CMSG_SIGNER_CERT_INFO_PARAM,
                    0,
                    buffer.as_mut_ptr() as *mut c_void,
                    &mut size,
                ) == 0
                {
                    return None;
                }
                let info = buffer.as_ptr() as *const CERT_INFO;
                let certificate: *mut CERT_CONTEXT = CertFindCertificateInStore(
                    store,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    0,
                    CERT_FIND_SUBJECT_CERT,
                    info as *const c_void,
                    null(),
                );
                if certificate.is_null() {
                    return None;
                }
                let length = CertGetNameStringW(
                    certificate,
                    CERT_NAME_ATTR_TYPE,
                    0,
                    szOID_COMMON_NAME as *const c_void,
                    null_mut(),
                    0,
                );
                let mut text = vec![0u16; length as usize];
                let written = CertGetNameStringW(
                    certificate,
                    CERT_NAME_ATTR_TYPE,
                    0,
                    szOID_COMMON_NAME as *const c_void,
                    text.as_mut_ptr(),
                    length,
                );
                CertFreeCertificateContext(certificate);
                if written <= 1 {
                    return None;
                }
                Some(String::from_utf16_lossy(&text[..written as usize - 1]))
            })();
            CertCloseStore(store, 0);
            CryptMsgClose(message);
            name
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unsigned_executable_is_rejected() {
            // このテスト実行ファイル自身は未署名。
            let me = std::env::current_exe().unwrap();
            assert!(verify_file(&me, EXPECTED_SIGNER_COMMON_NAME).is_err());
        }

        /// 署名済みファイルがある環境でだけ、署名者の CN の取得と照合を確かめる。
        /// `REMNIFOLD_TEST_SIGNED_FILE` と `REMNIFOLD_TEST_SIGNED_CN` を両方指定したときだけ走る。
        #[test]
        fn signed_file_probe() {
            let (Some(file), Some(cn)) = (
                std::env::var_os("REMNIFOLD_TEST_SIGNED_FILE"),
                std::env::var("REMNIFOLD_TEST_SIGNED_CN").ok(),
            ) else {
                return;
            };
            let file = std::path::PathBuf::from(file);
            assert_eq!(signer_common_name(&file).as_deref(), Some(cn.as_str()));
            verify_file(&file, &cn).unwrap();
            assert!(verify_file(&file, "Someone Else").is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn main_executable_and_updater_must_appear_exactly_once() {
        let base = [
            "a.nuspec",
            "lib/app/Remnifold.EXE",
            "lib/app/squirrel.exe",
            "lib/app/x.dll",
            "lib/app/README.md",
        ];
        // 検査するのは全ての .exe と .dll（README は対象外）。
        assert_eq!(package_pe_entries(&names(&base)).unwrap(), vec![1, 2, 3]);
        assert!(package_pe_entries(&names(&["lib/app/x.dll", "lib/app/Squirrel.exe"])).is_err());
        assert!(package_pe_entries(&names(&["lib/app/remnifold.exe"])).is_err());
        let mut duplicated = base.to_vec();
        duplicated.push("LIB/APP/remnifold.exe");
        assert!(package_pe_entries(&names(&duplicated)).is_err());
    }

    #[test]
    fn mac_package_requires_one_main_executable_and_updater() {
        let entries = |items: &[&str]| -> Vec<(String, u64)> {
            items.iter().map(|item| (item.to_string(), 10)).collect()
        };
        let base = [
            "RemnifoldApp.nuspec",
            "lib/app/Contents/Info.plist",
            "lib/app/Contents/MacOS/remnifold",
            "lib/app/Contents/MacOS/UpdateMac",
            "lib/app/Contents/_CodeSignature/CodeResources",
        ];
        assert_eq!(mac_package_names(&entries(&base)).unwrap(), 50);
        assert!(mac_package_names(&entries(&base[..3])).is_err());
        let mut duplicated = base.to_vec();
        duplicated.push("LIB/APP/Contents/MacOS/Remnifold");
        assert!(mac_package_names(&entries(&duplicated)).is_err());
        for bad in ["lib/app/../x", "/abs", "lib\\app\\x", "C:/x"] {
            let mut items = base.to_vec();
            items.push(bad);
            assert!(mac_package_names(&entries(&items)).is_err(), "{bad}");
        }
        let requirement = developer_id_requirement("ABCDE12345");
        assert!(requirement.starts_with("anchor apple generic"));
        assert!(requirement.ends_with("certificate leaf[subject.OU] = \"ABCDE12345\""));
    }

    #[test]
    fn linux_package_requires_one_appimage_and_a_private_location() {
        let base = ["RemnifoldApp.nuspec", "[Content_Types].xml", "_rels/.rels"];
        let with = |extra: &[&str]| {
            let mut items = base.to_vec();
            items.extend_from_slice(extra);
            names(&items)
        };
        assert!(linux_package_names(&with(&["lib/app/RemnifoldApp.AppImage"])).is_ok());
        assert!(linux_package_names(&with(&[])).is_err());
        assert!(linux_package_names(&with(&["lib/app/A.AppImage", "lib/app/B.appimage"])).is_err());
        // 入れ子の AppImage は数えない。
        assert!(linux_package_names(&with(&["lib/app/x/A.AppImage"])).is_err());
        for bad in ["lib/app/../x", "/abs", "lib\\app\\x", "C:/x"] {
            assert!(
                linux_package_names(&with(&["lib/app/A.AppImage", bad])).is_err(),
                "{bad}"
            );
        }
        // 所有者・権限: 自分の所有で 0755/0644 は可。other が書ける・他人の所有は不可。
        // グループが書ける場合は、そのグループが自分の主グループのときだけ可（umask 002 の利用者別グループ）。
        assert!(owned_and_private(1000, 1000, 0o40755, 1000, 1000));
        assert!(owned_and_private(1000, 1000, 0o100664, 1000, 1000));
        assert!(!owned_and_private(1000, 50, 0o40775, 1000, 1000));
        assert!(!owned_and_private(1000, 1000, 0o41777, 1000, 1000));
        assert!(!owned_and_private(1001, 1000, 0o40755, 1000, 1000));
    }

    #[test]
    fn ambiguous_entry_names_reject_the_whole_package() {
        for bad in [
            "lib/app/..\\x.exe",
            "lib/../remnifold.exe",
            "C:/x.exe",
            "/abs.exe",
            "lib\\app\\remnifold.exe",
        ] {
            assert!(
                package_pe_entries(&names(&[
                    "lib/app/remnifold.exe",
                    "lib/app/Squirrel.exe",
                    bad
                ]))
                .is_err(),
                "{bad}"
            );
        }
    }
}
