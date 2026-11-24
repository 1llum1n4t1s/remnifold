//! Claude Code の会話記録(JSONL)から、Codex と同じ「依頼と最終回答」のターンを取り出す。
//!
//! Claude Code の記録は Codex の rollout と形式が異なる。
//! - 行の種類は `type`（user / assistant / system / custom-title / attachment など）で判断する。
//!   `message.role` だけで判断すると、API要求の写し(`api-request-blob`)や
//!   キュー操作(`queue-operation`)の重複行まで拾ってしまうため使わない。
//! - 1つのアシスタント応答はブロックごとに別行へ分かれる。同じ `message.id` の text
//!   ブロックは連結し、新しい応答が来たら上書きして「最後の応答＝最終回答」を残す。
//! - 人の依頼以外の user 行（ツール結果、バックグラウンド通知、メタ、要約）は依頼にしない。
//!   バックグラウンド通知の後に続く回答は、保存済みターンの本文を変えないよう
//!   固定ラベルの「続き」ターンとして分ける（同じターンの本文が変わると捕捉IDが分裂する）。
//! - 再開・分岐で写された別セッションの履歴は、`sessionId` が一致しない行として除外する。
use crate::capture::{Transcript, Turn, clean_user_request, sanitize_text};
use crate::common::text;
use crate::session_events::{EventKind, QaPair, SessionEvent};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

/// バックグラウンド通知の後に続いた回答を収めるターンの依頼欄。通知本文は保存しない。
pub const CONTINUATION_LABEL: &str = "(Continuation after a background notification)";

/// Claude Code の記録として読めるときだけ、Codex の session_meta 相当のメタ情報を返す。
/// 指定セッションの user / assistant 行が1つも無ければ None。
pub fn read_meta(path: &Path, session: &str) -> Result<Option<Value>> {
    if session.trim().is_empty() {
        return Ok(None);
    }
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if !line.contains("\"sessionId\"") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !matches!(text(&row["type"]), "user" | "assistant")
            || text(&row["sessionId"]) != session
            || row["isSidechain"] == true
        {
            continue;
        }
        return Ok(Some(json!({
            "id": session,
            "rootSessionId": session,
            "parentThreadId": "",
            "agentRole": "",
            "timestamp": text(&row["timestamp"]),
            "isSubagent": false,
            "cwd": text(&row["cwd"]),
            "format": "claude",
        })));
    }
    Ok(None)
}

/// 人が入力した依頼か、通知などの自動入力か、無視すべき行かの分類。
enum UserRow {
    Human(String),
    Notification,
    Ignore,
}

/// スラッシュコマンドとローカルコマンド出力の包み。依頼本文として残さない。
fn is_command_wrapper(trimmed: &str) -> bool {
    trimmed.starts_with("<command-name>")
        || trimmed.starts_with("<command-message>")
        || trimmed.starts_with("<local-command-")
        || trimmed.starts_with("Caveat: The messages below were generated")
}

fn text_of_content(content: &Value, separator: &str) -> String {
    match (content.as_str(), content.as_array()) {
        (Some(s), _) => s.to_string(),
        (_, Some(blocks)) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .map(|b| text(&b["text"]))
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

/// ターンの構築とは独立に、圧縮要約・途中の発言・質問回答を同じ走査で集める。
#[derive(Default)]
struct EventCollector {
    events: Vec<SessionEvent>,
    seen: HashSet<(EventKind, String)>,
    question_tool_ids: HashSet<String>,
    /// 人の依頼文 → その発言の時刻（秒）。時刻が読めない発言は入れない。
    human_requests: HashMap<String, Vec<i64>>,
}

/// 同じ依頼文が、途中発言の前後この秒数以内にある人の行なら、同一発言の写しとみなす。
const DUPLICATE_WINDOW_SECS: i64 = 60;

/// `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM|±HHMM)` を UNIX 秒にする。読めなければ None。
fn epoch_secs(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if !ts.is_ascii() || b.len() < 19 || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    if [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')]
        .iter()
        .any(|&(i, c)| b[i] != c)
    {
        return None;
    }
    let n = |r: std::ops::Range<usize>| ts[r].parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, s) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let rest = ts[19..]
        .trim_start_matches('.')
        .trim_start_matches(|c: char| c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') if rest.len() <= 1 => 0,
        Some(sign @ (b'+' | b'-')) => {
            let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let secs =
                digits[..2].parse::<i64>().ok()? * 3600 + digits[2..].parse::<i64>().ok()? * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    // 暦日 → 1970-01-01 からの日数（Howard Hinnant の days_from_civil）。
    let (y, m) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s - offset)
}

impl EventCollector {
    fn push(&mut self, kind: EventKind, key: &str, row: &Value, text: String, qa: Vec<QaPair>) {
        if key.is_empty() || !self.seen.insert((kind, key.to_string())) {
            return;
        }
        self.events.push(SessionEvent {
            kind,
            key: key.to_string(),
            timestamp: self::text(&row["timestamp"]).to_string(),
            text,
            qa,
        });
    }

    fn observe(&mut self, row: &Value) {
        if row["isSidechain"] == true {
            return;
        }
        match text(&row["type"]) {
            "user" => self.user(row),
            "assistant" => self.assistant(row),
            "attachment" => self.queued_prompt(row),
            _ => {}
        }
    }

    fn user(&mut self, row: &Value) {
        if row["isCompactSummary"] == true {
            let summary = text_of_content(&row["message"]["content"], "\n");
            self.push(
                EventKind::Compaction,
                text(&row["uuid"]),
                row,
                summary,
                vec![],
            );
            return;
        }
        let answered = row["message"]["content"].as_array().and_then(|blocks| {
            blocks.iter().find(|b| {
                b["type"] == "tool_result"
                    && b["is_error"] != true
                    && self.question_tool_ids.contains(text(&b["tool_use_id"]))
            })
        });
        if let Some(block) = answered {
            let id = text(&block["tool_use_id"]).to_string();
            let qa = question_answers(&row["toolUseResult"]);
            if !qa.is_empty() {
                self.push(EventKind::Answer, &id, row, String::new(), qa);
            }
            return;
        }
        if let UserRow::Human(raw) = classify_user(row) {
            let request = clean_user_request(&raw);
            if let (false, Some(at)) = (request.is_empty(), epoch_secs(text(&row["timestamp"]))) {
                self.human_requests.entry(request).or_default().push(at);
            }
        }
    }

    fn assistant(&mut self, row: &Value) {
        let Some(blocks) = row["message"]["content"].as_array() else {
            return;
        };
        for block in blocks {
            if block["type"] == "tool_use" && block["name"] == "AskUserQuestion" {
                self.question_tool_ids
                    .insert(text(&block["id"]).to_string());
            }
        }
    }

    fn queued_prompt(&mut self, row: &Value) {
        let a = &row["attachment"];
        if a["type"] != "queued_command"
            || a["commandMode"] != "prompt"
            || a["origin"]["kind"] != "human"
        {
            return;
        }
        let raw = text_of_content(&a["prompt"], "\n");
        if is_command_wrapper(raw.trim_start()) {
            return;
        }
        let request = clean_user_request(&raw);
        if !request.is_empty() {
            self.push(EventKind::Message, text(&row["uuid"]), row, request, vec![]);
        }
    }

    /// 保存済みターンと同じ依頼文（前後60秒以内の同文の人の行がある発言）は捨て、秘密値を伏せ、空のイベントを除く。
    fn finish(self, secrets: &[&str]) -> Vec<SessionEvent> {
        let mut redactions = 0;
        let mut clean = |s: &str| {
            sanitize_text(s, secrets, &mut redactions)
                .trim()
                .to_string()
        };
        let human = self.human_requests;
        self.events
            .into_iter()
            .filter(|e| {
                let near = |at: i64| {
                    epoch_secs(&e.timestamp)
                        .is_some_and(|t| (t - at).abs() <= DUPLICATE_WINDOW_SECS)
                };
                !(e.kind == EventKind::Message
                    && human
                        .get(&e.text)
                        .is_some_and(|times| times.iter().any(|&at| near(at))))
            })
            .map(|mut e| {
                e.text = clean(&e.text);
                for p in &mut e.qa {
                    p.question = clean(&p.question);
                    p.header = clean(&p.header);
                    p.answer = clean(&p.answer);
                    p.labels = p.labels.iter().map(|l| clean(l)).collect();
                }
                e.qa.retain(|p| !p.question.is_empty() && !p.answer.is_empty());
                e
            })
            .filter(|e| match e.kind {
                EventKind::Answer => !e.qa.is_empty(),
                _ => !e.text.is_empty(),
            })
            .collect()
    }
}

/// `toolUseResult` が `{questions:[…], answers:{質問文: 回答}}` の形のときだけ質問と回答を取り出す。
fn question_answers(result: &Value) -> Vec<QaPair> {
    let (Some(questions), Some(answers)) = (
        result["questions"].as_array(),
        result["answers"].as_object(),
    ) else {
        return vec![];
    };
    questions
        .iter()
        .filter_map(|q| {
            let question = text(&q["question"]);
            let answer = answers.get(question)?.as_str()?;
            Some(QaPair {
                question: question.to_string(),
                header: text(&q["header"]).to_string(),
                answer: answer.to_string(),
                labels: q["options"]
                    .as_array()
                    .map(|o| o.iter().map(|o| text(&o["label"]).to_string()).collect())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn classify_user(row: &Value) -> UserRow {
    if row["isMeta"] == true || row["isCompactSummary"] == true || row["isSidechain"] == true {
        return UserRow::Ignore;
    }
    let content = &row["message"]["content"];
    let raw = if let Some(s) = content.as_str() {
        s.to_string()
    } else if let Some(blocks) = content.as_array() {
        // ツール結果を含む行はモデルへの返却であり、人の依頼ではない。
        if blocks.iter().any(|b| b["type"] == "tool_result") {
            return UserRow::Ignore;
        }
        blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .map(|b| text(&b["text"]))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        return UserRow::Ignore;
    };
    let trimmed = raw.trim_start();
    if is_command_wrapper(trimmed) {
        return UserRow::Ignore;
    }
    match row["origin"]["kind"].as_str() {
        Some("human") => UserRow::Human(raw),
        Some(_) => UserRow::Notification,
        // origin の無い旧形式では、既知の自動入力の包みで判断する。
        None if trimmed.starts_with("<task-notification>")
            || trimmed.starts_with("<system-reminder>") =>
        {
            UserRow::Notification
        }
        None => UserRow::Human(raw),
    }
}

/// 指定セッションの行だけからターンを組み立てる。最新のカスタムタイトルも返す。
pub fn parse(path: &Path, session: &str, secrets: &[&str]) -> Result<(Transcript, String)> {
    parse_reader(BufReader::new(fs::File::open(path)?), session, secrets)
}

pub(crate) fn parse_reader(
    reader: impl BufRead,
    session: &str,
    secrets: &[&str],
) -> Result<(Transcript, String)> {
    let (transcript, title, _) = parse_reader_with_events(reader, session, secrets)?;
    Ok((transcript, title))
}

/// `parse` と同じ走査で、圧縮要約・途中の発言・質問回答のイベントも返す。
/// `Transcript` のターンは `parse` と同一で、イベントの有無に影響されない。
pub fn parse_with_events(
    path: &Path,
    session: &str,
    secrets: &[&str],
) -> Result<(Transcript, String, Vec<SessionEvent>)> {
    parse_reader_with_events(BufReader::new(fs::File::open(path)?), session, secrets)
}

pub(crate) fn parse_reader_with_events(
    reader: impl BufRead,
    session: &str,
    secrets: &[&str],
) -> Result<(Transcript, String, Vec<SessionEvent>)> {
    let mut events = EventCollector::default();
    let mut turns: Vec<Turn> = vec![];
    let mut title = String::new();
    // 直前に回答へ取り込んだアシスタント応答のID。同じIDの続きは連結する。
    let mut current_message: Option<String> = None;
    for line in reader.lines() {
        let line = line?;
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if text(&row["sessionId"]) != session {
            continue;
        }
        events.observe(&row);
        match text(&row["type"]) {
            "custom-title" => {
                let value = text(&row["customTitle"]).trim();
                if !value.is_empty() {
                    title = value.to_string();
                }
            }
            "user" => match classify_user(&row) {
                UserRow::Human(raw) => {
                    let request = clean_user_request(&raw);
                    if request.is_empty() {
                        continue;
                    }
                    current_message = None;
                    if let Some(turn) = turns.last_mut().filter(|t| t.assistant.is_empty()) {
                        // 回答前に重ねて送られた依頼は同じターンへまとめる（Codex と同じ扱い）。
                        if turn.user != CONTINUATION_LABEL {
                            turn.user.push_str("\n\n");
                            turn.user.push_str(&request);
                            continue;
                        }
                        turn.user = request;
                        turn.source_timestamp = text(&row["timestamp"]).to_string();
                        continue;
                    }
                    turns.push(Turn {
                        user: request,
                        assistant: String::new(),
                        source_timestamp: text(&row["timestamp"]).to_string(),
                    });
                }
                UserRow::Notification => {
                    current_message = None;
                    // 回答済みのターンは閉じ、以降の回答は「続き」ターンへ入れる。
                    if turns.last().is_some_and(|t| !t.assistant.is_empty()) {
                        turns.push(Turn {
                            user: CONTINUATION_LABEL.to_string(),
                            assistant: String::new(),
                            source_timestamp: text(&row["timestamp"]).to_string(),
                        });
                    }
                }
                UserRow::Ignore => {}
            },
            "assistant" if row["isSidechain"] != true => {
                let Some(blocks) = row["message"]["content"].as_array() else {
                    continue;
                };
                // 思考・ツール呼び出し・ツール結果は保存せず、text ブロックだけを回答にする。
                let body = blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .map(|b| text(&b["text"]))
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if body.trim().is_empty() {
                    continue;
                }
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                let id = text(&row["message"]["id"]).to_string();
                if !id.is_empty() && current_message.as_deref() == Some(id.as_str()) {
                    turn.assistant.push('\n');
                    turn.assistant.push_str(&body);
                } else {
                    turn.assistant = body;
                    current_message = (!id.is_empty()).then_some(id);
                }
                if turn.source_timestamp.is_empty() {
                    turn.source_timestamp = text(&row["timestamp"]).to_string();
                }
            }
            _ => {}
        }
    }
    // 回答の無いターン（中断・続き未回答）は保存しない。秘匿値の伏せ字化は最後にまとめて行う。
    let mut redactions = 0;
    let turns = turns
        .into_iter()
        .filter(|t| !t.assistant.trim().is_empty())
        .map(|t| Turn {
            user: sanitize_text(&t.user, secrets, &mut redactions),
            assistant: sanitize_text(&t.assistant, secrets, &mut redactions),
            source_timestamp: t.source_timestamp,
        })
        .filter(|t| !t.user.is_empty() && !t.assistant.is_empty())
        .collect();
    let title = sanitize_text(&title, secrets, &mut redactions);
    let events = events.finish(secrets);
    Ok((Transcript { turns, redactions }, title, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "claude-session";

    fn row(value: Value) -> String {
        format!("{value}\n")
    }
    fn human(text: &str) -> String {
        row(
            json!({"type":"user","sessionId":S,"cwd":"D:\\work","timestamp":"2026-11-12T00:00:00Z","origin":{"kind":"human"},"message":{"role":"user","content":text}}),
        )
    }
    fn say(id: &str, blocks: Value) -> String {
        row(
            json!({"type":"assistant","sessionId":S,"timestamp":"2026-11-12T00:00:01Z","message":{"id":id,"role":"assistant","content":blocks}}),
        )
    }
    fn parse_str(input: &str) -> (Transcript, String) {
        parse_reader(input.as_bytes(), S, &["sk-secret-value"]).unwrap()
    }

    fn fixture() -> String {
        [
            row(json!({"type":"queue-operation","operation":"enqueue","sessionId":S,"content":"依頼A"})),
            human("依頼A"),
            row(json!({"type":"api-request-blob","sessionId":S,"message":{"role":"user","content":[{"type":"text","text":"重複した要求"}]}})),
            row(json!({"type":"attachment","sessionId":S,"attachment":{"content":"<remnifold-index>注入</remnifold-index>"}})),
            say("m1", json!([{"type":"thinking","thinking":"内部思考"}])),
            say("m1", json!([{"type":"text","text":"途中経過"}])),
            say("m1", json!([{"type":"tool_use","name":"Bash","input":{"command":"ls"}}])),
            row(json!({"type":"user","sessionId":S,"message":{"role":"user","content":[{"type":"tool_result","content":"ツール出力"}]}})),
            say("m2", json!([{"type":"text","text":"最終回答1"}])),
            say("m2", json!([{"type":"text","text":"最終回答2 sk-secret-value"}])),
            row(json!({"type":"api-request-blob","sessionId":S,"message":{"role":"assistant","content":[{"type":"text","text":"重複した回答"}]}})),
            row(json!({"type":"user","sessionId":S,"isMeta":true,"message":{"role":"user","content":"メタ"}})),
            row(json!({"type":"user","sessionId":S,"isSidechain":true,"message":{"role":"user","content":"サブエージェント"}})),
            row(json!({"type":"user","sessionId":"other-session","origin":{"kind":"human"},"message":{"role":"user","content":"別セッションの写し"}})),
            row(json!({"type":"custom-title","customTitle":"古いタイトル","sessionId":S})),
            row(json!({"type":"custom-title","customTitle":"新しいタイトル","sessionId":S})),
        ]
        .concat()
    }

    #[test]
    fn extracts_human_requests_and_final_text_without_internal_rows() {
        let (transcript, title) = parse_str(&fixture());
        assert_eq!(title, "新しいタイトル");
        assert_eq!(transcript.turns.len(), 1);
        let turn = &transcript.turns[0];
        assert_eq!(turn.user, "依頼A");
        assert!(turn.assistant.starts_with("最終回答1\n最終回答2 "));
        assert!(!turn.assistant.contains("sk-secret-value"));
        assert_eq!(transcript.redactions, 1);
        for leaked in [
            "内部思考",
            "途中経過",
            "ツール出力",
            "重複",
            "メタ",
            "サブエージェント",
            "別セッション",
            "注入",
        ] {
            assert!(
                !turn.user.contains(leaked) && !turn.assistant.contains(leaked),
                "{leaked}"
            );
        }
    }

    #[test]
    fn notifications_append_continuation_turns_without_changing_saved_turns() {
        let before = parse_str(&fixture()).0;
        let appended = format!(
            "{}{}{}{}{}",
            fixture(),
            row(
                json!({"type":"user","sessionId":S,"origin":{"kind":"task-notification"},"message":{"role":"user","content":"<task-notification>C:/private/path toolu_1</task-notification>"}})
            ),
            say("m3", json!([{"type":"text","text":"通知後の報告"}])),
            human("依頼B"),
            say("m4", json!([{"type":"text","text":"回答B"}])),
        );
        let after = parse_str(&appended).0;
        assert_eq!(after.turns[0].user, before.turns[0].user);
        assert_eq!(after.turns[0].assistant, before.turns[0].assistant);
        assert_eq!(after.turns.len(), 3);
        assert_eq!(after.turns[1].user, CONTINUATION_LABEL);
        assert_eq!(after.turns[1].assistant, "通知後の報告");
        assert!(!after.turns[1].user.contains("private"));
        assert_eq!(
            (
                after.turns[2].user.as_str(),
                after.turns[2].assistant.as_str()
            ),
            ("依頼B", "回答B")
        );
    }

    #[test]
    fn legacy_rows_without_origin_and_command_wrappers_are_classified() {
        let input = [
            row(json!({"type":"user","sessionId":S,"message":{"role":"user","content":"旧形式の依頼"}})),
            say("a", json!([{"type":"text","text":"旧形式の回答"}])),
            row(json!({"type":"user","sessionId":S,"message":{"role":"user","content":"<task-notification>done</task-notification>"}})),
            row(json!({"type":"user","sessionId":S,"origin":{"kind":"human"},"message":{"role":"user","content":"<command-name>/clear</command-name>"}})),
            row(json!({"type":"user","sessionId":S,"isCompactSummary":true,"message":{"role":"user","content":"This session is being continued"}})),
            say("b", json!([{"type":"text","text":"続き"}])),
        ]
        .concat();
        let turns = parse_str(&input).0.turns;
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user, "旧形式の依頼");
        assert_eq!(turns[1].user, CONTINUATION_LABEL);
        assert_eq!(turns[1].assistant, "続き");
    }

    fn with_events(input: &str) -> (Transcript, Vec<SessionEvent>) {
        let (t, _, e) =
            parse_reader_with_events(input.as_bytes(), S, &["sk-secret-value"]).unwrap();
        (t, e)
    }
    fn queued(uuid: &str, prompt: Value, origin: Value, mode: &str) -> String {
        let mut attachment = json!({"type":"queued_command","commandMode":mode,"prompt":prompt});
        if !origin.is_null() {
            attachment["origin"] = origin;
        }
        row(
            json!({"type":"attachment","sessionId":S,"uuid":uuid,"timestamp":"2026-11-12T00:00:05Z","attachment":attachment}),
        )
    }
    fn compact(uuid: &str, content: Value) -> String {
        row(
            json!({"type":"user","sessionId":S,"uuid":uuid,"isCompactSummary":true,"timestamp":"2026-11-12T00:00:04Z","message":{"role":"user","content":content}}),
        )
    }
    fn ask(id: &str) -> String {
        say(
            "ma",
            json!([{"type":"tool_use","id":id,"name":"AskUserQuestion","input":{}}]),
        )
    }
    fn answered(id: &str, result: Value, is_error: bool) -> String {
        row(
            json!({"type":"user","sessionId":S,"timestamp":"2026-11-12T00:00:06Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"is_error":is_error,"content":"x"}]},"toolUseResult":result}),
        )
    }
    fn good_result() -> Value {
        json!({"questions":[{"question":"方式は？","header":"方式","options":[{"label":"A"},{"label":"B"}]}],"answers":{"方式は？":"A"}})
    }
    fn event_rows() -> String {
        [
            compact("c1", json!("継続の要約 sk-secret-value")),
            compact("c1", json!("同じ uuid の写し")),
            queued("q1", json!([{"type":"text","text":"途中の発言"},{"type":"text","text":"二行目"}]), json!({"kind":"human"}), "prompt"),
            queued("q1", json!("重複"), json!({"kind":"human"}), "prompt"),
            queued("q2", json!("peer"), json!({"kind":"peer"}), "prompt"),
            queued("q3", json!("origin なし"), Value::Null, "prompt"),
            queued("q4", json!("<command-name>/x</command-name>"), json!({"kind":"human"}), "prompt"),
            queued("q5", json!("bash"), json!({"kind":"human"}), "bash"),
            queued("q6", json!("<task-notification>x</task-notification>"), json!({"kind":"task-notification"}), "prompt"),
            queued("q7", json!("依頼A"), json!({"kind":"human"}), "prompt"),
            ask("toolu_ok"),
            answered("toolu_ok", good_result(), false),
            answered("toolu_ok", good_result(), false),
            ask("toolu_err"),
            answered("toolu_err", good_result(), true),
            ask("toolu_str"),
            answered("toolu_str", json!("Your questions have been answered"), false),
            answered("toolu_unknown", good_result(), false),
            row(json!({"type":"user","sessionId":S,"isSidechain":true,"uuid":"c9","isCompactSummary":true,"message":{"role":"user","content":"sub"}})),
            row(json!({"type":"user","sessionId":"other","uuid":"c8","isCompactSummary":true,"message":{"role":"user","content":"他"}})),
        ]
        .concat()
    }

    #[test]
    fn events_leave_turns_identical_and_follow_extraction_rules() {
        let base = fixture();
        let mixed = [event_rows(), base.clone()].concat();
        let interleaved = format!(
            "{}{}{}",
            &base[..base.find('\n').unwrap() + 1],
            event_rows(),
            &base[base.find('\n').unwrap() + 1..]
        );
        let plain = parse_str(&base).0;
        for input in [&mixed, &interleaved] {
            let (t, _) = with_events(input);
            assert_eq!(t.turns.len(), plain.turns.len());
            for (a, b) in t.turns.iter().zip(&plain.turns) {
                assert_eq!(
                    (&a.user, &a.assistant, &a.source_timestamp),
                    (&b.user, &b.assistant, &b.source_timestamp)
                );
            }
            assert_eq!(parse_str(input).0.turns.len(), plain.turns.len());
        }
        let (_, events) = with_events(&mixed);
        let keys: Vec<_> = events
            .iter()
            .map(|e| (e.kind.as_str(), e.key.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("compaction", "c1"),
                ("message", "q1"),
                ("answer", "toolu_ok")
            ]
        );
        assert!(events[0].text.starts_with("継続の要約 "));
        assert!(!events[0].text.contains("sk-secret-value"));
        assert_eq!(events[1].text, "途中の発言\n二行目");
        assert_eq!(events[2].qa[0].labels, ["A", "B"]);
        assert_eq!(events[2].qa[0].answer, "A");
    }

    fn human_at(text: &str, ts: &str) -> String {
        row(
            json!({"type":"user","sessionId":S,"timestamp":ts,"origin":{"kind":"human"},"message":{"role":"user","content":text}}),
        )
    }
    fn queued_at(uuid: &str, text: &str, ts: &str) -> String {
        row(
            json!({"type":"attachment","sessionId":S,"uuid":uuid,"timestamp":ts,"attachment":{"type":"queued_command","commandMode":"prompt","prompt":text,"origin":{"kind":"human"}}}),
        )
    }

    #[test]
    fn identical_mid_turn_message_is_dropped_only_near_the_human_row() {
        let input = [
            human_at("続けて", "2026-11-12T00:00:00Z"),
            queued_at("near", "続けて", "2026-11-12T00:00:30Z"),
            queued_at("before", "続けて", "2026-11-11T23:59:30+00:00"),
            queued_at("far", "続けて", "2026-11-12T00:05:00Z"),
            queued_at("unreadable", "続けて", "???"),
            queued_at("other", "別の依頼", "2026-11-12T00:00:10Z"),
        ]
        .concat();
        let (_, events) = with_events(&input);
        let keys: Vec<_> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["far", "unreadable", "other"]);

        let adjacent = [
            human_at("続けて", "2026-11-12T09:00:00+09:00"),
            queued_at("same", "続けて", "2026-11-12T00:00:00.500Z"),
        ]
        .concat();
        assert!(with_events(&adjacent).1.is_empty());
    }

    #[test]
    fn epoch_secs_handles_offsets_and_rejects_garbage() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            epoch_secs("2026-11-12T09:00:00+09:00"),
            epoch_secs("2026-11-12T00:00:00Z")
        );
        assert_eq!(
            epoch_secs("2026-11-12T00:00:00.250Z"),
            epoch_secs("2026-11-12T00:00:00Z")
        );
        assert_eq!(
            epoch_secs("2024-03-01T00:00:00Z").unwrap()
                - epoch_secs("2024-02-28T00:00:00Z").unwrap(),
            2 * 86_400
        );
        assert_eq!(epoch_secs(""), None);
        assert_eq!(epoch_secs("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn meta_requires_a_matching_main_session_row() {
        let dir =
            std::env::temp_dir().join(format!("remnifold-claude-meta-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        fs::write(&path, fixture()).unwrap();
        let meta = read_meta(&path, S).unwrap().unwrap();
        assert_eq!(meta["cwd"], "D:\\work");
        assert_eq!(meta["format"], "claude");
        assert!(read_meta(&path, "missing").unwrap().is_none());
        assert!(read_meta(&path, "").unwrap().is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
