use crate::common::{Config, api, read_settings, text, truncate_utf16};
use crate::locale::pick;
use anyhow::{Context, Result, bail};
use chrono::TimeZone;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

fn re(pattern: &'static str) -> Regex {
    // 同じ文書群の採点で正規表現を再コンパイルしない。
    static PATTERNS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, Regex>>,
    > = std::sync::OnceLock::new();
    PATTERNS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(pattern)
        .or_insert_with(|| {
            Regex::new(&pattern.replace(r"\b", r"(?-u:\b)")).expect("constant regular expression")
        })
        .clone()
}
fn compact(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn choose<'a>(values: &[&'a Value]) -> &'a str {
    values
        .iter()
        .map(|v| text(v))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| unique(a.iter().filter_map(Value::as_str).map(str::to_owned)))
        .unwrap_or_default()
}
fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}
fn date(s: &str) -> Option<&str> {
    // 索引の公開日時には旧JSと同じT形式のshape guardを維持する。
    re(r"^\d{4}-\d{2}-\d{2}T")
        .is_match(s)
        .then(|| date_millis(s))
        .flatten()
        .map(|_| s)
}
// 過去のISO/SQLite形式を保持し、比較にだけミリ秒値を使う。
// Date.parseと同様、日付だけはUTC、zoneなし時刻は実行端末のlocalとする。
fn date_millis(s: &str) -> Option<i64> {
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(date.timestamp_millis());
    }
    // ISOの分精度+offsetも既存APIで有効。offsetを保ったまま秒だけ補う。
    if let Some(parts) = re(r"^(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2})(Z|[+-]\d{2}:\d{2})$").captures(s)
    {
        return chrono::DateTime::parse_from_rfc3339(&format!("{}:00{}", &parts[1], &parts[2]))
            .ok()
            .map(|date| date.timestamp_millis());
    }
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(s) {
        return Some(date.timestamp_millis());
    }
    if re(r"^\d{4}-\d{2}-\d{2}$").is_match(s) {
        return chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp_millis());
    }
    if !re(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d{1,9})?)?$").is_match(s) {
        return None;
    }
    let naive = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(s, format).ok())?;
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(date) => Some(date.timestamp_millis()),
        // 秋の重複時刻はDate.parse同様に早い方のinstantを選ぶ。
        chrono::LocalResult::Ambiguous(a, b) => {
            Some(a.timestamp_millis().min(b.timestamp_millis()))
        }
        chrono::LocalResult::None => {
            // 春の欠落時刻は遷移前offsetで解釈し、gapの分だけ先へ進める。
            for minutes in (30..=1440).step_by(30) {
                let before = naive.checked_sub_signed(chrono::Duration::minutes(minutes))?;
                if let Some(date) = chrono::Local.from_local_datetime(&before).earliest() {
                    return date
                        .offset()
                        .from_local_datetime(&naive)
                        .single()
                        .map(|d| d.timestamp_millis());
                }
            }
            None
        }
    }
}
pub fn short_text(s: &str, limit: usize) -> String {
    let s = re(r"<[^>]*>").replace_all(&compact(s), "").to_string();
    if s.encode_utf16().count() <= limit {
        s
    } else {
        format!(
            "{}…",
            truncate_utf16(&s, limit.saturating_sub(1)).trim_end()
        )
    }
}
fn prompt(s: &str, limit: usize) -> String {
    short_text(
        &short_text(s, limit)
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
        limit,
    )
}
fn plain_line(s: &str) -> String {
    let s = re(r"^\s*(?:#{1,6}\s+|[-*+]\s+|\d+[.)]\s+|>\s*)").replace(s, "");
    let s = re(r"\[([^\]]+)\]\([^)]+\)").replace_all(&s, "$1");
    re(r"(?i)^(?:desc|description|task|Rollout context):\s*")
        .replace(&s.replace(['*', '`'], ""), "")
        .trim()
        .to_owned()
}
fn meaningful(s: &str) -> impl Iterator<Item = String> + '_ {
    let mut fenced = false;
    s.lines().filter_map(move |line|{if line.trim_start().starts_with("```"){fenced=!fenced;return None} if fenced||re(r"^\s*#{1,6}\s").is_match(line)||re(r"^\s*\[[^\]\n]*\]\((?:thread|codex)://[^)]+\)\s*$").is_match(line){return None} let l=plain_line(line);if l.is_empty()||re(r"(?i)^(?:(?:Session|Turn|Part|Source(?: SHA-256)?|Imported snapshot|updated_at|rollout_path|cwd|thread_id|git_branch|task_group|task_outcome|keywords)\s*:|rollout_summaries[\\/]|User request|User comment:|Referenced assistant response:|Final assistant response|Codex saved memory import|Historical memory evidence|v\d+$|<|:codex-|::)").is_match(&l){None}else{Some(l)}})
}
fn first(s: &str) -> String {
    meaningful(s).find(|l|!re(r"^(?:ゆーくん[、,。!！\s]*|ありがとう[。!！\s]*|了解(?:です)?[。!！\s]*|承知しました[。!！\s]*)$").is_match(l)).unwrap_or_default()
}
pub fn index_request_text(s: &str) -> String {
    let projected = crate::capture::project_request_text(s, false);
    let ambient=re(r#"(?is)<in-app-browser-context\b[^>]*\bsource\s*=\s*["']ambient-ui-state["'][^>]*>.*?</in-app-browser-context\s*>"#).replace_all(&projected,"\n");
    re(r"(?is)<send_user_message_question_reply\s*>(.*?)</send_user_message_question_reply\s*>")
        .replace_all(&ambient, |c: &regex::Captures| {
            let Ok(v) = serde_json::from_str::<Value>(&c[1]) else {
                return c[0].to_owned();
            };
            let Some(a) = v.as_array() else {
                return c[0].to_owned();
            };
            if a.is_empty()
                || !a
                    .iter()
                    .all(|v| v["answer"].is_string() && !text(&v["question"]).trim().is_empty())
            {
                return c[0].to_owned();
            }
            a.iter()
                .map(|v| {
                    format!(
                        "回答: {}（質問: {}）",
                        compact(text(&v["answer"])),
                        compact(text(&v["question"]))
                    )
                })
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .to_string()
}
pub fn build_memory_index(
    title: &str,
    request: &str,
    response: &str,
    source_timestamp: &str,
    source_kind: &str,
) -> Value {
    let title = short_text(&crate::capture::project_request_text(title, false), 100);
    let projected = index_request_text(request);
    let request_line = first(
        projected
            .rsplit("### Current user request\n")
            .next()
            .unwrap_or(""),
    );
    let sections: Vec<_> = unique(
        response
            .lines()
            .filter(|s| re(r"^#{1,4}\s+").is_match(s))
            .map(plain_line),
    )
    .into_iter()
    .take(4)
    .map(|s| short_text(&s, 60))
    .collect();
    let brief=re(r"^(?:はい|いいえ|了解(?:です|しました)?|承知しました|ありがとう(?:ございます)?|お疲れ(?:様|さま)(?:です|でした)?|[Oo][Kk]|続けて|お願いします|よろしく(?:お願いします)?)[。！!\s]*$").is_match(&request_line);
    let invocation = request_line.is_empty()
        && re(r"(?i)^[$/][a-z][a-z0-9_:-]*$")
            .is_match(crate::capture::project_request_text(request, true).trim());
    let detailed = (brief || invocation)
        && (!sections.is_empty()
            || meaningful(response)
                .collect::<Vec<_>>()
                .join(" ")
                .encode_utf16()
                .count()
                >= 160);
    let fallback = if request_line.is_empty() {
        first(response)
    } else {
        String::new()
    };
    let mut v = json!({"version":1,"title":if !title.is_empty()&&!re(r"(?i)^(?:s|Codex task|Memory [a-f0-9]+)$").is_match(&title){title.clone()}else{short_text(if !request_line.is_empty(){&request_line}else if !fallback.is_empty(){&fallback}else{"保存された記録"},100)},"description":short_text(&if brief&&detailed{format!("{}の継続結果・詳細",if title.is_empty(){"作業"}else{&title})}else if !request_line.is_empty(){request_line.clone()}else{fallback},180),"sections":sections,"recallable":(!request_line.is_empty()||invocation&&detailed)&&(!brief||detailed),"sourceKind":source_kind});
    if let Some(d) = date(source_timestamp) {
        v["sourceUpdatedAt"] = json!(d)
    }
    v
}
fn matching_section(s: &str, terms: &[String]) -> String {
    let r = re(r"(?m)^(#{1,4})\s+(.+)$");
    let h: Vec<_> = r.captures_iter(s).collect();
    let mut best = 0;
    let mut selected = None;
    for (i, c) in h.iter().enumerate() {
        if c[2].starts_with("Codex saved memory import:") {
            continue;
        }
        let score = matching_topic_terms(&c[2], terms).len();
        if score > best {
            best = score;
            selected = Some(i)
        }
    }
    if let Some(i) = selected {
        let start = h[i].get(0).unwrap().start();
        let end = h
            .iter()
            .skip(i + 1)
            .find(|c| c[1].len() <= h[i][1].len())
            .map(|c| c.get(0).unwrap().start())
            .unwrap_or(s.len());
        s[start..end].to_owned()
    } else {
        s.to_owned()
    }
}
fn section(s: &str, heading: &str, stop: Option<&str>) -> String {
    let marker = format!("### {heading}");
    let Some(start) = s.find(&marker) else {
        return String::new();
    };
    let body = s[start + marker.len()..].trim_start_matches([' ', '\r', '\n']);
    let end = stop.and_then(|v| body.find(v)).unwrap_or(body.len());
    body[..end].to_owned()
}
pub fn document_index(doc: &Value, fallback_container: &str, terms: &[String]) -> Value {
    let m = &doc["metadata"];
    let saved = &m["memoryIndex"];
    let has_saved = saved["version"] == 1;
    let original = choose(&[&doc["content"], &doc["memory"], &doc["summary"]]);
    let content = matching_section(original, terms);
    let request = section(
        &content,
        "User request",
        Some("\n### Final assistant response"),
    );
    let response = section(&content, "Final assistant response", None);
    let heading = re(r"(?m)^#{1,4}\s+(.+)$")
        .captures_iter(&content)
        .map(|c| c[1].to_owned())
        .collect::<Vec<_>>();
    let heading = heading
        .iter()
        .find(|s| !s.starts_with("Codex saved memory import:"))
        .or(heading.first())
        .cloned()
        .unwrap_or_default();
    let mut index = if has_saved {
        let mut description = text(&saved["description"]).to_owned();
        let degraded = re(r#"(?i)^<(?:skill|name|path|external_codex_apps_open_page|send_user_message_question_reply)\b|^This block is automatically supplied ambient UI state|<in-app-browser-context\b[^>]*source\s*=\s*["']ambient-ui-state["']"#).is_match(&compact(&description))
            || re(r"^\[\s*\{").is_match(&compact(&description))
                && re(r#""questionItemId"\s*:"#).is_match(&description)
                && description.contains("request_user_input_async");
        if degraded {
            description = index_request_text(&request).trim().to_owned();
            if description.is_empty() {
                description = text(&saved["title"]).to_owned();
            }
        }
        build_memory_index(
            text(&saved["title"]),
            &description,
            "",
            text(&saved["sourceUpdatedAt"]),
            if saved["sourceKind"].is_null() {
                "conversation"
            } else {
                text(&saved["sourceKind"])
            },
        )
    } else {
        let dates = unique(
            re(r"(?m)^updated_at:\s*(\S+)\s*$")
                .captures_iter(original)
                .filter_map(|c| date(&c[1]).map(str::to_owned)),
        );
        let title = if !text(&m["title"]).is_empty() {
            text(&m["title"])
        } else if !text(&doc["title"]).is_empty()
            && !re(r"(?i)^Memory [a-f0-9]+$").is_match(text(&doc["title"]))
        {
            text(&doc["title"])
        } else {
            heading
                .strip_prefix("Codex saved memory import:")
                .unwrap_or(&heading)
                .trim()
        };
        let req = if request.is_empty() {
            re(r"(?m)^description:\s*(.+)$")
                .captures(&content)
                .map(|c| c[1].to_owned())
                .unwrap_or_else(|| first(&content))
        } else {
            request.clone()
        };
        let mut v = build_memory_index(
            title,
            &req,
            &response,
            choose(&[
                &m["sourceTimestamp"],
                &m["sourceUpdatedAt"],
                &m["completedAt"],
                &json!(if dates.len() == 1 {
                    dates[0].as_str()
                } else {
                    ""
                }),
            ]),
            if m["type"] == "conversation" {
                "conversation"
            } else {
                "historical-document"
            },
        );
        if request.is_empty() && text(&m["title"]).is_empty() && heading.is_empty() {
            v["recallable"] = json!(false)
        }
        v
    };
    if has_saved && saved["sections"].is_array() {
        index["sections"] = json!(
            saved["sections"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .take(4)
                .map(|s| short_text(s, 60))
                .collect::<Vec<_>>()
        )
    }
    if saved["recallable"] == false {
        index["recallable"] = json!(false)
    } else if saved["recallable"] == true
        && ((has_saved && saved["sourceKind"] == "organization")
            || text(&index["description"]).is_empty())
    {
        index["recallable"] = json!(true)
    }
    let container = choose(&[
        &doc["containerTag"],
        &doc["containerTags"][0],
        &json!(fallback_container),
    ])
    .to_owned();
    index["id"] = json!(text(&doc["id"]));
    index["containerTag"] = json!(container);
    index["topics"] = json!(if doc["topics"].is_array() {
        strings(&doc["topics"])
    } else if doc["topic"].is_string() {
        strings(&json!([doc["topic"]]))
    } else {
        strings(&m["topics"])
    });
    let p = &doc["provenance"];
    let mut provenance = json!({});
    for (key, value) in [
        ("project", text(&m["project"])),
        ("projectId", choose(&[&p["projectId"], &m["sm_project_id"]])),
        (
            "containerTag",
            choose(&[&p["containerTag"], &index["containerTag"]]),
        ),
        ("filepath", choose(&[&p["filepath"], &m["filepath"]])),
    ] {
        if !value.trim().is_empty() {
            provenance[key] = json!(value.trim())
        }
    }
    if provenance.as_object().is_some_and(|o| !o.is_empty()) {
        index["provenance"] = provenance
    }
    for key in ["createdAt", "updatedAt"] {
        if let Some(d) = date(text(&doc[key])) {
            index[key] = json!(d)
        }
    }
    if index["sourceUpdatedAt"].is_null() {
        for key in ["sourceTimestamp", "sourceUpdatedAt", "completedAt"] {
            if let Some(d) = date(text(&m[key])) {
                index["sourceUpdatedAt"] = json!(d);
                break;
            }
        }
    }
    if m["sessionId"].is_string() {
        index["sessionId"] = m["sessionId"].clone()
    }
    for key in ["turn", "part", "parts"] {
        if m[key].is_i64() || m[key].is_u64() {
            index[key] = m[key].clone()
        }
    }
    index["consolidation"] = json!(m["sm_consolidation"] == true);
    index["sourceMemoryIds"] = json!(if m["sm_consolidation"] == true {
        strings(&m["sourceMemoryIds"])
    } else {
        vec![]
    });
    if let Some(d) = date(text(&m["consolidationCreatedAt"])) {
        index["consolidationCreatedAt"] = json!(d)
    }
    if !text(&doc["searchExcerpt"]).trim().is_empty() {
        index["searchExcerpt"] = json!(short_text(text(&doc["searchExcerpt"]), 240))
    }
    index["evidence"] = json!("historical record; inspect the document before relying on it");
    index
}
pub fn public_index(item: &Value) -> Value {
    let mut v = json!({});
    for k in [
        "id",
        "title",
        "description",
        "containerTag",
        "topics",
        "sections",
        "recallable",
        "sourceUpdatedAt",
        "updatedAt",
        "createdAt",
    ] {
        if !item[k].is_null() {
            v[k] = item[k].clone()
        }
    }
    if item["part"].as_u64().is_some() && item["parts"].as_u64().unwrap_or(0) > 1 {
        v["part"] = item["part"].clone();
        v["parts"] = item["parts"].clone()
    }
    v
}
// 索引項目の見出し語は、索引を書き込む環境の言語（crate::locale）に合わせる。
// 記録の本文・題名・話題は保存時の言語のまま出し、翻訳しない。
pub fn format_index_item(v: &Value) -> String {
    let title = prompt(text(&v["title"]), 100);
    let mut result = format!("- ◪ {title}");
    if !text(&v["description"]).is_empty() && v["description"] != v["title"] {
        result += &format!(
            " — {}: {}",
            pick("参照する場面", "Use when"),
            prompt(text(&v["description"]), 180)
        )
    }
    let sections = strings(&v["sections"]);
    if !sections.is_empty() {
        let joined = sections
            .iter()
            .take(2)
            .map(|s| prompt(s, 60))
            .collect::<Vec<_>>();
        result += &match crate::locale::current() {
            crate::locale::Language::Japanese => format!("（収録: {}）", joined.join("、")),
            crate::locale::Language::English => format!(" (covers: {})", joined.join("; ")),
        }
    }
    result += &format!("\n  id={}", prompt(text(&v["id"]), 80));
    let topics = strings(&v["topics"]);
    if !topics.is_empty() {
        result += &format!(
            " | topics={}",
            topics
                .iter()
                .take(3)
                .map(|s| prompt(s, 60))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
    result += &format!(
        " | container={}",
        prompt(
            if text(&v["containerTag"]).is_empty() {
                "unknown"
            } else {
                text(&v["containerTag"])
            },
            160
        )
    );
    let p = &v["provenance"];
    let project = choose(&[&p["project"], &p["projectId"], &p["containerTag"]]);
    if !project.is_empty() {
        result += &format!(" | provenance={}", prompt(project, 160));
        if !text(&p["project"]).is_empty() && !text(&p["projectId"]).is_empty() {
            result += &format!(" ({})", prompt(text(&p["projectId"]), 160))
        }
    }
    result += &format!(
        " | {}={}",
        if v["sourceUpdatedAt"].is_string() {
            "sourceDate"
        } else {
            "storedDate"
        },
        prompt(
            choose(&[
                &v["sourceUpdatedAt"],
                &v["updatedAt"],
                &v["createdAt"],
                &json!(pick("日時不明", "unknown date"))
            ]),
            40
        )
    );
    if v["parts"].as_u64().unwrap_or(0) > 1 {
        result += &format!(" | part={}/{}", v["part"], v["parts"])
    }
    if v["consolidation"] == true {
        result += " | kind=consolidated-checkpoint"
    }
    if !text(&v["searchExcerpt"]).is_empty() {
        result += &format!(
            "\n  {}: {}",
            // 英語でも「原文（original）」と分かる語を残す。抜粋が保存された原文由来であることを
            // 利用側が見分けられるよう、client 契約もこの語を確認している。
            pick("原文の該当箇所", "Matching original excerpt"),
            prompt(text(&v["searchExcerpt"]), 240)
        )
    }
    result
}
pub fn format_recent_task(v: &Value) -> String {
    let description = if !text(&v["description"]).is_empty() && v["description"] != v["title"] {
        format!(" — {}", prompt(text(&v["description"]), 100))
    } else {
        String::new()
    };
    format!(
        "- ◷ {}{}\n  id={} | project={} | {}={}",
        prompt(text(&v["title"]), 80),
        description,
        prompt(text(&v["id"]), 64),
        prompt(
            choose(&[
                &v["provenance"]["project"],
                &v["provenance"]["projectId"],
                &v["containerTag"]
            ]),
            64
        ),
        if v["sourceUpdatedAt"].is_string() {
            "sourceDate"
        } else {
            "storedDate"
        },
        prompt(
            choose(&[
                &v["sourceUpdatedAt"],
                &v["createdAt"],
                &v["updatedAt"],
                &json!(pick("日時不明", "unknown date"))
            ]),
            40
        )
    )
}

const STOP: &str = "memory memories codex 情報 こと もの これ それ この その あの どの ここ そこ this that ため よう です ます ください する した して れる ある いる 確認 対応 作業 実装 調査";
const AUTO_STOP: &str = "修正 改善 最適化 機能 部分 発見 でき 出来 てい いた たら そう お願い お願いします please fix improve optimize optimization implement implementation investigate";
const AUTO_ACTION: &str = "ので なので から ない 時に とき って うん けど だけ まだ もう また 同じ 今回 前回 こちら それで 続き 続け 再度 問題 チェック 試行 再試行 発生 検索 調べ 大丈夫 再開 実行 結果 原因 みたい おかしい 思う";
const AUTO_CONTEXT: &str = "タイムアウト timeout 入力 エラー error 動作 現象 状態 必要 とき 場合 発生 問題 チェック 再試行 過去 最新 履歴 記憶 全部 全体 内容 情報 日間 ヶ月 か月 以前 以降";
fn has(words: &str, s: &str) -> bool {
    words.split_whitespace().any(|v| v == s)
}
fn stopped(s: &str, automatic: bool) -> bool {
    has(STOP, s) || automatic && (has(AUTO_STOP, s) || has(AUTO_ACTION, s))
}
fn automatic_stop_ranges(query: &str) -> Vec<(usize, usize)> {
    // 分かち書きが「してください」を「てく」「さい」へ分割しても、
    // 既存の隣接する除外語の範囲内にある断片を話題として残さない。
    let mut ranges: Vec<_> = [STOP, AUTO_STOP, AUTO_ACTION]
        .into_iter()
        .flat_map(str::split_whitespace)
        .filter(|s| s.chars().all(|c| !c.is_ascii()))
        .flat_map(|s| {
            query
                .match_indices(s)
                .map(move |(start, _)| (start, start + s.len()))
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = vec![];
    for (start, end) in ranges {
        if let Some(previous) = merged
            .last_mut()
            .filter(|(_, previous_end)| start <= *previous_end)
        {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}
fn path_topic(s: &str) -> String {
    let normalized = s.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let normalized_no_drive = re(r"(?i)^[a-z]:/").replace(normalized, "");
    let mut parts: Vec<_> = normalized_no_drive
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if normalized.starts_with("//")
        || parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("users") || p.eq_ignore_ascii_case("home"))
    {
        parts = parts.into_iter().skip(2).collect()
    } else if parts.first() == Some(&"~") {
        parts.remove(0);
    }
    if let Some(i) = parts
        .iter()
        .position(|p| re(r"(?i)^(?:dev|projects?|workspaces?|repos?|work)$").is_match(p))
    {
        parts.get(i + 1).copied().unwrap_or("").to_owned()
    } else {
        parts.last().copied().unwrap_or("").to_owned()
    }
}
pub fn automatic_search_text(query: &str) -> String {
    let normalized: String = query.nfkc().collect();
    let quoted = re(r#"(?i)["']([a-z]:[\\/][^\r\n]*?|\\\\[^"'\r\n]*?|/[^\r\n]*?)["']"#)
        .replace_all(&normalized, |c: &regex::Captures| path_topic(&c[1]));
    static PATH_PATTERN: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATH_PATTERN.get_or_init(|| fancy_regex::Regex::new(
        r#"(?i)(?<![A-Za-z0-9_])[a-z]:[\\/][^\s"'<>|]+|\\\\[^\s"'<>|]+|(?<![:A-Za-z0-9_/])(?:~)?/[^\s"'<>|]+"#,
    )
    .unwrap());
    pattern
        .replace_all(&quoted, |c: &fancy_regex::Captures<str>| path_topic(&c[0]))
        .to_string()
}
fn words(s: &str) -> Vec<(String, usize, usize)> {
    let segmenter = icu_segmenter::WordSegmenter::new_auto(Default::default());
    let boundaries: Vec<_> = segmenter.segment_str(s).collect();
    boundaries
        .windows(2)
        .filter_map(|pair| {
            let word = &s[pair[0]..pair[1]];
            if word.chars().any(char::is_alphanumeric) {
                Some((word.to_lowercase(), pair[0], pair[1]))
            } else {
                None
            }
        })
        .collect()
}
pub fn query_terms(query: &str, automatic: bool) -> Vec<String> {
    let query = if automatic {
        automatic_search_text(query)
    } else {
        query.to_owned()
    };
    let identifiers: Vec<_> = if automatic {
        re(r"(?i)\b[a-z\d]+(?:[._-][a-z\d]+)+\b")
            .find_iter(&query)
            .map(|m| (m.as_str().to_lowercase(), m.start(), m.end()))
            .collect()
    } else {
        vec![]
    };
    let stop_ranges = if automatic {
        automatic_stop_ranges(&query)
    } else {
        vec![]
    };
    let useful: Vec<_> = words(&query)
        .into_iter()
        .filter(|(w, start, end)| {
            w.encode_utf16().count() >= 2
                && !stopped(w, automatic)
                && !stop_ranges.iter().any(|(a, b)| start >= a && end <= b)
                && !identifiers.iter().any(|(_, a, b)| start >= a && start < b)
        })
        .collect();
    let japanese = re(r"^[\p{Han}\p{Hiragana}\p{Katakana}ー]+$");
    let mut compounded = HashSet::new();
    let mut compounds = vec![];
    for pair in useful.windows(2) {
        let (a, _, end) = &pair[0];
        let (b, start, _) = &pair[1];
        if end == start && japanese.is_match(a) && japanese.is_match(b) {
            compounds.push(format!("{a}{b}"));
            compounded.insert(a.clone());
            compounded.insert(b.clone());
        }
    }
    unique(
        identifiers
            .into_iter()
            .map(|(s, _, _)| s)
            .chain(
                useful
                    .into_iter()
                    .filter(|(s, _, _)| !compounded.contains(s))
                    .map(|(s, _, _)| s),
            )
            .chain(compounds),
    )
    .into_iter()
    .filter(|s| !stopped(s, automatic))
    .take(30)
    .collect()
}
pub fn recall_topic_terms(query: &str) -> Vec<String> {
    let terms = query_terms(query, true);
    if query.trim().eq_ignore_ascii_case("timeout") || query.trim() == "タイムアウト" {
        return terms;
    }
    terms
        .into_iter()
        .filter(|term| {
            !has(AUTO_CONTEXT, term)
                && words(term).iter().any(|(s, _, _)| {
                    s.encode_utf16().count() >= 2
                        && !s.chars().all(|c| c.is_ascii_digit())
                        && !has(AUTO_CONTEXT, s)
                        && !stopped(s, true)
                })
        })
        .collect()
}
fn normalized(s: &str) -> String {
    re(r"[._-]+")
        .replace_all(&s.nfkc().collect::<String>().to_lowercase(), " ")
        .to_string()
}
struct PreparedTerm<'a> {
    original: &'a str,
    normalized: String,
    token_like: bool,
    ends_ascii_digit: bool,
    weight: usize,
}
fn prepare_terms(terms: &[String]) -> (Vec<PreparedTerm<'_>>, usize) {
    let prepared: Vec<_> = terms
        .iter()
        .map(|term| {
            let normalized = normalized(term);
            PreparedTerm {
                original: term,
                token_like: re(r"^[a-z\d ]+$").is_match(&normalized),
                ends_ascii_digit: normalized.ends_with(|c: char| c.is_ascii_digit()),
                normalized,
                weight: term.encode_utf16().count().min(12),
            }
        })
        .collect();
    let total = prepared.iter().map(|term| term.weight).sum();
    (prepared, total)
}
fn matching_prepared_terms<'a, 't>(
    s: &str,
    terms: &'a [PreparedTerm<'t>],
) -> Vec<&'a PreparedTerm<'t>> {
    let s = normalized(s);
    terms
        .iter()
        .filter(|term| {
            let t = &term.normalized;
            if !term.token_like {
                return s.contains(t);
            }
            s.match_indices(t).any(|(pos, _)| {
                let before = s[..pos].chars().next_back();
                let after = s[pos + t.len()..].chars().next();
                !before.is_some_and(|c| c.is_ascii_alphanumeric())
                    && !after.is_some_and(|c| {
                        if term.ends_ascii_digit {
                            c.is_ascii_alphanumeric()
                        } else {
                            c.is_ascii_alphabetic()
                        }
                    })
            })
        })
        .collect()
}
pub fn matching_topic_terms(s: &str, terms: &[String]) -> Vec<String> {
    let (prepared, _) = prepare_terms(terms);
    matching_prepared_terms(s, &prepared)
        .into_iter()
        .map(|term| term.original.to_owned())
        .collect()
}

fn timeout(options: &Value) -> Result<u64> {
    let mut timeout = options["timeoutMs"].as_u64().unwrap_or(8000);
    if let Some(deadline) = options["deadlineEpochMs"].as_u64() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let remaining = deadline.saturating_sub(now);
        if remaining < 250 {
            bail!("Memory request deadline exceeded")
        }
        timeout = timeout.min(remaining).min(4000)
    }
    Ok(timeout)
}
fn request(config: &Config, path: &str, body: Option<&Value>, options: &Value) -> Result<Value> {
    api(
        config,
        path,
        if body.is_some() { "POST" } else { "GET" },
        body,
        timeout(options)?,
    )
}
pub fn discover_search_containers(config: &Config) -> Result<Value> {
    discover(config, &json!({}))
}
fn discover(config: &Config, options: &Value) -> Result<Value> {
    let response = request(config, "/v3/container-tags", None, options)
        .context("Remnifold space discovery failed; global topic search was not performed")?;
    let spaces=response["spaces"].as_array().context("Remnifold space discovery returned an invalid response; global topic search was not performed")?;
    let tags = unique(
        spaces
            .iter()
            .filter(|s| {
                s["containerTag"].is_string()
                    && (s["memoryCount"].is_null()
                        || s["memoryCount"]
                            .as_f64()
                            .or_else(|| text(&s["memoryCount"]).parse().ok())
                            .unwrap_or(0.)
                            > 0.)
            })
            .map(|s| text(&s["containerTag"]).to_owned()),
    );
    Ok(json!({"tags":tags,"complete":spaces.len()<100,"returnedCount":spaces.len()}))
}
#[derive(Clone)]
struct Candidate {
    row: Value,
    index: Value,
    rank: usize,
    relevance: f64,
    index_relevance: f64,
    matches: Vec<String>,
    project_only: bool,
}
fn score(candidate: &mut Candidate, terms: &[PreparedTerm<'_>], total: usize, automatic: bool) {
    let index = &candidate.index;
    let mut project = String::new();
    if !automatic {
        project = text(&index["provenance"]["project"]).to_owned();
        if project.is_empty() {
            if let Some(c) = re(r"(?i)^repo_(.+)__[a-f0-9]{16}$")
                .captures(text(&index["provenance"]["projectId"]))
            {
                project = c[1].to_owned()
            }
        }
    }
    let base = format!(
        "{} {} {} {} {}",
        text(&index["title"]),
        text(&index["description"]),
        strings(&index["sections"]).join(" "),
        strings(&index["topics"]).join(" "),
        project
    );
    let weight = |terms: &[&PreparedTerm<'_>]| {
        if terms.is_empty() {
            0.
        } else {
            terms.iter().map(|term| term.weight).sum::<usize>() as f64 / total as f64
        }
    };
    candidate.index_relevance = weight(&matching_prepared_terms(&base, terms));
    let matches =
        matching_prepared_terms(&format!("{base} {}", text(&index["searchExcerpt"])), terms);
    candidate.relevance = weight(&matches);
    candidate.matches = matches
        .into_iter()
        .map(|term| term.original.to_owned())
        .collect();
    candidate.project_only = !project.is_empty()
        && !terms.is_empty()
        && matching_prepared_terms(&project, terms).len() == terms.len()
}
fn semantic(c: &Candidate, minimum: f64) -> bool {
    c.row["semanticSimilarity"]
        .as_f64()
        .is_some_and(|v| v >= minimum)
}
fn lexical(c: &Candidate) -> bool {
    c.row["lexicalSimilarity"].is_number()
}
fn date_number(s: &str) -> i64 {
    date_millis(s).unwrap_or(i64::MIN)
}
fn compare_date(a: &Value, b: &Value, key: &str) -> std::cmp::Ordering {
    date_number(text(&a[key])).cmp(&date_number(text(&b[key])))
}
fn latest(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut latest = std::collections::HashMap::<String, usize>::new();
    let keys: Vec<_> = candidates
        .iter()
        .map(|c| {
            let i = &c.index;
            if i["consolidation"] == true
                || text(&i["sessionId"]).is_empty()
                || !i["turn"].is_number()
                || !i["part"].is_number()
            {
                None
            } else {
                Some(
                    json!([
                        choose(&[&i["provenance"]["projectId"], &i["containerTag"]]),
                        i["sessionId"],
                        i["turn"],
                        i["part"]
                    ])
                    .to_string(),
                )
            }
        })
        .collect();
    for (pos, key) in keys.iter().enumerate() {
        if let Some(key) = key {
            let update = latest
                .get(key)
                .map(|old| {
                    let a = &candidates[pos].index;
                    let b = &candidates[*old].index;
                    compare_date(a, b, "sourceUpdatedAt")
                        .then_with(|| compare_date(a, b, "createdAt"))
                        .then_with(|| compare_date(a, b, "updatedAt"))
                        .is_gt()
                })
                .unwrap_or(true);
            if update {
                latest.insert(key.clone(), pos);
            }
        }
    }
    candidates
        .into_iter()
        .enumerate()
        .filter(|(pos, _)| {
            keys[*pos]
                .as_ref()
                .map(|key| latest[key] == *pos)
                .unwrap_or(true)
        })
        .map(|(_, c)| c)
        .collect()
}
fn compare_project(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.project_only.cmp(&a.project_only).then_with(|| {
        if a.project_only && b.project_only {
            date_number(choose(&[
                &b.index["sourceUpdatedAt"],
                &b.index["updatedAt"],
            ]))
            .cmp(&date_number(choose(&[
                &a.index["sourceUpdatedAt"],
                &a.index["updatedAt"],
            ])))
        } else {
            std::cmp::Ordering::Equal
        }
    })
}
pub fn search_index(config: &Config, options: &Value) -> Result<Value> {
    let automatic = options["automatic"] == true;
    let mut query = truncate_utf16(text(&options["query"]).trim(), 1000);
    if automatic {
        query = automatic_search_text(&query)
    }
    let terms = if automatic {
        recall_topic_terms(&query)
    } else {
        query_terms(&query, false)
    };
    if automatic && terms.is_empty() {
        query.clear()
    }
    let container = text(&options["containerTag"]);
    let mut result = json!({"query":query,"containerTag":if container.is_empty(){Value::Null}else{json!(container)},"searchScope":"none","searchedContainers":[],"failedContainers":[],"failedDocuments":[],"spaceDiscoveryComplete":true,"results":[],"total":0});
    if query.is_empty() {
        return Ok(result);
    }
    let (prepared_terms, total_weight) = prepare_terms(&terms);
    let settings = if options["settings"].is_object() {
        options["settings"].clone()
    } else {
        read_settings(config)
    };
    let numeric_limit = |value: &Value| {
        value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.)
            .map(|v| v.floor() as usize)
    };
    let limit = numeric_limit(&options["limit"])
        .or_else(|| numeric_limit(&settings["maxMemories"]))
        .unwrap_or(3);
    let minimum = settings["minimumSimilarity"].as_f64().unwrap_or(0.7);
    let mut global = None;
    if container.is_empty() {
        let mut body = json!({"allContainers":true,"q":query,"limit":40});
        if automatic {
            body["indexOnly"] = json!(true)
        }
        match request(config, "/v4/search", Some(&body), options) {
            Ok(response) => {
                if response["searchScope"] == "all-containers" {
                    if !response["results"].is_array()
                        || !response["searchedContainers"].is_array()
                        || response["spaceDiscoveryComplete"] != true
                    {
                        bail!("Invalid global memory search response")
                    }
                    global = Some(response)
                }
            }
            Err(error) => {
                let unsupported =
                    error
                        .downcast_ref::<crate::common::ApiError>()
                        .is_some_and(|e| {
                            matches!(e.status, Some(404 | 501))
                                || e.status == Some(400) && e.unsupported_global_search
                        });
                if !unsupported {
                    return Err(error);
                }
            }
        }
    }
    let discovery = if let Some(g) = &global {
        json!({"tags":strings(&g["searchedContainers"]),"complete":true,"returnedCount":g["searchedContainers"].as_array().unwrap().len()})
    } else if !container.is_empty() {
        json!({"tags":[container],"complete":true,"returnedCount":1})
    } else {
        discover(config, options)?
    };
    let tags = strings(&discovery["tags"]);
    let mut failed = vec![];
    let mut candidates = vec![];
    let mut add = |response: &Value, tag: &str| -> Result<()> {
        let rows = response["results"]
            .as_array()
            .context("Invalid memory search response")?;
        for (rank, row) in rows.iter().enumerate() {
            candidates.push(Candidate {
                row: row.clone(),
                index: document_index(row, tag, &[]),
                rank,
                relevance: 0.,
                index_relevance: 0.,
                matches: vec![],
                project_only: false,
            })
        }
        Ok(())
    };
    if let Some(g) = &global {
        add(g, "")?
    } else {
        for chunk in tags.chunks(8) {
            let responses = std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|tag| {
                        let query = &query;
                        scope.spawn(move || {
                            let mut body = json!({"containerTag":tag,"q":query,"limit":20});
                            if automatic {
                                body["indexOnly"] = json!(true)
                            }
                            request(config, "/v4/search", Some(&body), options)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(anyhow::anyhow!("Memory search worker failed")))
                    })
                    .collect::<Vec<_>>()
            });
            for (tag, response) in chunk.iter().zip(responses) {
                if response.and_then(|r| add(&r, tag)).is_err() {
                    failed.push(tag.clone())
                }
            }
        }
        if !tags.is_empty() && failed.len() == tags.len() {
            bail!("Remnifold search unavailable for all requested spaces")
        }
    }
    for c in &mut candidates {
        score(c, &prepared_terms, total_weight, automatic)
    }
    candidates.retain(|c| {
        !text(&c.index["id"]).is_empty()
            && !text(&c.index["containerTag"]).is_empty()
            && (c.row["metadata"]["memoryIndex"]["version"] != 1
                || semantic(c, minimum)
                || lexical(c)
                || c.relevance > 0.)
    });
    if automatic {
        candidates = latest(candidates)
    }
    candidates.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| compare_project(a, b))
            .then_with(|| {
                b.row["semanticSimilarity"]
                    .as_f64()
                    .unwrap_or(-1.)
                    .total_cmp(&a.row["semanticSimilarity"].as_f64().unwrap_or(-1.))
            })
            .then(a.rank.cmp(&b.rank))
    });
    let mut seen = HashSet::new();
    candidates.retain(|c| {
        seen.insert(format!(
            "{}:{}",
            text(&c.index["containerTag"]),
            text(&c.index["id"])
        ))
    });
    candidates.truncate((limit * 8).clamp(20, 80));
    let mut failed_documents = vec![];
    for chunk in candidates.chunks_mut(8) {
        let responses = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|c| {
                    if c.row["metadata"]["memoryIndex"]["version"] == 1 {
                        return None;
                    }
                    Some(scope.spawn(move || {
                        let id = text(&c.index["id"]);
                        let path = format!("/v3/documents/{}", encode(id));
                        request(config, &path, None, options).and_then(|d| {
                            if text(&d["id"]) != id || !d["content"].is_string() {
                                bail!("Invalid memory document response")
                            }
                            Ok(d)
                        })
                    }))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.map(|h| {
                        h.join().unwrap_or_else(|_| {
                            Err(anyhow::anyhow!("Memory hydration worker failed"))
                        })
                    })
                })
                .collect::<Vec<_>>()
        });
        for (c, response) in chunk.iter_mut().zip(responses) {
            if let Some(response) = response {
                match response {
                    Ok(d) => c.index = document_index(&d, text(&c.index["containerTag"]), &terms),
                    Err(_) => {
                        failed_documents.push(text(&c.index["id"]).to_owned());
                        c.index["title"] = json!(if text(&c.row["metadata"]["title"]).is_empty() {
                            "保存された記録"
                        } else {
                            text(&c.row["metadata"]["title"])
                        });
                        c.index["description"] = json!("");
                        c.index["recallable"] = json!(false)
                    }
                }
                score(c, &prepared_terms, total_weight, automatic)
            }
        }
    }
    if automatic {
        candidates = latest(candidates)
    }
    candidates.retain(|c| {
        (semantic(c, minimum) || lexical(c) || c.relevance > 0.)
            && (!automatic || c.index["recallable"] == true && c.relevance > 0.)
    });
    candidates.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| compare_project(a, b))
            .then_with(|| b.index_relevance.total_cmp(&a.index_relevance))
            .then_with(|| {
                if automatic {
                    (b.index["consolidation"] == true).cmp(&(a.index["consolidation"] == true))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                b.row["semanticSimilarity"]
                    .as_f64()
                    .unwrap_or(-1.)
                    .total_cmp(&a.row["semanticSimilarity"].as_f64().unwrap_or(-1.))
            })
            .then(a.rank.cmp(&b.rank))
            .then_with(|| compare_date(&b.index, &a.index, "consolidationCreatedAt"))
            .then_with(|| {
                date_number(choose(&[
                    &b.index["sourceUpdatedAt"],
                    &b.index["updatedAt"],
                ]))
                .cmp(&date_number(choose(&[
                    &a.index["sourceUpdatedAt"],
                    &a.index["updatedAt"],
                ])))
            })
    });
    let mut selected: Vec<Candidate> = vec![];
    let mut seen = HashSet::new();
    for c in candidates {
        if !seen.insert(format!(
            "{}:{}",
            text(&c.index["containerTag"]),
            text(&c.index["id"])
        )) {
            continue;
        }
        if automatic
            && c.index["consolidation"] != true
            && selected.iter().any(|checkpoint| {
                checkpoint.index["consolidation"] == true
                    && checkpoint.index["sourceMemoryIds"]
                        .as_array()
                        .is_some_and(|ids| {
                            ids.iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .any(|id| !id.is_empty() && id == text(&c.index["id"]))
                        })
                    && c.matches
                        .iter()
                        .all(|term| checkpoint.matches.contains(term))
            })
        {
            continue;
        }
        selected.push(c);
        if selected.len() >= limit.clamp(1, 20) {
            break;
        }
    }
    result["searchScope"] = json!(if !container.is_empty() {
        "explicit-container"
    } else if global.is_some() {
        "all-containers"
    } else {
        "all-discovered-containers"
    });
    result["searchedContainers"] = json!(tags);
    result["failedContainers"] = json!(failed);
    result["failedDocuments"] = json!(failed_documents);
    result["spaceDiscoveryComplete"] = discovery["complete"].clone();
    result["discoveredContainerCount"] = discovery["returnedCount"].clone();
    result["total"] = json!(selected.len());
    result["results"] = json!(selected.into_iter().map(|c| c.index).collect::<Vec<_>>());
    Ok(result)
}
fn encode(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char)
        } else {
            encoded += &format!("%{byte:02X}")
        }
    }
    encoded
}
pub fn format_search_result(result: &Value) -> String {
    let rows = result["results"].as_array().cloned().unwrap_or_default();
    let mut output = if rows.is_empty() {
        pick(
            "一致する記憶の索引はありませんでした。",
            "No matching memory index entries found.",
        )
        .to_owned()
    } else {
        rows.iter()
            .map(format_index_item)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let spaces = result["failedContainers"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let documents = result["failedDocuments"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    if spaces + documents > 0 {
        output += &match crate::locale::current() {
            crate::locale::Language::Japanese => format!(
                "\n検索は不完全です: {spaces} 件の保存先と {documents} 件の文書索引を取得できませんでした。"
            ),
            crate::locale::Language::English => format!(
                "\nIncomplete search: {spaces} spaces and {documents} document indexes were unavailable."
            ),
        }
    }
    if result["spaceDiscoveryComplete"] == false {
        output += pick(
            "\n保存先の一覧は不完全です: API が上限の100件を返したため、ほかの保存先がある可能性があります。",
            "\nIncomplete space discovery: the API returned its 100-space limit, so additional spaces may exist.",
        )
    }
    output += "\nWhen an entry appears applicable, read the original with getDocument(documentId), validate it against the current implementation, and reuse the parts that still fit.";
    output
}
pub fn recent_task_index(config: &Config, options: &Value) -> Result<Value> {
    let limit = options["limit"].as_u64().unwrap_or(2) as usize;
    let session = text(&options["sessionId"]);
    let mut body = json!({"projection":"index","recentTasks":true,"page":1,"limit":limit});
    if !session.trim().is_empty() {
        body["excludeSessionId"] = json!(session.trim())
    }
    let response = request(config, "/v3/documents/list", Some(&body), options)?;
    if response["recentTasks"] != true {
        return Ok(json!([]));
    }
    let documents = response["documents"]
        .as_array()
        .context("Invalid recent task response")?;
    Ok(json!(
        documents
            .iter()
            .map(|d| document_index(d, "", &[]))
            .filter(|i| !text(&i["id"]).is_empty()
                && !text(&i["containerTag"]).is_empty()
                && i["recallable"] == true
                && i["consolidation"] != true
                && (session.is_empty() || text(&i["sessionId"]) != session))
            .take(limit)
            .collect::<Vec<_>>()
    ))
}
pub fn list_index(config: &Config, options: &Value) -> Result<Value> {
    let mut body = json!({"page":options["page"].as_u64().unwrap_or(1),"limit":options["limit"].as_u64().unwrap_or(10),"projection":"index"});
    for key in ["containerTag", "topic"] {
        if !text(&options[key]).is_empty() {
            body[key] = options[key].clone()
        }
    }
    let response = request(config, "/v3/documents/list", Some(&body), options)?;
    let documents = response["documents"]
        .as_array()
        .context("Invalid memory document-list response")?;
    let mut v = json!({"documents":documents.iter().map(|d|public_index(&document_index(d,text(&options["containerTag"]),&[]))).collect::<Vec<_>>(),"containerTag":body.get("containerTag"),"topic":body.get("topic"),"listScope":if body.get("containerTag").is_some(){"explicit-container"}else{"all-containers"}});
    if let Some(p) = response.get("pagination") {
        v["pagination"] = p.clone()
    }
    Ok(v)
}
pub fn list_topics(config: &Config, options: &Value) -> Result<Value> {
    let suffix = if options.get("page").is_some() || options.get("limit").is_some() {
        format!(
            "?page={}&limit={}",
            options["page"].as_u64().unwrap_or(1),
            options["limit"].as_u64().unwrap_or(100)
        )
    } else {
        String::new()
    };
    let response = request(config, &format!("/v4/topics{suffix}"), None, options)?;
    let topics = response["topics"]
        .as_array()
        .context("Invalid memory topic-list response")?;
    if response["unclassifiedCount"].as_u64().is_none()
        || topics
            .iter()
            .any(|t| text(&t["topic"]).is_empty() || t["documentCount"].as_u64().is_none())
    {
        bail!("Invalid memory topic-list response")
    }
    let mut v = json!({"topics":topics.iter().map(|t|json!({"topic":t["topic"],"documentCount":t["documentCount"]})).collect::<Vec<_>>(),"unclassifiedCount":response["unclassifiedCount"]});
    if let Some(p) = response.get("pagination") {
        v["pagination"] = p.clone()
    }
    Ok(v)
}
pub fn read_document(config: &Config, id: &str) -> Result<Value> {
    read_document_page(config, id, None)
}

pub fn read_document_page(config: &Config, id: &str, source_cursor: Option<&str>) -> Result<Value> {
    if source_cursor.is_some_and(|cursor| cursor.is_empty() || cursor.len() > 2048) {
        bail!("Invalid organization source cursor.");
    }
    let mut path = format!("/v3/documents/{}", encode(id));
    if let Some(cursor) = source_cursor {
        path += &format!("?sourceCursor={}", encode(cursor));
    }
    let document = request(config, &path, None, &json!({}))?;
    if source_cursor.is_some() {
        let page = document.get("organizationSources").ok_or_else(|| anyhow::anyhow!(
            "Organization source paging is unavailable; retrieve the summary again with a supported server."
        ))?;
        if document.get("content").is_some() {
            bail!(
                "Organization source paging is unavailable; retrieve the summary again with a supported server."
            );
        }
        let page = organization_source_page(page, id)?;
        return Ok(
            json!({"text":format!("Organization sources (use getDocument for each ID and truncated filepath; pass nextCursor as sourceCursor to continue):\n{}",serde_json::to_string(&page)?),
            "document":{"id":id,"organizationSources":page}}),
        );
    }
    let index = document_index(&document, "", &[]);
    let mut content = crate::capture::project_document_content(choose(&[
        &document["content"],
        &document["summary"],
    ]));
    let m = &document["metadata"];
    if m["captureVersion"] == 2
        && m["title"].is_string()
        && m["sessionId"].is_string()
        && (m["turn"].is_i64() || m["turn"].is_u64())
    {
        let title = format!("# {}", text(&m["title"]));
        let prefix = format!(
            "{title}\n\nSession: {}\nTurn: {}\n\n",
            text(&m["sessionId"]),
            m["turn"]
        );
        let has_parts = m.get("part").is_some() || m.get("parts").is_some();
        let prefix = if has_parts && m["part"].is_number() && m["parts"].is_number() {
            format!("{prefix}Part: {}/{}\n\n", m["part"], m["parts"])
        } else if has_parts {
            String::new()
        } else {
            prefix
        };
        if !prefix.is_empty() {
            if let Some(tail) = content.strip_prefix(&prefix) {
                content = format!("{title}\n\n{tail}")
            }
        }
    }
    let mut output = format!(
        "{}\n\n{}\n\n{content}",
        format_index_item(&index),
        pick(
            "これは過去の記録です。現在の状態と照合して利用してください。",
            "This is a historical record. Check it against the current state before relying on it."
        )
    );
    let mut v = json!({"id":text(&document["id"]),"content":content,"topics":index["topics"],"index":public_index(&index)});
    if let Some(page) = document.get("organizationSources") {
        let page = organization_source_page(page, id)?;
        output += &format!(
            "\n\nOrganization sources (use getDocument for each ID and truncated filepath; pass nextCursor as sourceCursor to continue):\n{}",
            serde_json::to_string(&page)?
        );
        v["organizationSources"] = page;
    }
    if let Some(p) = index.get("provenance") {
        v["provenance"] = p.clone()
    }
    if document["enrichment"].is_object() {
        let mut enrichment = json!({});
        for key in [
            "embeddingStatus",
            "factStatus",
            "vectorStatus",
            "topicStatus",
            "error",
            "embeddedAt",
            "factsExtractedAt",
            "topicsExtractedAt",
            "vectorAttemptedAt",
            "updatedAt",
        ] {
            if let Some(value) = document["enrichment"].get(key) {
                enrichment[key] = value.clone()
            }
        }
        v["enrichment"] = enrichment
    }
    Ok(json!({"text":output,"document":v}))
}

fn organization_source_page(page: &Value, id: &str) -> Result<Value> {
    let invalid = || anyhow::anyhow!("Invalid organization source page.");
    let bounded = |value: &Value, maximum: usize| {
        value
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= maximum)
            .map(str::to_owned)
            .ok_or_else(invalid)
    };
    if page["protocol"] != 1 || page["documentId"] != id || !page["hasMore"].is_boolean() {
        return Err(invalid());
    }
    let rows = page["sources"]
        .as_array()
        .filter(|rows| rows.len() <= 50)
        .ok_or_else(invalid)?;
    let mut sources = Vec::new();
    for row in rows {
        let mut source = json!({"id":bounded(&row["id"],64)?,"containerTag":bounded(&row["containerTag"],640)?,
            "createdAt":bounded(&row["createdAt"],80)?,"updatedAt":bounded(&row["updatedAt"],80)?});
        if let Some(kind) = row.get("sourceKind") {
            let kind = kind
                .as_str()
                .filter(|kind| matches!(*kind, "document" | "checkpoint" | "capturePart"))
                .ok_or_else(invalid)?;
            source["sourceKind"] = json!(kind);
            if kind == "capturePart" {
                source["groupId"] = json!(bounded(&row["groupId"], 64)?);
                source["part"] = json!(
                    row["part"]
                        .as_u64()
                        .filter(|part| (1..=10000).contains(part))
                        .ok_or_else(invalid)?
                );
            }
        }
        if let Some(path) = row.get("filepath") {
            let path = path
                .as_str()
                .filter(|path| !path.trim().is_empty() && path.encode_utf16().count() <= 4096)
                .ok_or_else(invalid)?;
            source["filepath"] = json!(path);
        }
        if let Some(truncated) = row.get("filepathTruncated") {
            if truncated.as_bool() != Some(true) || source.get("filepath").is_some() {
                return Err(invalid());
            }
            source["filepathTruncated"] = json!(true);
        }
        sources.push(source);
    }
    let has_more = page["hasMore"] == true;
    let next_cursor = if has_more {
        json!(bounded(&page["nextCursor"], 2048)?)
    } else {
        if !page["nextCursor"].is_null() {
            return Err(invalid());
        }
        Value::Null
    };
    let projected = json!({"protocol":1,"documentId":id,"revision":bounded(&page["revision"],160)?,
        "sources":sources,"hasMore":has_more,"nextCursor":next_cursor});
    if serde_json::to_vec(&projected)?.len() > 64 * 1024 {
        return Err(invalid());
    }
    Ok(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    // 認証済みローカル API の経路を通し、検索・詳細取得の境界を確認する。
    fn fixture(
        responses: Vec<Value>,
    ) -> (Config, std::thread::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let received = requests.clone();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            for response in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "fixture request deadline"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![];
                let mut buffer = [0; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|l| {
                                l.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            assert!(
                                headers
                                    .to_lowercase()
                                    .contains("authorization: bearer fixture-key")
                            );
                            let body =
                                serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                                    .unwrap_or(Value::Null);
                            received.lock().unwrap().push(json!({"path":headers.lines().next().unwrap().split_whitespace().nth(1).unwrap(),"body":body}));
                            break;
                        }
                    }
                }
                let body = response.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        (
            Config {
                base_url,
                api_key: "fixture-key".into(),
                raw: json!({}),
                codex_home: std::path::PathBuf::new(),
            },
            handle,
            requests,
        )
    }
    fn row(id: &str, title: &str) -> Value {
        json!({"id":id,"containerTag":"memories","metadata":{"memoryIndex":build_memory_index(title,title,"","2026-10-01T00:00:00.000Z","conversation")}})
    }

    #[test]
    fn project_path_recall_excludes_generic_repair_request() {
        let query = r#""C:\Users\IMT\dev\remnifold" メモリー機能側で修正で最適化が出来そうな部分が発見できていたら修正してください"#;
        let terms = recall_topic_terms(query);
        assert!(
            matching_topic_terms("Remnifold 索引の読込", &terms).contains(&"remnifold".to_owned())
        );
        assert!(
            matching_topic_terms("請求書 修正してください", &terms).is_empty(),
            "terms: {terms:?}"
        );
        assert!(recall_topic_terms("修正してください").is_empty());
        let adjacent = recall_topic_terms("Kirihaを修正してください Rust_api-v2 拡張機能");
        assert_eq!(adjacent, vec!["rust_api-v2", "kiriha", "拡張"]);
        assert_eq!(query_terms("改善 修正", false), vec!["改善", "修正"]);
        assert_eq!(recall_topic_terms("fixkit"), vec!["fixkit"]);
    }
    #[test]
    fn japanese_topics_and_technical_boundaries() {
        assert_eq!(
            query_terms("この その あの どの ここ そこ this that", false),
            Vec::<String>::new()
        );
        assert_eq!(
            query_terms("Chrome拡張機能を実装して", false),
            vec!["chrome", "拡張機能"]
        );
        assert_eq!(
            query_terms("fix Rust_api-v2 please", true),
            vec!["rust_api-v2"]
        );
        assert!(recall_topic_terms("まだ同じエラーが発生").is_empty());
        assert_eq!(recall_topic_terms("timeout"), vec!["timeout"]);
        let terms = vec!["net".into(), "net8".into(), "rust".into()];
        assert_eq!(
            matching_topic_terms(".NET8 RUST_analyzer rustacean", &terms),
            terms
        );
        let compatibility = vec!["ＲＵＳＴ_api-v2".into(), "net٣".into(), "net8".into()];
        assert_eq!(
            matching_topic_terms("Rust-api_v2 net٣2 net8x", &compatibility),
            compatibility[..2]
        );
        assert!(matching_topic_terms("xnet٣x", &["net٣".into()]).is_empty());
        let terms = vec!["ﬃ".into(), "ﬃ".into(), "rust".into()];
        assert_eq!(matching_topic_terms("ffi rust", &terms), terms);
        let (prepared, total) = prepare_terms(&terms);
        let mut candidate = Candidate {
            row: json!({}),
            index: json!({"title":"ffi","searchExcerpt":"rust"}),
            rank: 0,
            relevance: 0.,
            index_relevance: 0.,
            matches: vec![],
            project_only: false,
        };
        score(&mut candidate, &prepared, total, false);
        assert_eq!(candidate.matches, terms);
        assert_eq!(candidate.index_relevance, 2. / 6.);
        assert_eq!(candidate.relevance, 1.);
        candidate.index = json!({"provenance":{"project":"rust"}});
        let terms = vec!["rust".into()];
        let (prepared, total) = prepare_terms(&terms);
        score(&mut candidate, &prepared, total, false);
        assert!(candidate.project_only);
        assert_eq!(candidate.index_relevance, 1.);
        score(&mut candidate, &prepared, total, true);
        assert!(!candidate.project_only);
        assert_eq!(candidate.relevance, 0.);
        candidate.index = json!({"provenance":{"project":"base"},"searchExcerpt":"excerpt"});
        let terms = vec!["base excerpt".into()];
        let (prepared, total) = prepare_terms(&terms);
        score(&mut candidate, &prepared, total, false);
        assert_eq!(candidate.index_relevance, 0.);
        assert_eq!(candidate.relevance, 1.);
        assert_eq!(candidate.matches, terms);
        let terms = vec![String::new()];
        let (prepared, total) = prepare_terms(&terms);
        score(&mut candidate, &prepared, total, false);
        assert!(candidate.relevance.is_nan() && candidate.index_relevance.is_nan());
        assert_eq!(candidate.matches, terms);
        let (prepared, total) = prepare_terms(&[]);
        score(&mut candidate, &prepared, total, false);
        assert_eq!(candidate.relevance, 0.);
        assert!(candidate.matches.is_empty() && !candidate.project_only);
        assert_eq!(
            automatic_search_text(r#"修正 "D:\Dev\Remnifold\native\src\main.rs""#),
            "修正 Remnifold"
        );
        assert_eq!(
            automatic_search_text(r"参照C:\Users\Alice\Dev\Repo\src"),
            "参照Repo"
        );
        assert_eq!(
            automatic_search_text("参照/home/alice/Dev/Repo/src"),
            "参照Repo"
        );
    }
    #[test]
    fn japanese_adjacent_paths_are_projected_before_authenticated_search() {
        let empty = json!({"searchScope":"all-containers","searchedContainers":[],"spaceDiscoveryComplete":true,"results":[]});
        let (config, handle, requests) = fixture(vec![empty.clone(), empty]);
        for query in [
            r"参照C:\Users\Alice\Dev\Repo\src",
            "参照/home/alice/Dev/Repo/src",
        ] {
            let result = search_index(&config, &json!({"query":query,"automatic":true})).unwrap();
            assert_eq!(result["query"], "参照Repo");
        }
        handle.join().unwrap();
        for request in requests.lock().unwrap().iter() {
            assert_eq!(request["body"]["q"], "参照Repo");
            assert!(!request.to_string().to_lowercase().contains("alice"));
        }
    }
    #[test]
    fn legacy_projection_and_provenance() {
        let doc = json!({"id":"legacy","containerTags":["legacy-space"],"content":"# Old memory\nupdated_at: 2025-04-01T00:00:00Z\n### User request\nKiriha の選択を直す\n### Final assistant response\n## 検証\n修正済み", "metadata":{"sm_project_id":"repo_kiriha__1234567890abcdef","project":"Kiriha","filepath":"src/main.rs"},"topics":[" UI ","UI",""]});
        let index = document_index(&doc, "", &[]);
        assert_eq!(index["title"], "Old memory");
        assert_eq!(index["description"], "Kiriha の選択を直す");
        assert_eq!(index["sourceUpdatedAt"], "2025-04-01T00:00:00Z");
        assert_eq!(index["topics"], json!(["UI"]));
        assert_eq!(index["provenance"]["project"], "Kiriha");
        assert_eq!(index["recallable"], true);
        assert!(public_index(&index).get("provenance").is_none());
        let expected = |title: &str, description: &str, sections: &[&str], recallable| {
            json!({"version":1,"title":title,"description":description,"sections":sections,
                "recallable":recallable,"sourceKind":"conversation","sourceUpdatedAt":"2026-10-01T00:00:00Z"})
        };
        let below = format!("{}x", "😀".repeat(79));
        let boundary = "😀".repeat(80);
        let invocation = "<skill><name>gogo</name><path>x</path>definition</skill>\n/gogo";
        let long_response = format!(
            "## 検証\n```rust\nignored answer\n```\nSession: metadata\n[task](codex://thread/old)\nありがとう。\n了解です。\n- [保持した根拠](https://example.test)\n## 理由\n{}",
            "後続の詳しい説明。\n".repeat(100)
        );
        for (title, request, response, output) in [
            (
                "Kiriha",
                "選択を直す",
                long_response.clone(),
                expected("Kiriha", "選択を直す", &["検証", "理由"], true),
            ),
            (
                "Kiriha",
                "はい",
                "完了です。".to_owned(),
                expected("Kiriha", "はい", &[], false),
            ),
            (
                "Kiriha",
                "はい",
                "## 検証\n修正済み".to_owned(),
                expected("Kiriha", "Kirihaの継続結果・詳細", &["検証"], true),
            ),
            (
                "Kiriha",
                "はい",
                below,
                expected("Kiriha", "はい", &[], false),
            ),
            (
                "Kiriha",
                "はい",
                boundary.clone(),
                expected("Kiriha", "Kirihaの継続結果・詳細", &[], true),
            ),
            (
                "Kiriha",
                invocation,
                boundary.clone(),
                expected("Kiriha", &boundary, &[], true),
            ),
            (
                "Kiriha",
                invocation,
                "完了です。".to_owned(),
                expected("Kiriha", "完了です。", &[], false),
            ),
            (
                "Kiriha",
                "/gogo",
                "完了です。".to_owned(),
                expected("Kiriha", "/gogo", &[], true),
            ),
            (
                "Kiriha",
                "ありがとう。",
                "完了です。".to_owned(),
                expected("Kiriha", "完了です。", &[], false),
            ),
            (
                "Kiriha",
                "",
                long_response,
                expected("Kiriha", "保持した根拠", &["検証", "理由"], false),
            ),
            (
                "Codex task",
                "",
                String::new(),
                expected("保存された記録", "", &[], false),
            ),
            (
                "",
                "手動記録を保存",
                "未使用の回答\n".repeat(100),
                expected("手動記録を保存", "手動記録を保存", &[], true),
            ),
        ] {
            assert_eq!(
                build_memory_index(
                    title,
                    request,
                    &response,
                    "2026-10-01T00:00:00Z",
                    "conversation"
                ),
                output,
                "request={request:?}"
            );
        }
    }
    #[test]
    fn authored_organization_recall_survives_index_reconstruction() {
        for (version, kind, stored, expected) in [
            (1, "organization", true, true),
            (2, "organization", true, false),
            (1, "conversation", true, false),
            (1, "organization", false, false),
        ] {
            let document = json!({"id":"summary","content":"はい","metadata":{"memoryIndex":{
                "version":version,"title":"OrganizationNative final conclusion","description":"はい",
                "sections":["はい"],"sourceKind":kind,"recallable":stored}}});
            let index = document_index(&document, "memories", &[]);
            assert_eq!(
                index["recallable"], expected,
                "version={version}, kind={kind}, stored={stored}"
            );
            if version == 1 {
                assert_eq!(index["description"], "はい");
                assert_eq!(index["sections"], json!(["はい"]));
            }
        }
    }
    #[test]
    fn generated_summary_storage_date_does_not_become_the_source_date() {
        let source_date = "2020-04-01T00:00:00Z";
        let stored_date = "2026-11-06T00:00:00Z";
        let original = json!({"id":"original-2020","containerTag":"memories","content":"2020年に確認した根拠と採用理由。",
            "createdAt":source_date,"updatedAt":source_date,
            "metadata":{"memoryIndex":{"version":1,"title":"Original evidence","description":"2020年の根拠","recallable":true,"sourceUpdatedAt":source_date}}});
        let summary = json!({"id":"summary-2026","containerTag":"memories","content":"2020年の原文を根拠に採用した結論。要約の生成日は2026年。",
            "createdAt":stored_date,"updatedAt":stored_date,
            "metadata":{"sm_organization":true,"memoryIndex":{"version":1,"title":"Generated summary","description":"2020年の根拠から整理した結論","recallable":true}}});
        let (config, handle, requests) = fixture(vec![summary, original]);
        let generated = read_document(&config, "summary-2026").unwrap();
        let source = read_document(&config, "original-2020").unwrap();
        handle.join().unwrap();
        let index = &generated["document"]["index"];
        assert!(index.get("sourceUpdatedAt").is_none());
        assert_eq!(index["updatedAt"], stored_date);
        assert_eq!(index["createdAt"], stored_date);
        assert!(text(&generated["text"]).contains(&format!("storedDate={stored_date}")));
        assert!(!text(&generated["text"]).contains("sourceDate="));
        assert!(text(&generated["document"]["content"]).contains("2020年の原文"));
        assert!(text(&generated["document"]["content"]).contains("生成日は2026年"));
        assert_eq!(source["document"]["index"]["sourceUpdatedAt"], source_date);
        assert!(text(&source["text"]).contains(&format!("sourceDate={source_date}")));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn ordinary_metadata_markers_do_not_disable_automatic_recall() {
        let ordinary = row("ordinary", "Kiriha 選択解除");
        let mut manual_marker = ordinary.clone();
        manual_marker["metadata"]["sm_organization_manual_only"] = json!(true);
        let mut both_markers = manual_marker.clone();
        both_markers["metadata"]["sm_organization"] = json!(true);
        let mut opted_out = both_markers.clone();
        opted_out["metadata"]["memoryIndex"]["recallable"] = json!(false);
        let responses = [ordinary, manual_marker, both_markers, opted_out]
            .into_iter().map(|document| json!({"searchScope":"all-containers","searchedContainers":["memories"],"spaceDiscoveryComplete":true,"results":[document]})).collect();
        let (config, handle, requests) = fixture(responses);
        for expected in [true, true, true, false] {
            let result = search_index(
                &config,
                &json!({"query":"Kiriha 選択解除","automatic":true}),
            )
            .unwrap();
            let rows = result["results"].as_array().unwrap();
            assert_eq!(!rows.is_empty(), expected);
            if expected {
                assert_eq!(rows[0]["id"], "ordinary");
                assert_eq!(rows[0]["recallable"], true);
            }
        }
        handle.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 4);
    }
    #[test]
    fn structured_answers_and_prompt_escaping() {
        let request = r#"<send_user_message_question_reply>[{"question":"Which database?","answer":"Cloudflare D1"}]</send_user_message_question_reply>"#;
        assert_eq!(
            index_request_text(request),
            "回答: Cloudflare D1（質問: Which database?）"
        );
        let mut index = document_index(&row("x", "<b>unsafe</b> & title"), "", &[]);
        index["description"] = json!("<img src=x>description & data");
        let output = format_index_item(&index);
        assert!(!output.contains("<img"));
        assert!(output.contains("&amp;"));
        assert_eq!(short_text("a😀b", 3), "a…");
        assert_eq!(short_text("a😀b", 4), "a😀b");
    }
    #[test]
    fn authenticated_search_selects_latest_revision_and_checkpoint() {
        let mut old = row("old", "UNLHA32 実装");
        old["metadata"]["sessionId"] = json!("prior");
        old["metadata"]["turn"] = json!(1);
        old["metadata"]["part"] = json!(1);
        old["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!("2026-09-01T00:00:00Z");
        let mut source = old.clone();
        source["id"] = json!("new");
        source["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!("2026-10-01T00:00:00Z");
        let mut checkpoint = row("checkpoint", "UNLHA32 実装");
        checkpoint["metadata"]["sm_consolidation"] = json!(true);
        checkpoint["metadata"]["sourceMemoryIds"] =
            json!([" new ", "new", "", " ", null, 7, "new"]);
        let (config, handle, requests) = fixture(vec![
            json!({"searchScope":"all-containers","searchedContainers":["memories"],"spaceDiscoveryComplete":true,"results":[old,source,checkpoint]}),
        ]);
        let result = search_index(
            &config,
            &json!({"query":"UNLHA32 実装","automatic":true,"limit":5}),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(
            result["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| text(&i["id"]))
                .collect::<Vec<_>>(),
            vec!["checkpoint"]
        );
        assert_eq!(requests.lock().unwrap()[0]["body"]["indexOnly"], true);
        assert!(format_search_result(&result).contains("kind=consolidated-checkpoint"));
    }
    #[test]
    fn authenticated_legacy_hydration_and_document_sanitization() {
        let doc = json!({"id":"legacy","containerTag":"memories","content":"# Kiriha\n\nSession: old\nTurn: 1\n\nPart: 1/2\n\n### User request\nKiriha 選択解除\n### Final assistant response\n完了", "metadata":{"captureVersion":2,"title":"Kiriha","sessionId":"old","turn":1,"part":1,"parts":2}});
        let (config, handle, requests) = fixture(vec![
            json!({"searchScope":"all-containers","searchedContainers":["memories"],"spaceDiscoveryComplete":true,"results":[{"id":"legacy","containerTag":"memories","semanticSimilarity":0.9}]}),
            doc.clone(),
            doc,
        ]);
        let result = search_index(&config, &json!({"query":"Kiriha","automatic":true})).unwrap();
        assert_eq!(result["results"][0]["description"], "Kiriha 選択解除");
        let read = read_document(&config, "legacy").unwrap();
        handle.join().unwrap();
        assert!(!text(&read["document"]["content"]).contains("Session:"));
        assert!(!text(&read["document"]["content"]).contains("Part:"));
        assert_eq!(requests.lock().unwrap().len(), 3);
    }
    #[test]
    fn authenticated_organization_source_pages_and_legacy_server() {
        let page = json!({"protocol":1,"documentId":"summary","revision":"r1","sources":[
            {"id":"original","containerTag":"legacy/記憶","createdAt":"2026-11-01","updatedAt":"2026-11-02","sourceKind":"capturePart","groupId":"capture-group","part":21,"filepath":"D:/記憶/😀.md","content":"must not project"},
            {"id":"long-path","containerTag":"legacy/記憶","createdAt":"2026-11-01","updatedAt":"2026-11-02","sourceKind":"checkpoint","filepathTruncated":true}
        ],"hasMore":true,"nextCursor":"cursor&😀%"});
        let doc = json!({"id":"summary","content":"Summary conclusion","organizationSources":page});
        let mut empty_page = page.clone();
        empty_page["sources"] = json!([]);
        let (config, handle, requests) = fixture(vec![
            doc.clone(),
            json!({"organizationSources":page.clone()}),
            json!({"organizationSources":empty_page}),
            doc,
            json!({"id":"old","content":"Old server document"}),
        ]);
        let preview = read_document(&config, "summary").unwrap();
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][0]["containerTag"],
            "legacy/記憶"
        );
        assert!(
            preview["document"]["organizationSources"]["sources"][0]
                .get("content")
                .is_none()
        );
        assert!(text(&preview["text"]).contains("original"));
        assert!(text(&preview["text"]).contains("nextCursor"));
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][0]["filepath"],
            "D:/記憶/😀.md"
        );
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][1]["filepathTruncated"],
            true
        );
        assert!(text(&preview["text"]).contains("D:/記憶/😀.md"));
        assert!(text(&preview["text"]).contains("filepathTruncated"));
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][0]["sourceKind"],
            "capturePart"
        );
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][0]["groupId"],
            "capture-group"
        );
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][0]["part"],
            21
        );
        assert_eq!(
            preview["document"]["organizationSources"]["sources"][1]["sourceKind"],
            "checkpoint"
        );
        let next = read_document_page(&config, "summary", Some("cursor&😀%")).unwrap();
        assert!(next["document"].get("content").is_none());
        let empty = read_document_page(&config, "summary", Some("cursor")).unwrap();
        assert_eq!(empty["document"]["organizationSources"]["hasMore"], true);
        assert_eq!(
            empty["document"]["organizationSources"]["sources"],
            json!([])
        );
        assert!(
            read_document_page(&config, "summary", Some("cursor"))
                .unwrap_err()
                .to_string()
                .contains("Organization source paging is unavailable")
        );
        let error = read_document_page(&config, "old", Some("cursor")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Organization source paging is unavailable; retrieve the summary again with a supported server."
        );
        handle.join().unwrap();
        assert_eq!(
            requests.lock().unwrap()[1]["path"],
            "/v3/documents/summary?sourceCursor=cursor%26%F0%9F%98%80%25"
        );
        let mut oversized = page;
        oversized["sources"] = json!(vec![oversized["sources"][0].clone(); 51]);
        assert!(organization_source_page(&oversized, "summary").is_err());
        oversized["sources"] = json!([]);
        assert!(organization_source_page(&oversized, "summary").is_ok());
    }
    #[test]
    fn mixed_index_and_legacy_hydration_keeps_results_and_failures_aligned() {
        for valid in [true, false] {
            let document = if valid {
                json!({"id":"legacy","containerTag":"memories","content":"### User request\nKiriha legacy detail\n### Final assistant response\n完了","metadata":{"title":"Kiriha legacy"}})
            } else {
                json!({"id":"wrong-document","content":"Kiriha"})
            };
            let (config, handle, requests) = fixture(vec![
                json!({"searchScope":"all-containers","searchedContainers":["memories"],"spaceDiscoveryComplete":true,"results":[row("first","Kiriha first"),{"id":"legacy","containerTag":"memories","semanticSimilarity":0.9},row("last","Kiriha last")]}),
                document,
            ]);
            let result =
                search_index(&config, &json!({"query":"Kiriha","automatic":true})).unwrap();
            handle.join().unwrap();
            let ids: Vec<_> = result["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| text(&r["id"]))
                .collect();
            assert!(ids.contains(&"first") && ids.contains(&"last"));
            assert_eq!(ids.contains(&"legacy"), valid);
            assert_eq!(
                result["failedDocuments"],
                if valid { json!([]) } else { json!(["legacy"]) }
            );
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1]["path"], "/v3/documents/legacy");
        }
    }
    #[test]
    fn older_server_recent_list_is_not_recent_work() {
        let (config, handle, _) = fixture(vec![json!({"documents":[row("x","Kiriha")]})]);
        assert_eq!(recent_task_index(&config, &json!({})).unwrap(), json!([]));
        handle.join().unwrap();
    }
    #[test]
    fn legacy_global_shape_discovers_spaces_and_keeps_explicit_search_body() {
        let (config, handle, requests) = fixture(vec![
            json!({"results":[]}),
            json!({"spaces":[{"containerTag":"memories","memoryCount":2},{"containerTag":"empty","memoryCount":0}]}),
            json!({"results":[row("kiriha","Kiriha 選択解除")]}),
        ]);
        let result = search_index(&config, &json!({"query":"Kiriha"})).unwrap();
        handle.join().unwrap();
        assert_eq!(result["searchScope"], "all-discovered-containers");
        assert_eq!(result["searchedContainers"], json!(["memories"]));
        assert_eq!(result["results"][0]["id"], "kiriha");
        let requests = requests.lock().unwrap();
        assert_eq!(requests[1]["path"], "/v3/container-tags");
        assert_eq!(
            requests[2]["body"],
            json!({"containerTag":"memories","q":"Kiriha","limit":20})
        );
    }
    #[test]
    fn historical_dates_preserve_source_text_and_use_javascript_timezones() {
        assert_eq!(
            date_number("2025-03-01"),
            date_number("2025-03-01T00:00:00Z")
        );
        assert_eq!(
            date_number("Sat, 1 Mar 2025 12:00:00 +0000"),
            date_number("2025-03-01T12:00:00Z")
        );
        let naive = chrono::NaiveDate::from_ymd_opt(2025, 3, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let expected = chrono::Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp_millis();
        for value in [
            "2025-03-01T12:00:00",
            "2025-03-01 12:00:00",
            "2025-03-01T12:00",
        ] {
            assert_eq!(date_number(value), expected, "{value}");
            assert_eq!(
                date(value),
                if value.contains('T') {
                    Some(value)
                } else {
                    None
                }
            );
        }
        assert_eq!(
            date_number("2025-03-01T12:00:00+09:00"),
            date_number("2025-03-01T03:00:00Z")
        );
        for (source, expected) in [
            ("2025-03-01T12:00Z", "2025-03-01T12:00:00Z"),
            ("2025-03-01T12:00+09:00", "2025-03-01T03:00:00Z"),
        ] {
            assert_eq!(date_number(source), date_number(expected));
            assert_eq!(date(source), Some(source));
        }
        assert_eq!(date_number("2025-03-01 12:00:00.125"), expected + 125);
        for comparison_only in [
            "2025-03-01 12:00:00",
            "2025-03-01",
            "Sat, 1 Mar 2025 12:00:00 +0000",
        ] {
            assert!(date_millis(comparison_only).is_some());
            assert_eq!(date(comparison_only), None);
        }
        for invalid in [
            "2025-13-01",
            "2025-03-01 25:00:00",
            "next Tuesday",
            "1740830400000",
            "",
        ] {
            assert_eq!(date(invalid), None, "{invalid}");
        }
    }
    #[test]
    fn historical_dates_cross_api_revision_project_ranking_and_recent_boundaries() {
        let dates = [
            "2025-03-01T12:00:00",
            "2025-03-01 12:00:00",
            "2025-03-01",
            "Sat, 1 Mar 2025 12:00:00 +0000",
            "2025-03-01T12:00Z",
            "2025-03-01T12:00+09:00",
        ];
        let mut revisions = vec![];
        let mut recent = vec![];
        for (i, date) in dates.iter().enumerate() {
            let mut old = row(&format!("old-{i}"), "Kiriha selection");
            old["createdAt"] = json!("2030-01-01T00:00:00Z");
            old["metadata"]["sessionId"] = json!(format!("session-{i}"));
            old["metadata"]["turn"] = json!(1);
            old["metadata"]["part"] = json!(1);
            old["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!("2000-01-01T00:00:00Z");
            if (1..=3).contains(&i) {
                old["createdAt"] = json!("2019-01-01T00:00:00Z");
                old["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!("invalid");
            }
            let mut latest = old.clone();
            latest["id"] = json!(format!("latest-{i}"));
            latest["createdAt"] = json!("2020-01-01T00:00:00Z");
            latest["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!(date);
            revisions.push(old);
            revisions.push(latest.clone());
            recent.push(latest);
        }
        let project_row = |id: &str, date: &str| {
            let mut row = row(id, "Selection state");
            row["metadata"]["project"] = json!("Kiriha");
            row["metadata"]["memoryIndex"]["sourceUpdatedAt"] = json!(date);
            row
        };
        let global = |rows: Value| json!({"searchScope":"all-containers","searchedContainers":["memories"],"spaceDiscoveryComplete":true,"results":rows});
        let (config, handle, _) = fixture(vec![
            global(json!(revisions)),
            global(json!([
                project_row("project-old", "2000-01-01T00:00:00Z"),
                project_row("project-middle", "2024-03-01T12:00:00"),
                project_row("project-new", "2025-03-01T12:00:00")
            ])),
            json!({"recentTasks":true,"documents":recent}),
        ]);
        let automatic = search_index(
            &config,
            &json!({"query":"Kiriha","automatic":true,"limit":10}),
        )
        .unwrap();
        let entries = automatic["results"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        for (i, index) in entries.iter().enumerate() {
            assert_eq!(index["id"], format!("latest-{i}"));
            if i == 0 || i >= 4 {
                assert_eq!(index["sourceUpdatedAt"], dates[i]);
                assert_eq!(public_index(index)["sourceUpdatedAt"], dates[i]);
                assert!(format_index_item(index).contains(&format!("sourceDate={}", dates[i])));
            } else {
                assert!(index.get("sourceUpdatedAt").is_none());
                assert!(public_index(index).get("sourceUpdatedAt").is_none());
                assert!(format_index_item(index).contains("storedDate=2020-01-01T00:00:00Z"));
            }
        }
        let manual = search_index(
            &config,
            &json!({"query":"Kiriha","settings":{"maxMemories":2.5,"minimumSimilarity":0.7}}),
        )
        .unwrap();
        assert_eq!(
            manual["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| text(&i["id"]))
                .collect::<Vec<_>>(),
            vec!["project-new", "project-middle"]
        );
        let recent = recent_task_index(&config, &json!({"limit":6})).unwrap();
        handle.join().unwrap();
        for (i, index) in recent.as_array().unwrap().iter().enumerate() {
            if i == 0 || i >= 4 {
                assert_eq!(index["sourceUpdatedAt"], dates[i]);
                assert!(format_recent_task(index).contains(&format!("sourceDate={}", dates[i])));
            } else {
                assert!(index.get("sourceUpdatedAt").is_none());
                assert!(format_recent_task(index).contains("storedDate=2020-01-01T00:00:00Z"));
            }
        }
    }
    #[test]
    fn expired_deadline_rejects_before_network() {
        assert!(timeout(&json!({"deadlineEpochMs":1})).is_err());
        assert_eq!(timeout(&json!({"timeoutMs":300})).unwrap(), 300);
    }
}
