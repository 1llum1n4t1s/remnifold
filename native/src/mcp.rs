// JSONスキーマは既存のMCP公開契約を維持する。
fn tools() -> serde_json::Value {
    serde_json::from_str(r###"[{"name":"search_memory","description":"Search concise memory indexes by topic across every nonempty Remnifold space, independent of the current work folder. Returns document IDs, source spaces and dates; use getDocument for details. An explicit containerTag restricts the search to that space.","inputSchema":{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":1000},"includeProfile":{"type":"boolean","default":false,"description":"Compatibility option. true adds the searched-space index, never extracted factual profiles."},"containerTag":{"type":"string","minLength":1,"maxLength":160,"description":"Optional memory space tag"}},"required":["query"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"add_memory","description":"Save or forget a memory in the user's Cloudflare D1 database. Saves use the shared memories container by default; sourceFolder optionally records workspace provenance. Forget accepts a documentId from search/list/getDocument or exact stored content, and is always restricted to one explicit or default container.","inputSchema":{"type":"object","properties":{"content":{"type":"string","minLength":1,"maxLength":200000,"description":"Content to save, or exact stored content to forget when documentId is unavailable"},"documentId":{"type":"string","minLength":1,"maxLength":64,"description":"Document ID returned by search_memory, listDocuments, or getDocument; valid only for forget"},"action":{"type":"string","enum":["save","forget"],"default":"save"},"containerTag":{"type":"string","minLength":1,"maxLength":160,"description":"Optional memory space tag"},"sourceFolder":{"type":"string","minLength":1,"maxLength":4096,"description":"Absolute current workspace folder. Use when the MCP client does not provide exactly one workspace root."}},"anyOf":[{"required":["content"]},{"properties":{"action":{"const":"forget"}},"required":["action","documentId"]}],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":false}},{"name":"consolidate_memory","description":"Create or refresh the consolidated checkpoint for one project immediately. Originals are retained; sourceFolder identifies the current project when projectId is omitted.","inputSchema":{"type":"object","properties":{"projectId":{"type":"string","minLength":1,"maxLength":160,"description":"Project provenance ID. Omit to derive it from sourceFolder or the MCP workspace root."},"sourceFolder":{"type":"string","minLength":1,"maxLength":4096,"description":"Absolute current workspace folder. Use when the MCP client does not provide exactly one workspace root."}},"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"listMemories","description":"List recent memory indexes across all physical spaces; use getDocument for full details. An exact topic filters by content label, and an explicit containerTag restricts the physical space.","inputSchema":{"type":"object","properties":{"page":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":50},"containerTag":{"type":"string","minLength":1,"maxLength":160,"description":"Optional memory space tag"},"topic":{"type":"string","minLength":1,"maxLength":80,"description":"Optional exact topic label; use __unclassified__ for documents without a topic"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"listDocuments","description":"List stored document indexes with topic and provenance across all physical spaces; use getDocument for full details. An exact topic filters by content label, and an explicit containerTag restricts the physical space.","inputSchema":{"type":"object","properties":{"page":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":50},"containerTag":{"type":"string","minLength":1,"maxLength":160,"description":"Optional memory space tag"},"topic":{"type":"string","minLength":1,"maxLength":80,"description":"Optional exact topic label; use __unclassified__ for documents without a topic"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"getDocument","description":"Read one memory document from Cloudflare D1. Organization summaries include source IDs; pass nextCursor as sourceCursor for the next page. Cursor pages contain source identities only, without document content. Continue while hasMore is true, including empty pages.","inputSchema":{"type":"object","properties":{"documentId":{"type":"string","minLength":1},"sourceCursor":{"type":"string","minLength":1,"maxLength":2048,"description":"Organization source page cursor; returns source identities without document content"}},"required":["documentId"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"listTopics","description":"List content topics across all legacy and shared memory spaces, including the count of unclassified documents.","inputSchema":{"type":"object","properties":{"page":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"listSpaces","description":"List physical memory spaces and legacy project-folder provenance tags for compatibility. Use listTopics for ordinary browsing.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},{"name":"whoAmI","description":"Show the active self-hosted Remnifold context. sourceFolder can identify the current workspace when client roots are unavailable.","inputSchema":{"type":"object","properties":{"sourceFolder":{"type":"string","minLength":1,"maxLength":4096,"description":"Absolute current workspace folder. Use when the MCP client does not provide exactly one workspace root."}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}]"###).expect("static tool schemas")
}
use crate::{
    common::{self, Config, text},
    search,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::Path,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Duration,
};

fn text_result(message: impl Into<String>, structured: Value) -> Value {
    json!({"content":[{"type":"text","text":message.into()}],"structuredContent":structured})
}

fn validate_string(args: &Value, key: &str, maximum: usize, required: bool) -> Result<()> {
    if let Some(value) = args.get(key) {
        if !value
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.encode_utf16().count() <= maximum)
        {
            bail!("{key} must be a nonempty string of at most {maximum} characters.");
        }
    } else if required {
        bail!("{key} must be a nonempty string of at most {maximum} characters.");
    }
    Ok(())
}

fn source_context(config: &Config, value: &Value, source: &str) -> Result<Value> {
    let failure = "sourceFolder must be an absolute existing directory path.";
    let path = value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 4096)
        .map(Path::new)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| anyhow::anyhow!(failure))?;
    let folder = path.canonicalize().map_err(|_| anyhow::anyhow!(failure))?;
    if !folder.is_dir() {
        bail!("{failure}");
    }
    let mut context = common::get_read_context(&folder, &common::read_settings(config))?;
    context["workspaceSource"] = json!(source);
    Ok(context)
}

#[derive(Default)]
struct Roots {
    supported: bool,
    generation: u64,
    cached: Option<(u64, Option<Value>)>,
    resolving: Option<u64>,
    next: u64,
    pending: HashMap<String, mpsc::Sender<Value>>,
}

#[derive(Default)]
struct Server {
    roots: Mutex<Roots>,
    changed: Condvar,
    output: Mutex<()>,
}

impl Server {
    fn send(&self, value: &Value) {
        let _guard = self.output.lock().unwrap();
        let mut out = io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, value);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }

    fn context(&self, config: &Config) -> Option<Value> {
        loop {
            let mut roots = self.roots.lock().unwrap();
            if !roots.supported {
                return None;
            }
            let generation = roots.generation;
            if let Some((cached_generation, value)) = &roots.cached {
                if *cached_generation == generation {
                    return value.clone();
                }
            }
            if roots.resolving == Some(generation) {
                drop(self.changed.wait(roots).unwrap());
                continue;
            }
            roots.resolving = Some(generation);
            roots.next += 1;
            let id = format!("remnifold-{}", roots.next);
            let (tx, rx) = mpsc::channel();
            roots.pending.insert(id.clone(), tx);
            self.changed.notify_all();
            drop(roots);
            self.send(&json!({"jsonrpc":"2.0","id":id,"method":"roots/list","params":{}}));
            let response = rx.recv_timeout(Duration::from_secs(1)).ok();
            let context = response.and_then(|value| {
                let mut folders = Vec::new();
                for root in value["roots"].as_array()? {
                    let Some(uri) = root["uri"].as_str() else {
                        continue;
                    };
                    let folder = url::Url::parse(uri)
                        .ok()
                        .and_then(|u| u.to_file_path().ok())
                        .and_then(|p| p.canonicalize().ok());
                    if let Some(folder) = folder {
                        if !folders.contains(&folder) {
                            folders.push(folder);
                        }
                    }
                }
                if folders.len() != 1 {
                    return None;
                }
                // インストール先そのものを作業プロジェクトとして記録しない。
                let executable_root = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent()?.parent().map(Path::to_path_buf))
                    .and_then(|p| p.canonicalize().ok());
                if executable_root.as_ref() == folders.first() {
                    return None;
                }
                source_context(config, &json!(folders[0].to_string_lossy()), "mcp-roots").ok()
            });
            let mut roots = self.roots.lock().unwrap();
            roots.pending.remove(&id);
            if roots.resolving == Some(generation) {
                roots.resolving = None;
            }
            if generation == roots.generation {
                roots.cached = Some((generation, context.clone()));
            }
            self.changed.notify_all();
            if generation == roots.generation {
                return context;
            }
        }
    }
}

fn call_tool(server: &Server, name: &str, args: &Value) -> Result<Value> {
    if !args.is_object() {
        bail!("Tool arguments must be an object.");
    }
    validate_string(args, "containerTag", 160, false)?;
    let config = common::load_config(None)?;
    let explicit = args
        .get("sourceFolder")
        .map(|v| source_context(&config, v, "sourceFolder"))
        .transpose()?;
    let workspace = || explicit.clone().or_else(|| server.context(&config));
    let tag = args["containerTag"].as_str().map(str::trim);
    let mut read_options = args.clone();
    if let Some(tag) = tag {
        read_options["containerTag"] = json!(tag);
    }
    if let Some(topic) = args["topic"].as_str() {
        read_options["topic"] = json!(topic.trim());
    }
    let request = |path: &str, method: &str, body: Option<&Value>, timeout| {
        common::api(&config, path, method, body, timeout)
    };
    match name {
        "search_memory" => {
            validate_string(args, "query", 1000, true)?;
            let result = search::search_index(&config, &read_options)?;
            let mut structured = result.clone();
            if args["includeProfile"] == true {
                structured["profile"] =
                    json!({"type":"space-index","containers":result["searchedContainers"]});
            }
            Ok(text_result(
                search::format_search_result(&result),
                structured,
            ))
        }
        "add_memory" => {
            let action = args.get("action").map(text).unwrap_or("save");
            if action != "save" && action != "forget" {
                bail!("action must be save or forget.");
            }
            validate_string(args, "content", 200000, action == "save")?;
            validate_string(args, "documentId", 64, false)?;
            if action == "save" && args.get("documentId").is_some() {
                bail!("documentId is valid only when action is forget.");
            }
            if action == "forget"
                && args.get("content").is_none()
                && args.get("documentId").is_none()
            {
                bail!("forget requires content or documentId.");
            }
            let container = tag.unwrap_or("memories");
            if action == "forget" {
                let mut body = json!({"containerTag":container});
                if let Some(v) = args.get("documentId") {
                    body["documentId"] = json!(text(v).trim());
                }
                if let Some(v) = args.get("content") {
                    body["content"] = v.clone();
                }
                let result = request("/v4/memories", "DELETE", Some(&body), 30000)?;
                return Ok(text_result(
                    text(&result["message"]),
                    json!({"action":"forget","success":true,"containerTag":container,"message":result["message"],"id":result["id"]}),
                ));
            }
            let context = if explicit.is_some() {
                explicit.clone()
            } else if tag.is_none() {
                workspace()
            } else {
                None
            };
            let mut metadata = json!({"sm_source":"codex-mcp","sm_scope":if context.is_some() { "project" } else { "shared" },"memoryIndex":search::build_memory_index("", text(&args["content"]), "", "", "explicit-memory")});
            if let Some(context) = context {
                metadata["project"] = context["projectName"].clone();
                metadata["sm_project_id"] = context["containerTag"].clone();
            }
            let result = request(
                "/v3/documents",
                "POST",
                Some(
                    &json!({"containerTag":container,"content":args["content"],"metadata":metadata}),
                ),
                30000,
            )?;
            let message = format!(
                "Memory saved in Cloudflare D1 (ID: {})",
                text(&result["id"])
            );
            Ok(text_result(
                &message,
                json!({"action":"save","success":true,"containerTag":container,"message":message,"id":result["id"],"status":result["status"]}),
            ))
        }
        "consolidate_memory" => {
            validate_string(args, "projectId", 160, false)?;
            let context = if args.get("projectId").is_none() {
                workspace()
            } else {
                None
            };
            let project = args["projectId"]
                .as_str()
                .map(str::trim)
                .or_else(|| context.as_ref().and_then(|v| v["containerTag"].as_str()))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "projectId or exactly one workspace root/sourceFolder is required."
                    )
                })?;
            let mut result = request(
                "/v4/consolidate",
                "POST",
                Some(&json!({"projectId":project,"force":true})),
                120000,
            )?;
            let message = result["message"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| match text(&result["status"]) {
                    "consolidated" => format!("Memory consolidation completed for {project}."),
                    "no_unconsolidated_memories" => {
                        format!("No unconsolidated memories were found for {project}.")
                    }
                    "not_due" => format!("Memory consolidation is not due for {project}."),
                    "busy" => format!("Memory consolidation is already running for {project}."),
                    _ => format!("Memory consolidation request finished for {project}."),
                });
            result["projectId"] = json!(project);
            Ok(text_result(message, result))
        }
        "listMemories" | "listDocuments" | "listTopics" => {
            let maximum = if name == "listTopics" { 100 } else { 50 };
            if args
                .get("page")
                .is_some_and(|v| v.as_u64().is_none_or(|n| n < 1))
            {
                bail!("page must be a positive integer.");
            }
            if args
                .get("limit")
                .is_some_and(|v| v.as_u64().is_none_or(|n| n < 1 || n > maximum))
            {
                bail!("limit must be an integer from 1 through {maximum}.");
            }
            if name == "listTopics" {
                let result = search::list_topics(&config, &read_options)?;
                return Ok(text_result(serde_json::to_string_pretty(&result)?, result));
            }
            validate_string(args, "topic", 80, false)?;
            let result = search::list_index(&config, &read_options)?;
            let mut structured = json!({"pagination":result["pagination"],"containerTag":result["containerTag"],"topic":result["topic"],"listScope":result["listScope"]});
            structured[if name == "listMemories" {
                "memoryEntries"
            } else {
                "documents"
            }] = result["documents"].clone();
            Ok(text_result(
                serde_json::to_string(&result["documents"])?,
                structured,
            ))
        }
        "getDocument" => {
            if !args["documentId"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            {
                bail!("documentId must be a nonempty string.");
            }
            validate_string(args, "sourceCursor", 2048, false)?;
            let cursor = args.get("sourceCursor").and_then(Value::as_str);
            let result = if cursor.is_some() {
                search::read_document_page(&config, text(&args["documentId"]).trim(), cursor)?
            } else {
                search::read_document(&config, text(&args["documentId"]).trim())?
            };
            Ok(text_result(
                text(&result["text"]),
                json!({"document":result["document"]}),
            ))
        }
        "listSpaces" => {
            let result = request("/v3/container-tags", "GET", None, 30000)?;
            Ok(text_result(
                serde_json::to_string_pretty(&result["spaces"])?,
                result,
            ))
        }
        "whoAmI" => {
            let context = workspace();
            let session = request("/v3/session", "GET", None, 30000)?;
            let discovery = search::discover_search_containers(&config)?;
            let value = json!({"userId":session["user"]["id"],"name":session["user"]["name"],"role":session["role"],"accessType":session["accessType"],"activeSpace":"memories","defaultStorageContainer":"memories","workspaceProvenance":context.as_ref().map(|v| &v["containerTag"]),"workspaceSource":context.as_ref().and_then(|v| v["workspaceSource"].as_str()).unwrap_or("unavailable"),"searchScope":"all-discovered-containers","readSpaces":discovery["tags"],"spaceDiscoveryComplete":discovery["complete"],"recallFormat":"document-index","storage":"cloudflare-d1"});
            Ok(text_result(serde_json::to_string(&value)?, value))
        }
        _ => bail!("Unknown tool"),
    }
}

pub fn start() -> Result<()> {
    let server = Arc::new(Server::default());
    let mut workers = Vec::new();
    // 読み取りは常時このスレッドに残し、roots応答をAPI処理と独立して受信する。
    let mut input = io::stdin().lock();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        // 1MiBを超える行は保存せず末尾まで消費する。
        let mut oversized = false;
        loop {
            let available = input.fill_buf()?;
            if available.is_empty() {
                break;
            }
            let length = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |n| n + 1);
            let finished = available[length - 1] == b'\n';
            if buffer.len() + length <= 1024 * 1024 {
                if !oversized {
                    buffer.extend_from_slice(&available[..length]);
                }
            } else {
                oversized = true;
                buffer.clear();
            }
            input.consume(length);
            if finished {
                break;
            }
        }
        if buffer.is_empty() && !oversized {
            break;
        }
        if oversized || buffer.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let message: Value = match serde_json::from_slice(&buffer) {
            Ok(v) => v,
            Err(_) => {
                server.send(&json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}));
                continue;
            }
        };
        if !message.is_object() {
            continue;
        }
        let method = message["method"].as_str().unwrap_or("");
        if method.is_empty() && message.get("id").is_some() {
            if let Some(id) = message["id"].as_str() {
                if let Some(sender) = server.roots.lock().unwrap().pending.remove(id) {
                    let _ = sender.send(message["result"].clone());
                }
            }
            continue;
        }
        if message.get("id").is_none() {
            if method == "notifications/roots/list_changed" {
                let mut roots = server.roots.lock().unwrap();
                roots.generation += 1;
                roots.cached = None;
                server.changed.notify_all();
            }
            continue;
        }
        let result = match method {
            "initialize" => {
                let mut roots = server.roots.lock().unwrap();
                roots.supported = message["params"]["capabilities"].get("roots").is_some();
                roots.generation += 1;
                roots.cached = None;
                server.changed.notify_all();
                Some(
                    json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"remnifold","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({"tools":tools()})),
            "resources/list" => Some(json!({"resources":[]})),
            "prompts/list" => Some(json!({"prompts":[]})),
            "tools/call" => {
                workers.retain(|w: &thread::JoinHandle<()>| !w.is_finished());
                let server = Arc::clone(&server);
                let message = message.clone();
                workers.push(thread::spawn(move || {
                    let args = message["params"].get("arguments").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!({}));
                    let result = call_tool(&server, text(&message["params"]["name"]), &args).unwrap_or_else(|error| json!({"content":[{"type":"text","text":safe_error(&error)}],"isError":true}));
                    server.send(&json!({"jsonrpc":"2.0","id":message["id"],"result":result}));
                }));
                None
            }
            _ => {
                server.send(&json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Method not found"}}));
                None
            }
        };
        if let Some(result) = result {
            server.send(&json!({"jsonrpc":"2.0","id":message["id"],"result":result}));
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

// 入力・URL・HTTP例外の本文をクライアントへ転送しない。
fn safe_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    let known = [
        "Tool arguments must be an object.",
        "sourceFolder must be an absolute existing directory path.",
        "action must be save or forget.",
        "documentId is valid only when action is forget.",
        "forget requires content or documentId.",
        "projectId or exactly one workspace root/sourceFolder is required.",
        "page must be a positive integer.",
        "limit must be an integer from 1 through 50.",
        "limit must be an integer from 1 through 100.",
        "documentId must be a nonempty string.",
        "Unknown tool",
    ];
    if known.contains(&message.as_str())
        || [
            "containerTag",
            "query",
            "content",
            "documentId",
            "projectId",
            "topic",
        ]
        .iter()
        .any(|key| {
            [64, 80, 160, 1000, 200000].iter().any(|limit| {
                message == format!("{key} must be a nonempty string of at most {limit} characters.")
            })
        })
    {
        message
    } else {
        "Remnifold request failed. Check connection settings and tool arguments.".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_retain_public_contract() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 9);
        for tool in tools {
            let has_source = tool["inputSchema"]["properties"]
                .get("sourceFolder")
                .is_some();
            assert_eq!(
                has_source,
                matches!(
                    text(&tool["name"]),
                    "add_memory" | "consolidate_memory" | "whoAmI"
                )
            );
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn utf16_validation_and_safe_errors() {
        assert!(validate_string(&json!({"query":"😀".repeat(500)}), "query", 1000, true).is_ok());
        let error =
            validate_string(&json!({"query":"😀".repeat(501)}), "query", 1000, true).unwrap_err();
        assert_eq!(
            safe_error(&error),
            "query must be a nonempty string of at most 1000 characters."
        );
        assert!(
            !safe_error(&anyhow::anyhow!("https://host/?api_key=secret-value"))
                .contains("secret-value")
        );
        assert!(
            !safe_error(&anyhow::anyhow!(
                "query must be a nonempty string of at most secret-value characters."
            ))
            .contains("secret-value")
        );
    }

    #[test]
    fn concurrent_roots_follow_changed_generation_and_cache() {
        let server = Arc::new(Server::default());
        server.roots.lock().unwrap().supported = true;
        let folder = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = Config {
            base_url: String::new(),
            api_key: String::new(),
            raw: json!({}),
            codex_home: folder.clone(),
            roots: crate::paths::Roots {
                config: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
            },
        };
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let server = server.clone();
                let config = config.clone();
                thread::spawn(move || server.context(&config))
            })
            .collect();
        let sender = |server: &Server, minimum: u64| {
            let mut roots = server.roots.lock().unwrap();
            while roots.next < minimum || roots.pending.is_empty() {
                let (guard, timeout) = server
                    .changed
                    .wait_timeout(roots, Duration::from_secs(2))
                    .unwrap();
                assert!(!timeout.timed_out(), "roots request missing");
                roots = guard;
            }
            roots
                .pending
                .get(&format!("remnifold-{minimum}"))
                .unwrap()
                .clone()
        };
        let first = sender(&server, 1);
        {
            let mut roots = server.roots.lock().unwrap();
            roots.generation += 1;
            roots.cached = None;
            server.changed.notify_all();
        }
        first.send(json!({"roots":[]})).unwrap();
        let second = sender(&server, 2);
        second.send(json!({"roots":[{"uri":"https://invalid.example/"},{"uri":url::Url::from_directory_path(&folder).unwrap().as_str()}]})).unwrap();
        let values: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(values[0].is_some());
        assert_eq!(values[0], values[1]);
        assert_eq!(values[0].as_ref().unwrap()["workspaceSource"], "mcp-roots");
        assert_eq!(server.context(&config), values[0]);
        assert_eq!(server.roots.lock().unwrap().next, 2);
    }
}
