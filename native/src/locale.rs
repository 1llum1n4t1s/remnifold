// 記憶の索引（<remnifold-index>）や検索結果の固定文言を、索引を書き込む環境の言語設定に合わせる。
//
// 判定順:
// 1. REMNIFOLD_LANGUAGE（明示の上書き。テストや、OS と別の言語で使いたい利用者向け）
// 2. POSIX の LC_ALL → LC_MESSAGES → LANG（C / POSIX は「未設定」とみなして次へ進む）
//    Windows でも Claude Code のフックは Git Bash 経由で起動され、Git Bash は Windows の
//    利用者ロケールから LANG を設定するため、同じ判定で揃う。
// 3. OS の表示言語（Windows は GetUserDefaultUILanguage、macOS は AppleLanguages の先頭）
// 4. どれも判定できなければ英語
//
// 文言は日本語と英語の2種類だけを持ち、日本語以外の言語は英語にする。
// 1プロセスで判定は1回だけ行い（macOS は子プロセスを使うため）、結果を使い回す。
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Japanese,
    English,
}

static CURRENT: OnceLock<Language> = OnceLock::new();

/// 現在の環境の言語。初回呼び出しで判定し、以後は同じ値を返す。
pub fn current() -> Language {
    *CURRENT.get_or_init(detect)
}

fn detect() -> Language {
    // 明示の上書きと POSIX のロケール変数は、空・C・POSIX 以外なら最初に見つかった値で決める。
    for name in ["REMNIFOLD_LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(language) = std::env::var(name).ok().as_deref().and_then(parse) {
            return language;
        }
    }
    os_language().unwrap_or(Language::English)
}

/// `ja`、`ja_JP.UTF-8`、`ja-JP`、`Japanese_Japan.932` などを日本語とみなす。
/// 空文字・`C`・`POSIX`（`C.UTF-8` を含む）は判定材料にならないため None を返す。
pub fn parse(value: &str) -> Option<Language> {
    let value = value.trim();
    let base = value
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if base.is_empty() || base == "c" || base == "posix" {
        return None;
    }
    let primary = base.split(['_', '-']).next().unwrap_or_default();
    Some(if primary == "ja" || primary == "japanese" {
        Language::Japanese
    } else {
        Language::English
    })
}

#[cfg(windows)]
fn os_language() -> Option<Language> {
    // LANGID の下位10bitが主言語。0x11 は LANG_JAPANESE。
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    Some(if id & 0x3ff == 0x11 {
        Language::Japanese
    } else {
        Language::English
    })
}

#[cfg(target_os = "macos")]
fn os_language() -> Option<Language> {
    // GUI から起動された Codex などでは LANG が無いことがあるため、OS の言語順の先頭を読む。
    // 出力は `(\n    "ja-JP",\n    "en-JP"\n)` の形式。
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text
        .split(['(', ')', ',', '\n'])
        .map(|part| part.trim().trim_matches('"'))
        .find(|part| !part.is_empty())?;
    parse(first)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn os_language() -> Option<Language> {
    None
}

/// 言語ごとの固定文言を選ぶ。
pub fn pick<'a>(japanese: &'a str, english: &'a str) -> &'a str {
    match current() {
        Language::Japanese => japanese,
        Language::English => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locale_identifiers() {
        for value in [
            "ja",
            "ja_JP.UTF-8",
            "ja-JP",
            "JA_jp",
            "Japanese_Japan.932",
            "ja_JP@calendar=japanese",
        ] {
            assert_eq!(parse(value), Some(Language::Japanese), "{value}");
        }
        for value in ["en_US.UTF-8", "en", "de_DE", "zh-Hans-CN", "fr_FR@euro"] {
            assert_eq!(parse(value), Some(Language::English), "{value}");
        }
        for value in ["", "  ", "C", "C.UTF-8", "POSIX"] {
            assert_eq!(parse(value), None, "{value:?}");
        }
    }
}
