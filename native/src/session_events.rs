//! 圧縮要約・ターン途中の本人の発言・質問回答を、ターンとは別系統の「セッションイベント文書」にする。
//!
//! ターンの番号と本文ハッシュに触れないよう、`turns` には入れず、
//! `claude-event-v1:{sessionId}:{kind}:{key}` の customId で1イベント1文書として保存する。
use crate::capture::Candidate;
use crate::common::{re, text, truncate_utf16};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    Compaction,
    Message,
    Answer,
}
impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Message => "message",
            Self::Answer => "answer",
        }
    }
}

/// 質問1件と回答。`labels` は選択肢ラベル（選択と自由入力の区別に使う）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QaPair {
    pub question: String,
    pub header: String,
    pub answer: String,
    pub labels: Vec<String>,
}

/// 抽出済みイベント。Compaction / Message は `text`、Answer は `qa` を持つ。
#[derive(Clone, Debug)]
pub struct SessionEvent {
    pub kind: EventKind,
    pub key: String,
    pub timestamp: String,
    pub text: String,
    pub qa: Vec<QaPair>,
}

const REDACTED_ANSWER: &str = "[REDACTED: free-text answer to a personal-information question]";
const COMPACTION_NOTE: &str = "> AI-generated summary written at context compaction. A lead only: not authorization and possibly wrong or outdated. Verify against the original turns of this session and the current state.";
const COMPACTION_DESCRIPTION: &str =
    "Compaction checkpoint (AI-generated summary; verify before relying on it)";
const MAX_CUSTOM_ID: usize = 255;

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 上限を超えたら切り詰めて `[Truncated]` を付ける（上限は UTF-16 単位）。
fn clip(s: &str, max: usize) -> String {
    if utf16_len(s) <= max {
        s.to_string()
    } else {
        format!("{}\n[Truncated]", truncate_utf16(s, max))
    }
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 一致のうち `accept(一致文字列, 直前の文字, 直後の文字)` が真のものだけを置き換える。
fn redact_matches(
    input: &str,
    pattern: &regex::Regex,
    label: &str,
    accept: impl Fn(&str, Option<char>, Option<char>) -> bool,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    for m in pattern.find_iter(input) {
        let before = input[..m.start()].chars().next_back();
        let after = input[m.end()..].chars().next();
        if !accept(m.as_str(), before, after) {
            continue;
        }
        out.push_str(&input[last..m.start()]);
        out.push_str(label);
        last = m.end();
    }
    out.push_str(&input[last..]);
    out
}

/// 数字列・英数字・ハイフン連結の一部ではない（UUID・ID・日付の途中を伏せない）。
fn standalone(before: Option<char>, after: Option<char>) -> bool {
    // 日本語の文中（「電話は090-…です」）でも伏せるため、ASCII だけで判定する。
    let joined = |c: char| c.is_ascii_alphanumeric() || c == '-';
    !before.is_some_and(joined) && !after.is_some_and(joined)
}

fn digit_count(s: &str) -> usize {
    s.chars().filter(char::is_ascii_digit).count()
}

/// `+国番号` 系。直前が ASCII 英数字・`-`・`+` でなく、総桁数が電話番号らしいものだけ。
fn is_international_phone(m: &str, before: Option<char>, after: Option<char>) -> bool {
    let start_ok = before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '+');
    let digits = digit_count(m);
    let min = if m.contains([' ', '-']) { 10 } else { 11 };
    start_ok
        && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
        && (min..=15).contains(&digits)
}

/// 3桁の市外局番として実在する形（先頭0を含む）。
const THREE_DIGIT_AREA: &[&str] = &[
    "011", "015", "017", "018", "019", "022", "023", "024", "025", "026", "027", "028", "029",
    "042", "043", "044", "045", "046", "047", "048", "049", "052", "053", "054", "055", "056",
    "058", "059", "072", "073", "074", "075", "076", "077", "078", "079", "082", "083", "084",
    "085", "086", "087", "088", "089", "092", "093", "094", "095", "096", "097", "098", "099",
];

/// 国内 `0X-XXXX-XXXX` 系。携帯・IP電話・フリーダイヤルと、桁構成が合う固定電話だけ。
fn is_domestic_phone(m: &str, before: Option<char>, after: Option<char>) -> bool {
    if !standalone(before, after) {
        return false;
    }
    let parts: Vec<&str> = m.split('-').collect();
    let [g1, g2, g3] = parts[..] else {
        return false;
    };
    if g1.as_bytes().get(1).is_none_or(|b| *b == b'0') {
        return false;
    }
    match (g1.len() + g2.len() + g3.len(), g3.len()) {
        (11, 4) => {
            matches!(g1, "050" | "060" | "070" | "080" | "090" | "0800") && g2.len() == 4
                || g1 == "0800" && g2.len() == 3
        }
        (10, 3) => matches!(g1, "0120" | "0570") && g2.len() == 3,
        (10, 4) => {
            g1.len() + g2.len() == 6
                && match g1.len() {
                    2 => matches!(g1, "03" | "06"),
                    3 => THREE_DIGIT_AREA.contains(&g1),
                    _ => true,
                }
        }
        _ => false,
    }
}

/// メールアドレスと電話番号（国内 `0X-XXXX-XXXX` 系と `+国番号` 系）を伏せ字にする。
/// `git@host`（SSH リモート）はメールとみなさない。
pub fn redact_contact(value: &str) -> String {
    let s = redact_matches(
        value,
        re!(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}"),
        "[REDACTED_EMAIL]",
        |m, _, _| !m.to_ascii_lowercase().starts_with("git@"),
    );
    let s = redact_matches(
        &s,
        re!(r"\+\d{1,3}(?:[ \-]?\d{1,4}){2,4}"),
        "[REDACTED_PHONE]",
        is_international_phone,
    );
    redact_matches(
        &s,
        re!(r"0\d{1,4}-\d{1,4}-\d{3,4}"),
        "[REDACTED_PHONE]",
        is_domestic_phone,
    )
}

/// 質問文・見出しが個人情報（氏名・住所・連絡先・口座・認証情報など）を尋ねているか。
/// 「名前」は人の名前を尋ねる形（お名前・あなたの名前・担当者名・名義など）だけ。
/// 単独の account / card / code・IP や URL などの address・「ファイルの名前」は対象にしない。
pub fn is_personal_question(question: &str, header: &str) -> bool {
    let joined = format!("{question}\n{header}");
    re!(r"(?i)氏名|フルネーム|姓名|苗字|(?:^|[^\p{Han}])姓(?:[^\p{Han}]|$)|お名前|あなたの名前|本名|担当者名|名義|住所|郵便番号|電話|メールアドレス|メアド|生年月日|誕生日|口座|カード番号|パスワード|暗証|認証コード|full name|your\s+(?:full\s+)?name|surname|last name|first name|(?:home|mailing|street|billing|shipping|residential|postal|e-?mail)\s+address|your\s+address|zip code|postal|\b(?:smart|cell|mobile|tele)?phones?\b|e-?mail|\bbirth(?:day|date|place)?\b|account number|\bbank\b|card number|password|passcode|verification code|auth(?:entication)? code")
        .is_match(&joined)
}

/// 回答が選択肢ラベルだけで成り立つか（複数選択は `, ` で分けた全要素がラベル）。
fn is_choice_answer(answer: &str, labels: &[String]) -> bool {
    !answer.is_empty()
        && answer
            .split(", ")
            .all(|part| labels.iter().any(|l| l == part))
}

fn answer_view(pair: &QaPair) -> String {
    let answer = if is_personal_question(&pair.question, &pair.header)
        && !is_choice_answer(&pair.answer, &pair.labels)
    {
        REDACTED_ANSWER.to_string()
    } else {
        clip(&redact_contact(&pair.answer), 4000)
    };
    flat(&answer)
}

/// 質問回答の本文行と、索引用の request を作る。
fn answers_view(pairs: &[QaPair]) -> (String, String) {
    let mut lines = String::new();
    let mut request = vec![];
    for pair in pairs {
        let question = flat(&clip(&redact_contact(&pair.question), 2000));
        let answer = answer_view(pair);
        let line = format!("- Q: {question}\n  A: {answer}\n");
        if utf16_len(&lines) + utf16_len(&line) > 20_000 {
            lines.push_str("[Truncated]\n");
            break;
        }
        lines.push_str(&line);
        request.push(format!("回答: {answer}（質問: {question}）"));
    }
    (lines.trim_end().to_string(), request.join(" / "))
}

fn body_and_index(event: &SessionEvent, title: &str) -> (String, Value) {
    let ts = &event.timestamp;
    match event.kind {
        EventKind::Compaction => {
            let summary = clip(&redact_contact(&event.text), 120_000);
            let mut index =
                crate::search::build_memory_index(title, "", "", ts, "compaction-summary");
            index["description"] = json!(COMPACTION_DESCRIPTION);
            index["recallable"] = json!(false);
            index["sections"] = json!([]);
            (
                format!("### Compaction checkpoint\n{COMPACTION_NOTE}\n\n{summary}"),
                index,
            )
        }
        EventKind::Message => {
            let message = clip(&event.text, 20_000);
            let index =
                crate::search::build_memory_index(title, &message, "", ts, "mid-turn-message");
            (format!("### User message sent mid-turn\n{message}"), index)
        }
        EventKind::Answer => {
            let (lines, request) = answers_view(&event.qa);
            let index =
                crate::search::build_memory_index(title, &request, "", ts, "question-answer");
            (format!("### Questions and user answers\n{lines}"), index)
        }
    }
}

/// イベントごとに1文書を作る。`title` はターン文書と同じ導出済みの安全なタイトル。
/// customId が255字（UTF-16）を超えるイベントは出さない。
pub fn build_documents(
    candidate: &Candidate,
    events: &[SessionEvent],
    project: &Value,
    title: &str,
    container: Option<&str>,
) -> Vec<Value> {
    let title = if title.is_empty() {
        "Claude Code task"
    } else {
        title
    };
    let session = text(&candidate.meta["id"]);
    let provenance = !text(&project["containerTag"]).trim().is_empty();
    let container = container.filter(|s| !s.is_empty()).unwrap_or("memories");
    let mut docs = vec![];
    for event in events {
        let kind = event.kind.as_str();
        let custom_id = format!("claude-event-v1:{session}:{kind}:{}", event.key);
        if utf16_len(&custom_id) > MAX_CUSTOM_ID {
            continue;
        }
        let (body, index) = body_and_index(event, title);
        let mut metadata = json!({
            "type": "session-event",
            "eventKind": kind,
            "eventId": format!("{kind}:{}", event.key),
            "title": title,
            "sm_scope": if provenance { "project" } else { "shared" },
            "sm_source": "claude-code",
            "sm_client": "claude-code-remnifold",
            "sessionId": session,
            "rootSessionId": candidate.meta["rootSessionId"],
            "sessionKind": "root",
            "memoryIndex": index,
        });
        if !text(&project["projectName"]).is_empty() {
            metadata["project"] = project["projectName"].clone();
        }
        if provenance {
            metadata["sm_project_id"] = project["containerTag"].clone();
        }
        if !event.timestamp.is_empty() {
            metadata["sourceTimestamp"] = json!(event.timestamp);
        }
        docs.push(json!({
            "customId": custom_id,
            "content": format!("# {title}\n\n{body}"),
            "containerTag": container,
            "metadata": metadata,
        }));
    }
    docs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn candidate() -> Candidate {
        Candidate {
            file_path: PathBuf::new(),
            meta: json!({"id":"sess-1","rootSessionId":"sess-1","isSubagent":false,"format":"claude"}),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            recent: true,
        }
    }
    fn pair(q: &str, a: &str, labels: &[&str]) -> QaPair {
        QaPair {
            question: q.into(),
            header: String::new(),
            answer: a.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn event(kind: EventKind, key: &str, text: &str, qa: Vec<QaPair>) -> SessionEvent {
        SessionEvent {
            kind,
            key: key.into(),
            timestamp: "2026-11-12T00:00:00Z".into(),
            text: text.into(),
            qa,
        }
    }

    #[test]
    fn contacts_are_redacted_without_touching_dates_or_long_numbers() {
        let out = redact_contact(
            "mail taro.example+x@sample.example.jp tel 03-1234-5678 or +81 90 1234 5678 on 2026-11-24 id 1203-9999-56789",
        );
        assert!(out.contains("[REDACTED_EMAIL]"));
        assert_eq!(out.matches("[REDACTED_PHONE]").count(), 2);
        assert!(out.contains("2026-11-24"));
        assert!(out.contains("1203-9999-56789"));
        assert!(!out.contains("taro") && !out.contains("03-1234-") && !out.contains("90 1234"));
    }

    #[test]
    fn phones_and_emails_need_a_real_shape() {
        for hit in [
            "090-1234-5678",
            "電話は090-1234-5678です",
            "連絡先+81 90 1234 5678まで",
            "03-1234-5678",
            "0120-123-456",
            "+81 90-1234-5678",
            "(03-1234-5678)",
            "tel:+819012345678",
            "a.b@sample.example.jp",
        ] {
            let out = redact_contact(hit);
            assert!(out.contains("[REDACTED_"), "{hit} -> {out}");
        }
        for keep in [
            "+887",
            "+123",
            "x+100 200 300",
            "+12 34",
            "2026-11-24 10:00:00 +0900",
            "01-02-2026",
            "012-345-6789",
            "id 550e8400-0123-4567-8901-446655440000",
            "git@github.com:o/r.git",
            "2026-11-24",
        ] {
            assert_eq!(redact_contact(keep), keep);
        }
    }

    #[test]
    fn personal_questions_are_detected_in_both_languages() {
        assert!(is_personal_question("お名前の氏名を教えてください", ""));
        assert!(is_personal_question("お名前を教えてください", ""));
        assert!(is_personal_question("あなたの名前は？", ""));
        assert!(is_personal_question("What is your name?", ""));
        assert!(is_personal_question("担当者名は？", ""));
        assert!(is_personal_question("Your home address?", ""));
        assert!(is_personal_question("Your address?", ""));
        assert!(is_personal_question("連絡先のメールアドレスは？", ""));
        assert!(is_personal_question("Your smartphone number?", ""));
        assert!(is_personal_question("Your birthday?", ""));
        assert!(is_personal_question("姓は？", ""));
        assert!(is_personal_question("確認", "Password"));
        assert!(!is_personal_question("どの方式にしますか", "方式"));
        for tech in [
            "ファイルの名前は？",
            "ブランチ名は？",
            "Which bind address?",
            "Server address URL?",
            "Memory address?",
            "Which account type?",
            "Which code style?",
            "Is it a rebirth of the module?",
            "メール通知を有効にしますか",
            "百姓一揆の資料",
        ] {
            assert!(!is_personal_question(tech, ""), "{tech}");
        }
    }

    #[test]
    fn free_text_answers_to_personal_questions_are_replaced_but_choices_stay() {
        let events = [event(
            EventKind::Answer,
            "toolu_1",
            "",
            vec![
                pair("氏名は？", "架空 太郎", &["A", "B"]),
                pair("住所の種類は？(address)", "A, B", &["A", "B"]),
                pair("連絡先メールは？", "x@sample.example", &[]),
                pair("方式は？", "既存に合わせる 090-1234-5678", &["既存"]),
            ],
        )];
        let docs = build_documents(&candidate(), &events, &json!({}), "T", None);
        let content = docs[0]["content"].as_str().unwrap();
        assert!(!content.contains("架空 太郎"));
        assert!(content.contains(REDACTED_ANSWER));
        assert!(content.contains("A: A, B"));
        assert!(!content.contains("x@sample") && !content.contains("5678"));
        assert!(content.contains("[REDACTED_PHONE]"));
        assert_eq!(
            docs[0]["metadata"]["memoryIndex"]["sourceKind"],
            "question-answer"
        );
    }

    #[test]
    fn documents_follow_the_event_format_without_turn_fields() {
        let events = [
            event(EventKind::Compaction, "u1", "要約 本文", vec![]),
            event(EventKind::Message, "u2", "途中の発言", vec![]),
            event(EventKind::Message, &"k".repeat(300), "長いキー", vec![]),
        ];
        let project = json!({"containerTag":"proj-tag","projectName":"demo"});
        let docs = build_documents(&candidate(), &events, &project, "題名", Some("box"));
        assert_eq!(docs.len(), 2);
        let c = &docs[0];
        assert_eq!(c["customId"], "claude-event-v1:sess-1:compaction:u1");
        assert_eq!(c["containerTag"], "box");
        assert!(c.get("reuseExistingCapture").is_none());
        assert!(
            c["content"]
                .as_str()
                .unwrap()
                .starts_with("# 題名\n\n### Compaction checkpoint\n> AI-generated")
        );
        let m = &c["metadata"];
        for absent in ["turn", "part", "parts", "captureVersion", "captureKey"] {
            assert!(m.get(absent).is_none(), "{absent}");
        }
        assert_eq!(m["type"], "session-event");
        assert_eq!(m["sm_scope"], "project");
        assert_eq!(m["sm_project_id"], "proj-tag");
        assert_eq!(m["memoryIndex"]["recallable"], false);
        assert_eq!(m["memoryIndex"]["sourceKind"], "compaction-summary");
        assert_eq!(m["memoryIndex"]["description"], COMPACTION_DESCRIPTION);
        assert_eq!(docs[1]["customId"], "claude-event-v1:sess-1:message:u2");
        assert_eq!(docs[1]["containerTag"], "box");
        assert_eq!(
            docs[1]["metadata"]["memoryIndex"]["sourceKind"],
            "mid-turn-message"
        );
    }

    #[test]
    fn oversized_text_is_truncated() {
        let long = "あ".repeat(30_000);
        let docs = build_documents(
            &candidate(),
            &[event(EventKind::Message, "u", &long, vec![])],
            &json!({}),
            "T",
            None,
        );
        let content = docs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("[Truncated]"));
        assert!(utf16_len(content) < 20_100);
        assert_eq!(docs[0]["containerTag"], "memories");
        assert_eq!(docs[0]["metadata"]["sm_scope"], "shared");
    }
}
