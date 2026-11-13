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
use anyhow::Result;
use serde_json::{Value, json};
use std::{
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
    // スラッシュコマンドとローカルコマンド出力は依頼本文として残さない。
    if trimmed.starts_with("<command-name>")
        || trimmed.starts_with("<command-message>")
        || trimmed.starts_with("<local-command-")
        || trimmed.starts_with("Caveat: The messages below were generated")
    {
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
    Ok((Transcript { turns, redactions }, title))
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
