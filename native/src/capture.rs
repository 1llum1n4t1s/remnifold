//! Local transcript capture. Persisted v2 identities deliberately use JavaScript UTF-16 slices.
use crate::common::{self, Config, get_project_context, sha256, text, truncate_utf16};
use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{LazyLock, OnceLock},
    time::{Duration, Instant, SystemTime},
};

#[derive(Debug)]
pub struct CaptureFailure(pub &'static str);
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for CaptureFailure {}
fn stage<T>(code: &'static str, result: Result<T>) -> Result<T> {
    result.map_err(|e| {
        if e.downcast_ref::<CaptureFailure>().is_some() {
            e
        } else {
            anyhow::Error::new(CaptureFailure(code))
        }
    })
}
pub fn failure_message(error: &anyhow::Error) -> String {
    let (s, c, a) = match error.downcast_ref::<CaptureFailure>().map(|e| e.0) {
        Some("configuration") => ("configuration", "invalid", "repair-required"),
        Some("transcript") => ("transcript", "unavailable", "repair-required"),
        Some("receipt-read") => ("receipt", "read-failed", "repair-required"),
        Some("receipt-invalid") => ("receipt", "invalid", "repair-required"),
        Some("receipt-write") => ("receipt", "write-failed", "repair-required"),
        Some("transport") => ("transport", "request-failed", "retry-or-repair"),
        Some("acknowledgement") => ("transport", "invalid-acknowledgement", "retry-or-repair"),
        Some("payload") => ("payload", "invalid", "repair-required"),
        _ => ("capture", "unknown", "retry-or-repair"),
    };
    format!(
        "Remnifold capture failed; unacknowledged records will be retried. [stage={s}; cause={c}; action={a}]\n"
    )
}
fn compile_regex(p: &str) -> Regex {
    // JavaScript's word boundary is ASCII, including next to Japanese prose.
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).expect("constant regex")
}
// 固定式だけを呼出し箇所ごとに共有し、動的なskill名は保持しない。
macro_rules! re {
    ($pattern:literal) => {{
        static PATTERN: LazyLock<Regex> = LazyLock::new(|| compile_regex($pattern));
        &*PATTERN
    }};
}
fn strip_tag(s: &str, tag: &str) -> String {
    static TAGS: LazyLock<HashMap<&'static str, OnceLock<Regex>>> = LazyLock::new(|| {
        [
            "recommended_plugins",
            "environment_context",
            "codex_internal_context",
            "remnifold-context",
            "remnifold-recall",
            "remnifold-index",
            "kagayoi-memory-context",
            "kagayoi-memory-recall",
            "kagayoi-memory-index",
            "supermemory-context",
            "supermemory-recall",
            "supermemory-index",
            "system-reminder",
            "supermemory-containers",
            "image",
        ]
        .into_iter()
        .map(|tag| (tag, OnceLock::new()))
        .collect()
    });
    TAGS[tag]
        .get_or_init(|| {
            compile_regex(&format!(
                r"(?is)<{}\b[^>]*>.*?</{}\s*>",
                regex::escape(tag),
                regex::escape(tag)
            ))
        })
        .replace_all(s, "")
        .into_owned()
}

pub fn clean_user_request(value: &str) -> String {
    let mut s = value.replace("\r\n", "\n").trim().to_string();
    let agents = re!(r"(?i)^# AGENTS\.md instructions(?: for [^\n]*)?\s*\n");
    if agents.is_match(&s) {
        return String::new();
    }
    for tag in [
        "recommended_plugins",
        "environment_context",
        "codex_internal_context",
        "remnifold-context",
        "remnifold-recall",
        "remnifold-index",
        "kagayoi-memory-context",
        "kagayoi-memory-recall",
        "kagayoi-memory-index",
        "supermemory-context",
        "supermemory-recall",
        "supermemory-index",
        "system-reminder",
    ] {
        s = strip_tag(&s, tag)
    }
    let mut blocks = Vec::new();
    let mut had = false;
    s = re!(r"(?is)<response-annotations\b[^>]*>(.*?)</response-annotations>")
        .replace_all(&s, |c: &regex::Captures| {
            had = true;
            if let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&c[1]) {
                for row in rows {
                    let reference = text(&row["text"]).trim();
                    let comment = text(&row["annotation"]).trim();
                    if reference.is_empty() && comment.is_empty() {
                        continue;
                    }
                    let mut parts = vec![format!(
                        "### Feedback on earlier response {}",
                        blocks.len() + 1
                    )];
                    if !reference.is_empty() {
                        parts.push(format!("Referenced assistant response:\n{reference}"))
                    }
                    if !comment.is_empty() {
                        parts.push(format!("User comment:\n{comment}"))
                    }
                    blocks.push(parts.join("\n\n"));
                }
            }
            String::new()
        })
        .into_owned();
    s = re!(r"(?s)\[SUPERMEMORY CONTAINERS\].*?\[END SUPERMEMORY CONTAINERS\]")
        .replace_all(&s, "")
        .into_owned();
    s = strip_tag(&s, "supermemory-containers").trim().to_string();
    if agents.is_match(&s) {
        return String::new();
    }
    if let Some(i) = s.find("## My request:") {
        if had
            || s.starts_with("## Referenced chats with Codex:")
            || s.starts_with("# Files mentioned by the user:")
        {
            s = s[i + 14..].trim().to_string()
        }
    }
    s = strip_tag(&s, "image").trim().to_string();
    if !s.is_empty() {
        blocks.push(if blocks.is_empty() {
            s
        } else {
            format!("### Current user request\n{s}")
        })
    }
    blocks.join("\n\n").trim().to_string()
}

#[derive(Clone, Debug)]
struct Edit {
    start: usize,
    end: usize,
    value: String,
}
fn add_edit(edits: &mut Vec<Edit>, mut start: usize, mut end: usize, value: String) {
    let overlaps: Vec<_> = edits
        .iter()
        .filter(|e| start < e.end && end > e.start)
        .cloned()
        .collect();
    if !overlaps.is_empty() {
        if !value.is_empty() || overlaps.iter().any(|e| !e.value.is_empty()) {
            return;
        }
        for e in &overlaps {
            start = start.min(e.start);
            end = end.max(e.end)
        }
        edits.retain(|e| {
            !overlaps
                .iter()
                .any(|o| o.start == e.start && o.end == e.end)
        });
    }
    edits.push(Edit { start, end, value });
}
fn projection_edits(s: &str, keep: bool) -> Vec<Edit> {
    let mut edits: Vec<Edit> = vec![];
    let mut add = |pattern: &Regex, replacement: Option<&str>| {
        for c in pattern.captures_iter(s) {
            let m = c.get(0).unwrap();
            let value = replacement
                .map(|_| format!("${}", &c[1]))
                .unwrap_or_default();
            add_edit(&mut edits, m.start(), m.end(), value);
        }
    };
    add(
        re!(r"(?is)<skill\b[^>]*>\s*<name>[^<]+</name>\s*<path>[^<]+</path>.*?</skill\s*>"),
        None,
    );
    add(
        re!(
            r#"(?is)<in-app-browser-context\b[^>]*\bsource\s*=\s*["']ambient-ui-state["'][^>]*>.*?</in-app-browser-context\s*>"#
        ),
        None,
    );
    add(
        re!(r"(?is)<external_codex_apps_open_page\b[^>]*>.*?</external_codex_apps_open_page\s*>"),
        None,
    );
    add(
        re!(r"(?i)\[\$([A-Za-z0-9_:-]+)\]\([^\r\n)]*\bSKILL\.md(?:[?#][^\r\n)]*)?\)"),
        if keep { Some("") } else { None },
    );
    if !keep {
        for c in
            re!(r"(?is)<skill\b[^>]*>\s*<name>([A-Za-z0-9_:-]+)</name>\s*<path>").captures_iter(s)
        {
            // Preserve whitespace after the invocation, just as the original zero-width assertion does.
            let p = format!(r"(?im)^[ \t]*[$/]{}(?:\s|$)", regex::escape(&c[1]));
            for m in compile_regex(&p).find_iter(s) {
                let mut end = m.end();
                if let Some(ch) = m.as_str().chars().last() {
                    if ch.is_whitespace() {
                        end -= ch.len_utf8()
                    }
                }
                add_edit(&mut edits, m.start(), end, String::new())
            }
        }
    }
    edits.sort_by_key(|e| e.start);
    edits
}
fn project_slice(s: &str, start: usize, end: usize, edits: &[Edit]) -> String {
    let mut out = String::new();
    let mut cursor = start;
    for e in edits {
        if e.end <= start || e.start >= end {
            continue;
        }
        out.push_str(&s[cursor..cursor.max(e.start)]);
        if e.start >= start {
            out.push_str(&e.value)
        }
        cursor = cursor.max(end.min(e.end));
    }
    out.push_str(&s[cursor..end]);
    out
}
pub fn project_request_text(value: &str, keep_invocation: bool) -> String {
    project_slice(
        value,
        0,
        value.len(),
        &projection_edits(value, keep_invocation),
    )
}
pub fn project_document_content(value: &str) -> String {
    let Some(m) = re!(r"(?m)^### User request\s*\n").find(value) else {
        return value.to_string();
    };
    let start = m.end();
    let end = re!(r"(?m)^### Final assistant response\s*$")
        .find(&value[start..])
        .map(|m| start + m.start())
        .unwrap_or(value.len());
    format!(
        "{}{}{}",
        &value[..start],
        project_request_text(&value[start..end], true),
        &value[end..]
    )
}

pub fn sanitize_text(value: &str, secrets: &[&str], count: &mut usize) -> String {
    let mut s = re!(r"(?is)<private>.*?</private>")
        .replace_all(value, "[REDACTED]")
        .into_owned();
    s = strip_tag(&s, "system-reminder");
    s=re!(r"(?is)<(?:remnifold|kagayoi-memory|supermemory)-context>.*?</(?:remnifold|kagayoi-memory|supermemory)-context>").replace_all(&s,"").into_owned();
    s = s.replace("\r\n", "\n").trim().to_string();
    for secret in secrets {
        if !secret.is_empty() && s.contains(secret) {
            s = s.replace(secret, "[REDACTED]");
            *count += 1
        }
    }
    static PATTERNS: LazyLock<[(Regex, &str); 2]> = LazyLock::new(|| {
        [
            (
                compile_regex(
                    r"(?s)-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----.*?-----END(?: [A-Z0-9]+)? PRIVATE KEY-----",
                ),
                "[REDACTED]",
            ),
            (
                compile_regex(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{12,}\b"),
                "Bearer [REDACTED]",
            ),
        ]
    });
    for (pattern, r) in PATTERNS.iter() {
        s = pattern
            .replace_all(&s, |_: &regex::Captures| {
                *count += 1;
                (*r).to_string()
            })
            .into_owned()
    }
    static ASSIGNMENTS: LazyLock<[fancy_regex::Regex; 3]> = LazyLock::new(|| {
        let name = r"(?:[a-z0-9]+[_-])*(?:api[_-]?key|api[_-]?token|access[_-]?key|access[_-]?token|refresh[_-]?token|authorization|auth[_-]?token|client[_-]?secret|secret|password|private[_-]?key|token)";
        let quoted = r#"(?:"((?:\\[\s\S]|[^"\\])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))"#;
        let shell = r#"(?:"((?:\\[\s\S]|\x60[\s\S]|[^"\\\x60])*)"|'((?:\\[\s\S]|''|[^'\\])*)'|(\[REDACTED\]|[^\s,"'`}\]\)]+))"#;
        [
            (format!(r#"(?<![A-Za-z0-9_-])(["'])({name})\1"#), quoted),
            (
                format!(r#"(?<![A-Za-z0-9_"'-])(?<!\$env:)()({name})"#),
                quoted,
            ),
            (format!(r"(?<=\$env:)()({name})"), shell),
        ]
        .map(|(prefix, val)| {
            fancy_regex::Regex::new(&format!("(?i){prefix}(\\s*[:=]\\s*){val}")).unwrap()
        })
    });
    for pattern in ASSIGNMENTS.iter() {
        s = pattern
            .replace_all(&s, |c: &fancy_regex::Captures<str>| {
                let q = if c.get(4).is_some() {
                    "\""
                } else if c.get(5).is_some() {
                    "'"
                } else {
                    ""
                };
                let value = c.get(4).or(c.get(5)).or(c.get(6)).unwrap().as_str();
                if value == "[REDACTED]" {
                    return c.get(0).unwrap().as_str().to_string();
                }
                *count += 1;
                format!("{}{}{}{}{}[REDACTED]{}", &c[1], &c[2], &c[1], &c[3], q, q)
            })
            .into_owned();
    }
    s=re!(r"(?i)([?&](?:api[_-]?key|api[_-]?token|access[_-]?token|refresh[_-]?token|authorization|auth[_-]?token|client[_-]?secret|secret|password|token)=)[^&#\s]+").replace_all(&s,|c:&regex::Captures|{*count+=1;format!("{}[REDACTED]",&c[1])}).into_owned();
    s=re!(r"\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{16,}|sm_[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9_-]{16,}|github_pat_[A-Za-z0-9_]{16,}|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[A-Za-z0-9_-]{20,})\b").replace_all(&s,|_:&regex::Captures|{*count+=1;"[REDACTED]"}).into_owned();
    s.trim().to_string()
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub file_path: PathBuf,
    pub meta: Value,
    pub size: u64,
    pub modified: SystemTime,
    pub recent: bool,
}
#[derive(Clone, Debug)]
pub struct Turn {
    pub user: String,
    pub assistant: String,
    pub source_timestamp: String,
}
#[derive(Debug)]
pub struct Transcript {
    pub turns: Vec<Turn>,
    pub redactions: usize,
}
pub fn list_jsonl_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut pending = vec![directory.to_path_buf()];
    let mut files = vec![];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                pending.push(entry.path())
            } else if ty.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
                files.push(entry.path())
            }
        }
    }
    Ok(files)
}
pub fn read_session_meta(path: &Path) -> Result<Option<Value>> {
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if !line.contains("session_meta") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if row["type"] != "session_meta" || !row["payload"].is_object() {
            continue;
        }
        let p = &row["payload"];
        let id = text(&p["id"]).trim();
        if id.is_empty() {
            return Ok(None);
        }
        let root = text(&p["session_id"]).trim();
        let parent = text(&p["parent_thread_id"]).trim();
        let agent = text(&p["agent_path"]).trim();
        return Ok(Some(
            json!({"id":id,"rootSessionId":if root.is_empty(){id}else{root},"parentThreadId":parent,"agentRole":text(&p["agent_role"]).trim(),"timestamp":text(&p["timestamp"]),"isSubagent":!parent.is_empty()||(!agent.is_empty()&&agent!="/root")||(!root.is_empty()&&id!=root),"cwd":text(&p["cwd"])}),
        ));
    }
    Ok(None)
}
pub fn choose_candidates(
    files: &[PathBuf],
    root_only: bool,
) -> Result<(Vec<Candidate>, usize, usize)> {
    let mut grouped: HashMap<String, Candidate> = HashMap::new();
    let mut eligible = 0;
    for file in files {
        let Some(meta) = read_session_meta(file)? else {
            continue;
        };
        if root_only && meta["isSubagent"] == true {
            continue;
        }
        let stat = fs::metadata(file)?;
        let modified = stat.modified()?;
        let recent = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            < Duration::from_secs(600);
        eligible += 1;
        let c = Candidate {
            file_path: file.clone(),
            meta,
            size: stat.len(),
            modified,
            recent,
        };
        let id = text(&c.meta["id"]).to_string();
        if let Some(old) = grouped.get_mut(&id) {
            let active = old.recent || c.recent;
            if c.size > old.size || (c.size == old.size && c.modified > old.modified) {
                *old = c
            }
            old.recent = active
        } else {
            grouped.insert(id, c);
        }
    }
    let mut rows: Vec<_> = grouped.into_values().collect();
    rows.sort_by(|a, b| {
        text(&a.meta["timestamp"])
            .cmp(text(&b.meta["timestamp"]))
            .then(text(&a.meta["id"]).cmp(text(&b.meta["id"])))
    });
    let duplicates = eligible - rows.len();
    let recent = rows.iter().filter(|r| r.recent).count();
    Ok((rows, duplicates, recent))
}
fn extract_blocks(value: &Value, kind: &str) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["type"] == kind && r["text"].is_string())
                .map(|r| text(&r["text"]))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
pub fn parse_task_transcript(path: &Path, secrets: &[&str]) -> Result<Transcript> {
    parse_transcript_reader(BufReader::new(fs::File::open(path)?), secrets)
}
fn parse_transcript_reader(reader: impl BufRead, secrets: &[&str]) -> Result<Transcript> {
    let mut turns: Vec<Turn> = vec![];
    let mut count = 0;
    let mut previous: Option<(bool, String, usize)> = None;
    for (line_index, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let p = &row["payload"];
        let final_output = if !text(&p["phase"]).is_empty() {
            p["phase"] == "final_answer"
        } else {
            text(&p["channel"]).is_empty() || p["channel"] == "final"
        };
        let (user, raw) = match (text(&row["type"]), text(&p["type"]), text(&p["role"])) {
            ("event_msg", "user_message", _) if p["message"].is_string() => {
                (true, text(&p["message"]).to_string())
            }
            ("event_msg", "assistant_output_text", _) if p["text"].is_string() && final_output => {
                (false, text(&p["text"]).to_string())
            }
            ("response_item", _, "user") => (true, extract_blocks(&p["content"], "input_text")),
            ("response_item", _, "assistant") if final_output => {
                (false, extract_blocks(&p["content"], "output_text"))
            }
            _ => continue,
        };
        let content = sanitize_text(
            &if user { clean_user_request(&raw) } else { raw },
            secrets,
            &mut count,
        );
        if content.is_empty() {
            continue;
        }
        if previous
            .as_ref()
            .is_some_and(|(u, s, i)| *u == user && *s == content && line_index - i <= 5)
        {
            continue;
        }
        previous = Some((user, content.clone(), line_index));
        let timestamp = text(&row["timestamp"]).to_string();
        if user {
            if let Some(turn) = turns.last_mut().filter(|t| t.assistant.is_empty()) {
                turn.user.push_str("\n\n");
                turn.user.push_str(&content)
            } else {
                turns.push(Turn {
                    user: content,
                    assistant: String::new(),
                    source_timestamp: timestamp,
                })
            }
        } else if let Some(turn) = turns.last_mut() {
            turn.assistant = content;
            if turn.source_timestamp.is_empty() {
                turn.source_timestamp = timestamp
            }
        }
    }
    turns.retain(|t| !t.assistant.is_empty());
    Ok(Transcript {
        turns,
        redactions: count,
    })
}
fn titles(home: &Path, only_id: Option<&str>) -> Result<HashMap<String, String>> {
    let path = home.join("session_index.jsonl");
    let mut out = HashMap::new();
    if path.exists() {
        for line in BufReader::new(fs::File::open(path)?).lines() {
            let Ok(row) = serde_json::from_str::<Value>(&line?) else {
                continue;
            };
            if row["id"].is_string()
                && row["thread_name"].is_string()
                && only_id.is_none_or(|id| text(&row["id"]) == id)
            {
                out.insert(
                    text(&row["id"]).to_string(),
                    text(&row["thread_name"]).to_string(),
                );
            }
        }
    }
    Ok(out)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}
pub fn build_turn_documents(
    candidate: &Candidate,
    transcript: &Transcript,
    project: &Value,
    title: &str,
    max_chars: usize,
    container: Option<&str>,
) -> Result<Vec<Value>> {
    // Claude Code 由来の記録は出所を区別する。捕捉キーと customId の形式は共通のまま保つ
    // （サーバーは codex-turn-v2:<captureKey> の対応を検証するため）。
    let claude = candidate.meta["format"] == "claude";
    let (source, client, fallback) = if claude {
        ("claude-code", "claude-code-remnifold", "Claude Code task")
    } else {
        ("codex", "codex-kagayoi-memory", "Codex task")
    };
    let title = if title.is_empty() {
        transcript
            .turns
            .first()
            .map(|t| t.user.lines().next().unwrap_or(fallback))
            .unwrap_or(fallback)
    } else {
        title
    };
    let title = sanitize_text(title, &[], &mut 0);
    let legacy_title: Vec<u16> = title.encode_utf16().take(200).collect();
    let projected = project_request_text(&title, false).trim().to_string();
    let safe_title = truncate_utf16(
        if projected.is_empty() {
            fallback
        } else {
            &projected
        },
        200,
    );
    let provenance = !text(&project["containerTag"]).trim().is_empty();
    let reuse = container.is_none() && provenance;
    let container = container.filter(|s| !s.is_empty()).unwrap_or("memories");
    let mut result = vec![];
    for (i, turn) in transcript.turns.iter().enumerate() {
        let body = format!(
            "### User request\n{}\n\n### Final assistant response\n{}",
            turn.user, turn.assistant
        );
        let id = text(&candidate.meta["id"]);
        let identity = format!("{id}:{}:{}", i + 1, sha256(&body));
        let header_length = 2
            + legacy_title.len()
            + 2
            + 9
            + utf16_len(id)
            + 7
            + utf16_len(&(i + 1).to_string())
            + 2;
        let size = max_chars
            .checked_sub(header_length + 80)
            .filter(|s| *s >= 100)
            .ok_or_else(|| anyhow::anyhow!("Document size limit is too small."))?;
        let units: Vec<u16> = body.encode_utf16().collect();
        let mut edits = projection_edits(&turn.user, true);
        for e in &mut edits {
            e.start += "### User request\n".len();
            e.end += "### User request\n".len()
        }
        let mut parts = vec![];
        let mut start = 0;
        let mut cursor = body.char_indices().peekable();
        let mut consumed_units = 0;
        for offset in (0..units.len()).step_by(size) {
            let limit = (offset + size).min(units.len());
            // surrogateの途中ならその文字を次のpartへ残す。hashは旧UTF-16境界を使う。
            while let Some(&(_, ch)) = cursor.peek() {
                if consumed_units + ch.len_utf16() > limit {
                    break;
                }
                consumed_units += ch.len_utf16();
                cursor.next();
            }
            let end = cursor.peek().map(|&(index, _)| index).unwrap_or(body.len());
            let old = String::from_utf16_lossy(&units[offset..(offset + size).min(units.len())]);
            parts.push((
                project_slice(&body, start, end, &edits),
                sha256(old).chars().take(16).collect::<String>(),
            ));
            start = end;
        }
        let memory_index = crate::search::build_memory_index(
            &safe_title,
            &turn.user,
            &turn.assistant,
            &turn.source_timestamp,
            "conversation",
        );
        for (p, (content, hash)) in parts.iter().enumerate() {
            let key = format!("{identity}:{}:{hash}", p + 1);
            let mut index = memory_index.clone();
            if re!(r"(?m)^### (?:User request|Final assistant response)\s*$")
                .replace_all(content, "")
                .trim()
                .is_empty()
            {
                index["recallable"] = json!(false);
                index["sections"] = json!([])
            }
            let mut metadata = json!({"type":"conversation","title":safe_title,"sm_scope":if provenance{"project"}else{"shared"},"sm_source":source,"sm_client":client,"sessionId":id,"rootSessionId":candidate.meta["rootSessionId"],"sessionKind":if candidate.meta["isSubagent"]==true{"subagent"}else{"root"},"turn":i+1,"part":p+1,"parts":parts.len(),"captureVersion":2,"captureKey":key,"memoryIndex":index});
            if !text(&project["projectName"]).is_empty() {
                metadata["project"] = project["projectName"].clone()
            }
            if provenance {
                metadata["sm_project_id"] = project["containerTag"].clone()
            }
            if !turn.source_timestamp.is_empty() {
                metadata["sourceTimestamp"] = json!(turn.source_timestamp)
            }
            let mut doc = json!({"customId":format!("codex-turn-v2:{key}"),"content":format!("# {safe_title}\n\n{content}"),"containerTag":container,"metadata":metadata});
            if reuse {
                doc["reuseExistingCapture"] = json!(true)
            }
            result.push(doc);
        }
    }
    Ok(result)
}

fn receipt_union(paths: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut receipts = BTreeSet::new();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let data = stage("receipt-read", fs::read_to_string(path).map_err(Into::into))?;
        let rows = stage(
            "receipt-invalid",
            serde_json::from_str::<Value>(&data).map_err(Into::into),
        )?;
        let Some(rows) = rows.as_array() else {
            return Err(CaptureFailure("receipt-invalid").into());
        };
        for row in rows {
            let s = text(row);
            if s.is_empty() {
                return Err(CaptureFailure("receipt-invalid").into());
            }
            receipts.insert(s.to_string());
        }
    }
    Ok(receipts)
}
fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    common::atomic_write(path, &serde_json::to_vec(value)?, true)
}
fn api(
    config: &Config,
    path: &str,
    method: &str,
    body: Option<&Value>,
    timeout_ms: u64,
) -> Result<Value> {
    common::api_with_source(
        config,
        path,
        method,
        body,
        timeout_ms,
        "codex-history-backfill",
    )
}
fn record_receipt(paths: &[PathBuf], id: &str) -> Result<()> {
    fs::create_dir_all(paths[0].parent().unwrap())?;
    let mut db = rusqlite::Connection::open(
        paths[0]
            .parent()
            .unwrap()
            .join("capture-coordination.sqlite3"),
    )?;
    db.busy_timeout(Duration::from_secs(10))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS capture_mutex (id INTEGER PRIMARY KEY CHECK (id = 1), touched_at INTEGER NOT NULL); INSERT OR IGNORE INTO capture_mutex (id, touched_at) VALUES (1, 0);")?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE capture_mutex SET touched_at = ? WHERE id = 1",
        [chrono::Utc::now().timestamp_millis()],
    )?;
    let mut receipts = receipt_union(paths)?;
    receipts.insert(id.to_string());
    atomic_json(&paths[0], &json!(receipts))?;
    tx.commit()?;
    Ok(())
}
fn find_transcript(payload: &Value, home: &Path) -> Result<Candidate> {
    let id = text(&payload["session_id"]);
    if id.is_empty() {
        bail!("Missing session ID.")
    }
    let explicit = PathBuf::from(text(&payload["transcript_path"]));
    if !explicit.as_os_str().is_empty() && explicit.exists() {
        // Codex の session_meta を持つ記録はその ID だけを信頼する。持たない記録は
        // Claude Code 形式として、同じ sessionId の本会話行があるときだけ受け付ける。
        let meta = match read_session_meta(&explicit)? {
            Some(meta) => Some(meta).filter(|m| m["id"] == id),
            None => crate::claude_transcript::read_meta(&explicit, id)?,
        }
        .ok_or_else(|| anyhow::anyhow!("Transcript session ID mismatch."))?;
        return Ok(Candidate {
            file_path: explicit,
            meta,
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: false,
        });
    }
    let mut files = list_jsonl_files(&home.join("sessions"))?;
    files.extend(list_jsonl_files(&home.join("archived_sessions"))?);
    files.retain(|p| p.to_string_lossy().contains(id));
    choose_candidates(&files, false)?
        .0
        .into_iter()
        .find(|c| c.meta["id"] == id)
        .ok_or_else(|| anyhow::anyhow!("Session transcript was not found."))
}
pub fn capture(payload: &Value, config: &Config) -> Result<Value> {
    let candidate = stage("transcript", find_transcript(payload, &config.codex_home))?;
    let cwd = text(&candidate.meta["cwd"]);
    let cwd = if cwd.is_empty() {
        text(&payload["cwd"])
    } else {
        cwd
    };
    if cwd.is_empty() {
        return Err(CaptureFailure("transcript").into());
    }
    let project = stage("transcript", get_project_context(Path::new(cwd)))?;
    let (transcript, title) = if candidate.meta["format"] == "claude" {
        // Claude Code はタイトルを記録内の custom-title 行に持つ。
        stage(
            "transcript",
            crate::claude_transcript::parse(
                &candidate.file_path,
                text(&candidate.meta["id"]),
                &[&config.api_key],
            ),
        )?
    } else {
        let transcript = stage(
            "transcript",
            parse_task_transcript(&candidate.file_path, &[&config.api_key]),
        )?;
        let titles = stage(
            "transcript",
            titles(&config.codex_home, Some(text(&candidate.meta["id"]))),
        )?;
        let title = sanitize_text(
            titles
                .get(text(&candidate.meta["id"]))
                .map(String::as_str)
                .unwrap_or(""),
            &[&config.api_key],
            &mut 0,
        );
        (transcript, title)
    };
    let documents = stage(
        "transcript",
        build_turn_documents(&candidate, &transcript, &project, &title, 120_000, None),
    )?;
    let state_id = sha256(format!(
        "{}:{}:{}",
        config.base_url,
        text(&project["containerTag"]),
        text(&candidate.meta["id"])
    ));
    let paths: Vec<_> = ["remnifold", "kagayoi-memory", "cloudflare-memory"]
        .iter()
        .map(|n| {
            config
                .codex_home
                .join(n)
                .join("capture-state")
                .join(format!("{state_id}.json"))
        })
        .collect();
    let receipts = receipt_union(&paths)?;
    let pending: Vec<_> = documents
        .iter()
        .filter(|d| !receipts.contains(text(&d["customId"])))
        .collect();
    let start = Instant::now();
    let mut saved = 0;
    for doc in &pending {
        if start.elapsed() >= Duration::from_secs(90) {
            break;
        }
        let result = stage(
            "transport",
            api(config, "/v3/documents", "POST", Some(doc), 30_000),
        )?;
        if text(&result["id"]).is_empty() {
            return Err(CaptureFailure("acknowledgement").into());
        }
        stage(
            "receipt-write",
            record_receipt(&paths, text(&doc["customId"])),
        )?;
        saved += 1;
    }
    Ok(json!({"saved":saved,"pending":pending.len()-saved,"completedTurns":transcript.turns.len()}))
}

pub fn run_import(args: &[String]) -> Result<()> {
    let mut apply = false;
    let mut root_only = false;
    let mut include_recent = false;
    let mut home = common::codex_home();
    let mut container = String::new();
    let mut limit = 0usize;
    let mut max_chars = 120_000usize;
    let mut state_path = None;
    let mut session = String::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--apply" => apply = true,
            "--root-only" => root_only = true,
            "--include-recent" => include_recent = true,
            "--codex-home"
            | "--container-tag"
            | "--limit"
            | "--max-document-chars"
            | "--state-path"
            | "--session-id" => {
                i += 1;
                let v = args
                    .get(i)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("{arg} requires a value."))?;
                match arg.as_str() {
                    "--codex-home" => home = PathBuf::from(v),
                    "--container-tag" => container = v.clone(),
                    "--limit" => {
                        limit = v.parse().map_err(|_| {
                            anyhow::anyhow!("--limit must be a non-negative integer.")
                        })?
                    }
                    "--max-document-chars" => max_chars = v.parse().unwrap_or(0),
                    "--state-path" => state_path = Some(PathBuf::from(v)),
                    _ => session = v.clone(),
                }
            }
            _ => bail!("Unknown argument: {arg}"),
        }
        i += 1
    }
    if !(10_000..=180_000).contains(&max_chars) {
        bail!("--max-document-chars must be an integer from 10000 through 180000.")
    }
    let home = std::path::absolute(home)?;
    if !home.exists() {
        bail!("Codex home directory was not found.")
    }
    let config = common::load_config(Some(&home))?;
    let mut files = list_jsonl_files(&home.join("sessions"))?;
    files.extend(list_jsonl_files(&home.join("archived_sessions"))?);
    files.retain(|p| session.is_empty() || p.to_string_lossy().contains(&session));
    let (candidates, duplicates, recent) = choose_candidates(&files, root_only)?;
    let selected: Vec<_> = candidates
        .iter()
        .filter(|c| {
            (session.is_empty() || c.meta["id"] == session) && (include_recent || !c.recent)
        })
        .take(if limit == 0 { usize::MAX } else { limit })
        .collect();
    let titles = titles(&home, None)?;
    let mut planned = vec![];
    let mut turns = 0;
    let mut redactions = 0;
    for c in &selected {
        let cwd = text(&c.meta["cwd"]);
        let project = if cwd.is_empty() {
            json!({})
        } else {
            get_project_context(Path::new(cwd))?
        };
        let transcript = parse_task_transcript(&c.file_path, &[&config.api_key])?;
        turns += transcript.turns.len();
        redactions += transcript.redactions;
        let title = sanitize_text(
            titles
                .get(text(&c.meta["id"]))
                .map(String::as_str)
                .unwrap_or(""),
            &[&config.api_key],
            &mut 0,
        );
        planned.extend(build_turn_documents(
            c,
            &transcript,
            &project,
            &title,
            max_chars,
            if container.is_empty() {
                None
            } else {
                Some(&container)
            },
        )?);
    }
    let mut existing_count = 0;
    let mut keys = BTreeSet::new();
    if !planned.is_empty() {
        for page in 1..=10_001 {
            if page > 10_000 {
                bail!("Too many document-list pages.")
            }
            let response = api(
                &config,
                "/v3/documents/list",
                "POST",
                Some(&json!({"page":page,"limit":50,"projection":"capture"})),
                30_000,
            )?;
            let batch = response["documents"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            existing_count += batch.len();
            for doc in &batch {
                if doc["metadata"]["captureVersion"] == 2
                    && !text(&doc["metadata"]["captureKey"]).is_empty()
                {
                    keys.insert(text(&doc["metadata"]["captureKey"]).to_string());
                }
            }
            if response["pagination"]["totalPages"]
                .as_f64()
                .is_some_and(|p| page as f64 >= p)
                || batch.len() < 50
            {
                break;
            }
        }
    }
    let default_state = state_path.is_none();
    let state_path = std::path::absolute(
        state_path.unwrap_or_else(|| common::home_dir().join(".remnifold/history-import-v2.json")),
    )?;
    let load_state = |path: &Path| -> Result<Value> {
        if !path.exists() {
            return Ok(json!({"version":2,"documents":{}}));
        }
        let state: Value = serde_json::from_slice(&fs::read(path)?)
            .map_err(|_| anyhow::anyhow!("History import state is not valid JSON."))?;
        if state["version"] != 2
            || (!state["documents"].is_object() && !state["documents"].is_array())
        {
            bail!("History import state has an unsupported format.")
        }
        Ok(state)
    };
    let mut state = if apply {
        load_state(&state_path)?
    } else {
        Value::Null
    };
    if apply && default_state {
        for name in [".kagayoi-memory", ".codex-supermemory"] {
            let old = load_state(&common::home_dir().join(name).join("history-import-v2.json"))?;
            if let Some(docs) = old["documents"].as_object() {
                for (k, v) in docs {
                    if state["documents"].get(k).is_none() {
                        state["documents"][k] = v.clone()
                    }
                }
            }
        }
    }
    let mut created = 0;
    let mut unchanged = 0;
    for doc in &planned {
        if keys.contains(text(&doc["metadata"]["captureKey"])) {
            unchanged += 1;
            continue;
        }
        if apply {
            let response = api(&config, "/v3/documents", "POST", Some(doc), 30_000)?;
            if text(&response["id"]).is_empty() {
                bail!("Memory API did not acknowledge the document.")
            }
            created += 1;
            let project = text(&doc["metadata"]["sm_project_id"]);
            let key = format!(
                "{}:{}",
                if project.is_empty() {
                    text(&doc["containerTag"])
                } else {
                    project
                },
                text(&doc["metadata"]["captureKey"])
            );
            state["documents"][key] = json!({"completedAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)});
            atomic_json(&state_path, &state)?;
        }
    }
    let root = selected
        .iter()
        .filter(|c| c.meta["isSubagent"] != true)
        .count();
    let tags: BTreeSet<_> = planned.iter().map(|d| text(&d["containerTag"])).collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"mode":if apply{"applied"}else{"plan"},"containerTags":tags,"sourceFiles":files.len(),"duplicateTranscriptFiles":duplicates,"discoveredSessions":candidates.len(),"recentlyActiveSessions":recent,"selectedSessions":selected.len(),"rootSessions":root,"subagentSessions":selected.len()-root,"taskTurns":turns,"plannedDocuments":planned.len(),"plannedCharacters":planned.iter().map(|d|utf16_len(text(&d["content"]))).sum::<usize>(),"secretRedactions":redactions,"existingDocuments":existing_count,"existingHistoricalDocuments":keys.len(),"created":created,"updated":0,"unchanged":unchanged,"stateFile":if apply{state_path.to_string_lossy().into_owned()}else{"not created in plan mode".to_string()},"nextStep":if apply{"Run the same command again to verify idempotency."}else{"Re-run with --apply to upload the sanitized task records."}})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_session_title_keeps_last_valid_match_and_reads_remaining_lines() {
        let home = std::env::temp_dir().join(format!("remnifold-titles-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        let current = "現在😀";
        assert!(titles(&home, Some(current)).unwrap().is_empty());
        let path = home.join("session_index.jsonl");
        fs::write(
            &path,
            [
                json!({"id":current,"thread_name":"以前のtitle"}).to_string(),
                json!({"id":"other","thread_name":"別session"}).to_string(),
                "invalid JSON".to_string(),
                json!({"id":current,"thread_name":"最後のtitle"}).to_string(),
                json!({"id":current,"thread_name":false}).to_string(),
                json!({"id":123,"thread_name":"型が異なるID"}).to_string(),
            ]
            .join("\n"),
        )
        .unwrap();
        let current_titles = titles(&home, Some(current)).unwrap();
        assert_eq!(current_titles.len(), 1);
        assert_eq!(current_titles.get(current).unwrap(), "最後のtitle");
        let all_titles = titles(&home, None).unwrap();
        assert_eq!(all_titles.len(), 2);
        assert_eq!(all_titles.get(current), current_titles.get(current));
        assert_eq!(all_titles.get("other").unwrap(), "別session");
        assert!(titles(&home, Some("missing")).unwrap().is_empty());
        let mut invalid_utf8 =
            format!("{}\n", json!({"id":current,"thread_name":"一致後も読む"})).into_bytes();
        invalid_utf8.extend_from_slice(b"\xff\n");
        fs::write(path, invalid_utf8).unwrap();
        assert!(titles(&home, Some(current)).is_err());
        assert!(titles(&home, None).is_err());
        if std::env::var("REMNIFOLD_KEEP_TEST_STATE").as_deref() != Ok("1") {
            fs::remove_dir_all(home).unwrap();
        }
    }
    #[test]
    fn injection_cleaning() {
        assert_eq!(
            clean_user_request("<environment_context>private</environment_context>\nfix this"),
            "fix this"
        );
        assert_eq!(
            clean_user_request("# AGENTS.md instructions for x\nsecret"),
            ""
        );
        assert_eq!(clean_user_request("<image>x</image>hello\0"), "hello\0");
    }
    #[test]
    fn feedback() {
        assert_eq!(
            clean_user_request(
                r#"<response-annotations>[{"text":"old","annotation":"fix"}]</response-annotations>## My request: now"#
            ),
            "### Feedback on earlier response 1\n\nReferenced assistant response:\nold\n\nUser comment:\nfix\n\n### Current user request\nnow"
        );
    }
    #[test]
    fn projection() {
        let s = "[$gogo](C:/skills/gogo/SKILL.md)\n<skill><name>gogo</name><path>x</path>definition</skill>\n$gogo fix";
        assert_eq!(project_request_text(s, true), "$gogo\n\n$gogo fix");
        assert_eq!(project_request_text(s, false), "\n\n fix");
        assert_eq!(
            project_request_text("$HOME /tmp/file", false),
            "$HOME /tmp/file"
        );
    }
    #[test]
    fn redaction() {
        let mut n = 0;
        let s = sanitize_text(
            "password='a b'; $env:API_KEY=\"a`\"b\"; foo=ok\n{\"access_token\":\"secret\"}\nBearer abcdefghijklmnop\nknown",
            &["known"],
            &mut n,
        );
        assert!(!s.contains("a b"));
        assert!(!s.contains("secret"));
        assert!(s.contains("foo=ok"));
        assert!(!s.contains("a`\"b"));
        assert_eq!(n, 5);
    }
    #[test]
    fn redaction_uses_javascript_ascii_boundaries_in_japanese_prose() {
        let mut n = 0;
        let result = sanitize_text(
            "鍵sk-proj-abcdefghijklmnopです ghp_abcdefghijklmnop。認証Bearer abcdefghijklmnopです\n秘密token='assignment'",
            &[],
            &mut n,
        );
        assert_eq!(
            result,
            "鍵[REDACTED]です [REDACTED]。認証Bearer [REDACTED]です\n秘密token='[REDACTED]'"
        );
        assert_eq!(n, 4);
    }
    #[test]
    fn stable_ids() {
        let c = Candidate {
            file_path: PathBuf::new(),
            meta: json!({"id":"session","rootSessionId":"session","isSubagent":false}),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: false,
        };
        let t = Transcript {
            turns: vec![Turn {
                user: "request".into(),
                assistant: "response".into(),
                source_timestamp: "2026-01-01T00:00:00Z".into(),
            }],
            redactions: 0,
        };
        let docs = build_turn_documents(
            &c,
            &t,
            &json!({"containerTag":"project"}),
            "Task",
            120000,
            None,
        )
        .unwrap();
        let body = "### User request\nrequest\n\n### Final assistant response\nresponse";
        assert_eq!(
            docs[0]["customId"],
            format!(
                "codex-turn-v2:session:1:{}:1:{}",
                sha256(body),
                &sha256(body)[..16]
            )
        );
        assert_eq!(docs[0]["reuseExistingCapture"], true);
        assert_eq!(
            docs[0]["metadata"]["sourceTimestamp"],
            "2026-01-01T00:00:00Z"
        );
    }
    #[test]
    fn fixed_diagnostic() {
        assert!(
            failure_message(&CaptureFailure("transport").into())
                .contains("stage=transport; cause=request-failed")
        );
        assert!(!failure_message(&anyhow::anyhow!("secret")).contains("secret"));
    }
    #[test]
    fn javascript_multipart_identity_fixture() {
        let c = Candidate {
            file_path: PathBuf::new(),
            meta: json!({"id":"session","rootSessionId":"session","isSubagent":false}),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: false,
        };
        let t = Transcript {
            turns: vec![Turn {
                user: format!("{}😀\0[$gogo](C:/skills/gogo/SKILL.md)", "a".repeat(9810)),
                assistant: "response".into(),
                source_timestamp: String::new(),
            }],
            redactions: 0,
        };
        for (title, hashes, lengths) in [
            (
                "Task".to_string(),
                ["3ab93a0f10597e3c", "56c75911f7753b8b"],
                [9867, 23],
            ),
            (
                format!("{}😀z", "あ".repeat(199)),
                ["65255b8c5b611d70", "9a302abf455bad85"],
                [9893, 387],
            ),
        ] {
            let docs = build_turn_documents(
                &c,
                &t,
                &json!({"containerTag":"project"}),
                &title,
                10000,
                None,
            )
            .unwrap();
            assert_eq!(docs.len(), 2);
            for (i, doc) in docs.iter().enumerate() {
                assert_eq!(
                    doc["customId"],
                    format!(
                        "codex-turn-v2:session:1:6d1f64b219155ebccd19a92249f92fb8b82d5a1682fba52050e3183b08f82bc0:{}:{}",
                        i + 1,
                        hashes[i]
                    )
                );
                assert_eq!(utf16_len(text(&doc["content"])), lengths[i]);
            }
        }
    }
    #[test]
    fn transcript_final_only_concat_dedup_and_source_timestamp() {
        let rows = [
            json!({"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"user_message","message":"request known"}}),
            json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":"request known"}]}}),
            json!({"type":"response_item","payload":{"role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"progress"}]}}),
            json!({"type":"response_item","payload":{"role":"user","content":"correction"}}),
            json!({"type":"response_item","payload":{"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done\0😀"}]}}),
            json!({"type":"event_msg","payload":{"type":"assistant_output_text","text":"done\0😀"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"unfinished"}}),
        ];
        let source = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = parse_transcript_reader(std::io::Cursor::new(source), &["known"]).unwrap();
        assert_eq!(transcript.turns.len(), 1);
        assert_eq!(transcript.turns[0].user, "request [REDACTED]\n\ncorrection");
        assert_eq!(transcript.turns[0].assistant, "done\0😀");
        assert_eq!(transcript.turns[0].source_timestamp, "2026-01-01T00:00:00Z");
        assert_eq!(transcript.redactions, 2);
    }

    #[test]
    fn multipart_preserves_three_split_surrogates_and_legacy_ids() {
        let candidate = Candidate {
            file_path: PathBuf::new(),
            meta: json!({"id":"session","rootSessionId":"session","isSubagent":false}),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: false,
        };
        let transcript = Transcript {
            turns: vec![Turn {
                user: format!(
                    "{}😀{}🚀{}🌸z",
                    "a".repeat(124),
                    "b".repeat(140),
                    "c".repeat(140)
                ),
                assistant: "response".into(),
                source_timestamp: String::new(),
            }],
            redactions: 0,
        };
        let docs = build_turn_documents(
            &candidate,
            &transcript,
            &json!({"containerTag":"project"}),
            "Task",
            256,
            None,
        )
        .unwrap();
        // JavaScriptの142 UTF-16単位sliceから算出した固定fixture。
        let hashes = [
            "3760505106760124",
            "dfec22f7a0d7ec56",
            "e2006a3c5e4cf1ae",
            "e6dd4e12ac9b331d",
        ];
        let bodies = [
            format!("### User request\n{}", "a".repeat(124)),
            format!("😀{}", "b".repeat(140)),
            format!("🚀{}", "c".repeat(140)),
            "🌸z\n\n### Final assistant response\nresponse".into(),
        ];
        assert_eq!(docs.len(), 4);
        for (i, doc) in docs.iter().enumerate() {
            let key = format!(
                "session:1:cdf4717441177f49f84b0d1e30f14e92204c2642e665b41341efcd802b099b6f:{}:{}",
                i + 1,
                hashes[i]
            );
            assert_eq!(doc["customId"], format!("codex-turn-v2:{key}"));
            assert_eq!(doc["metadata"]["captureKey"], key);
            assert_eq!(doc["metadata"]["parts"], 4);
            assert_eq!(doc["content"], format!("# Task\n\n{}", bodies[i]));
        }
    }

    #[test]
    fn split_surrogate_uses_legacy_hash_without_losing_output_character() {
        let candidate = Candidate {
            file_path: PathBuf::new(),
            meta: json!({"id":"session","rootSessionId":"session","isSubagent":false}),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: false,
        };
        let transcript = Transcript {
            turns: vec![Turn {
                user: format!("{}😀z", "a".repeat(9868)),
                assistant: "response".into(),
                source_timestamp: String::new(),
            }],
            redactions: 0,
        };
        let docs = build_turn_documents(
            &candidate,
            &transcript,
            &json!({"containerTag":"project"}),
            "Task",
            10000,
            None,
        )
        .unwrap();
        let hashes = ["88c776ea0feedc46", "e6dd4e12ac9b331d"];
        for (i, doc) in docs.iter().enumerate() {
            assert_eq!(
                doc["customId"],
                format!(
                    "codex-turn-v2:session:1:7fb7072329cca0e60652a5bf51f47144293bccfb73a289a2b222f14e23fd5776:{}:{}",
                    i + 1,
                    hashes[i]
                )
            );
        }
        assert_eq!(utf16_len(text(&docs[0]["content"])), 9893);
        assert!(text(&docs[1]["content"]).starts_with("# Task\n\n😀z"));
        assert!(!text(&docs[0]["content"]).contains('\u{fffd}'));
    }
}
