//! Node/Wrangler-free Cloudflare provisioning. Remote changes require --apply.
use anyhow::{Result, anyhow, bail};
use reqwest::blocking::{Client, multipart};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const NAMES: [&str; 3] = ["remnifold", "kagayoi-memory", "cloudflare-supermemory"];
const CRONS: [&str; 2] = ["*/15 * * * *", "0 18 * * *"];
const API: &str = "https://api.cloudflare.com/client/v4";
const HELP: &str = "Remnifold server setup (no Node.js or Wrangler required)\n\nUsage: remnifold server-setup [options]\nDefault: authenticated read-only discovery. --apply creates/reuses resources, applies migrations and deploys.\n\n  --apply\n  --worker-name <name>\n  --database-name <name>\n  --vector-index-name <name>\n  --account-id <id>\n  --config <absolute-jsonc-path>   Existing installation input (never overwritten)\n  --state <absolute-json-path>    External resume configuration\n  --location <weur|eeur|apac|oc|wnam|enam>\n  --jurisdiction <eu|fedramp|us>\n  --api-key-env <name>            Default REMNIFOLD_API_KEY\n  --base-url <https-url>\n  --token-stdin                  Read Cloudflare API token from stdin\n  --help\n\nAuthentication: CLOUDFLARE_API_TOKEN (or --token-stdin); no Wrangler OAuth import.\nThe token needs account read, Workers scripts, D1 and Vectorize permissions.\nWithout explicit names: existing configuration, remnifold, kagayoi-memory, cloudflare-supermemory.\n";

#[derive(Default)]
struct Options {
    apply: bool,
    help: bool,
    token_stdin: bool,
    worker: Option<String>,
    database: Option<String>,
    vector: Option<String>,
    account: Option<String>,
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    location: Option<String>,
    jurisdiction: Option<String>,
    key_env: String,
    base: Option<String>,
}
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && s.as_bytes()[0] != b'-'
        && !s.ends_with('-')
}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn endpoint(s: &str) -> Result<String> {
    let u = url::Url::parse(s).map_err(|_| anyhow!("--base-url must be an absolute HTTPS URL."))?;
    if !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || !(u.scheme() == "https"
            || (u.scheme() == "http" && matches!(u.host_str(), Some("localhost" | "127.0.0.1"))))
    {
        bail!(
            "--base-url must use HTTPS (HTTP is accepted only for localhost), without credentials, query or fragment."
        );
    }
    Ok(s.trim_end_matches('/').to_owned())
}
fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options {
        key_env: "REMNIFOLD_API_KEY".into(),
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => o.apply = true,
            "--help" | "-h" => o.help = true,
            "--token-stdin" => o.token_stdin = true,
            flag => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.starts_with("--"))
                    .ok_or_else(|| anyhow!("Missing option value. Use server-setup --help."))?
                    .clone();
                match flag {
                    "--worker-name" => o.worker = Some(value),
                    "--database-name" => o.database = Some(value),
                    "--vector-index-name" => o.vector = Some(value),
                    "--account-id" => o.account = Some(value),
                    "--config" => o.config = Some(value.into()),
                    "--state" => o.state = Some(value.into()),
                    "--location" => o.location = Some(value),
                    "--jurisdiction" => o.jurisdiction = Some(value),
                    "--api-key-env" => o.key_env = value,
                    "--base-url" => o.base = Some(endpoint(&value)?),
                    _ => bail!("Unknown server-setup option. Use server-setup --help."),
                }
                i += 1;
            }
        }
        i += 1;
    }
    for n in [&o.worker, &o.database, &o.vector].into_iter().flatten() {
        if !valid_name(n) {
            bail!("Resource names must contain 1-63 lowercase letters, digits, or hyphens.");
        }
    }
    if !o
        .key_env
        .bytes()
        .enumerate()
        .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
        || o.key_env.is_empty()
    {
        bail!("--api-key-env must be a valid environment variable name.");
    }
    if o.location
        .as_deref()
        .is_some_and(|x| !["weur", "eeur", "apac", "oc", "wnam", "enam"].contains(&x))
    {
        bail!("Unsupported D1 location.");
    }
    if o.jurisdiction
        .as_deref()
        .is_some_and(|x| !["eu", "fedramp", "us"].contains(&x))
    {
        bail!("Unsupported D1 jurisdiction.");
    }
    if o.location.is_some() && o.jurisdiction.is_some() {
        bail!("Use either --location or --jurisdiction, not both.");
    }
    if o.account.as_deref().is_some_and(|x| !identifier(x)) {
        bail!("Invalid Cloudflare account ID.");
    }
    for p in [&o.config, &o.state].into_iter().flatten() {
        if !p.is_absolute() {
            bail!("Configuration and state paths must be absolute.");
        }
    }
    Ok(o)
}

/// The transport is internal: public CLI cannot redirect Cloudflare credentials.
trait Transport {
    fn api(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value>;
    fn upload(&self, path: &str, metadata: Value) -> Result<()>;
    fn sql_upload(&self, url: &str, sql: &str) -> Result<()>;
    fn verify(&self, base: &str, key: Option<&str>) -> Result<()>;
}
struct Http {
    client: Client,
    token: String,
}
fn failure(stage: &str) -> anyhow::Error {
    anyhow!(
        "Server setup failed while {stage}. No credentials were printed; rerun the same command to resume."
    )
}
impl Transport for Http {
    fn api(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self
            .client
            .request(
                method.parse().map_err(|_| failure("building request"))?,
                format!("{API}{path}"),
            )
            .bearer_auth(&self.token);
        if let Some(body) = body {
            r = r.json(&body);
        }
        let r = r.send().map_err(|_| failure("contacting Cloudflare API"))?;
        let status = r.status();
        let v: Value = r
            .json()
            .map_err(|_| failure("reading Cloudflare API response"))?;
        if !status.is_success() || v["success"] != true {
            return Err(failure("executing Cloudflare API request"));
        }
        Ok(v)
    }
    fn upload(&self, path: &str, metadata: Value) -> Result<()> {
        let form = multipart::Form::new()
            .part(
                "metadata",
                multipart::Part::text(metadata.to_string())
                    .mime_str("application/json")
                    .map_err(|_| failure("building Worker metadata"))?,
            )
            .part(
                "worker.js",
                multipart::Part::text(crate::embedded::WORKER_BUNDLE.to_owned())
                    .file_name("worker.js")
                    .mime_str("application/javascript+module")
                    .map_err(|_| failure("building Worker upload"))?,
            );
        let response = self
            .client
            .put(format!("{API}{path}"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .map_err(|_| failure("uploading Worker"))?;
        let status = response.status();
        let v: Value = response
            .json()
            .map_err(|_| failure("reading Worker upload"))?;
        if !status.is_success() || v["success"] != true {
            return Err(failure("deploying Worker"));
        }
        Ok(())
    }
    fn sql_upload(&self, url: &str, sql: &str) -> Result<()> {
        let u = url::Url::parse(url).map_err(|_| failure("validating D1 upload URL"))?;
        if u.scheme() != "https"
            || !u.username().is_empty()
            || u.password().is_some()
            || !u
                .host_str()
                .is_some_and(|h| h.ends_with(".r2.cloudflarestorage.com"))
        {
            return Err(failure("validating D1 upload URL"));
        }
        // Presigned R2 upload receives SQL only, never the account token or owner key.
        let r = self
            .client
            .put(url)
            .body(sql.to_owned())
            .send()
            .map_err(|_| failure("uploading D1 migration"))?;
        if !r.status().is_success() {
            return Err(failure("uploading D1 migration"));
        }
        Ok(())
    }
    fn verify(&self, base: &str, key: Option<&str>) -> Result<()> {
        verify_endpoint(&self.client, base, key)
    }
}
fn list(t: &dyn Transport, path: &str) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for page in 1..=1000 {
        let separator = if path.contains('?') { '&' } else { '?' };
        let v = t.api(
            "GET",
            &format!("{path}{separator}page={page}&per_page=100"),
            None,
        )?;
        let result = v["result"]
            .as_array()
            .ok_or_else(|| failure("reading resource list"))?;
        rows.extend(result.iter().cloned());
        if v.get("result_info").is_none()
            || v["result_info"]["total_pages"]
                .as_u64()
                .is_some_and(|n| page >= n)
            || (v["result_info"]["total_pages"].is_null() && result.len() < 100)
        {
            return Ok(rows);
        }
    }
    bail!("Cloudflare resource listing exceeded the page limit.")
}
fn binding<'a>(config: &'a Value, field: &str, name: &str) -> Option<&'a Value> {
    config[field]
        .as_array()?
        .iter()
        .find(|v| v["binding"] == name)
}
fn replace_binding(config: &mut Value, field: &str, name: &str, binding: Value) {
    let mut rows = config[field].as_array().cloned().unwrap_or_default();
    rows.retain(|v| v["binding"] != name);
    rows.push(binding);
    config[field] = json!(rows);
}
fn choose(explicit: &Option<String>, configured: Option<&str>, rows: &[Value]) -> Result<String> {
    let selected = explicit.as_deref().or(configured);
    if let Some(n) = selected {
        if !valid_name(n) {
            bail!("Existing configuration has an invalid resource name.");
        }
        return Ok(n.into());
    }
    Ok(NAMES
        .iter()
        .find(|n| rows.iter().any(|r| r["name"] == **n || r["id"] == **n))
        .unwrap_or(&NAMES[0])
        .to_string())
}
fn find<'a>(rows: &'a [Value], name: &str) -> Result<Option<&'a Value>> {
    let matches: Vec<_> = rows.iter().filter(|r| r["name"] == name).collect();
    if matches.len() > 1 {
        bail!("Multiple Cloudflare resources have the requested name.");
    }
    Ok(matches.first().copied())
}
fn vector_check(t: &dyn Transport, prefix: &str, name: &str) -> Result<()> {
    let v = t.api(
        "GET",
        &format!("{prefix}/vectorize/v2/indexes/{name}"),
        None,
    )?;
    if v["result"]["config"]["dimensions"] != 1024 || v["result"]["config"]["metric"] != "cosine" {
        bail!("Vectorize index must use 1024 dimensions and cosine metric.");
    }
    Ok(())
}
fn rediscover(t: &dyn Transport, path: &str, name: &str) -> Result<Value> {
    for attempt in 0..4 {
        if let Some(r) = find(&list(t, path)?, name)? {
            return Ok(r.clone());
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_millis(250 * (attempt + 1)));
        }
    }
    bail!("Created resource was not visible. Rerun the same command to resume.")
}
fn query(t: &dyn Transport, prefix: &str, id: &str, sql: &str) -> Result<Vec<Value>> {
    let v = t.api(
        "POST",
        &format!("{prefix}/d1/database/{id}/query"),
        Some(json!({"sql":sql,"params":[]})),
    )?;
    let results = v["result"]
        .as_array()
        .ok_or_else(|| failure("reading D1 query"))?;
    if results.is_empty() || results.iter().any(|x| x["success"] != true) {
        return Err(failure("executing D1 query"));
    }
    Ok(results
        .iter()
        .flat_map(|r| r["results"].as_array().into_iter().flatten().cloned())
        .collect())
}
#[cfg(test)]
fn migration_sql(t: &dyn Transport, prefix: &str, id: &str) -> Result<String> {
    migration_sql_table(t, prefix, id, "d1_migrations")
}
fn quoted_table(table: &str) -> Result<String> {
    if table.is_empty() || table.contains('\0') {
        bail!("Invalid migrations_table setting.");
    }
    Ok(format!("\"{}\"", table.replace('"', "\"\"")))
}
fn migration_sql_table(t: &dyn Transport, prefix: &str, id: &str, table: &str) -> Result<String> {
    let quoted = quoted_table(table)?;
    let exists = query(
        t,
        prefix,
        id,
        &format!(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='{}' COLLATE NOCASE",
            table.replace('\'', "''")
        ),
    )?;
    let applied = if exists.is_empty() {
        Vec::new()
    } else {
        query(t, prefix, id, &format!("SELECT name FROM {quoted}"))?
    };
    let mut sql = String::new();
    for (name, body) in crate::embedded::MIGRATIONS {
        if applied.iter().any(|r| r["name"] == *name) {
            continue;
        }
        if sql.is_empty() {
            sql.push_str(&format!("CREATE TABLE IF NOT EXISTS {quoted} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL);\n"));
        }
        sql.push_str(body);
        sql.push_str(&format!("\nINSERT INTO {quoted}(name) VALUES ('"));
        sql.push_str(&name.replace('\'', "''"));
        sql.push_str("');\n");
    }
    Ok(sql)
}
#[cfg(test)]
fn migrate(t: &dyn Transport, prefix: &str, id: &str) -> Result<()> {
    migrate_table(t, prefix, id, "d1_migrations")
}
fn migrate_table(t: &dyn Transport, prefix: &str, id: &str, table: &str) -> Result<()> {
    use md5::{Digest, Md5};
    let sql = migration_sql_table(t, prefix, id, table)?;
    if sql.is_empty() {
        return Ok(());
    }
    // D1's bulk-import service commits the whole SQL file atomically. Do not split
    // statements (triggers contain semicolons), or use REST query batch atomicity.
    let etag = crate::common::encode_hex(&Md5::digest(sql.as_bytes()));
    let path = format!("{prefix}/d1/database/{id}/import");
    let init = t.api("POST", &path, Some(json!({"action":"init","etag":etag})))?;
    let mut v = if let Some(url) = init["result"]["upload_url"].as_str() {
        let filename = init["result"]["filename"]
            .as_str()
            .ok_or_else(|| failure("initializing D1 import"))?;
        t.sql_upload(url, &sql)?;
        t.api(
            "POST",
            &path,
            Some(json!({"action":"ingest","etag":etag,"filename":filename})),
        )?
    } else {
        init
    };
    let started = Instant::now();
    loop {
        match v["result"]["status"].as_str() {
            Some("complete") if v["result"]["success"] == true => return Ok(()),
            Some("error") => return Err(failure("applying D1 migrations")),
            Some("active") | Some("queued") | Some("running") => (),
            _ => return Err(failure("reading D1 import status")),
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err(failure("waiting for D1 migrations (bounded timeout)"));
        }
        let bookmark = v["result"]["at_bookmark"]
            .as_str()
            .ok_or_else(|| failure("reading D1 import bookmark"))?
            .to_owned();
        std::thread::sleep(Duration::from_secs(1));
        v = t.api(
            "POST",
            &path,
            Some(json!({"action":"poll","current_bookmark":bookmark})),
        )?;
    }
}

#[cfg(test)]
fn metadata(config: &Value, id: &str, vector: &str, key: Option<&str>) -> Result<Value> {
    metadata_existing(config, id, vector, key, &json!({}))
}
fn metadata_existing(
    config: &Value,
    id: &str,
    vector: &str,
    key: Option<&str>,
    existing: &Value,
) -> Result<Value> {
    validate_config_fields(config)?;
    validate_binding_names(config, existing)?;
    if existing.get("assets").is_some_and(|v| !v.is_null()) || existing["has_assets"] == true {
        bail!("Existing Worker assets are unsupported; setup stopped before changes.");
    }
    let mut bindings = vec![
        json!({"name":"DB","type":"d1","id":id}),
        json!({"name":"MEMORY_VECTORS","type":"vectorize","index_name":vector}),
        json!({"name":"AI","type":"ai"}),
    ];
    for (field, primary) in [("d1_databases", "DB"), ("vectorize", "MEMORY_VECTORS")] {
        if let Some(rows) = config[field].as_array() {
            for row in rows {
                let name = row["binding"]
                    .as_str()
                    .ok_or_else(|| failure("reading additional bindings"))?;
                if name == primary {
                    continue;
                }
                let binding = if field == "d1_databases" {
                    if row["database_id"].as_str().is_none_or(|s| !identifier(s)) {
                        return Err(failure("validating additional D1 binding"));
                    }
                    json!({"name":name,"type":"d1","id":row["database_id"]})
                } else {
                    if row["index_name"].as_str().is_none_or(|s| !valid_name(s)) {
                        return Err(failure("validating additional Vectorize binding"));
                    }
                    json!({"name":name,"type":"vectorize","index_name":row["index_name"]})
                };
                bindings.push(binding);
            }
        }
    }
    for (field, kind, target, source) in [
        ("kv_namespaces", "kv_namespace", "namespace_id", "id"),
        ("r2_buckets", "r2_bucket", "bucket_name", "bucket_name"),
        ("services", "service", "service", "service"),
    ] {
        if let Some(rows) = config[field].as_array() {
            for row in rows {
                let name = row["binding"]
                    .as_str()
                    .ok_or_else(|| failure("reading configured binding"))?;
                if name.is_empty()
                    || matches!(name, "DB" | "MEMORY_VECTORS" | "AI" | "MEMORY_API_KEY")
                    || row[source].as_str().is_none_or(|s| s.is_empty())
                {
                    return Err(failure("validating configured binding"));
                }
                let mut b = json!({"name":name,"type":kind});
                b[target] = row[source].clone();
                for extra in ["environment", "entrypoint", "jurisdiction"] {
                    if let Some(v) = row.get(extra) {
                        b[extra] = v.clone();
                    }
                }
                bindings.push(b);
            }
        }
    }
    if let Some(rows) = config["queues"]["producers"].as_array() {
        for row in rows {
            if row["binding"].as_str().is_none_or(|s| {
                s.is_empty() || matches!(s, "DB" | "MEMORY_VECTORS" | "AI" | "MEMORY_API_KEY")
            }) || row["queue"].as_str().is_none_or(str::is_empty)
            {
                return Err(failure("validating Queue producer binding"));
            }
            let mut binding =
                json!({"name":row["binding"],"type":"queue","queue_name":row["queue"]});
            if let Some(delay) = row.get("delivery_delay") {
                binding["delivery_delay"] = delay.clone();
            }
            bindings.push(binding);
        }
    }
    if let Some(vars) = config["vars"].as_object() {
        for (name, value) in vars {
            if matches!(
                name.as_str(),
                "DB" | "MEMORY_VECTORS" | "AI" | "MEMORY_API_KEY"
            ) {
                bail!("Configuration vars must not contain binding or secret names.");
            }
            bindings.push(if value.is_string() {
                json!({"name":name,"type":"plain_text","text":value})
            } else {
                json!({"name":name,"type":"json","json":value})
            });
        }
    }
    if let Some(key) = key {
        bindings.push(json!({"name":"MEMORY_API_KEY","type":"secret_text","text":key}));
    }
    let mut merged = Vec::new();
    if let Some(remote) = existing["bindings"].as_array() {
        for binding in remote {
            let kind = binding["type"]
                .as_str()
                .ok_or_else(|| failure("reading existing binding"))?;
            if matches!(kind, "secret_text" | "secret_key") {
                continue;
            }
            if ![
                "plain_text",
                "json",
                "kv_namespace",
                "r2_bucket",
                "service",
                "d1",
                "vectorize",
                "ai",
                "queue",
            ]
            .contains(&kind)
            {
                bail!(
                    "Existing Worker has an unsupported binding type; setup stopped before changes."
                );
            }
            merged.push(binding.clone());
        }
    }
    for binding in bindings {
        merged.retain(|v| v["name"] != binding["name"]);
        merged.push(binding);
    }
    if !merged.iter().any(|b| b["name"] == "AI_ENRICHMENT_MODE") {
        merged.push(json!({"name":"AI_ENRICHMENT_MODE","type":"plain_text","text":"on"}));
    }
    let mut metadata = json!({"main_module":"worker.js","bindings":merged,"keep_bindings":["secret_text","secret_key"]});
    for field in [
        "compatibility_date",
        "compatibility_flags",
        "observability",
        "limits",
        "placement",
        "tail_consumers",
        "tags",
        "logpush",
        "usage_model",
    ] {
        if let Some(v) = config.get(field).or_else(|| existing.get(field)) {
            metadata[field] = v.clone();
        }
    }
    if metadata.get("compatibility_date").is_none() {
        metadata["compatibility_date"] = json!("2026-09-02");
    }
    if metadata.get("observability").is_none() {
        metadata["observability"] =
            json!({"enabled":true,"logs":{"enabled":true,"head_sampling_rate":1}});
    }
    Ok(metadata)
}
fn validate_binding_names(config: &Value, existing: &Value) -> Result<()> {
    let reserved = |name: &str| matches!(name, "DB" | "MEMORY_VECTORS" | "AI" | "MEMORY_API_KEY");
    let mut names = std::collections::HashSet::new();
    for field in [
        "d1_databases",
        "vectorize",
        "kv_namespaces",
        "r2_buckets",
        "services",
    ] {
        if let Some(rows) = config[field].as_array() {
            for row in rows {
                let name = row["binding"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| failure("validating binding name"))?;
                let required = field == "d1_databases" && name == "DB"
                    || field == "vectorize" && name == "MEMORY_VECTORS";
                if reserved(name) && !required || !names.insert(name.to_owned()) {
                    bail!(
                        "Configuration has a reserved or duplicate binding name; setup stopped before changes."
                    );
                }
            }
        }
    }
    if let Some(rows) = config["queues"]["producers"].as_array() {
        for row in rows {
            let name = row["binding"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure("validating Queue binding name"))?;
            if reserved(name) || !names.insert(name.to_owned()) {
                bail!(
                    "Configuration has a reserved or duplicate binding name; setup stopped before changes."
                );
            }
        }
    }
    if let Some(vars) = config["vars"].as_object() {
        for name in vars.keys() {
            if reserved(name) || !names.insert(name.to_owned()) {
                bail!(
                    "Configuration vars conflict with a required or configured binding; setup stopped before changes."
                );
            }
        }
    }
    if config["ai"].get("binding").is_some_and(|v| v != "AI") {
        bail!("The required AI binding cannot be renamed.");
    }
    let mut remote_names = std::collections::HashSet::new();
    if let Some(rows) = existing["bindings"].as_array() {
        for binding in rows {
            let name = binding["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure("validating existing binding name"))?;
            if !remote_names.insert(name) {
                bail!("Existing Worker has duplicate binding names; setup stopped before changes.");
            }
            let expected = match name {
                "DB" => Some("d1"),
                "MEMORY_VECTORS" => Some("vectorize"),
                "AI" => Some("ai"),
                "MEMORY_API_KEY" => Some("secret_text"),
                _ => None,
            };
            if expected.is_some_and(|kind| binding["type"] != kind) {
                bail!(
                    "Existing Worker has a conflicting required binding type; setup stopped before changes."
                );
            }
        }
    }
    Ok(())
}
fn validate_config_fields(config: &Value) -> Result<()> {
    let allowed = [
        "$schema",
        "name",
        "main",
        "account_id",
        "base_url",
        "base_url_source",
        "compatibility_date",
        "compatibility_flags",
        "workers_dev",
        "preview_urls",
        "triggers",
        "vars",
        "ai",
        "vectorize",
        "d1_databases",
        "observability",
        "secrets",
        "routes",
        "route",
        "kv_namespaces",
        "r2_buckets",
        "services",
        "queues",
        "limits",
        "placement",
        "tail_consumers",
        "tags",
        "logpush",
        "usage_model",
    ];
    for key in config
        .as_object()
        .ok_or_else(|| failure("reading configuration"))?
        .keys()
    {
        if !allowed.contains(&key.as_str()) {
            bail!(
                "Configuration contains an unsupported Worker setting; setup stopped before changes."
            );
        }
    }
    if config
        .get("base_url_source")
        .is_some_and(|v| v != "explicit" && v != "discovered")
    {
        bail!("Invalid base_url_source setting.");
    }
    for field in [
        "routes",
        "kv_namespaces",
        "r2_buckets",
        "services",
        "d1_databases",
        "vectorize",
        "compatibility_flags",
        "tail_consumers",
        "tags",
    ] {
        if config.get(field).is_some_and(|v| !v.is_array()) {
            return Err(failure("validating Worker configuration array"));
        }
    }
    for field in ["vars", "triggers", "queues", "ai"] {
        if config.get(field).is_some_and(|v| !v.is_object()) {
            return Err(failure("validating Worker configuration object"));
        }
    }
    for field in ["workers_dev", "preview_urls", "logpush"] {
        if config.get(field).is_some_and(|v| !v.is_boolean()) {
            return Err(failure("validating Worker configuration flag"));
        }
    }
    if let Some(crons) = config["triggers"].get("crons") {
        if !crons.is_array()
            || crons
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().is_none_or(str::is_empty))
        {
            return Err(failure("validating Worker cron configuration"));
        }
    }
    if config["queues"].get("consumers").is_some() {
        bail!("Queue consumer configuration is unsupported; setup stopped before changes.");
    }
    for (field, allowed) in [
        (
            "kv_namespaces",
            &["binding", "id", "preview_id", "remote"][..],
        ),
        (
            "r2_buckets",
            &[
                "binding",
                "bucket_name",
                "preview_bucket_name",
                "jurisdiction",
                "remote",
            ][..],
        ),
        (
            "services",
            &["binding", "service", "environment", "entrypoint"][..],
        ),
        (
            "d1_databases",
            &[
                "binding",
                "database_id",
                "database_name",
                "migrations_dir",
                "migrations_table",
                "preview_database_id",
                "remote",
            ][..],
        ),
        ("vectorize", &["binding", "index_name", "remote"][..]),
    ] {
        if let Some(rows) = config[field].as_array() {
            for row in rows {
                let fields = row
                    .as_object()
                    .ok_or_else(|| failure("validating binding configuration object"))?;
                if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
                    bail!(
                        "Configured binding contains an unsupported setting; setup stopped before changes."
                    );
                }
            }
        }
    }
    for (object, allowed) in [
        (&config["triggers"], &["crons"][..]),
        (&config["queues"], &["producers"][..]),
        (&config["ai"], &["binding", "remote"][..]),
    ] {
        if object
            .as_object()
            .is_some_and(|o| o.keys().any(|key| !allowed.contains(&key.as_str())))
        {
            bail!(
                "Configuration contains an unsupported nested Worker setting; setup stopped before changes."
            );
        }
    }
    if let Some(rows) = config["queues"]["producers"].as_array() {
        for row in rows {
            if row.as_object().is_none_or(|o| {
                o.keys()
                    .any(|key| !["binding", "queue", "delivery_delay"].contains(&key.as_str()))
            }) {
                bail!(
                    "Queue producer contains an unsupported setting; setup stopped before changes."
                );
            }
        }
    }
    Ok(())
}
struct RouteChange {
    method: String,
    path: String,
    body: Value,
}
struct RoutePlan {
    changes: Vec<RouteChange>,
    public_endpoint: Option<String>,
}
fn plan_routes(
    t: &dyn Transport,
    prefix: &str,
    account: &str,
    worker: &str,
    config: &Value,
) -> Result<RoutePlan> {
    let mut configured = config["routes"].as_array().cloned().unwrap_or_default();
    if let Some(route) = config.get("route") {
        if !route.is_null() {
            configured.push(route.clone());
        }
    }
    if configured.is_empty() {
        return Ok(RoutePlan {
            changes: Vec::new(),
            public_endpoint: None,
        });
    }
    let mut changes = Vec::new();
    let mut zones: Option<Vec<Value>> = None;
    let mut domains: Option<Vec<Value>> = None;
    let mut zone_routes = std::collections::HashMap::<String, Vec<Value>>::new();
    let mut public_endpoint = None;
    for route in configured {
        if !route.is_string() {
            let object = route
                .as_object()
                .ok_or_else(|| anyhow!("Worker route must be a string or object."))?;
            if object.keys().any(|key| {
                ![
                    "pattern",
                    "zone_id",
                    "zone_name",
                    "custom_domain",
                    "enabled",
                    "previews_enabled",
                ]
                .contains(&key.as_str())
            }) {
                bail!(
                    "Worker route contains an unsupported setting; setup stopped before changes."
                );
            }
            for key in ["pattern", "zone_id", "zone_name"] {
                if object.get(key).is_some_and(|v| !v.is_string()) {
                    bail!("Worker route string field has an invalid type.");
                }
            }
            for key in ["custom_domain", "enabled", "previews_enabled"] {
                if object.get(key).is_some_and(|v| !v.is_boolean()) {
                    bail!("Worker route boolean field has an invalid type.");
                }
            }
        }
        let pattern = route
            .as_str()
            .or_else(|| route["pattern"].as_str())
            .ok_or_else(|| anyhow!("Worker route must have a pattern."))?;
        if pattern.is_empty()
            || pattern.chars().any(char::is_control)
            || pattern.contains(['?', '#', '@'])
        {
            bail!("Invalid Worker route pattern.");
        }
        let host = pattern
            .strip_prefix("https://")
            .or_else(|| pattern.strip_prefix("http://"))
            .unwrap_or(pattern)
            .split('/')
            .next()
            .unwrap_or("")
            .trim_start_matches("*.");
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            bail!("Worker route hostname is invalid.");
        }
        let custom = route["custom_domain"] == true;
        if !custom && (route.get("enabled").is_some() || route.get("previews_enabled").is_some()) {
            bail!("Domain enable and preview settings require custom_domain: true.");
        }
        if custom && (pattern != host || pattern.contains('*')) {
            bail!("Custom domain patterns must be hostnames without wildcards, paths or schemes.");
        }
        let zone = if let Some(id) = route["zone_id"].as_str() {
            if !identifier(id) {
                bail!("Invalid route zone ID.");
            }
            id.to_owned()
        } else {
            if zones.is_none() {
                zones = Some(list(t, &format!("/zones?account.id={account}"))?);
            }
            let requested = route["zone_name"].as_str();
            let matches = zones
                .as_ref()
                .unwrap()
                .iter()
                .filter(|zone| {
                    zone["name"].as_str().is_some_and(|name| {
                        requested.map_or_else(
                            || host == name || host.ends_with(&format!(".{name}")),
                            |r| name == r,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let zone = matches
                .iter()
                .max_by_key(|zone| zone["name"].as_str().unwrap_or("").len())
                .ok_or_else(|| {
                    anyhow!("No accessible Cloudflare zone matches the Worker route.")
                })?;
            let id = zone["id"]
                .as_str()
                .filter(|id| identifier(id))
                .ok_or_else(|| failure("reading route zone"))?;
            id.to_owned()
        };
        if custom {
            if domains.is_none() {
                domains = Some(list(t, &format!("{prefix}/workers/domains"))?);
            }
            let found = domains.as_ref().unwrap().iter().find(|d| {
                d["hostname"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(host))
            });
            let enabled = route["enabled"]
                .as_bool()
                .or_else(|| found.and_then(|d| d["enabled"].as_bool()))
                .unwrap_or(true);
            let previews = route["previews_enabled"]
                .as_bool()
                .or_else(|| found.and_then(|d| d["previews_enabled"].as_bool()))
                .unwrap_or(false);
            if !enabled && !previews {
                bail!("Custom domain must enable hostname or preview routing.");
            }
            if enabled && public_endpoint.is_none() {
                public_endpoint = Some(format!("https://{host}"));
            }
            if let Some(found) = found {
                if found["service"] != worker
                    || found["environment"]
                        .as_str()
                        .is_some_and(|e| e != "production")
                {
                    bail!(
                        "Custom domain already belongs to a different Worker; setup stopped before changes."
                    );
                }
                if found["zone_id"] != zone {
                    bail!("Custom domain has a conflicting zone; setup stopped before changes.");
                }
                if found["enabled"].as_bool().unwrap_or(true) == enabled
                    && found["previews_enabled"].as_bool().unwrap_or(false) == previews
                {
                    continue;
                }
            }
            changes.push(RouteChange{method:"PUT".into(),path:format!("{prefix}/workers/domains"),body:json!({"hostname":host,"service":worker,"environment":"production","zone_id":zone,"enabled":enabled,"previews_enabled":previews})});
        } else {
            if !zone_routes.contains_key(&zone) {
                zone_routes.insert(
                    zone.clone(),
                    list(t, &format!("/zones/{zone}/workers/routes"))?,
                );
            }
            if let Some(found) = zone_routes[&zone].iter().find(|r| r["pattern"] == pattern) {
                if found["script"] != worker {
                    bail!(
                        "Worker route already belongs to a different Worker; setup stopped before changes."
                    );
                }
                continue;
            }
            changes.push(RouteChange {
                method: "POST".into(),
                path: format!("/zones/{zone}/workers/routes"),
                body: json!({"pattern":pattern,"script":worker}),
            });
        }
    }
    Ok(RoutePlan {
        changes,
        public_endpoint,
    })
}
fn jsonc_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}
fn read_config(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).map_err(|_| failure("reading configuration"))?;
    let v: Value =
        jsonc_parser::parse_to_serde_value(text.trim_start_matches('\u{feff}'), &jsonc_options())
            .map_err(|_| failure("parsing JSONC configuration"))?;
    if !v.is_object() {
        bail!("Configuration must be a JSON object.");
    }
    Ok(v)
}
fn state_path(o: &Options) -> Result<PathBuf> {
    if let Some(p) = &o.state {
        return Ok(p.clone());
    }
    // 既定は Remnifold 自身の設定フォルダ（crate::paths）。旧来の Codex home にだけある状態は一度だけ移す。
    crate::paths::server_state_file(
        &crate::paths::Roots::current(),
        &crate::common::codex_home(),
        o.apply,
    )
}
fn validate_state(path: &Path) -> Result<()> {
    // State is user-owned, never the plugin source or cache. Reject links before writing.
    let absolute = path.is_absolute();
    if !absolute {
        bail!("Server state path must be absolute.");
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        bail!("Server state path must not contain parent or current-directory components.");
    }
    let within_repo = runtime_roots(None)?.iter().any(|root| within(path, root));
    if within_repo
        || path.components().any(|c| {
            c.as_os_str().to_str().is_some_and(|s| {
                matches!(s.to_ascii_lowercase().as_str(), "plugins" | "marketplaces")
            })
        })
    {
        bail!("Server state must be outside the repository and plugin directories.");
    }
    for p in path.ancestors() {
        if let Ok(m) = fs::symlink_metadata(p) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    bail!("Server state path has a linked ancestor.");
                }
            }
            if m.file_type().is_symlink() {
                bail!("Server state path has a linked ancestor.");
            }
        }
    }
    Ok(())
}
fn within(path: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let p = path
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\");
        let r = root
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\");
        p == r || p.starts_with(&format!("{}\\", r.trim_end_matches('\\')))
    } else {
        path.starts_with(root)
    }
}
fn marked_root(path: &Path) -> bool {
    path.join(".git").exists()
        || path.join(".codex-plugin/plugin.json").exists()
        || (path.join("native/Cargo.toml").is_file() && path.join("server").is_dir())
}
fn runtime_roots(input: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    let cwd = env::current_dir().map_err(|_| failure("resolving current directory"))?;
    let mut starts = vec![cwd];
    if let Ok(exe) = env::current_exe() {
        if let Some(p) = exe.parent() {
            starts.push(p.to_path_buf());
        }
    }
    if let Some(input) = input {
        if let Some(p) = input.parent() {
            starts.push(p.to_path_buf());
        }
        if let Ok(resolved) = fs::canonicalize(input) {
            if let Some(p) = resolved.parent() {
                starts.push(p.to_path_buf());
            }
        }
    }
    for start in starts {
        for ancestor in start.ancestors() {
            if marked_root(ancestor) && !roots.iter().any(|r| r == ancestor) {
                roots.push(ancestor.to_path_buf());
            }
        }
    }
    Ok(roots)
}
fn same_input(input: &Path, state: &Path) -> Result<bool> {
    let input = crate::common::absolute_path(input)?;
    let state = crate::common::absolute_path(state)?;
    if within(&input, &state) && within(&state, &input) {
        return Ok(true);
    }
    if input.exists() && state.exists() {
        return same_file::is_same_file(&input, &state)
            .map_err(|_| failure("checking configuration identity"));
    }
    Ok(false)
}
fn validate_output(o: &Options, state: &Path) -> Result<()> {
    validate_state(state)?;
    if let Some(input) = &o.config {
        if same_input(input, state)? {
            bail!("--config is read-only and must not identify the --state file.");
        }
        if runtime_roots(Some(input))?
            .iter()
            .any(|root| within(state, root))
        {
            bail!("Server state must be outside the runtime repository and plugin roots.");
        }
    }
    Ok(())
}
fn discover_config() -> Result<Option<PathBuf>> {
    for root in runtime_roots(None)? {
        let path = root.join("server/wrangler.jsonc");
        if path.is_file() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
fn load_setup_config(o: &mut Options, state: &Path, discovered: Option<PathBuf>) -> Result<Value> {
    if o.config.is_none() && !state.exists() {
        o.config = discovered;
    }
    if let Some(input) = &o.config {
        read_config(input)
    } else if state.exists() {
        read_config(state)
    } else {
        Ok(json!({}))
    }
}
fn explicit_base_url(o: &Options, config: &Value) -> Result<Option<String>> {
    if let Some(base) = o.base.as_deref() {
        return Ok(Some(endpoint(base)?));
    }
    let Some(base) = config["base_url"].as_str() else {
        return Ok(None);
    };
    let base = endpoint(base)?;
    if let (Some(previous), Some(current)) = (config["name"].as_str(), o.worker.as_deref()) {
        if previous != current
            && url::Url::parse(&base)
                .ok()
                .and_then(|u| {
                    u.host_str().map(|host| {
                        host.ends_with(".workers.dev") && host.starts_with(&format!("{previous}."))
                    })
                })
                .unwrap_or(false)
        {
            return Ok(None);
        }
    }
    match config["base_url_source"].as_str() {
        Some("explicit") => return Ok(Some(base)),
        Some("discovered") => return Ok(None),
        _ => (),
    }
    // Old resume files did not record provenance. workers.dev and configured
    // custom domains can be rediscovered. An explicitly supplied input file, or
    // an external endpoint without configured routes, remains authoritative.
    if o.config.is_some() {
        return Ok(Some(base));
    }
    let routed = config["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(config.get("route"))
        .any(|r| r["custom_domain"] == true);
    let workers_dev = url::Url::parse(&base)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.ends_with(".workers.dev")))
        .unwrap_or(false);
    if routed || workers_dev {
        Ok(None)
    } else {
        Ok(Some(base))
    }
}
fn write_state(path: &Path, config: &Value) -> Result<()> {
    validate_state(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| failure("resolving server state"))?;
    fs::create_dir_all(parent).map_err(|_| failure("creating external state directory"))?;
    let mut public = config.clone();
    public.as_object_mut().unwrap().remove("secrets");
    if public["vars"].get("MEMORY_API_KEY").is_some() {
        bail!("Configuration must not store MEMORY_API_KEY in vars.");
    }
    crate::common::atomic_write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&public)?).as_bytes(),
        true,
    )
    .map_err(|_| failure("saving external server state"))
}

fn execute(
    t: &dyn Transport,
    o: &Options,
    mut config: Value,
    state: &Path,
    key: Option<&str>,
) -> Result<()> {
    validate_output(o, state)?;
    validate_config_fields(&config)?;
    let explicit_base = explicit_base_url(o, &config)?;
    let account = if let Some(a) = o.account.as_deref().or(config["account_id"].as_str()) {
        a.to_owned()
    } else {
        let accounts = list(t, "/accounts")?;
        if accounts.len() != 1 {
            bail!("Specify --account-id when the token can access zero or multiple accounts.");
        }
        accounts[0]["id"]
            .as_str()
            .ok_or_else(|| failure("selecting account"))?
            .into()
    };
    if !identifier(&account) {
        bail!("Invalid Cloudflare account ID.");
    }
    let prefix = format!("/accounts/{account}");
    let dbpath = format!("{prefix}/d1/database");
    let vpath = format!("{prefix}/vectorize/v2/indexes");
    let databases = list(t, &dbpath)?;
    let vectors = list(t, &vpath)?;
    let workers = list(t, &format!("{prefix}/workers/scripts"))?;
    let old_db = binding(&config, "d1_databases", "DB");
    let migration_table = if let Some(value) = old_db.and_then(|db| db.get("migrations_table")) {
        value
            .as_str()
            .ok_or_else(|| anyhow!("migrations_table must be a nonempty string."))?
    } else {
        "d1_migrations"
    }
    .to_owned();
    quoted_table(&migration_table)?;
    let mut primary_db = old_db.cloned().unwrap_or(json!({}));
    let old_vector = binding(&config, "vectorize", "MEMORY_VECTORS");
    let worker = choose(&o.worker, config["name"].as_str(), &workers)?;
    let database = choose(
        &o.database,
        old_db.and_then(|v| v["database_name"].as_str()),
        &databases,
    )?;
    let vector = choose(
        &o.vector,
        old_vector.and_then(|v| v["index_name"].as_str()),
        &vectors,
    )?;
    let mut db = find(&databases, &database)?.cloned();
    let v = find(&vectors, &vector)?;
    if o.database.is_none()
        && let Some(old) = old_db
    {
        if db.is_none()
            || old["database_id"]
                .as_str()
                .is_some_and(|id| db.as_ref().unwrap()["uuid"] != id)
        {
            bail!("Configured D1 database is missing or has a conflicting ID.");
        }
    }
    if o.vector.is_none() && old_vector.is_some() && v.is_none() {
        bail!("Configured Vectorize index is missing.");
    }
    if v.is_some() {
        vector_check(t, &prefix, &vector)?;
    }
    let workerpath = format!("{prefix}/workers/scripts/{worker}");
    let worker_exists = workers
        .iter()
        .any(|w| w["id"] == worker || w["name"] == worker);
    let has_key = if worker_exists {
        t.api("GET", &format!("{workerpath}/secrets"), None)?["result"]
            .as_array()
            .ok_or_else(|| failure("reading Worker secrets"))?
            .iter()
            .any(|s| s["name"] == "MEMORY_API_KEY")
    } else {
        false
    };
    let existing = if worker_exists {
        t.api("GET", &format!("{workerpath}/settings"), None)?["result"].clone()
    } else {
        json!({})
    };
    if worker_exists {
        let domain = t.api("GET", &format!("{workerpath}/subdomain"), None)?;
        if !domain["result"]["enabled"].is_boolean()
            || !domain["result"]["previews_enabled"].is_boolean()
        {
            return Err(failure("reading existing Worker subdomain settings"));
        }
        if config.get("workers_dev").is_none() {
            config["workers_dev"] = domain["result"]["enabled"].clone();
        }
        if config.get("preview_urls").is_none() {
            config["preview_urls"] = domain["result"]["previews_enabled"].clone();
        }
        let schedules = t.api("GET", &format!("{workerpath}/schedules"), None)?;
        let remote = schedules["result"]["schedules"]
            .as_array()
            .or_else(|| schedules["result"].as_array())
            .ok_or_else(|| failure("reading existing schedules"))?;
        let mut crons = config["triggers"]["crons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for schedule in remote {
            if let Some(cron) = schedule["cron"].as_str() {
                if !crons.contains(&json!(cron)) {
                    crons.push(json!(cron));
                }
            }
        }
        config["triggers"] = json!({"crons":crons});
    }
    let route_changes = plan_routes(t, &prefix, &account, &worker, &config)?;
    metadata_existing(&config, "pending", &vector, key, &existing)?;
    println!(
        "Mode: {}\nCloudflare authentication: ready\nD1: {}\nVectorize: {}\nWorker: {}\nMEMORY_API_KEY: {}",
        if o.apply {
            "apply"
        } else {
            "dry run (read-only; pass --apply to make changes)"
        },
        if db.is_some() { "reuse" } else { "create" },
        if v.is_some() { "reuse" } else { "create" },
        worker,
        if key.is_some() {
            "supplied (hidden)"
        } else if has_key {
            "preserve existing"
        } else {
            "required before apply"
        }
    );
    if !o.apply {
        return Ok(());
    }
    if key.is_none() && !has_key {
        bail!(
            "MEMORY_API_KEY is not available on the Worker. Supply it using --api-key-env before applying."
        );
    }
    // Validate configuration and state location before any remote mutation.
    validate_state(state)?;
    config["account_id"] = json!(account);
    config["name"] = json!(worker);
    write_state(state, &config)?;
    if db.is_none() {
        let mut body = json!({"name":database});
        if let Some(l) = &o.location {
            body["primary_location_hint"] = json!(l);
        }
        if let Some(j) = &o.jurisdiction {
            body["jurisdiction"] = json!(j);
        }
        // A failed response may follow successful creation. Rediscovery resolves it safely.
        let created = t.api("POST", &dbpath, Some(body));
        db = Some(rediscover(t, &dbpath, &database).map_err(|_| {
            created
                .err()
                .unwrap_or_else(|| failure("discovering created D1 database"))
        })?);
    }
    let id = db.as_ref().unwrap()["uuid"]
        .as_str()
        .ok_or_else(|| failure("reading D1 ID"))?
        .to_owned();
    if !identifier(&id) {
        bail!("Invalid D1 ID.");
    }
    primary_db["binding"] = json!("DB");
    primary_db["database_name"] = json!(database);
    primary_db["database_id"] = json!(id);
    primary_db["migrations_dir"] = json!("migrations");
    replace_binding(&mut config, "d1_databases", "DB", primary_db);
    // Persist D1 immediately so a later Vectorize failure resumes the same database.
    write_state(state, &config)?;
    if v.is_none() {
        let created = t.api(
            "POST",
            &vpath,
            Some(json!({"name":vector,"config":{"dimensions":1024,"metric":"cosine"}})),
        );
        rediscover(t, &vpath, &vector).map_err(|_| {
            created
                .err()
                .unwrap_or_else(|| failure("discovering created Vectorize index"))
        })?;
    }
    vector_check(t, &prefix, &vector)?;
    replace_binding(
        &mut config,
        "vectorize",
        "MEMORY_VECTORS",
        json!({"binding":"MEMORY_VECTORS","index_name":vector}),
    );
    write_state(state, &config)?;
    migrate_table(t, &prefix, &id, &migration_table)?;
    println!("D1 migrations: applied");
    // Upload sets the supplied secret together with the first real Worker, avoiding
    // an unauthenticated placeholder deployment. Existing unknown secrets survive.
    t.upload(
        &workerpath,
        metadata_existing(&config, &id, &vector, key, &existing)?,
    )?;
    for change in route_changes.changes {
        t.api(&change.method, &change.path, Some(change.body))?;
    }
    t.api("POST",&format!("{workerpath}/subdomain"),Some(json!({"enabled":config["workers_dev"]!=false,"previews_enabled":config["preview_urls"]==true})))?;
    let mut crons = config["triggers"]["crons"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for cron in CRONS {
        if !crons.contains(&json!(cron)) {
            crons.push(json!(cron));
        }
    }
    t.api(
        "PUT",
        &format!("{workerpath}/schedules"),
        Some(Value::Array(
            crons.iter().map(|c| json!({"cron":c})).collect(),
        )),
    )?;
    config["triggers"] = json!({"crons":crons});
    let base = if let Some(base) = &explicit_base {
        Some(base.clone())
    } else if let Some(base) = route_changes.public_endpoint {
        Some(endpoint(&base)?)
    } else if config["workers_dev"] != false {
        let sub = t.api("GET", &format!("{prefix}/workers/subdomain"), None)?;
        let s = sub["result"]["subdomain"]
            .as_str()
            .ok_or_else(|| failure("reading workers.dev subdomain"))?;
        if !valid_name(s) {
            bail!("Invalid workers.dev subdomain.");
        }
        Some(format!("https://{worker}.{s}.workers.dev"))
    } else {
        None
    };
    config["base_url_source"] = json!(if explicit_base.is_some() {
        "explicit"
    } else {
        "discovered"
    });
    if let Some(base) = &base {
        config["base_url"] = json!(base);
    } else {
        config.as_object_mut().unwrap().remove("base_url");
    }
    write_state(state, &config)?;
    if let Some(base) = base {
        t.verify(&base, key)?;
        println!(
            "Deployment health: ready{}",
            if key.is_some() {
                "; authenticated session and MCP: ready"
            } else {
                "; authenticated checks withheld (owner key not supplied)"
            }
        );
    } else {
        println!(
            "Deployment complete; endpoint verification withheld: supply --base-url for the existing custom domain."
        );
    }
    Ok(())
}

fn verify_endpoint(client: &Client, base: &str, key: Option<&str>) -> Result<()> {
    let health = client
        .get(format!("{base}/health"))
        .send()
        .map_err(|_| failure("verifying deployment health"))?;
    if !health.status().is_success() {
        return Err(failure("verifying deployment health"));
    }
    health
        .bytes()
        .map_err(|_| failure("reading deployment health"))?;
    let Some(key) = key else {
        return Ok(());
    };
    let session = client
        .get(format!("{base}/v3/session"))
        .bearer_auth(key)
        .send()
        .map_err(|_| failure("verifying authenticated session"))?;
    if !session.status().is_success() {
        bail!("Authenticated session verification failed.");
    }
    session
        .bytes()
        .map_err(|_| failure("reading authenticated session"))?;
    let rpc = |id: u64, method: &str, params: Value, version: Option<&str>| -> Result<Value> {
        let mut r = client
            .post(format!("{base}/mcp"))
            .bearer_auth(key)
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        if let Some(version) = version {
            r = r.header("MCP-Protocol-Version", version);
        }
        let r = r.send().map_err(|_| failure("verifying remote MCP"))?;
        let status = r.status();
        let v: Value = r.json().map_err(|_| failure("verifying remote MCP"))?;
        if !status.is_success()
            || v["jsonrpc"] != "2.0"
            || v["id"] != id
            || v.get("error").is_some()
            || !v["result"].is_object()
        {
            bail!("Remote MCP verification failed.");
        }
        Ok(v["result"].clone())
    };
    let init = rpc(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"remnifold-setup","version":"1"}}),
        None,
    )?;
    let version = init["protocolVersion"]
        .as_str()
        .filter(|s| ["2025-03-26", "2025-06-18", "2025-11-25"].contains(s))
        .ok_or_else(|| anyhow!("Remote MCP initialize verification failed."))?;
    let tools = rpc(2, "tools/list", json!({}), Some(version))?;
    if !["search_memory", "getDocument", "add_memory"]
        .iter()
        .all(|n| {
            tools["tools"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["name"] == *n))
        })
    {
        bail!("Remote MCP tool discovery verification failed.");
    }
    Ok(())
}
pub fn run(args: &[String]) -> Result<()> {
    let mut o = parse(args)?;
    if o.help {
        print!("{HELP}");
        return Ok(());
    }
    let state = state_path(&o)?;
    let discovered = if o.config.is_none() && !state.exists() {
        discover_config()?
    } else {
        None
    };
    let config = load_setup_config(&mut o, &state, discovered)?;
    let token = if o.token_stdin {
        let mut token = String::new();
        io::stdin()
            .take(65537)
            .read_to_string(&mut token)
            .map_err(|_| failure("reading Cloudflare token"))?;
        token.trim().to_owned()
    } else {
        env::var("CLOUDFLARE_API_TOKEN").unwrap_or_default()
    };
    if token.is_empty() || token.len() > 65536 || token.chars().any(char::is_control) {
        bail!(
            "Supply CLOUDFLARE_API_TOKEN or --token-stdin (Cloudflare API token, not Wrangler OAuth)."
        );
    }
    let key = env::var(&o.key_env)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| failure("initializing HTTP client"))?;
    execute(&Http { client, token }, &o, config, &state, key.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Fixture {
        calls: RefCell<Vec<(String, String, Option<Value>)>>,
        db: RefCell<bool>,
        vector: RefCell<bool>,
        existing: bool,
        worker_name: String,
        remote_settings: RefCell<Value>,
        remote_subdomain: RefCell<Value>,
        remote_crons: RefCell<Vec<Value>>,
        domains: RefCell<Vec<Value>>,
        routes: RefCell<Vec<Value>>,
        owner_key: RefCell<String>,
        upload_failure: bool,
        import_resume: u8,
        dims: u64,
        create_failure: bool,
        imported: RefCell<String>,
        applied: bool,
        ledger_name: String,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                calls: RefCell::new(vec![]),
                db: RefCell::new(false),
                vector: RefCell::new(false),
                existing: false,
                worker_name: "remnifold".into(),
                remote_settings: RefCell::new(json!({"bindings":[]})),
                remote_subdomain: RefCell::new(json!({"enabled":true,"previews_enabled":false})),
                remote_crons: RefCell::new(vec![]),
                domains: RefCell::new(vec![]),
                routes: RefCell::new(vec![]),
                owner_key: RefCell::new("fixture-old-key".into()),
                upload_failure: false,
                import_resume: 0,
                dims: 1024,
                create_failure: false,
                imported: RefCell::new(String::new()),
                applied: false,
                ledger_name: "d1_migrations".into(),
            }
        }
    }
    impl Transport for Fixture {
        fn api(&self, m: &str, p: &str, b: Option<Value>) -> Result<Value> {
            self.calls
                .borrow_mut()
                .push((m.into(), p.into(), b.clone()));
            let path = p.split('?').next().unwrap();
            let body = b.unwrap_or(Value::Null);
            let r = if path == "/accounts" {
                json!([{"id":"account"}])
            } else if path == "/zones" {
                json!([{"id":"zone-id","name":"example.test"}])
            } else if path.ends_with("/workers/domains") {
                if m == "PUT" {
                    self.domains
                        .borrow_mut()
                        .retain(|d| d["hostname"] != body["hostname"]);
                    self.domains.borrow_mut().push(body.clone());
                }
                json!(*self.domains.borrow())
            } else if path.ends_with("/workers/routes") {
                if m == "POST" {
                    self.routes.borrow_mut().push(body.clone());
                }
                json!(*self.routes.borrow())
            } else if path.ends_with("/settings") {
                self.remote_settings.borrow().clone()
            } else if path.ends_with("/d1/database") {
                if m == "POST" {
                    *self.db.borrow_mut() = true;
                    if self.create_failure {
                        return Err(failure("creating D1"));
                    }
                }
                if *self.db.borrow() {
                    json!([{"name":"remnifold","uuid":"database-id"}])
                } else {
                    json!([])
                }
            } else if path.ends_with("/vectorize/v2/indexes") {
                if m == "POST" {
                    *self.vector.borrow_mut() = true;
                }
                if *self.vector.borrow() {
                    json!([{"name":"remnifold"}])
                } else {
                    json!([])
                }
            } else if path.ends_with("/vectorize/v2/indexes/remnifold") {
                json!({"config":{"dimensions":self.dims,"metric":"cosine"}})
            } else if path.ends_with("/workers/scripts") {
                if self.existing {
                    json!([{"id":self.worker_name}])
                } else {
                    json!([])
                }
            } else if path.ends_with("/secrets") {
                if m == "PUT" {
                    *self.owner_key.borrow_mut() = body["text"].as_str().unwrap().into();
                }
                json!([{"name":"MEMORY_API_KEY"}])
            } else if path.ends_with("/query") {
                let sql = body["sql"].as_str().unwrap();
                let rows = if sql.contains("sqlite_master") && self.applied {
                    json!([{"name":self.ledger_name}])
                } else if sql
                    == format!(
                        "SELECT name FROM {}",
                        quoted_table(&self.ledger_name).unwrap()
                    )
                {
                    json!(
                        crate::embedded::MIGRATIONS
                            .iter()
                            .map(|(name, _)| json!({"name":name}))
                            .collect::<Vec<_>>()
                    )
                } else {
                    json!([])
                };
                json!([{"success":true,"results":rows}])
            } else if path.ends_with("/import") {
                if body["action"] == "init" {
                    match self.import_resume {
                        1 => json!({"status":"active","at_bookmark":"bookmark"}),
                        2 => json!({"status":"complete","success":true}),
                        _ => {
                            json!({"filename":"fixture.sql","upload_url":"https://fixture.r2.cloudflarestorage.com/sql"})
                        }
                    }
                } else {
                    json!({"status":"complete","success":true})
                }
            } else if path.ends_with("/workers/subdomain") {
                json!({"subdomain":"fixture"})
            } else if path.ends_with("/schedules") {
                if m == "PUT" {
                    *self.remote_crons.borrow_mut() = body.as_array().unwrap().clone();
                }
                json!({"schedules":*self.remote_crons.borrow()})
            } else if path.ends_with("/subdomain") {
                if m == "POST" {
                    *self.remote_subdomain.borrow_mut() = body;
                }
                self.remote_subdomain.borrow().clone()
            } else {
                panic!("unexpected fixture request {m} {p}")
            };
            Ok(json!({"success":true,"result":r}))
        }
        fn upload(&self, path: &str, body: Value) -> Result<()> {
            if self.upload_failure {
                return Err(failure("uploading Worker fixture"));
            }
            if let Some(key) = body["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["name"] == "MEMORY_API_KEY")
                .and_then(|b| b["text"].as_str())
            {
                *self.owner_key.borrow_mut() = key.into();
            }
            let mut stored = body.clone();
            if let Some(previous) = self.remote_settings.borrow()["bindings"].as_array() {
                for binding in previous {
                    if body["keep_bindings"]
                        .as_array()
                        .is_some_and(|types| types.contains(&binding["type"]))
                        && !stored["bindings"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|b| b["name"] == binding["name"])
                    {
                        stored["bindings"]
                            .as_array_mut()
                            .unwrap()
                            .push(binding.clone());
                    }
                }
            }
            *self.remote_settings.borrow_mut() = stored;
            self.calls
                .borrow_mut()
                .push(("UPLOAD".into(), path.into(), Some(body)));
            Ok(())
        }
        fn sql_upload(&self, _: &str, sql: &str) -> Result<()> {
            *self.imported.borrow_mut() = sql.into();
            Ok(())
        }
        fn verify(&self, base: &str, key: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push((
                "VERIFY".into(),
                base.into(),
                Some(json!({"key_supplied":key.is_some()})),
            ));
            Ok(())
        }
    }
    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }
    fn state() -> PathBuf {
        env::temp_dir()
            .join(format!("remnifold-provision-{}", uuid::Uuid::new_v4()))
            .join("config.json")
    }
    #[test]
    fn argument_boundaries() {
        for a in [
            args(&["--worker-name", "Bad name"]),
            args(&["--location", "apac", "--jurisdiction", "eu"]),
            args(&["--base-url", "http://example.com"]),
            args(&["--base-url", "https://user:secret@example.com"]),
            args(&["--config", "relative.json"]),
            args(&["--api-key-env", "BAD-NAME"]),
            args(&["--profile", "legacy"]),
        ] {
            assert!(parse(&a).is_err());
        }
        assert!(parse(&args(&["--base-url", "http://localhost:9000"])).is_ok());
    }
    #[test]
    fn dry_run_is_read_only() {
        let f = Fixture::new();
        let p = state();
        execute(&f, &parse(&[]).unwrap(), json!({}), &p, None).unwrap();
        assert!(!p.exists());
        assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
    }
    #[test]
    fn missing_owner_key_rejects_before_creating() {
        let f = Fixture::new();
        assert!(
            execute(
                &f,
                &parse(&args(&["--apply"])).unwrap(),
                json!({}),
                &state(),
                None
            )
            .is_err()
        );
        assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
    }
    #[test]
    fn conflicting_database_id_rejects() {
        let f = Fixture::new();
        *f.db.borrow_mut() = true;
        let c = json!({"d1_databases":[{"binding":"DB","database_name":"remnifold","database_id":"wrong"}]});
        assert!(
            execute(&f, &parse(&[]).unwrap(), c, &state(), None)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
    }
    #[test]
    fn missing_configured_vector_rejects() {
        let f = Fixture::new();
        let c = json!({"vectorize":[{"binding":"MEMORY_VECTORS","index_name":"remnifold"}]});
        assert!(execute(&f, &parse(&[]).unwrap(), c, &state(), None).is_err());
    }
    #[test]
    fn incompatible_vector_rejects() {
        let mut f = Fixture::new();
        *f.vector.borrow_mut() = true;
        f.dims = 768;
        assert!(
            execute(&f, &parse(&[]).unwrap(), json!({}), &state(), None)
                .unwrap_err()
                .to_string()
                .contains("1024")
        );
    }
    #[test]
    fn legacy_priority_and_explicit_names() {
        let rows = vec![
            json!({"name":"cloudflare-supermemory"}),
            json!({"name":"kagayoi-memory"}),
        ];
        assert_eq!(choose(&None, None, &rows).unwrap(), "kagayoi-memory");
        assert_eq!(choose(&None, Some("custom"), &rows).unwrap(), "custom");
        assert_eq!(
            choose(&Some("explicit".into()), Some("custom"), &rows).unwrap(),
            "explicit"
        );
    }
    #[test]
    fn create_response_failure_recovers_by_discovery() {
        let mut f = Fixture::new();
        f.create_failure = true;
        let p = state();
        execute(
            &f,
            &parse(&args(&["--apply"])).unwrap(),
            json!({}),
            &p,
            Some("owner-secret"),
        )
        .unwrap();
        assert!(p.exists());
        assert!(!fs::read_to_string(&p).unwrap().contains("owner-secret"));
    }
    #[test]
    fn apply_sets_bindings_crons_and_verifies() {
        let f = Fixture::new();
        execute(
            &f,
            &parse(&args(&["--apply"])).unwrap(),
            json!({"triggers":{"crons":["0 1 * * *"]},"vars":{"CUSTOM":"preserved"}}),
            &state(),
            Some("owner-secret"),
        )
        .unwrap();
        let calls = f.calls.borrow();
        let upload = calls
            .iter()
            .find(|(m, _, _)| m == "UPLOAD")
            .unwrap()
            .2
            .as_ref()
            .unwrap();
        assert!(
            upload["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "MEMORY_API_KEY" && v["text"] == "owner-secret")
        );
        assert!(
            upload["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "CUSTOM")
        );
        let cron = calls
            .iter()
            .find(|(_, p, _)| p.ends_with("/schedules"))
            .unwrap()
            .2
            .as_ref()
            .unwrap();
        assert_eq!(cron.as_array().unwrap().len(), 3);
        assert!(calls.iter().any(|(m, _, _)| m == "VERIFY"));
    }
    #[test]
    fn existing_secret_is_preserved_without_key() {
        let mut f = Fixture::new();
        f.existing = true;
        execute(
            &f,
            &parse(&args(&["--apply"])).unwrap(),
            json!({}),
            &state(),
            None,
        )
        .unwrap();
        let calls = f.calls.borrow();
        let upload = calls
            .iter()
            .find(|(m, _, _)| m == "UPLOAD")
            .unwrap()
            .2
            .as_ref()
            .unwrap();
        assert_eq!(
            upload["keep_bindings"],
            json!(["secret_text", "secret_key"])
        );
        assert!(
            !upload["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["name"] == "MEMORY_API_KEY")
        );
        assert!(
            calls
                .iter()
                .any(|(m, _, b)| m == "VERIFY" && b.as_ref().unwrap()["key_supplied"] == false)
        );
    }
    #[test]
    fn legacy_workers_are_reused_without_resupplying_owner_key() {
        for name in ["kagayoi-memory", "cloudflare-supermemory"] {
            let mut f = Fixture::new();
            f.existing = true;
            f.worker_name = name.into();
            let path = state();
            execute(
                &f,
                &parse(&args(&["--apply"])).unwrap(),
                json!({}),
                &path,
                None,
            )
            .unwrap();
            let calls = f.calls.borrow();
            let upload = calls
                .iter()
                .find(|(method, _, _)| method == "UPLOAD")
                .unwrap();
            assert!(upload.1.ends_with(&format!("/scripts/{name}")));
            assert!(
                !calls
                    .iter()
                    .any(|(_, path, _)| path.contains("/scripts/remnifold"))
            );
            assert_eq!(read_config(&path).unwrap()["name"], name);
        }
    }
    #[test]
    fn migration_ledger_prevents_reapplication() {
        let mut f = Fixture::new();
        f.applied = true;
        assert!(
            migration_sql(&f, "/accounts/account", "database-id")
                .unwrap()
                .is_empty()
        );
        migrate(&f, "/accounts/account", "database-id").unwrap();
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(_, p, _)| p.ends_with("/import"))
        );
    }
    #[test]
    fn migration_sql_is_one_import_including_triggers_and_ledger() {
        let f = Fixture::new();
        migrate(&f, "/accounts/account", "database-id").unwrap();
        let sql = f.imported.borrow();
        assert!(sql.contains("CREATE TRIGGER"));
        for (name, body) in crate::embedded::MIGRATIONS {
            assert!(sql.contains(body));
            assert!(sql.contains(&format!("VALUES ('{name}')")));
        }
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(&sql).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM d1_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, crate::embedded::MIGRATIONS.len() as i64);
    }
    #[test]
    fn vars_cannot_expose_owner_key() {
        assert!(
            metadata(
                &json!({"vars":{"MEMORY_API_KEY":"secret"}}),
                "id",
                "index",
                None
            )
            .is_err()
        );
    }
    #[test]
    fn existing_remote_bindings_settings_crons_and_subdomain_survive_upload() {
        let mut f = Fixture::new();
        f.existing = true;
        *f.remote_settings.borrow_mut() = json!({"compatibility_date":"2026-08-01","compatibility_flags":["nodejs_compat"],"limits":{"cpu_ms":50},"bindings":[{"name":"CACHE","type":"kv_namespace","namespace_id":"kv-id"},{"name":"BUCKET","type":"r2_bucket","bucket_name":"uploads"},{"name":"SERVICE","type":"service","service":"other-worker"},{"name":"QUEUE","type":"queue","queue_name":"jobs"},{"name":"OTHER_DB","type":"d1","id":"other-id"},{"name":"AI_ENRICHMENT_MODE","type":"plain_text","text":"off"},{"name":"OTHER_SECRET","type":"secret_text"},{"name":"KEY","type":"secret_key"}]});
        *f.remote_subdomain.borrow_mut() = json!({"enabled":false,"previews_enabled":true});
        *f.remote_crons.borrow_mut() = vec![json!({"cron":"0 9 * * *"})];
        execute(
            &f,
            &parse(&args(&["--apply"])).unwrap(),
            json!({"kv_namespaces":[{"binding":"CACHE","id":"replacement"}]}),
            &state(),
            None,
        )
        .unwrap();
        let uploaded = f.remote_settings.borrow();
        let bindings = uploaded["bindings"].as_array().unwrap();
        for name in [
            "CACHE",
            "BUCKET",
            "SERVICE",
            "QUEUE",
            "OTHER_DB",
            "AI_ENRICHMENT_MODE",
            "OTHER_SECRET",
            "KEY",
        ] {
            assert!(bindings.iter().any(|b| b["name"] == name));
        }
        assert_eq!(
            bindings.iter().find(|b| b["name"] == "CACHE").unwrap()["namespace_id"],
            "replacement"
        );
        assert_eq!(
            bindings
                .iter()
                .find(|b| b["name"] == "AI_ENRICHMENT_MODE")
                .unwrap()["text"],
            "off"
        );
        assert_eq!(
            uploaded["keep_bindings"],
            json!(["secret_text", "secret_key"])
        );
        assert_eq!(uploaded["compatibility_flags"], json!(["nodejs_compat"]));
        assert_eq!(uploaded["compatibility_date"], "2026-08-01");
        assert_eq!(uploaded["limits"]["cpu_ms"], 50);
        assert_eq!(f.remote_subdomain.borrow()["enabled"], false);
        assert_eq!(f.remote_subdomain.borrow()["previews_enabled"], true);
        assert!(
            f.remote_crons
                .borrow()
                .iter()
                .any(|c| c["cron"] == "0 9 * * *")
        );
        assert_eq!(f.owner_key.borrow().as_str(), "fixture-old-key");
    }
    #[test]
    fn unsupported_binding_or_setting_rejects_before_remote_changes() {
        for unsupported_remote in [false, true] {
            let mut f = Fixture::new();
            f.existing = true;
            let config = if unsupported_remote {
                *f.remote_settings.borrow_mut() = json!({"bindings":[{"name":"OBJECT","type":"durable_object_namespace","namespace_id":"opaque"}]});
                json!({})
            } else {
                json!({"assets":{"directory":"public"}})
            };
            assert!(
                execute(
                    &f,
                    &parse(&args(&["--apply"])).unwrap(),
                    config,
                    &state(),
                    None
                )
                .is_err()
            );
            assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
        }
    }
    #[test]
    fn owner_key_changes_only_with_successful_worker_upload() {
        let mut f = Fixture::new();
        f.existing = true;
        f.upload_failure = true;
        assert!(
            execute(
                &f,
                &parse(&args(&["--apply"])).unwrap(),
                json!({}),
                &state(),
                Some("fixture-new-key")
            )
            .is_err()
        );
        assert_eq!(f.owner_key.borrow().as_str(), "fixture-old-key");
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "PUT" && p.ends_with("/secrets"))
        );
    }
    #[test]
    fn all_local_binding_types_and_vars_reject_reserved_names_or_duplicates_before_changes() {
        for config in [
            json!({"d1_databases":[{"binding":"AI","database_id":"aux-id"}]}),
            json!({"vectorize":[{"binding":"MEMORY_API_KEY","index_name":"index"}]}),
            json!({"d1_databases":[{"binding":"DB"},{"binding":"DB"}]}),
            json!({"vectorize":[{"binding":"MEMORY_VECTORS"},{"binding":"MEMORY_VECTORS"}]}),
            json!({"services":[{"binding":"X","service":"other"}],"vars":{"X":"duplicate"}}),
            json!({"d1_databases":[{"binding":"X","database_id":"aux-id"}],"kv_namespaces":[{"binding":"X","id":"namespace"}]}),
            json!({"queues":{"producers":[{"binding":"DB","queue":"queue"}]}}),
            json!({"ai":{"binding":"OTHER"}}),
        ] {
            let f = Fixture::new();
            assert!(
                execute(
                    &f,
                    &parse(&args(&["--apply"])).unwrap(),
                    config,
                    &state(),
                    Some("fixture-key")
                )
                .is_err()
            );
            assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
        }
        for existing in [
            json!({"bindings":[{"name":"AI","type":"d1","id":"id"}]}),
            json!({"bindings":[{"name":"MEMORY_API_KEY","type":"plain_text","text":"bad"}]}),
            json!({"bindings":[{"name":"X","type":"plain_text","text":"one"},{"name":"X","type":"plain_text","text":"two"}]}),
        ] {
            assert!(metadata_existing(&json!({}), "id", "index", None, &existing).is_err());
        }
    }
    #[test]
    fn custom_domain_flags_match_config_or_preserve_existing_state() {
        let options = parse(&args(&["--apply"])).unwrap();
        for existing in [false, true] {
            let f = Fixture::new();
            if existing {
                f.domains.borrow_mut().push(json!({"hostname":"preview.example.test","service":"remnifold","zone_id":"zone-id","enabled":true,"previews_enabled":false}));
            }
            execute(&f,&options,json!({"workers_dev":false,"routes":[{"pattern":"preview.example.test","custom_domain":true,"enabled":false,"previews_enabled":true}]}),&state(),Some("fixture-key")).unwrap();
            let domains = f.domains.borrow();
            let domain = domains
                .iter()
                .find(|d| d["hostname"] == "preview.example.test")
                .unwrap();
            assert_eq!(domain["enabled"], false);
            assert_eq!(domain["previews_enabled"], true);
            assert!(!f.calls.borrow().iter().any(|(m, _, _)| m == "VERIFY"));
        }
        let f = Fixture::new();
        f.domains.borrow_mut().push(json!({"hostname":"preview.example.test","service":"remnifold","zone_id":"zone-id","enabled":false,"previews_enabled":true}));
        execute(&f,&options,json!({"workers_dev":false,"routes":[{"pattern":"preview.example.test","custom_domain":true}]}),&state(),Some("fixture-key")).unwrap();
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "PUT" && p.ends_with("/domains"))
        );
        assert_eq!(f.domains.borrow()[0]["enabled"], false);
        assert_eq!(f.domains.borrow()[0]["previews_enabled"], true);
    }
    #[test]
    fn invalid_route_types_and_domain_routing_states_reject_before_changes() {
        for route in [
            json!({"pattern":"preview.example.test","custom_domain":"true"}),
            json!({"pattern":"preview.example.test","custom_domain":true,"enabled":"false"}),
            json!({"pattern":"preview.example.test","custom_domain":true,"enabled":false,"previews_enabled":false}),
            json!({"pattern":"api.example.test/*","enabled":false}),
            json!({"pattern":"preview.example.test","custom_domain":true,"unknown":true}),
        ] {
            let f = Fixture::new();
            assert!(
                execute(
                    &f,
                    &parse(&args(&["--apply"])).unwrap(),
                    json!({"routes":[route]}),
                    &state(),
                    Some("fixture-key")
                )
                .is_err()
            );
            assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
        }
    }
    #[test]
    fn custom_migration_ledger_is_quoted_and_preserved_without_reapplying_history() {
        let mut f = Fixture::new();
        f.applied = true;
        f.ledger_name = "custom\"'; DROP TABLE memories; --".into();
        assert!(
            migration_sql_table(&f, "/accounts/account", "database-id", &f.ledger_name)
                .unwrap()
                .is_empty()
        );
        *f.db.borrow_mut() = true;
        let path = state();
        execute(&f,&parse(&args(&["--apply"])).unwrap(),json!({"d1_databases":[{"binding":"DB","database_name":"remnifold","database_id":"database-id","migrations_table":f.ledger_name}]}),&path,Some("fixture-key")).unwrap();
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(_, p, _)| p.ends_with("/import"))
        );
        assert_eq!(
            read_config(&path).unwrap()["d1_databases"][0]["migrations_table"],
            f.ledger_name
        );
        let fresh = Fixture::new();
        let sql = migration_sql_table(&fresh, "/accounts/account", "database-id", &f.ledger_name)
            .unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(&sql).unwrap();
        let count: i64 = db
            .query_row(
                &format!(
                    "SELECT count(*) FROM {}",
                    quoted_table(&f.ledger_name).unwrap()
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, crate::embedded::MIGRATIONS.len() as i64);
        assert!(db.prepare("SELECT id FROM memories").is_ok());
        assert!(quoted_table("").is_err());
        assert!(quoted_table("bad\0name").is_err());
    }
    #[test]
    fn persisted_discovery_is_recomputed_for_current_worker_and_custom_domain() {
        let f = Fixture::new();
        let path = state();
        let apply = parse(&args(&["--apply"])).unwrap();
        execute(&f, &apply, json!({}), &path, Some("fixture-key")).unwrap();
        let config = read_config(&path).unwrap();
        assert_eq!(config["base_url_source"], "discovered");
        assert_eq!(config["base_url"], "https://remnifold.fixture.workers.dev");
        f.calls.borrow_mut().clear();
        execute(
            &f,
            &parse(&args(&["--apply", "--worker-name", "other"])).unwrap(),
            config,
            &path,
            Some("fixture-key"),
        )
        .unwrap();
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://other.fixture.workers.dev")
        );
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://remnifold.fixture.workers.dev")
        );
        let mut config = read_config(&path).unwrap();
        config["routes"] = json!([{"pattern":"current.example.test","custom_domain":true}]);
        f.calls.borrow_mut().clear();
        execute(&f, &apply, config, &path, Some("fixture-key")).unwrap();
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://current.example.test")
        );
    }
    #[test]
    fn explicit_url_provenance_and_legacy_endpoint_settings_remain_authoritative() {
        let f = Fixture::new();
        let path = state();
        let options = parse(&args(&[
            "--apply",
            "--base-url",
            "https://explicit.example.test",
        ]))
        .unwrap();
        execute(&f, &options, json!({}), &path, Some("fixture-key")).unwrap();
        let config = read_config(&path).unwrap();
        assert_eq!(config["base_url_source"], "explicit");
        f.calls.borrow_mut().clear();
        execute(
            &f,
            &parse(&args(&["--apply", "--worker-name", "other"])).unwrap(),
            config,
            &path,
            Some("fixture-key"),
        )
        .unwrap();
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://explicit.example.test")
        );
        let mut input = parse(&[]).unwrap();
        input.config = Some(path.clone());
        assert_eq!(
            explicit_base_url(
                &input,
                &json!({"base_url":"https://fixed.example.workers.dev"})
            )
            .unwrap(),
            Some("https://fixed.example.workers.dev".into())
        );
        assert_eq!(
            explicit_base_url(
                &parse(&[]).unwrap(),
                &json!({"base_url":"https://legacy.example.test"})
            )
            .unwrap(),
            Some("https://legacy.example.test".into())
        );
        assert_eq!(
            explicit_base_url(
                &parse(&[]).unwrap(),
                &json!({"base_url":"https://old.fixture.workers.dev"})
            )
            .unwrap(),
            None
        );
        assert_eq!(explicit_base_url(&parse(&[]).unwrap(),&json!({"base_url":"https://old.example.test","routes":[{"pattern":"current.example.test","custom_domain":true}]})).unwrap(),None);
    }
    #[test]
    fn automatically_discovered_config_has_the_same_explicit_url_origin_as_config_flag() {
        let input = state();
        fs::create_dir_all(input.parent().unwrap()).unwrap();
        fs::write(&input,json!({"base_url":"https://explicit.example.test","routes":[{"pattern":"current.example.test","custom_domain":true}]}).to_string()).unwrap();
        let mut options = parse(&args(&["--apply"])).unwrap();
        let output = state();
        let config = load_setup_config(&mut options, &output, Some(input.clone())).unwrap();
        assert_eq!(options.config.as_ref(), Some(&input));
        assert_eq!(
            explicit_base_url(&options, &config).unwrap(),
            Some("https://explicit.example.test".into())
        );
        let f = Fixture::new();
        execute(&f, &options, config, &output, Some("fixture-key")).unwrap();
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://explicit.example.test")
        );
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://current.example.test")
        );
    }
    #[test]
    fn saved_explicit_workers_dev_url_is_rebased_after_worker_override_unless_repeated_on_cli() {
        let f = Fixture::new();
        let path = state();
        execute(
            &f,
            &parse(&args(&[
                "--apply",
                "--base-url",
                "https://remnifold.fixture.workers.dev",
            ]))
            .unwrap(),
            json!({}),
            &path,
            Some("fixture-key"),
        )
        .unwrap();
        let config = read_config(&path).unwrap();
        assert_eq!(config["base_url_source"], "explicit");
        f.calls.borrow_mut().clear();
        execute(
            &f,
            &parse(&args(&["--apply", "--worker-name", "other"])).unwrap(),
            config.clone(),
            &path,
            Some("fixture-key"),
        )
        .unwrap();
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://other.fixture.workers.dev")
        );
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|(m, p, _)| m == "VERIFY" && p == "https://remnifold.fixture.workers.dev")
        );
        let repeated = parse(&args(&[
            "--worker-name",
            "other",
            "--base-url",
            "https://remnifold.fixture.workers.dev",
        ]))
        .unwrap();
        assert_eq!(
            explicit_base_url(&repeated, &config).unwrap(),
            Some("https://remnifold.fixture.workers.dev".into())
        );
    }
    #[test]
    fn existing_ledger_with_different_identifier_case_is_not_reapplied() {
        struct SqliteTransport(rusqlite::Connection);
        impl Transport for SqliteTransport {
            fn api(&self, _: &str, path: &str, body: Option<Value>) -> Result<Value> {
                assert!(path.ends_with("/query"));
                let body = body.unwrap();
                let sql = body["sql"].as_str().unwrap();
                let mut statement = self.0.prepare(sql)?;
                let rows = statement
                    .query_map([], |row| Ok(json!({"name":row.get::<_,String>(0)?})))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(json!({"success":true,"result":[{"success":true,"results":rows}]}))
            }
            fn upload(&self, _: &str, _: Value) -> Result<()> {
                unreachable!()
            }
            fn sql_upload(&self, _: &str, _: &str) -> Result<()> {
                unreachable!()
            }
            fn verify(&self, _: &str, _: Option<&str>) -> Result<()> {
                unreachable!()
            }
        }
        for (existing, configured) in [("Ledger", "ledger"), ("D1_MIGRATIONS", "d1_migrations")] {
            let fixture = Fixture::new();
            let sql = migration_sql_table(&fixture, "/accounts/account", "db", existing).unwrap();
            let db = rusqlite::Connection::open_in_memory().unwrap();
            db.execute_batch(&sql).unwrap();
            let transport = SqliteTransport(db);
            assert!(
                migration_sql_table(&transport, "/accounts/account", "db", configured)
                    .unwrap()
                    .is_empty()
            );
            migrate_table(&transport, "/accounts/account", "db", configured).unwrap();
        }
    }
    #[test]
    fn import_without_upload_url_resumes_or_accepts_completion_without_ingest() {
        for mode in [1, 2] {
            let mut f = Fixture::new();
            f.import_resume = mode;
            migrate(&f, "/accounts/account", "database-id").unwrap();
            assert!(f.imported.borrow().is_empty());
            assert!(
                !f.calls
                    .borrow()
                    .iter()
                    .any(|(_, _, b)| b.as_ref().is_some_and(|b| b["action"] == "ingest"))
            );
            assert_eq!(
                f.calls
                    .borrow()
                    .iter()
                    .filter(|(_, _, b)| b.as_ref().is_some_and(|b| b["action"] == "poll"))
                    .count(),
                if mode == 1 { 1 } else { 0 }
            );
        }
    }
    #[test]
    fn routes_and_custom_domains_are_applied_without_removing_existing_hosts() {
        let f = Fixture::new();
        f.domains.borrow_mut().push(json!({"hostname":"legacy.example.test","service":"remnifold","zone_id":"zone-id","enabled":true}));
        let config = json!({"routes":[{"pattern":"memory.example.test","custom_domain":true},{"pattern":"api.example.test/*","zone_name":"example.test"}]});
        execute(
            &f,
            &parse(&args(&["--apply"])).unwrap(),
            config,
            &state(),
            Some("fixture-key"),
        )
        .unwrap();
        assert!(
            f.domains
                .borrow()
                .iter()
                .any(|d| d["hostname"] == "legacy.example.test")
        );
        assert!(
            f.domains
                .borrow()
                .iter()
                .any(|d| d["hostname"] == "memory.example.test" && d["service"] == "remnifold")
        );
        assert!(
            f.routes
                .borrow()
                .iter()
                .any(|r| r["pattern"] == "api.example.test/*" && r["script"] == "remnifold")
        );
        assert!(!f.calls.borrow().iter().any(|(m, _, _)| m == "DELETE"));
    }
    #[test]
    fn conflicting_route_or_custom_domain_rejects_before_migrations_or_upload() {
        for custom in [true, false] {
            let f = Fixture::new();
            let pattern = if custom {
                "memory.example.test"
            } else {
                "api.example.test/*"
            };
            if custom {
                f.domains
                    .borrow_mut()
                    .push(json!({"hostname":pattern,"service":"another","zone_id":"zone-id"}));
            } else {
                f.routes
                    .borrow_mut()
                    .push(json!({"pattern":pattern,"script":"another"}));
            }
            assert!(
                execute(
                    &f,
                    &parse(&args(&["--apply"])).unwrap(),
                    json!({"routes":[{"pattern":pattern,"custom_domain":custom}]}),
                    &state(),
                    Some("fixture-key")
                )
                .is_err()
            );
            assert!(f.calls.borrow().iter().all(|(m, _, _)| m == "GET"));
        }
    }
    #[test]
    fn config_is_never_overwritten_even_through_hardlink() {
        let input = state();
        fs::create_dir_all(input.parent().unwrap()).unwrap();
        fs::write(&input, "{\"name\":\"remnifold\"}").unwrap();
        let linked = input.with_file_name("alias.json");
        fs::hard_link(&input, &linked).unwrap();
        for target in [&input, &linked] {
            let mut options = parse(&args(&["--apply"])).unwrap();
            options.config = Some(input.clone());
            let f = Fixture::new();
            assert!(execute(&f, &options, json!({}), target, Some("key")).is_err());
            assert!(f.calls.borrow().is_empty());
            assert_eq!(
                fs::read_to_string(&input).unwrap(),
                "{\"name\":\"remnifold\"}"
            );
        }
    }
    #[test]
    fn relocated_repository_and_plugin_roots_protect_state_at_runtime() {
        for plugin in [false, true] {
            let root = state().parent().unwrap().to_path_buf();
            let marker = if plugin {
                root.join(".codex-plugin/plugin.json")
            } else {
                root.join(".git/config")
            };
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            fs::write(&marker, "{}").unwrap();
            let input = root.join("server/wrangler.jsonc");
            fs::create_dir_all(input.parent().unwrap()).unwrap();
            fs::write(&input, "{}").unwrap();
            let mut options = parse(&[]).unwrap();
            options.config = Some(input);
            let roots = runtime_roots(options.config.as_deref()).unwrap();
            assert!(roots.contains(&root));
            assert!(validate_output(&options, &root.join("state/config.json")).is_err());
        }
    }
    #[test]
    fn jsonc_comments_and_trailing_commas_preserve_strings() {
        let s =
            "{ // comment\n\"base_url\":\"https://example.test/a,}\",\"vars\":{\"X\":\"ok\",},}";
        let v = jsonc_parser::parse_to_serde_value::<Value>(s, &jsonc_options()).unwrap();
        assert_eq!(v["base_url"], "https://example.test/a,}");
    }
    #[test]
    fn actual_http_checks_authenticated_session_and_mcp_tools() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let responses=["{}".to_owned(),"{}".to_owned(),json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}).to_string(),json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"search_memory"},{"name":"getDocument"},{"name":"add_memory"}]}}).to_string()];
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..count]);
                    if let Some(end) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&raw[..end]);
                        let len = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(raw).unwrap());
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            }
            requests
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        verify_endpoint(&client, &base, Some("fixture-owner-key")).unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /health "));
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
        assert!(requests[1].starts_with("GET /v3/session "));
        for request in &requests[1..] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-owner-key")
            );
        }
        assert!(requests[2].contains("initialize"));
        assert!(requests[3].contains("tools/list"));
        assert!(
            requests[3]
                .to_ascii_lowercase()
                .contains("mcp-protocol-version: 2025-06-18")
        );
    }
    #[test]
    fn jsonc_rejects_json5_extensions() {
        for input in [
            "{unquoted:1}",
            "{'single':'quotes'}",
            "{\"x\":1 \"y\":2}",
            "{\"x\":0xff}",
        ] {
            assert!(jsonc_parser::parse_to_serde_value::<Value>(input, &jsonc_options()).is_err());
        }
    }
    #[test]
    fn extra_bindings_and_custom_domain_settings_survive() {
        let mut config = json!({"workers_dev":false,"routes":[{"pattern":"memory.example.test","custom_domain":true}],"d1_databases":[{"binding":"AUX","database_id":"aux-id"}],"vectorize":[{"binding":"AUX_VECTOR","index_name":"aux-index"}]});
        replace_binding(
            &mut config,
            "d1_databases",
            "DB",
            json!({"binding":"DB","database_id":"primary"}),
        );
        let upload = metadata(&config, "primary", "index", None).unwrap();
        assert!(
            upload["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["name"] == "AUX")
        );
        assert!(
            upload["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["name"] == "AUX_VECTOR")
        );
        assert_eq!(config["routes"][0]["custom_domain"], true);
        assert_eq!(config["workers_dev"], false);
    }
    #[test]
    fn http_verification_bounds_headers_and_body_and_redacts_errors() {
        use std::{io::Write, net::TcpListener};
        for delayed_body in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                if delayed_body {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
                }
                std::thread::sleep(Duration::from_millis(300));
            });
            let c = Client::builder()
                .timeout(Duration::from_millis(75))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let start = Instant::now();
            let error = verify_endpoint(&c, &format!("http://{addr}"), Some("owner-secret"))
                .unwrap_err()
                .to_string();
            assert!(start.elapsed() < Duration::from_secs(2));
            assert!(!error.contains("owner-secret"));
            thread.join().unwrap();
        }
    }
}
