//! Toolport ゲートウェイ経由で、この製品の MCP サーバーが既に提供されているかの判定。
//!
//! 3リポジトリでバイト単位で同一。正本は Edictora。編集は正本で行い複製する（複製先では局所的に編集しない）:
//! - `D:\Dev\Edictora\crates\edictora\src\install\toolport.rs`（正本）
//! - `D:\Dev\Remnifold\native\src\toolport.rs`
//! - `D:\Dev\Plumivox\src\toolport.rs`
//!
//! 製品の差は、呼び出し側が渡す [`Product`] だけ。依存は `std`・`serde_json`・`regex` だけで、`crate::` の
//! import も製品固有の補助関数も持たない。環境（ホーム・`%APPDATA%`・環境変数・ファイルの存在確認と読み取り）は
//! [`Env`] で注入するので、テストはバイト列とパスだけで完結する（一時フォルダーを作らない）。
//!
//! 判定の結果（[`Detection`]）:
//! - `Served` — クライアントが Toolport ゲートウェイにつながっていて、Toolport がこの製品のサーバーを有効にして提供している。
//! - `NotConnected` — クライアントの設定に、実在する Toolport ゲートウェイが無い（実行ファイルが消えた場合を含む）。
//! - `NotServed` — ゲートウェイにはつながっているが、この製品のサーバーは提供されていない（Toolport 側で無効化・削除された）。
//! - `Unknown(理由)` — 読めない・形が想定外・registry が見つからないなど、確かめられない。
//!
//! 判定ルール（番号はテストの名前の接頭辞 `r1_`〜`r12_` と対応する）:
//! - R1 Windows 以外は何も読まず `Unknown("non-windows")`。R2 ホームを決められなければ `Unknown`。
//! - R3 ファイルは `fs::read` 相当（サイズ上限なし・リンクをたどる）。読めなければ `Unknown`、無ければ各段の「無い」扱い。
//! - R4 Claude Code は `<CLAUDE_CONFIG_DIR>\.claude.json`（絶対パスのときだけ。無ければ `<ホーム>\.claude.json`）の最上位
//!   `mcpServers`。ゲートウェイ候補でない項目は形を問わず無視する。
//! - R5 Codex は `<CODEX_HOME>\config.toml`（無ければ `<ホーム>\.codex\config.toml`）の `[mcp_servers.<名前>]` と
//!   `[mcp_servers.<名前>.env]` だけを読む。それ以外の `mcp_servers` 系、これらの節の中のドット付きキー・
//!   インラインテーブル・複数行の文字列/配列は `Unknown`。節の外の複数行文字列は正しく読み飛ばす。
//! - R6 ゲートウェイ候補は `command` のファイル名が `toolport-gateway[-…][.exe]`、または registry の
//!   `clientManagedEntries[<ID>].command` と正規化一致する項目。実在する絶対パスのものだけが有効。
//! - R7 registry の場所（ゲートウェイ env → プロセス env → `%APPDATA%\Toolport`）。`CONDUIT_*` があれば `Unknown`。
//! - R8 registry は `version` が整数 3 で、`servers`・`profiles` が配列。R9 `clientScopes` は先に確かめる。
//! - R10 使われるプロファイルの集合。R11 サーバーの一致（stdio・`command` のファイル名・`args`・`enabled`・
//!   プロファイル）。R12 サーバーの `env` は読まず、結果にも含めない。
//!
//! `quarantine.json` は見ない。Toolport 側の設定には書かない。

// 製品によって使わない公開項目がある（`mod toolport;` の非公開モジュールとして取り込む製品でも、
// dead_code の警告でビルド・lint が落ちないようにする）
#![allow(dead_code)]

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// 製品ごとの差（呼び出し側が渡す）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Product {
    /// 実行ファイルの名前（拡張子なし、小文字）。例 `edictora`
    pub binary_stem: &'static str,
    /// MCP サーバーとして起動する引数。例 `mcp`、`--stdio`
    pub mode_arg: &'static str,
}

/// Toolport 側のクライアント ID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientId {
    ClaudeCode,
    Codex,
}

impl ClientId {
    /// registry とログで使う ID
    pub fn as_str(self) -> &'static str {
        match self {
            ClientId::ClaudeCode => "claude-code",
            ClientId::Codex => "codex",
        }
    }
}

/// 判定の結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// Toolport が提供している（サーバーの id と command だけ。env は含めない）
    Served { server_id: String, command: String },
    /// クライアントが Toolport ゲートウェイにつながっていない
    NotConnected,
    /// つながっているが、この製品のサーバーは提供されていない
    NotServed,
    /// 確かめられない（理由。秘密値は含めない）
    Unknown(String),
}

impl Detection {
    /// ログ用の種類名（`served` / `not-connected` / `not-served` / `unknown`）
    pub fn kind(&self) -> &'static str {
        match self {
            Detection::Served { .. } => "served",
            Detection::NotConnected => "not-connected",
            Detection::NotServed => "not-served",
            Detection::Unknown(_) => "unknown",
        }
    }
}

/// ファイルの存在確認と読み取り（パスは Windows 形式の文字列）。テストは注入した内容を返す
pub trait Files {
    /// ファイルとして存在するか
    fn exists(&self, path: &str) -> bool;
    /// 全体を読む（サイズ上限なし、シンボリックリンクはたどる）
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;
}

/// 実際のファイルシステム
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFiles;

impl Files for RealFiles {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

/// 判定に使う環境。テストは任意の値を渡す
pub struct Env {
    /// Windows か（偽なら何も読まず `Unknown("non-windows")`）
    pub windows: bool,
    /// ホームフォルダー（`USERPROFILE`、無ければ `HOME`）
    pub home: Option<String>,
    /// `%APPDATA%`
    pub appdata: Option<String>,
    /// 判定に関わるプロセスの環境変数（`CLAUDE_CONFIG_DIR`・`CODEX_HOME`・`TOOLPORT_*`・`CONDUIT_*`。
    /// 設定されているものだけ。空文字も含む）
    pub vars: BTreeMap<String, String>,
    /// ファイルの存在確認と読み取り
    pub files: Box<dyn Files>,
}

/// [`Env::vars`] に取り込むプロセスの環境変数
const PROCESS_VARS: [&str; 8] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "TOOLPORT_REGISTRY",
    "TOOLPORT_DATA_DIR",
    "TOOLPORT_PROFILE",
    "CONDUIT_REGISTRY",
    "CONDUIT_DATA_DIR",
    "CONDUIT_PROFILE",
];

/// 引き継がれていたら registry の場所を決められなくする環境変数（Toolport の前身 Conduit のもの）
const CONDUIT_VARS: [&str; 3] = ["CONDUIT_REGISTRY", "CONDUIT_DATA_DIR", "CONDUIT_PROFILE"];

impl Env {
    /// 実際のプロセスの環境から作る（本番の呼び出し側だけが使う）
    pub fn process() -> Env {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Env {
            windows: cfg!(windows),
            home: var("USERPROFILE").or_else(|| var("HOME")),
            appdata: var("APPDATA"),
            vars: PROCESS_VARS
                .iter()
                .filter_map(|name| Some(((*name).to_string(), std::env::var(name).ok()?)))
                .collect(),
            files: Box::new(RealFiles),
        }
    }
}

// ---------------------------------------------------------------------------
// パス（Windows の形式の文字列として扱う。実行する OS に依存しない）
// ---------------------------------------------------------------------------

fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    drive || path.starts_with("\\\\")
}

fn join(dir: &str, name: &str) -> String {
    format!("{}\\{name}", dir.trim_end_matches(['\\', '/']))
}

/// パスの最後の要素（`/` と `\` の両方を区切りとみなす）
fn file_name_of(command: &str) -> &str {
    command.rsplit(['/', '\\']).next().unwrap_or(command)
}

fn gateway_name_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^toolport-gateway(-[0-9A-Za-z.+_-]+)?(\.exe)?$").expect("static pattern")
    })
}

/// 比較用（前後の空白と `"` を除き、`/` を `\` に、小文字にする）
fn normalize_command(command: &str) -> String {
    command
        .trim_matches(|c: char| c.is_whitespace() || c == '"')
        .replace('/', "\\")
        .to_lowercase()
}

fn existing_absolute_file(env: &Env, command: &str) -> bool {
    is_absolute(command) && env.files.exists(command)
}

/// 空文字は「無い」扱い
fn non_empty<'a>(map: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    map.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

fn json_from(bytes: &[u8]) -> Option<Value> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice(bytes).ok()
}

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

/// 判定する（設定ファイルと registry を `env` 経由で読む）。確かめられなければ `Unknown`
pub fn detect(product: &Product, client: ClientId, env: &Env) -> Detection {
    detect_inner(product, client, env).unwrap_or_else(Detection::Unknown)
}

/// registry の中でこの製品を指すサーバーの、Toolport のツール名の前置き（`id` の `[A-Za-z0-9_]` 以外を
/// `_` にしたもの）。判定は名前（`command` のファイル名）と `args` だけ。`enabled`・プロファイル・
/// 実行ファイルの実在は見ない。registry はプロセスの環境と既定の場所だけで解決し、読めなければ空
pub fn product_server_ids(product: &Product, env: &Env) -> Vec<String> {
    if !env.windows {
        return Vec::new();
    }
    let Ok(path) = resolve_registry_path(&BTreeMap::new(), env) else {
        return Vec::new();
    };
    let Ok(registry) = load_registry(env, &path) else {
        return Vec::new();
    };
    registry["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|server| name_args_match(product, server))
        .map(|(id, _)| tool_prefix(id))
        .collect()
}

fn tool_prefix(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn detect_inner(product: &Product, client: ClientId, env: &Env) -> Result<Detection, String> {
    if !env.windows {
        return Err("non-windows".to_string());
    }
    let home = env
        .home
        .as_deref()
        .filter(|home| is_absolute(home))
        .ok_or_else(|| "ホームフォルダーを決められません".to_string())?;
    let path = client_config_path(client, home, env);
    let bytes = match env.files.read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Detection::NotConnected),
        Err(e) => return Err(format!("クライアントの設定を読めません（{}）", e.kind())),
    };
    let managed: OnceCell<Option<String>> = OnceCell::new();
    let is_candidate = |command: &str| {
        gateway_name_regex().is_match(file_name_of(command))
            || managed
                .get_or_init(|| managed_command(client, env))
                .as_deref()
                .is_some_and(|managed| normalize_command(command) == managed)
    };
    let candidates = match client {
        ClientId::ClaudeCode => claude_candidates(&bytes, &is_candidate)?,
        ClientId::Codex => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| "Codex の設定が UTF-8 ではありません".to_string())?;
            codex_candidates(text, &is_candidate)?
        }
    };
    let Some(gateway) = effective_gateway(candidates, env)? else {
        return Ok(Detection::NotConnected);
    };
    let registry_path = resolve_registry_path(&gateway.env, env)?;
    let registry = load_registry(env, &registry_path)?;
    evaluate(product, client, &registry, &gateway.env, env)
}

fn client_config_path(client: ClientId, home: &str, env: &Env) -> String {
    let absolute_var = |name: &str| non_empty(&env.vars, name).filter(|v| is_absolute(v));
    match client {
        ClientId::ClaudeCode => join(
            absolute_var("CLAUDE_CONFIG_DIR").unwrap_or(home),
            ".claude.json",
        ),
        ClientId::Codex => {
            let dir = absolute_var("CODEX_HOME")
                .map(str::to_owned)
                .unwrap_or_else(|| join(home, ".codex"));
            join(&dir, "config.toml")
        }
    }
}

/// registry（プロセスの環境と既定の場所）の `clientManagedEntries[<ID>].command`（正規化済み）
fn managed_command(client: ClientId, env: &Env) -> Option<String> {
    let path = resolve_registry_path(&BTreeMap::new(), env).ok()?;
    let registry = json_from(&env.files.read(&path).ok()?)?;
    let command = registry
        .get("clientManagedEntries")?
        .get(client.as_str())?
        .get("command")?
        .as_str()?;
    Some(normalize_command(command))
}

/// クライアント設定のゲートウェイ候補（env は呼び出し側が読む registry の場所の解決にだけ使う）
struct Candidate {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

/// 候補のうち、実在する絶対パスのものを有効な候補とする（R6）。無ければ None
fn effective_gateway(candidates: Vec<Candidate>, env: &Env) -> Result<Option<Candidate>, String> {
    let mut valid: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| existing_absolute_file(env, &c.command))
        .collect();
    let Some(first) = valid.first() else {
        return Ok(None);
    };
    if valid.iter().any(|c| !c.args.is_empty()) {
        return Err("ゲートウェイに引数が指定されています".to_string());
    }
    if valid.iter().any(|c| c.env != first.env) {
        return Err("複数のゲートウェイ項目で env が食い違っています".to_string());
    }
    Ok(Some(valid.swap_remove(0)))
}

// ---------------------------------------------------------------------------
// Claude Code（.claude.json）
// ---------------------------------------------------------------------------

fn claude_candidates(
    bytes: &[u8],
    is_candidate: &dyn Fn(&str) -> bool,
) -> Result<Vec<Candidate>, String> {
    let value = json_from(bytes)
        .filter(Value::is_object)
        .ok_or_else(|| "Claude Code の設定の JSON が壊れています".to_string())?;
    let Some(servers) = value.get("mcpServers") else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| "mcpServers がオブジェクトではありません".to_string())?;
    let mut candidates = Vec::new();
    for entry in servers.values() {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(command) = entry.get("command").and_then(Value::as_str) else {
            continue;
        };
        if !is_candidate(command) {
            continue;
        }
        match entry.get("type") {
            None => {}
            Some(Value::String(kind)) if kind == "stdio" => {}
            Some(Value::String(_)) => continue,
            Some(_) => return Err("ゲートウェイ項目の type が文字列ではありません".to_string()),
        }
        let args = match entry.get("args") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    "ゲートウェイ項目の args が文字列の配列ではありません".to_string()
                })?,
            Some(_) => {
                return Err("ゲートウェイ項目の args が文字列の配列ではありません".to_string());
            }
        };
        let env = match entry.get("env") {
            None => BTreeMap::new(),
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect::<Option<_>>()
                .ok_or_else(|| "ゲートウェイ項目の env が文字列の組ではありません".to_string())?,
            Some(_) => return Err("ゲートウェイ項目の env が文字列の組ではありません".to_string()),
        };
        candidates.push(Candidate {
            command: command.to_owned(),
            args,
            env,
        });
    }
    Ok(candidates)
}

// ---------------------------------------------------------------------------
// Codex（config.toml。必要な形だけを読む。複数行の文字列・配列も位置を正しく進める）
// ---------------------------------------------------------------------------

const UNSUPPORTED_TOML: &str = "Codex の設定の mcp_servers に、対応していない書き方があります";

/// TOML の値（必要な形だけ）
#[derive(Debug, PartialEq)]
enum Val {
    Str(String),
    Bool(bool),
    /// 1 行の文字列だけの配列
    Strings(Vec<String>),
    /// それ以外（数値・入れ子の配列・複数行の文字列など）
    Other,
}

enum Section {
    Root,
    Other,
    Server(String),
    ServerEnv(String),
}

#[derive(Default)]
struct RawServer {
    command: Option<String>,
    args: Option<Val>,
    enabled: Option<bool>,
    has_url: bool,
    env: BTreeMap<String, Val>,
}

fn codex_candidates(
    text: &str,
    is_candidate: &dyn Fn(&str) -> bool,
) -> Result<Vec<Candidate>, String> {
    let servers = scan_codex(text).map_err(|()| UNSUPPORTED_TOML.to_string())?;
    let mut candidates = Vec::new();
    for raw in servers.into_values() {
        let Some(command) = raw.command else {
            continue;
        };
        if raw.has_url || raw.enabled == Some(false) || !is_candidate(&command) {
            continue;
        }
        let args = match raw.args {
            None => Vec::new(),
            Some(Val::Strings(items)) => items,
            Some(_) => {
                return Err("ゲートウェイ項目の args が文字列の配列ではありません".to_string());
            }
        };
        let mut env = BTreeMap::new();
        for (key, value) in raw.env {
            let Val::Str(value) = value else {
                return Err("ゲートウェイ項目の env が文字列の組ではありません".to_string());
            };
            env.insert(key, value);
        }
        candidates.push(Candidate { command, args, env });
    }
    Ok(candidates)
}

fn is_bare_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn skip_inline_space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

/// 基本文字列（先頭の `"` の次から。1 行だけ）を読む。(値, 残り)
fn parse_basic_string(rest: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &rest[i + 1..])),
            '\n' => return None,
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'e' => out.push('\u{1b}'),
                    'u' | 'U' => {
                        let width = if e == 'u' { 4 } else { 8 };
                        let mut code = 0u32;
                        for _ in 0..width {
                            code = code.checked_mul(16)? + chars.next()?.1.to_digit(16)?;
                        }
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            _ => out.push(c),
        }
    }
    None
}

/// リテラル文字列（先頭の `'` の次から。1 行だけ）を読む。(値, 残り)
fn parse_literal_string(rest: &str) -> Option<(String, &str)> {
    let end = rest.find(['\'', '\n'])?;
    rest[end..]
        .starts_with('\'')
        .then(|| (rest[..end].to_owned(), &rest[end + 1..]))
}

fn parse_string(text: &str) -> Option<(String, &str)> {
    if let Some(rest) = text.strip_prefix('"') {
        parse_basic_string(rest)
    } else if let Some(rest) = text.strip_prefix('\'') {
        parse_literal_string(rest)
    } else {
        None
    }
}

/// ドット区切りのキー（裸のキー・引用符つきのキー）を読む。(キーの並び, 残り)
fn parse_key_path(text: &str) -> Option<(Vec<String>, &str)> {
    let mut keys = Vec::new();
    let mut rest = skip_inline_space(text);
    loop {
        let (key, after) = if rest.starts_with(['"', '\'']) {
            parse_string(rest)?
        } else {
            let end = rest
                .find(|c: char| !is_bare_key_char(c))
                .unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            (rest[..end].to_owned(), &rest[end..])
        };
        keys.push(key);
        rest = skip_inline_space(after);
        match rest.strip_prefix('.') {
            Some(next) => rest = skip_inline_space(next),
            None => return Some((keys, rest)),
        }
    }
}

fn only_comment_left(rest: &str) -> bool {
    let rest = rest.trim();
    rest.is_empty() || rest.starts_with('#')
}

/// テーブルの見出し行（`[...]`）を解釈する。`mcp_servers` に関わる対応外の形は Err
fn parse_header(line: &str) -> Result<Section, ()> {
    let array_table = line.starts_with("[[");
    let body = &line[if array_table { 2 } else { 1 }..];
    let parsed = parse_key_path(body).and_then(|(keys, rest)| {
        let rest = rest.strip_prefix(']')?;
        let rest = if array_table {
            rest.strip_prefix(']')?
        } else {
            rest
        };
        only_comment_left(rest).then_some(keys)
    });
    let Some(keys) = parsed else {
        return if line.contains("mcp_servers") {
            Err(())
        } else {
            Ok(Section::Other)
        };
    };
    if keys.first().map(String::as_str) != Some("mcp_servers") {
        return Ok(Section::Other);
    }
    match keys.as_slice() {
        [_, name] if !array_table => Ok(Section::Server(name.clone())),
        [_, name, env] if !array_table && env == "env" => Ok(Section::ServerEnv(name.clone())),
        _ => Err(()),
    }
}

/// 文字列全体を前から読む走査器（複数行の文字列・配列・インラインテーブルを閉じるまで進める）
struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn set_rest(&mut self, rest: &str) {
        self.pos = self.text.len() - rest.len();
    }

    fn skip_inline_space(&mut self) {
        let rest = skip_inline_space(self.rest());
        self.set_rest(rest);
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            let rest = self.rest();
            let end = rest.find('\n').unwrap_or(rest.len());
            self.pos += end;
        }
    }

    /// 空白・改行・コメントを読み飛ばす。改行を越えたら true
    fn skip_blank(&mut self) -> bool {
        let mut crossed = false;
        while let Some(c) = self.peek() {
            if c == '#' {
                self.skip_comment();
            } else if c.is_whitespace() {
                crossed |= c == '\n';
                self.bump();
            } else {
                break;
            }
        }
        crossed
    }

    fn take_line(&mut self) -> &'a str {
        let rest = self.rest();
        let end = rest.find('\n').unwrap_or(rest.len());
        self.pos += end;
        &rest[..end]
    }

    /// 複数行の文字列を閉じるまで読み飛ばす（開きの 3 連続の引用符の後から）
    fn skip_multiline_string(&mut self, quote: char) -> Result<(), ()> {
        let closing = quote.to_string().repeat(3);
        loop {
            let rest = self.rest();
            if rest.starts_with(&closing) {
                self.pos += 3;
                // 閉じの直前に引用符が 1〜2 個続く書き方（`""""`）
                for _ in 0..2 {
                    if self.peek() == Some(quote) {
                        self.bump();
                    }
                }
                return Ok(());
            }
            match self.peek() {
                None => return Err(()),
                Some('\\') if quote == '"' => {
                    self.bump();
                    self.bump();
                }
                Some(_) => self.bump(),
            }
        }
    }

    /// `{` の次からインラインテーブルを閉じるまで読み飛ばす
    fn skip_inline_table(&mut self) -> Result<(), ()> {
        let mut depth = 1usize;
        while depth > 0 {
            let rest = self.rest();
            match self.peek().ok_or(())? {
                '{' => {
                    depth += 1;
                    self.bump();
                }
                '}' => {
                    depth -= 1;
                    self.bump();
                }
                '#' => self.skip_comment(),
                quote @ ('"' | '\'') => {
                    if rest.starts_with(&quote.to_string().repeat(3)) {
                        self.pos += 3;
                        self.skip_multiline_string(quote)?;
                    } else {
                        let (_, after) = parse_string(rest).ok_or(())?;
                        self.set_rest(after);
                    }
                }
                _ => self.bump(),
            }
        }
        Ok(())
    }

    /// 値を読む。(値, 複数行の文字列・複数行の配列・インラインテーブルを含むか)
    fn read_value(&mut self) -> Result<(Val, bool), ()> {
        let rest = self.rest();
        for quote in ['"', '\''] {
            if rest.starts_with(&quote.to_string().repeat(3)) {
                self.pos += 3;
                self.skip_multiline_string(quote)?;
                return Ok((Val::Other, true));
            }
        }
        match self.peek().ok_or(())? {
            '"' | '\'' => {
                let (value, after) = parse_string(rest).ok_or(())?;
                self.set_rest(after);
                Ok((Val::Str(value), false))
            }
            '[' => {
                self.bump();
                self.read_array()
            }
            '{' => {
                self.bump();
                self.skip_inline_table()?;
                Ok((Val::Other, true))
            }
            _ => {
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, '#' | ',' | ']' | '}'))
                    .unwrap_or(rest.len());
                let value = match &rest[..end] {
                    "" => return Err(()),
                    "true" => Val::Bool(true),
                    "false" => Val::Bool(false),
                    _ => Val::Other,
                };
                self.pos += end;
                Ok((value, false))
            }
        }
    }

    /// `[` の次から配列を閉じるまで読む
    fn read_array(&mut self) -> Result<(Val, bool), ()> {
        let mut items = Vec::new();
        let mut only_strings = true;
        let mut complex = false;
        loop {
            complex |= self.skip_blank();
            if self.peek() == Some(']') {
                self.bump();
                let value = if only_strings {
                    Val::Strings(items)
                } else {
                    Val::Other
                };
                return Ok((value, complex));
            }
            let (value, element_complex) = self.read_value()?;
            complex |= element_complex;
            match value {
                Val::Str(s) => items.push(s),
                _ => only_strings = false,
            }
            complex |= self.skip_blank();
            match self.peek() {
                Some(',') => self.bump(),
                Some(']') => {}
                _ => return Err(()),
            }
        }
    }

    fn end_of_statement(&mut self) -> Result<(), ()> {
        self.skip_inline_space();
        self.skip_comment();
        match self.peek() {
            None | Some('\n' | '\r') => Ok(()),
            Some(_) => Err(()),
        }
    }
}

/// `mcp_servers` に関わる節だけを集める。対応していない書き方は Err
fn scan_codex(text: &str) -> Result<BTreeMap<String, RawServer>, ()> {
    let mut scanner = Scanner {
        text: text.trim_start_matches('\u{feff}'),
        pos: 0,
    };
    let mut servers: BTreeMap<String, RawServer> = BTreeMap::new();
    let mut section = Section::Root;
    loop {
        scanner.skip_blank();
        if scanner.peek().is_none() {
            return Ok(servers);
        }
        if scanner.peek() == Some('[') {
            section = parse_header(scanner.take_line())?;
            continue;
        }
        let (keys, after) = parse_key_path(scanner.rest()).ok_or(())?;
        scanner.set_rest(after);
        let Some(after_equals) = scanner.rest().strip_prefix('=') else {
            return Err(());
        };
        scanner.set_rest(after_equals);
        scanner.skip_inline_space();
        let (value, complex) = scanner.read_value()?;
        scanner.end_of_statement()?;
        apply_assignment(&mut servers, &section, &keys, value, complex)?;
    }
}

fn apply_assignment(
    servers: &mut BTreeMap<String, RawServer>,
    section: &Section,
    keys: &[String],
    value: Val,
    complex: bool,
) -> Result<(), ()> {
    let (name, in_env) = match section {
        Section::Root => {
            return if keys.first().map(String::as_str) == Some("mcp_servers") {
                Err(())
            } else {
                Ok(())
            };
        }
        Section::Other => return Ok(()),
        Section::Server(name) => (name, false),
        Section::ServerEnv(name) => (name, true),
    };
    let [key] = keys else {
        return Err(());
    };
    if complex {
        return Err(());
    }
    let server = servers.entry(name.clone()).or_default();
    if in_env {
        server.env.insert(key.clone(), value);
        return Ok(());
    }
    match (key.as_str(), value) {
        ("command", Val::Str(s)) => server.command = Some(s),
        ("enabled", Val::Bool(b)) => server.enabled = Some(b),
        ("url", _) => server.has_url = true,
        ("args", value) => server.args = Some(value),
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

/// registry.json の場所を決める（ゲートウェイ項目の env → プロセスの環境変数 → Windows 既定）。R7
fn resolve_registry_path(
    gateway_env: &BTreeMap<String, String>,
    env: &Env,
) -> Result<String, String> {
    let has_conduit =
        |map: &BTreeMap<String, String>| CONDUIT_VARS.iter().any(|name| map.contains_key(*name));
    if has_conduit(gateway_env) || has_conduit(&env.vars) {
        return Err("Toolport ではなく Conduit の環境変数があります".to_string());
    }
    let located = |value: &str, is_dir: bool| {
        if is_dir {
            join(value, "registry.json")
        } else {
            value.to_owned()
        }
    };
    let candidates = [("TOOLPORT_REGISTRY", false), ("TOOLPORT_DATA_DIR", true)];
    for (key, is_dir) in candidates {
        if let Some(value) = non_empty(gateway_env, key) {
            return if is_absolute(value) {
                Ok(located(value, is_dir))
            } else {
                Err("ゲートウェイの env の Toolport の場所が相対パスです".to_string())
            };
        }
    }
    for (key, is_dir) in candidates {
        if let Some(value) = non_empty(&env.vars, key).filter(|v| is_absolute(v)) {
            return Ok(located(value, is_dir));
        }
    }
    let appdata = env
        .appdata
        .as_deref()
        .filter(|appdata| is_absolute(appdata))
        .ok_or_else(|| "Toolport の registry の既定の場所を決められません".to_string())?;
    let default = join(&join(appdata, "Toolport"), "registry.json");
    if env.files.exists(&default) {
        Ok(default)
    } else if env
        .files
        .exists(&join(&join(appdata, "Conduit"), "registry.json"))
    {
        Err("Toolport の registry が無く、Conduit の registry だけがあります".to_string())
    } else {
        Err("Toolport の registry が見つかりません".to_string())
    }
}

/// registry を読む（R8: `version` が整数 3、`servers`・`profiles` が配列）
fn load_registry(env: &Env, path: &str) -> Result<Value, String> {
    let bytes = env
        .files
        .read(path)
        .map_err(|e| format!("registry を読めません（{}）", e.kind()))?;
    let registry =
        json_from(&bytes).ok_or_else(|| "registry の JSON が壊れています".to_string())?;
    if registry.get("version").and_then(Value::as_i64) != Some(3) {
        return Err("registry の version が整数の 3 ではありません".to_string());
    }
    if !registry.get("servers").is_some_and(Value::is_array) {
        return Err("registry の servers が配列ではありません".to_string());
    }
    if !registry.get("profiles").is_some_and(Value::is_array) {
        return Err("registry の profiles が配列ではありません".to_string());
    }
    Ok(registry)
}

/// R9: `clientScopes` が無い、または `clientScopes[<ID>]` が無いか空文字のときだけ通す
fn check_client_scope(registry: &Value, client: ClientId) -> Result<(), String> {
    let Some(scopes) = registry.get("clientScopes") else {
        return Ok(());
    };
    let scopes = scopes
        .as_object()
        .ok_or_else(|| "clientScopes がオブジェクトではありません".to_string())?;
    match scopes.get(client.as_str()) {
        None => Ok(()),
        Some(Value::String(s)) if s.is_empty() => Ok(()),
        Some(_) => Err("クライアントに範囲（clientScopes）が設定されています".to_string()),
    }
}

/// 名前（`command` のファイル名）と `args` がこの製品を指すサーバーなら (id, command)
fn name_args_match<'a>(product: &Product, server: &'a Value) -> Option<(&'a str, &'a str)> {
    let command = server.get("command")?.as_str()?;
    let id = server.get("id")?.as_str()?;
    let lower = file_name_of(command).to_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let args = server.get("args")?.as_array()?;
    let matches = stem == product.binary_stem
        && args.iter().all(Value::is_string)
        && args.iter().any(|a| a.as_str() == Some(product.mode_arg));
    matches.then_some((id, command))
}

fn find_profile<'a>(profiles: &'a [Value], key: &str) -> Option<&'a Value> {
    let by = |field: &str| {
        profiles
            .iter()
            .find(|p| p.get(field).and_then(Value::as_str) == Some(key))
    };
    by("id").or_else(|| by("name"))
}

/// R10: 使われるプロファイルの `enabledServerIds`（ゲートウェイの env → プロセスの `TOOLPORT_PROFILE`
/// があればそれだけ、なければ `activeProfileId` と `folderProfiles[].profile` の全部）
fn selected_enabled_ids<'a>(
    registry: &'a Value,
    gateway_env: &BTreeMap<String, String>,
    env: &Env,
) -> Result<Vec<Vec<&'a str>>, String> {
    let mut keys: Vec<String> = Vec::new();
    if let Some(key) =
        non_empty(gateway_env, "TOOLPORT_PROFILE").or(non_empty(&env.vars, "TOOLPORT_PROFILE"))
    {
        keys.push(key.to_owned());
    } else {
        match registry.get("activeProfileId") {
            None => {}
            Some(Value::String(active)) => {
                if !active.is_empty() {
                    keys.push(active.clone());
                }
            }
            Some(_) => return Err("activeProfileId が文字列ではありません".to_string()),
        }
        match registry.get("folderProfiles") {
            None => {}
            Some(Value::Array(items)) => {
                for item in items {
                    let key = item
                        .get("profile")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "folderProfiles の profile を読めません".to_string())?;
                    keys.push(key.to_owned());
                }
            }
            Some(_) => return Err("folderProfiles が配列ではありません".to_string()),
        }
    }
    if keys.is_empty() {
        return Err("使われるプロファイルを決められません".to_string());
    }
    let profiles = registry["profiles"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    keys.iter()
        .map(|key| {
            let profile = find_profile(profiles, key)
                .ok_or_else(|| "使われるプロファイルを registry で引けません".to_string())?;
            profile
                .get("enabledServerIds")
                .and_then(Value::as_array)
                .and_then(|ids| ids.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .ok_or_else(|| "プロファイルの enabledServerIds を読めません".to_string())
        })
        .collect()
}

/// R11: registry の内容から、この製品が提供されているかを決める
fn evaluate(
    product: &Product,
    client: ClientId,
    registry: &Value,
    gateway_env: &BTreeMap<String, String>,
    env: &Env,
) -> Result<Detection, String> {
    check_client_scope(registry, client)?;
    let named: Vec<(&str, &str)> = registry["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("transport").and_then(Value::as_str) == Some("stdio")
                && s.get("enabled") == Some(&Value::Bool(true))
        })
        .filter_map(|s| name_args_match(product, s))
        .collect();
    if named.is_empty() {
        return Ok(Detection::NotServed);
    }
    let enabled_sets = selected_enabled_ids(registry, gateway_env, env)?;
    let mut missing_executable = false;
    for (id, command) in named {
        if !enabled_sets.iter().all(|ids| ids.contains(&id)) {
            continue;
        }
        if existing_absolute_file(env, command) {
            return Ok(Detection::Served {
                server_id: id.to_owned(),
                command: command.to_owned(),
            });
        }
        missing_executable = true;
    }
    // 条件を満たすが実行ファイルが無いものだけ: Toolport が提供できない状態なので直接登録に戻す側（解釈）
    Ok(if missing_executable {
        Detection::NotConnected
    } else {
        Detection::NotServed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    const HOME: &str = r"C:\Users\u";
    const APPDATA: &str = r"C:\Users\u\AppData\Roaming";
    const CLAUDE_JSON: &str = r"C:\Users\u\.claude.json";
    const CODEX_TOML: &str = r"C:\Users\u\.codex\config.toml";
    const GATEWAY: &str = r"C:\tp\toolport-gateway.exe";
    const REGISTRY: &str = r"C:\data\registry.json";
    const DEFAULT_REGISTRY: &str = r"C:\Users\u\AppData\Roaming\Toolport\registry.json";
    const PRODUCT_EXE: &str = r"C:\app\current\edictora.exe";
    const PRODUCT: Product = Product {
        binary_stem: "edictora",
        mode_arg: "mcp",
    };

    fn key(path: &str) -> String {
        path.replace('/', "\\").to_lowercase()
    }

    struct FakeFiles {
        files: BTreeMap<String, Vec<u8>>,
        unreadable: BTreeSet<String>,
    }

    impl Files for FakeFiles {
        fn exists(&self, path: &str) -> bool {
            self.files.contains_key(&key(path))
        }

        fn read(&self, path: &str) -> io::Result<Vec<u8>> {
            let k = key(path);
            if self.unreadable.contains(&k) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            self.files
                .get(&k)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
        }
    }

    struct PanicFiles;

    impl Files for PanicFiles {
        fn exists(&self, _: &str) -> bool {
            panic!("読んではいけない")
        }

        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("読んではいけない")
        }
    }

    fn registry() -> Value {
        json!({
            "version": 3,
            "activeProfileId": "p1",
            "profiles": [{"id": "p1", "name": "Default", "enabledServerIds": ["s1"]}],
            "servers": [{
                "id": "s1", "transport": "stdio", "command": PRODUCT_EXE,
                "args": ["mcp"], "enabled": true, "env": {"SECRET": "hunter2"}
            }]
        })
    }

    fn gateway_entry() -> Value {
        json!({"type": "stdio", "command": GATEWAY, "args": [],
               "env": {"TOOLPORT_REGISTRY": REGISTRY}})
    }

    const CODEX_GATEWAY: &str = "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nargs = []\n[mcp_servers.toolport.env]\nTOOLPORT_REGISTRY = 'C:\\data\\registry.json'\n";

    struct World {
        files: BTreeMap<String, Vec<u8>>,
        unreadable: BTreeSet<String>,
        vars: BTreeMap<String, String>,
        home: Option<String>,
        windows: bool,
    }

    impl World {
        /// Claude Code がゲートウェイにつながり、registry が製品を提供している標準の状態
        fn new() -> World {
            let mut world = World {
                files: BTreeMap::new(),
                unreadable: BTreeSet::new(),
                vars: BTreeMap::new(),
                home: Some(HOME.to_string()),
                windows: true,
            };
            world.put(GATEWAY, "");
            world.put(PRODUCT_EXE, "");
            world.put_json(
                CLAUDE_JSON,
                &json!({"mcpServers": {"toolport": gateway_entry()}}),
            );
            world.put_json(REGISTRY, &registry());
            world
        }

        fn put(&mut self, path: &str, text: &str) {
            self.files.insert(key(path), text.as_bytes().to_vec());
        }

        fn put_json(&mut self, path: &str, value: &Value) {
            self.put(path, &value.to_string());
        }

        fn var(&mut self, name: &str, value: &str) {
            self.vars.insert(name.to_string(), value.to_string());
        }

        fn remove(&mut self, path: &str) {
            self.files.remove(&key(path));
        }

        fn env(&self) -> Env {
            Env {
                windows: self.windows,
                home: self.home.clone(),
                appdata: Some(APPDATA.to_string()),
                vars: self.vars.clone(),
                files: Box::new(FakeFiles {
                    files: self.files.clone(),
                    unreadable: self.unreadable.clone(),
                }),
            }
        }

        fn detect(&self) -> Detection {
            detect(&PRODUCT, ClientId::ClaudeCode, &self.env())
        }

        fn detect_codex(&self) -> Detection {
            detect(&PRODUCT, ClientId::Codex, &self.env())
        }

        /// registry を書き換えて置く
        fn registry(&mut self, edit: impl FnOnce(&mut Value)) {
            let mut value = registry();
            edit(&mut value);
            self.put_json(REGISTRY, &value);
        }

        /// ゲートウェイ項目を書き換えて置く
        fn gateway(&mut self, edit: impl FnOnce(&mut Value)) {
            let mut entry = gateway_entry();
            edit(&mut entry);
            self.put_json(CLAUDE_JSON, &json!({"mcpServers": {"toolport": entry}}));
        }

        fn codex(&mut self, toml: &str) {
            self.put(CODEX_TOML, toml);
        }
    }

    fn served() -> Detection {
        Detection::Served {
            server_id: "s1".to_string(),
            command: PRODUCT_EXE.to_string(),
        }
    }

    fn is_unknown(detection: &Detection) -> bool {
        matches!(detection, Detection::Unknown(_))
    }

    #[test]
    fn 標準の状態ではservedになる() {
        assert_eq!(World::new().detect(), served());
    }

    #[test]
    fn r1_windows以外は何も読まずunknown() {
        let env = Env {
            windows: false,
            home: Some(HOME.to_string()),
            appdata: None,
            vars: BTreeMap::new(),
            files: Box::new(PanicFiles),
        };
        assert_eq!(
            detect(&PRODUCT, ClientId::ClaudeCode, &env),
            Detection::Unknown("non-windows".to_string())
        );
        assert_eq!(product_server_ids(&PRODUCT, &env), Vec::<String>::new());
    }

    #[test]
    fn r2_ホームを決められなければunknown() {
        let mut w = World::new();
        w.home = None;
        assert!(is_unknown(&w.detect()));
        w.home = Some("relative".to_string());
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r3_読めなければunknown_無ければnotconnected() {
        let mut w = World::new();
        w.unreadable.insert(key(CLAUDE_JSON));
        assert!(is_unknown(&w.detect()));
        w.unreadable.clear();
        w.remove(CLAUDE_JSON);
        assert_eq!(w.detect(), Detection::NotConnected);
        // registry が読めない・無い（無いのは R7 の「どこにも無い」）
        let mut w = World::new();
        w.unreadable.insert(key(REGISTRY));
        assert!(is_unknown(&w.detect()));
        w.unreadable.clear();
        w.remove(REGISTRY);
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r3_utf8のbomつきでも読める() {
        let mut w = World::new();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(registry().to_string().as_bytes());
        w.files.insert(key(REGISTRY), bytes);
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r4_ゲートウェイ候補でない項目は形が壊れていても無視する() {
        let mut w = World::new();
        w.put_json(
            CLAUDE_JSON,
            &json!({"mcpServers": {
                "a": {"command": "node", "args": 5, "env": "x", "type": 7},
                "b": {"command": "node", "args": [1], "env": {"K": 1}},
                "c": "not an object",
                "d": {"type": "http", "url": "https://x"},
                "toolport": gateway_entry()
            }}),
        );
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r4_ゲートウェイ候補の形が想定外ならunknown() {
        let cases = [
            json!({"type": 7, "command": GATEWAY}),
            json!({"command": GATEWAY, "args": "x"}),
            json!({"command": GATEWAY, "args": [1]}),
            json!({"command": GATEWAY, "env": {"TOOLPORT_REGISTRY": 1}}),
            json!({"command": GATEWAY, "env": []}),
        ];
        for entry in cases {
            let mut w = World::new();
            w.put_json(CLAUDE_JSON, &json!({"mcpServers": {"toolport": entry}}));
            assert!(is_unknown(&w.detect()), "{entry}");
        }
        let mut w = World::new();
        w.put_json(CLAUDE_JSON, &json!({"mcpServers": []}));
        assert!(is_unknown(&w.detect()));
        w.put(CLAUDE_JSON, "{ broken");
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r4_typeが無いかstdio以外の扱い() {
        let mut w = World::new();
        w.put_json(
            CLAUDE_JSON,
            &json!({"mcpServers": {"toolport": {"command": GATEWAY, "env": {"TOOLPORT_REGISTRY": REGISTRY}}}}),
        );
        assert_eq!(w.detect(), served());
        w.gateway(|e| e["type"] = json!("http"));
        assert_eq!(w.detect(), Detection::NotConnected);
    }

    #[test]
    fn r4_claude_config_dirは絶対パスのときだけ使う() {
        let mut w = World::new();
        w.put_json(
            r"D:\cfg\.claude.json",
            &json!({"mcpServers": {"toolport": gateway_entry()}}),
        );
        w.remove(CLAUDE_JSON);
        assert_eq!(w.detect(), Detection::NotConnected);
        w.var("CLAUDE_CONFIG_DIR", r"D:\cfg");
        assert_eq!(w.detect(), served());
        w.var("CLAUDE_CONFIG_DIR", "cfg");
        assert_eq!(w.detect(), Detection::NotConnected);
    }

    #[test]
    fn r5_codexのgateway() {
        let mut w = World::new();
        w.codex(CODEX_GATEWAY);
        assert_eq!(w.detect_codex(), served());
        // CODEX_HOME（絶対パスのときだけ）
        w.put(r"D:\ch\config.toml", CODEX_GATEWAY);
        w.remove(CODEX_TOML);
        assert_eq!(w.detect_codex(), Detection::NotConnected);
        w.var("CODEX_HOME", r"D:\ch");
        assert_eq!(w.detect_codex(), served());
    }

    #[test]
    fn r5_基本文字列とbomと見出し内の空白と引用符つきの名前() {
        let mut w = World::new();
        w.codex(
            "\u{feff}# c\n[ mcp_servers . \"my gate\" ] # x\ncommand = \"C:\\\\tp\\\\toolport-gateway.exe\" # y\nargs = [ ]\n[mcp_servers.\"my gate\".env]\nTOOLPORT_REGISTRY = \"C:\\\\data\\\\registry.json\"\n",
        );
        assert_eq!(w.detect_codex(), served());
    }

    #[test]
    fn r5_対応外のmcp_servers系はunknown() {
        let bad = [
            // 別の見出し
            "[mcp_servers]\n",
            "[mcp_servers.toolport.tools.x]\napprove = true\n",
            "[[mcp_servers.toolport]]\n",
            // ドット付きキー・インラインテーブル
            "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nenv.A = \"b\"\n",
            "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nenv = { A = \"b\" }\n",
            // 複数行の文字列・配列
            "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nnote = \"\"\"\nx\n\"\"\"\n",
            "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nargs = [\n]\n",
            // ルートでの書き方
            "mcp_servers.toolport.command = 'x'\n",
            "mcp_servers = { }\n",
        ];
        for toml in bad {
            let mut w = World::new();
            w.codex(toml);
            assert!(is_unknown(&w.detect_codex()), "{toml}");
        }
    }

    #[test]
    fn r5_節の外の複数行文字列は正しく読み飛ばす() {
        let mut w = World::new();
        w.codex(&format!(
            "[profile]\nnote = \"\"\"\n[mcp_servers.evil.tools.x]\nmcp_servers.a = 1\n\"\"\"\nlit = '''\n[[mcp_servers.z]]\n'''\nlist = [\n  [\"a\"],\n  \"[mcp_servers.q]\",\n]\ninline = {{ a = \"\"\"x\"\"\" }}\n{CODEX_GATEWAY}"
        ));
        assert_eq!(w.detect_codex(), served());
        // 閉じられない複数行文字列は壊れている
        w.codex("[p]\nnote = \"\"\"\nx\n");
        assert!(is_unknown(&w.detect_codex()));
    }

    #[test]
    fn r5_urlのある項目とenabledがfalseの項目は候補にしない() {
        let mut w = World::new();
        w.codex(&format!(
            "[mcp_servers.toolport]\ncommand = 'C:\\tp\\toolport-gateway.exe'\nurl = \"https://x\"\n{}",
            "[mcp_servers.toolport.env]\nTOOLPORT_REGISTRY = 'C:\\data\\registry.json'\n"
        ));
        assert_eq!(w.detect_codex(), Detection::NotConnected);
        w.codex(&format!("{CODEX_GATEWAY}\n").replace("args = []", "args = []\nenabled = false"));
        assert_eq!(w.detect_codex(), Detection::NotConnected);
        w.codex(&format!("{CODEX_GATEWAY}\n").replace("args = []", "args = []\nenabled = true"));
        assert_eq!(w.detect_codex(), served());
    }

    #[test]
    fn r5_ゲートウェイ候補のargsとenvの形() {
        let mut w = World::new();
        w.codex(&CODEX_GATEWAY.replace("args = []", "args = [\"x\"]"));
        assert!(is_unknown(&w.detect_codex()));
        w.codex(&CODEX_GATEWAY.replace("args = []", "args = 3"));
        assert!(is_unknown(&w.detect_codex()));
        w.codex(&format!("{CODEX_GATEWAY}N = 5\n"));
        assert!(is_unknown(&w.detect_codex()));
        // 候補でない項目の形は無視する
        w.codex(&format!("{CODEX_GATEWAY}[mcp_servers.other]\ncommand = 'node'\nargs = 3\n[mcp_servers.other.env]\nN = 5\n"));
        assert_eq!(w.detect_codex(), served());
        // 壊れた TOML
        w.codex("[mcp_servers.toolport\ncommand = 1\n");
        assert!(is_unknown(&w.detect_codex()));
        w.codex("what is this\n");
        assert!(is_unknown(&w.detect_codex()));
    }

    #[test]
    fn r6_候補の絞り込みと実在() {
        // 名前が違う・registry の clientManagedEntries と一致しない → つながっていない
        let mut w = World::new();
        w.put_json(
            CLAUDE_JSON,
            &json!({"mcpServers": {"x": {"command": r"C:\tp\other.exe", "env": {"TOOLPORT_REGISTRY": REGISTRY}}}}),
        );
        w.put(r"C:\tp\other.exe", "");
        assert_eq!(w.detect(), Detection::NotConnected);
        // 名前の接尾辞つき・大文字・拡張子なし
        for name in [
            "TOOLPORT-GATEWAY-1.2.3+x",
            "toolport-gateway-win_x64.EXE",
            "toolport-gateway",
        ] {
            let path = format!(r"C:\tp\{name}");
            let mut w = World::new();
            w.put(&path, "");
            w.put_json(
                CLAUDE_JSON,
                &json!({"mcpServers": {"x": {"command": path, "env": {"TOOLPORT_REGISTRY": REGISTRY}}}}),
            );
            assert_eq!(w.detect(), served(), "{name}");
        }
        // 実行ファイルが無い・相対パス → 有効な候補が無い
        let mut w = World::new();
        w.remove(GATEWAY);
        assert_eq!(w.detect(), Detection::NotConnected);
        w.gateway(|e| e["command"] = json!("toolport-gateway.exe"));
        assert_eq!(w.detect(), Detection::NotConnected);
    }

    #[test]
    fn r6_clientmanagedentriesと正規化一致する項目も候補() {
        let mut w = World::new();
        w.put(r"C:\Tools\My Gate.exe", "");
        w.put_json(
            CLAUDE_JSON,
            &json!({"mcpServers": {"x": {"command": "C:/tools/my gate.EXE", "env": {"TOOLPORT_REGISTRY": REGISTRY}}}}),
        );
        w.var("TOOLPORT_REGISTRY", REGISTRY);
        w.registry(|r| {
            r["clientManagedEntries"] =
                json!({"claude-code": {"command": r#" "C:\Tools\My Gate.exe" "#}})
        });
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r6_argsが空でない_複数候補でenvが食い違うときはunknown() {
        let mut w = World::new();
        w.gateway(|e| e["args"] = json!(["--x"]));
        assert!(is_unknown(&w.detect()));
        let a = json!({"command": GATEWAY, "env": {"TOOLPORT_REGISTRY": REGISTRY}});
        let b = json!({"command": GATEWAY, "env": {"TOOLPORT_REGISTRY": REGISTRY, "X": "1"}});
        w.put_json(CLAUDE_JSON, &json!({"mcpServers": {"a": a, "b": b}}));
        assert!(is_unknown(&w.detect()));
        w.put_json(CLAUDE_JSON, &json!({"mcpServers": {"a": a, "b": a}}));
        assert_eq!(w.detect(), served());
        // 実在しない候補は数えない
        w.put_json(
            CLAUDE_JSON,
            &json!({"mcpServers": {"a": a, "gone": {"command": r"C:\gone\toolport-gateway.exe", "args": ["x"]}}}),
        );
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r7_registryの場所の優先順() {
        // ゲートウェイ env の REGISTRY が DATA_DIR より先
        let mut w = World::new();
        w.gateway(|e| {
            e["env"] = json!({"TOOLPORT_REGISTRY": REGISTRY, "TOOLPORT_DATA_DIR": r"C:\other"})
        });
        assert_eq!(w.detect(), served());
        // DATA_DIR（末尾の区切りつき）
        w.gateway(|e| e["env"] = json!({"TOOLPORT_DATA_DIR": r"C:\data\"}));
        assert_eq!(w.detect(), served());
        // プロセス env（REGISTRY、次に DATA_DIR）
        w.gateway(|e| e["env"] = json!({}));
        assert!(is_unknown(&w.detect()));
        w.var("TOOLPORT_DATA_DIR", r"C:\data");
        assert_eq!(w.detect(), served());
        w.var("TOOLPORT_REGISTRY", r"C:\nowhere\registry.json");
        assert!(is_unknown(&w.detect()));
        // 既定の場所
        let mut w = World::new();
        w.gateway(|e| e["env"] = json!({}));
        w.put_json(DEFAULT_REGISTRY, &registry());
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r7_空文字は無い扱い_相対パスはゲートウェイ側ならunknown_プロセス側なら無視() {
        let mut w = World::new();
        w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": "", "TOOLPORT_DATA_DIR": r"C:\data"}));
        assert_eq!(w.detect(), served());
        w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": r"data\registry.json"}));
        assert!(is_unknown(&w.detect()));
        w.gateway(|e| e["env"] = json!({"TOOLPORT_DATA_DIR": "data"}));
        assert!(is_unknown(&w.detect()));
        // プロセス env の空文字と相対パスは次の段へ
        w.gateway(|e| e["env"] = json!({}));
        w.put_json(DEFAULT_REGISTRY, &registry());
        w.var("TOOLPORT_REGISTRY", "");
        w.var("TOOLPORT_DATA_DIR", "relative");
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r7_conduitの変数か登録だけがあればunknown() {
        for name in CONDUIT_VARS {
            let mut w = World::new();
            w.var(name, "x");
            assert!(is_unknown(&w.detect()), "process {name}");
            let mut w = World::new();
            w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": REGISTRY, name: ""}));
            assert!(is_unknown(&w.detect()), "gateway {name}");
        }
        let mut w = World::new();
        w.gateway(|e| e["env"] = json!({}));
        w.put_json(
            r"C:\Users\u\AppData\Roaming\Conduit\registry.json",
            &registry(),
        );
        assert!(is_unknown(&w.detect()));
        // どこにも無い・既定の場所を決められない
        w.remove(r"C:\Users\u\AppData\Roaming\Conduit\registry.json");
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r8_registryの形が違えばunknown() {
        let edits: [fn(&mut Value); 6] = [
            |r| r["version"] = json!(2),
            |r| r["version"] = json!("3"),
            |r| r["version"] = json!(3.0),
            |r| r["servers"] = json!({}),
            |r| r["profiles"] = json!(null),
            |r| {
                r.as_object_mut().unwrap().remove("version");
            },
        ];
        for edit in edits {
            let mut w = World::new();
            w.registry(edit);
            assert!(is_unknown(&w.detect()));
        }
        let mut w = World::new();
        w.put(REGISTRY, "{ broken");
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r9_clientscopes() {
        let mut w = World::new();
        w.registry(|r| r["clientScopes"] = json!({"codex": "x"}));
        assert_eq!(w.detect(), served());
        w.registry(|r| r["clientScopes"] = json!({"claude-code": ""}));
        assert_eq!(w.detect(), served());
        for scopes in [
            json!([]),
            json!(null),
            json!({"claude-code": "work"}),
            json!({"claude-code": null}),
        ] {
            w.registry(|r| r["clientScopes"] = scopes.clone());
            assert!(is_unknown(&w.detect()), "{scopes}");
        }
        // サーバーの探索より先に確かめる（一致するサーバーが無くても Unknown）
        w.registry(|r| {
            r["clientScopes"] = json!({"claude-code": "work"});
            r["servers"] = json!([]);
        });
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r10_プロファイルの集合() {
        // folderProfiles の全部で有効でなければ NotServed
        let mut w = World::new();
        w.registry(|r| {
            r["profiles"] = json!([
                {"id": "p1", "name": "A", "enabledServerIds": ["s1"]},
                {"id": "p2", "name": "B", "enabledServerIds": []}
            ]);
            r["folderProfiles"] = json!([{"profile": "p1"}]);
        });
        assert_eq!(w.detect(), served());
        w.registry(|r| {
            r["profiles"] = json!([
                {"id": "p1", "name": "A", "enabledServerIds": ["s1"]},
                {"id": "p2", "name": "B", "enabledServerIds": []}
            ]);
            r["folderProfiles"] = json!([{"profile": "B"}]);
        });
        assert_eq!(w.detect(), Detection::NotServed);
        // activeProfileId が無い・空でも folderProfiles だけで決まる
        w.registry(|r| {
            r.as_object_mut().unwrap().remove("activeProfileId");
            r["folderProfiles"] = json!([{"profile": "p1"}]);
        });
        assert_eq!(w.detect(), served());
        w.registry(|r| {
            r["activeProfileId"] = json!("");
            r["folderProfiles"] = json!([{"profile": "p1"}]);
        });
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r10_toolport_profileがあればそれだけ() {
        let profiles = json!([
            {"id": "p1", "name": "A", "enabledServerIds": ["s1"]},
            {"id": "p2", "name": "B", "enabledServerIds": []}
        ]);
        let mut w = World::new();
        w.registry(|r| {
            r["profiles"] = profiles.clone();
            r["activeProfileId"] = json!("p2");
        });
        assert_eq!(w.detect(), Detection::NotServed);
        // プロセス env（name で引く）
        w.var("TOOLPORT_PROFILE", "A");
        assert_eq!(w.detect(), served());
        // ゲートウェイ env が先。空文字は無い扱い
        w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": REGISTRY, "TOOLPORT_PROFILE": "p2"}));
        assert_eq!(w.detect(), Detection::NotServed);
        w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": REGISTRY, "TOOLPORT_PROFILE": ""}));
        assert_eq!(w.detect(), served());
    }

    #[test]
    fn r10_決められない形はunknown() {
        let edits: [fn(&mut Value); 8] = [
            // P が空
            |r| {
                r.as_object_mut().unwrap().remove("activeProfileId");
            },
            // 引けない
            |r| r["activeProfileId"] = json!("nope"),
            |r| r["activeProfileId"] = json!(5),
            |r| r["activeProfileId"] = json!(null),
            |r| r["folderProfiles"] = json!({}),
            |r| r["folderProfiles"] = json!([{"x": 1}]),
            |r| r["profiles"][0]["enabledServerIds"] = json!([1]),
            |r| {
                r["profiles"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("enabledServerIds");
            },
        ];
        for (i, edit) in edits.into_iter().enumerate() {
            let mut w = World::new();
            w.registry(edit);
            assert!(is_unknown(&w.detect()), "{i}");
        }
        let mut w = World::new();
        w.var("TOOLPORT_PROFILE", "nope");
        assert!(is_unknown(&w.detect()));
    }

    #[test]
    fn r11_サーバーの一致条件() {
        let not_served: [fn(&mut Value); 7] = [
            |r| r["servers"][0]["enabled"] = json!(false),
            |r| r["servers"][0]["enabled"] = json!("true"),
            |r| r["servers"][0]["transport"] = json!("http"),
            |r| r["servers"][0]["args"] = json!(["serve"]),
            |r| r["servers"][0]["args"] = json!(["mcp", 1]),
            |r| r["servers"][0]["command"] = json!(r"C:\app\current\other.exe"),
            |r| r["profiles"][0]["enabledServerIds"] = json!(["other"]),
        ];
        for (i, edit) in not_served.into_iter().enumerate() {
            let mut w = World::new();
            w.registry(edit);
            assert_eq!(w.detect(), Detection::NotServed, "{i}");
        }
        let mut w = World::new();
        w.registry(|r| r["servers"] = json!([]));
        assert_eq!(w.detect(), Detection::NotServed);
        // ファイル名は大小を区別せず .exe を除く。引数に mode_arg を含む
        w.put(r"C:\app\EDICTORA.EXE", "");
        w.registry(|r| {
            r["servers"][0]["command"] = json!(r"C:\app\EDICTORA.EXE");
            r["servers"][0]["args"] = json!(["--x", "mcp"]);
        });
        assert_eq!(
            w.detect(),
            Detection::Served {
                server_id: "s1".to_string(),
                command: r"C:\app\EDICTORA.EXE".to_string()
            }
        );
    }

    #[test]
    fn r11_実行ファイルが無ければnotconnected() {
        let mut w = World::new();
        w.remove(PRODUCT_EXE);
        assert_eq!(w.detect(), Detection::NotConnected);
        w.put(PRODUCT_EXE, "");
        w.registry(|r| r["servers"][0]["command"] = json!("edictora.exe"));
        assert_eq!(w.detect(), Detection::NotConnected);
        // 実在するサーバーが 1 つでもあれば Served
        w.registry(|r| {
            r["servers"] = json!([
                {"id": "gone", "transport": "stdio", "command": r"C:\gone\edictora.exe", "args": ["mcp"], "enabled": true},
                {"id": "s1", "transport": "stdio", "command": PRODUCT_EXE, "args": ["mcp"], "enabled": true}
            ]);
            r["profiles"][0]["enabledServerIds"] = json!(["gone", "s1"]);
        });
        assert_eq!(w.detect(), served());
        // 無効化されたものだけが実在しないなら NotServed
        w.registry(|r| {
            r["servers"][0]["enabled"] = json!(false);
            r["servers"][0]["command"] = json!(r"C:\gone\edictora.exe");
        });
        assert_eq!(w.detect(), Detection::NotServed);
    }

    #[test]
    fn r12_結果とログ用の文言に秘密値を含めない() {
        let mut w = World::new();
        let shown = format!("{:?}", w.detect());
        assert!(
            !shown.contains("hunter2") && !shown.contains("SECRET"),
            "{shown}"
        );
        // サーバーの env が壊れていても読まない
        w.registry(|r| r["servers"][0]["env"] = json!(42));
        assert_eq!(w.detect(), served());
        // Unknown の理由にも env の値を入れない
        w.gateway(|e| e["env"] = json!({"TOOLPORT_REGISTRY": REGISTRY, "TOKEN": 1}));
        let Detection::Unknown(reason) = w.detect() else {
            panic!("unknown のはず");
        };
        assert!(
            !reason.contains(REGISTRY) && !reason.contains("TOKEN"),
            "{reason}"
        );
    }

    #[test]
    fn 種類名() {
        assert_eq!(served().kind(), "served");
        assert_eq!(Detection::NotConnected.kind(), "not-connected");
        assert_eq!(Detection::NotServed.kind(), "not-served");
        assert_eq!(Detection::Unknown(String::new()).kind(), "unknown");
    }

    #[test]
    fn product_server_idsはidをツール名の前置きに変換して返す() {
        let mut w = World::new();
        w.var("TOOLPORT_REGISTRY", REGISTRY);
        w.registry(|r| {
            r["servers"] = json!([
                {"id": "etw-mcp.v2", "transport": "stdio", "command": r"C:\gone\Edictora.exe", "args": ["mcp"], "enabled": false},
                {"id": "other", "command": PRODUCT_EXE, "args": ["serve"]},
                {"command": PRODUCT_EXE, "args": ["mcp"]}
            ]);
        });
        assert_eq!(
            product_server_ids(&PRODUCT, &w.env()),
            vec!["etw_mcp_v2".to_string()]
        );
        // 読めない・形が違う・Conduit → 空
        w.put(REGISTRY, "{ broken");
        assert!(product_server_ids(&PRODUCT, &w.env()).is_empty());
        w.registry(|r| r["version"] = json!(2));
        assert!(product_server_ids(&PRODUCT, &w.env()).is_empty());
        w.registry(|_| {});
        w.var("CONDUIT_PROFILE", "x");
        assert!(product_server_ids(&PRODUCT, &w.env()).is_empty());
        // 既定の場所
        let mut w = World::new();
        w.put_json(DEFAULT_REGISTRY, &registry());
        assert_eq!(
            product_server_ids(&PRODUCT, &w.env()),
            vec!["s1".to_string()]
        );
    }

    #[test]
    fn パスの補助() {
        assert!(is_absolute(r"C:\a") && is_absolute("c:/a") && is_absolute(r"\\host\share"));
        assert!(
            !is_absolute("a") && !is_absolute(r"\a") && !is_absolute("C:a") && !is_absolute("")
        );
        assert_eq!(join(r"C:\a\", "b"), r"C:\a\b");
        assert_eq!(join("C:/", "b"), r"C:\b");
        assert_eq!(file_name_of(r"C:\a/b\c.exe"), "c.exe");
        assert_eq!(tool_prefix("a-b c_d9"), "a_b_c_d9");
    }
}
