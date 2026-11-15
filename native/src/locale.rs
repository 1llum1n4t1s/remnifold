// 人が読む固定文言（CLI の出力・ヘルプ・フックの通知・索引の見出し）を、利用者の言語設定に合わせて出す。
//
// 判定順:
// 1. REMNIFOLD_LANGUAGE（明示の上書き。テストや、OS と別の言語で使いたい利用者向け）
// 2. POSIX の LC_ALL → LC_MESSAGES → LANG（C / POSIX は「未設定」とみなして次へ進む）
//    Windows でも Claude Code のフックは Git Bash 経由で起動され、Git Bash は Windows の
//    利用者ロケールから LANG を設定するため、同じ判定で揃う。
// 3. OS の表示言語（Windows は GetUserDefaultUILanguage、macOS は AppleLanguages の先頭）
// 4. どれも判定できない・未対応の言語なら英語
//
// 対応言語は ja・en・zh-CN・zh-TW・ko・es・fr・de・pt-BR の9つ。文言は言語ごとの JSON 辞書
// （`locales/<タグ>.json`）に持ち、コードはキーで参照する。辞書に無いキーは英語、英語にも無ければキーそのものを返す。
// AI が読む文言（MCP ツールの説明など）と、保存データ・識別子・技術的なエラー詳細は訳さない。
// 1プロセスで判定は1回だけ行い（macOS は子プロセスを使うため）、結果を使い回す。
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Japanese,
    English,
    ChineseSimplified,
    ChineseTraditional,
    Korean,
    Spanish,
    French,
    German,
    PortugueseBrazil,
}

impl Language {
    /// 対応する全言語。辞書ファイルの一覧・整合テストはこの並びを基準にする。
    pub const ALL: [Language; 9] = [
        Language::Japanese,
        Language::English,
        Language::ChineseSimplified,
        Language::ChineseTraditional,
        Language::Korean,
        Language::Spanish,
        Language::French,
        Language::German,
        Language::PortugueseBrazil,
    ];

    /// 辞書ファイル名にも使う言語タグ。
    pub fn tag(self) -> &'static str {
        match self {
            Language::Japanese => "ja",
            Language::English => "en",
            Language::ChineseSimplified => "zh-CN",
            Language::ChineseTraditional => "zh-TW",
            Language::Korean => "ko",
            Language::Spanish => "es",
            Language::French => "fr",
            Language::German => "de",
            Language::PortugueseBrazil => "pt-BR",
        }
    }

    /// 辞書の JSON 原文（コンパイル時に埋め込む）。
    pub fn dictionary_source(self) -> &'static str {
        match self {
            Language::Japanese => include_str!("locales/ja.json"),
            Language::English => include_str!("locales/en.json"),
            Language::ChineseSimplified => include_str!("locales/zh-CN.json"),
            Language::ChineseTraditional => include_str!("locales/zh-TW.json"),
            Language::Korean => include_str!("locales/ko.json"),
            Language::Spanish => include_str!("locales/es.json"),
            Language::French => include_str!("locales/fr.json"),
            Language::German => include_str!("locales/de.json"),
            Language::PortugueseBrazil => include_str!("locales/pt-BR.json"),
        }
    }

    fn index(self) -> usize {
        Language::ALL
            .iter()
            .position(|language| *language == self)
            .expect("every language is listed")
    }
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

/// `ja`、`ja_JP.UTF-8`、`zh-Hant-TW`、`pt_BR`、`Japanese_Japan.932` などを対応言語に照合する。
/// 照合規則: `zh-Hans`・`zh-CN`・`zh-SG` は zh-CN、`zh-Hant`・`zh-TW`・`zh-HK`・`zh-MO` は zh-TW、
/// `pt`（`pt-PT` を含む）は pt-BR、それ以外は主言語で照合する。未対応の言語は英語。
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
    let mut subtags = base.split(['_', '-']);
    let primary = subtags.next().unwrap_or_default();
    let rest: Vec<&str> = subtags.collect();
    Some(match primary {
        "ja" | "japanese" => Language::Japanese,
        "zh" => chinese(&rest),
        // Windows の英語名ロケール（`Chinese (Traditional)_Taiwan.950` など）。
        name if name.starts_with("chinese") => {
            if ["traditional", "taiwan", "hong kong", "macao", "macau"]
                .iter()
                .any(|marker| base.contains(marker))
            {
                Language::ChineseTraditional
            } else {
                Language::ChineseSimplified
            }
        }
        "ko" | "korean" => Language::Korean,
        "es" | "spanish" => Language::Spanish,
        "fr" | "french" => Language::French,
        "de" | "german" => Language::German,
        "pt" | "portuguese" => Language::PortugueseBrazil,
        _ => Language::English,
    })
}

/// `zh` の続きのタグから簡体字・繁体字を決める。文字体系（Hans/Hant）を地域より優先し、
/// どちらも無い `zh` だけの指定は簡体字とする。
fn chinese(rest: &[&str]) -> Language {
    if rest.contains(&"hant") {
        return Language::ChineseTraditional;
    }
    if rest.contains(&"hans") {
        return Language::ChineseSimplified;
    }
    if rest.iter().any(|tag| ["tw", "hk", "mo"].contains(tag)) {
        return Language::ChineseTraditional;
    }
    Language::ChineseSimplified
}

#[cfg(windows)]
fn os_language() -> Option<Language> {
    // LANGID の下位10bitが主言語、上位6bitが副言語。中国語は副言語で簡体字・繁体字を分ける。
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    Some(language_from_langid(id))
}

/// Windows の LANGID を対応言語へ変換する。未対応の言語は英語。
#[cfg_attr(not(windows), allow(dead_code))]
fn language_from_langid(id: u16) -> Language {
    match id & 0x3ff {
        0x11 => Language::Japanese,
        0x12 => Language::Korean,
        0x0a => Language::Spanish,
        0x0c => Language::French,
        0x07 => Language::German,
        // pt-PT（0x0816）も pt-BR として扱う。
        0x16 => Language::PortugueseBrazil,
        0x04 => {
            // 副言語: 0x01 台湾・0x03 香港・0x05 マカオ・0x1f（zh-Hant 0x7c04）は繁体字。
            match id >> 10 {
                0x01 | 0x03 | 0x05 | 0x1f => Language::ChineseTraditional,
                _ => Language::ChineseSimplified,
            }
        }
        _ => Language::English,
    }
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

type Dictionary = HashMap<String, String>;

/// 言語ごとの辞書。初回参照時に JSON を読み、以後は使い回す（壊れた辞書は空として扱い、英語へ落とす）。
fn dictionary(language: Language) -> &'static Dictionary {
    static DICTIONARIES: [OnceLock<Dictionary>; 9] = [const { OnceLock::new() }; 9];
    DICTIONARIES[language.index()]
        .get_or_init(|| serde_json::from_str(language.dictionary_source()).unwrap_or_default())
}

/// 指定した言語でキーの文言を返す。辞書に無ければ英語、英語にも無ければキーそのもの。
pub fn tr_in(language: Language, key: &'static str) -> &'static str {
    dictionary(language)
        .get(key)
        .or_else(|| dictionary(Language::English).get(key))
        .map_or(key, String::as_str)
}

/// 現在の言語でキーの文言を返す。
pub fn tr(key: &'static str) -> &'static str {
    tr_in(current(), key)
}

/// `{name}` 形式のプレースホルダーを値に置き換える。
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut result = template.to_owned();
    for (name, value) in values {
        result = result.replace(&format!("{{{name}}}"), value);
    }
    result
}

/// 現在の言語でキーの文言を返し、プレースホルダーを置き換える。
pub fn tr_with(key: &'static str, values: &[(&str, &str)]) -> String {
    fill(tr(key), values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

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
        for value in ["en_US.UTF-8", "en", "en-GB", "it_IT", "ru"] {
            assert_eq!(parse(value), Some(Language::English), "{value}");
        }
        for value in ["", "  ", "C", "C.UTF-8", "POSIX"] {
            assert_eq!(parse(value), None, "{value:?}");
        }
    }

    #[test]
    fn matches_chinese_scripts_and_regions() {
        for value in [
            "zh",
            "zh-CN",
            "zh_CN.UTF-8",
            "zh-Hans",
            "zh-Hans-CN",
            "zh-SG",
            "zh-Hans-SG",
            "zh-Hans-HK",
            "Chinese_China.936",
        ] {
            assert_eq!(parse(value), Some(Language::ChineseSimplified), "{value}");
        }
        for value in [
            "zh-TW",
            "zh_TW.UTF-8",
            "zh-Hant",
            "zh-Hant-TW",
            "zh-HK",
            "zh-MO",
            "zh-Hant-CN",
            "Chinese (Traditional)_Taiwan.950",
        ] {
            assert_eq!(parse(value), Some(Language::ChineseTraditional), "{value}");
        }
    }

    #[test]
    fn matches_other_primary_languages() {
        for (value, expected) in [
            ("ko", Language::Korean),
            ("ko_KR.UTF-8", Language::Korean),
            ("Korean_Korea.949", Language::Korean),
            ("es", Language::Spanish),
            ("es-MX", Language::Spanish),
            ("es_ES@euro", Language::Spanish),
            ("fr_FR@euro", Language::French),
            ("fr-CA", Language::French),
            ("de_DE", Language::German),
            ("de-AT", Language::German),
            ("pt", Language::PortugueseBrazil),
            ("pt-BR", Language::PortugueseBrazil),
            ("pt_PT.UTF-8", Language::PortugueseBrazil),
            ("Portuguese_Brazil.1252", Language::PortugueseBrazil),
        ] {
            assert_eq!(parse(value), Some(expected), "{value}");
        }
    }

    #[test]
    fn maps_windows_language_ids() {
        for (id, expected) in [
            (0x0411, Language::Japanese),
            (0x0409, Language::English),
            (0x0804, Language::ChineseSimplified),
            (0x1004, Language::ChineseSimplified),
            (0x0004, Language::ChineseSimplified),
            (0x0404, Language::ChineseTraditional),
            (0x0c04, Language::ChineseTraditional),
            (0x1404, Language::ChineseTraditional),
            (0x7c04, Language::ChineseTraditional),
            (0x0412, Language::Korean),
            (0x0c0a, Language::Spanish),
            (0x040c, Language::French),
            (0x0407, Language::German),
            (0x0416, Language::PortugueseBrazil),
            (0x0816, Language::PortugueseBrazil),
            (0x0410, Language::English),
        ] {
            assert_eq!(language_from_langid(id), expected, "{id:#06x}");
        }
    }

    #[test]
    fn tags_are_unique_and_round_trip() {
        let tags: BTreeSet<_> = Language::ALL.iter().map(|l| l.tag()).collect();
        assert_eq!(tags.len(), Language::ALL.len());
        for language in Language::ALL {
            assert_eq!(parse(language.tag()), Some(language));
        }
    }

    #[test]
    fn missing_keys_fall_back_to_english_then_key() {
        assert_eq!(
            tr_in(Language::French, "no.such.key"),
            "no.such.key",
            "unknown keys return themselves"
        );
        assert_eq!(
            fill("a {x} b {x} {y}", &[("x", "1"), ("y", "2")]),
            "a 1 b 1 2"
        );
    }

    fn dictionary_of(language: Language) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_str(language.dictionary_source()).expect("dictionary is JSON") {
            serde_json::Value::Object(map) => map,
            _ => panic!("{} is not a JSON object", language.tag()),
        }
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                break;
            };
            let name = &rest[start + 1..start + len];
            if !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                found.insert(name.to_owned());
            }
            rest = &rest[start + len + 1..];
        }
        found
    }

    #[test]
    fn dictionaries_share_keys_and_placeholders() {
        let english = dictionary_of(Language::English);
        assert!(!english.is_empty());
        let english_keys: BTreeSet<_> = english.keys().cloned().collect();
        for language in Language::ALL {
            let dictionary = dictionary_of(language);
            let keys: BTreeSet<_> = dictionary.keys().cloned().collect();
            assert_eq!(keys, english_keys, "key set differs in {}", language.tag());
            for (key, value) in &dictionary {
                let text = value.as_str().unwrap_or_else(|| {
                    panic!("{}: {key} is not a string", language.tag());
                });
                assert!(
                    !text.trim().is_empty(),
                    "{}: {key} is empty",
                    language.tag()
                );
                assert_eq!(
                    placeholders(text),
                    placeholders(english[key].as_str().unwrap()),
                    "placeholders differ in {}: {key}",
                    language.tag()
                );
            }
        }
    }

    fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }

    /// コードが `tr("…")` / `tr_with("…", …)` で参照するキーを、ソースから集める。
    fn referenced_keys() -> BTreeSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        let pattern = regex::Regex::new(r#"\btr(?:_with)?\(\s*"([A-Za-z0-9_.]+)""#).unwrap();
        let mut keys = BTreeSet::new();
        for file in files {
            // この辞書の実装・テスト自身（上のテキスト）は走査しない。
            if file.file_name().is_some_and(|name| name == "locale.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&file).unwrap();
            for capture in pattern.captures_iter(&source) {
                keys.insert(capture[1].to_owned());
            }
        }
        keys
    }

    #[test]
    fn code_references_only_known_keys_and_every_key_is_used() {
        let english = dictionary_of(Language::English);
        let referenced = referenced_keys();
        assert!(!referenced.is_empty(), "no key references were found");
        for key in &referenced {
            assert!(english.contains_key(key), "unknown dictionary key: {key}");
        }
        for key in english.keys() {
            assert!(referenced.contains(key), "unused dictionary key: {key}");
        }
    }
}
