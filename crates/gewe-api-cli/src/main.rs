use clap::{Parser, Subcommand};
use regex::Regex;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OFFICIAL_INDEX_URL: &str = "https://doc.geweapi.com/llms.txt";
const DEFAULT_BASE_URL: &str = "http://api.geweapi.com";
const DEFAULT_REMOTE_BINARY: &str = "/usr/local/bin/gewe-api";
const DEFAULT_ENV_FILE: &str = "/opt/gewe-skill-memory/config/gewe-skill-memory.env";
const BUNDLED_CATALOG: &str = include_str!("../../../skills/gewe-api/references/operations.jsonl");

#[derive(Debug, Parser)]
#[command(
    name = "gewe-api",
    version,
    about = "Agent-oriented discovery and guarded execution for official GeWe APIs"
)]
struct Cli {
    /// Emit stable machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Disable configured SSH forwarding. Used by the remote installation.
    #[arg(long, global = true, hide = true)]
    local_only: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check catalog, auth, live endpoint, and Oracle forwarding.
    Doctor,

    /// Find the most relevant official API from natural-language intent.
    Find {
        /// User's original request or a short capability description.
        #[arg(long)]
        intent: String,

        /// Maximum candidates to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },

    /// Load one operation's official OpenAPI request schema.
    Describe {
        /// Stable operation id returned by `find`, such as api-139908354.
        operation: String,

        /// Ignore the local document cache.
        #[arg(long)]
        refresh: bool,
    },

    /// Call a catalog operation. Mutations preview unless explicitly confirmed.
    Call {
        /// Stable operation id returned by `find`.
        operation: String,

        /// JSON request body. appId is injected automatically.
        #[arg(long, default_value = "{}")]
        body: String,

        /// Perform a mutation after preview and explicit user authorization.
        #[arg(long)]
        execute: bool,

        /// Must exactly equal the operation id for a mutation.
        #[arg(long)]
        confirm: Option<String>,
    },

    /// Raw endpoint escape hatch. Unknown POST/PUT/PATCH/DELETE calls are mutations.
    Request {
        /// HTTP method.
        method: String,

        /// Absolute GeWe API path, beginning with /gewe/.
        path: String,

        /// JSON request body. appId is injected automatically.
        #[arg(long, default_value = "{}")]
        body: String,

        /// Perform a non-GET/HEAD request.
        #[arg(long)]
        execute: bool,

        /// Must exactly equal the path for a non-GET/HEAD request.
        #[arg(long)]
        confirm: Option<String>,
    },

    /// Inspect or refresh the compact official capability catalog.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommands,
    },

    /// Store non-secret SSH runner configuration.
    Init {
        /// SSH destination used to reach the Oracle GeWe host.
        #[arg(long)]
        ssh_target: String,

        /// SSH private-key path.
        #[arg(long)]
        ssh_identity_file: PathBuf,

        /// Remote installed binary path.
        #[arg(long, default_value = DEFAULT_REMOTE_BINARY)]
        remote_binary: String,

        /// Run the remote binary through sudo.
        #[arg(long, default_value_t = true)]
        remote_sudo: bool,
    },

    /// Execute a prevalidated request forwarded by the local CLI.
    #[command(name = "__remote", hide = true)]
    Remote { payload: String },
}

#[derive(Debug, Subcommand)]
enum CatalogCommands {
    /// Download and cache the current compact official operation index.
    Sync,

    /// List bounded catalog entries, optionally filtered by category.
    List {
        #[arg(long)]
        category: Option<String>,

        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Operation {
    id: String,
    title: String,
    category: String,
    doc_url: String,
    risk: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunnerConfig {
    ssh_target: String,
    ssh_identity_file: PathBuf,
    remote_binary: String,
    remote_sudo: bool,
}

#[derive(Clone, Debug)]
struct Auth {
    base_url: String,
    token: String,
    app_id: String,
    source: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RemotePayload {
    method: String,
    path: String,
    body: Value,
    operation_id: String,
    risk: String,
}

#[derive(Clone, Debug)]
struct OperationSpec {
    method: String,
    path: String,
    summary: String,
    description: String,
    required: Vec<String>,
    fields: Vec<Value>,
    example: Value,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(value) => println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON serialization must succeed")
        ),
        Err(message) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": {"message": message}
                }))
                .expect("JSON serialization must succeed")
            );
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<Value, String> {
    match cli.command {
        Commands::Doctor => doctor(cli.local_only).await,
        Commands::Find { intent, limit } => find_operations(&intent, limit),
        Commands::Describe { operation, refresh } => {
            let item = get_operation(&operation)?;
            let spec = load_operation_spec(&item, refresh).await?;
            Ok(describe_value(&item, &spec))
        }
        Commands::Call {
            operation,
            body,
            execute,
            confirm,
        } => {
            call_operation(
                &operation,
                &body,
                execute,
                confirm.as_deref(),
                cli.local_only,
            )
            .await
        }
        Commands::Request {
            method,
            path,
            body,
            execute,
            confirm,
        } => {
            raw_request(
                &method,
                &path,
                &body,
                execute,
                confirm.as_deref(),
                cli.local_only,
            )
            .await
        }
        Commands::Catalog { command } => match command {
            CatalogCommands::Sync => sync_catalog().await,
            CatalogCommands::List { category, limit } => list_catalog(category.as_deref(), limit),
        },
        Commands::Init {
            ssh_target,
            ssh_identity_file,
            remote_binary,
            remote_sudo,
        } => init_config(RunnerConfig {
            ssh_target,
            ssh_identity_file,
            remote_binary,
            remote_sudo,
        }),
        Commands::Remote { payload } => execute_remote_payload(&payload).await,
    }
}

async fn doctor(local_only: bool) -> Result<Value, String> {
    let catalog = load_catalog()?;
    if let Some(auth) = load_auth()? {
        let response = send_gewe(&auth, "POST", "/gewe/v2/api/label/list", json!({})).await?;
        return Ok(json!({
            "ok": api_succeeded(&response),
            "catalog_count": catalog.len(),
            "runner": "local",
            "auth": {"available": true, "source": auth.source},
            "probe": {"path": "/gewe/v2/api/label/list", "ret": response.get("ret"), "msg": response.get("msg")}
        }));
    }

    if !local_only {
        if let Some(config) = load_runner_config()? {
            return forward_doctor(&config);
        }
    }

    Ok(json!({
        "ok": false,
        "catalog_count": catalog.len(),
        "runner": if local_only {"local_only"} else {"local"},
        "auth": {"available": false, "source": "missing"},
        "next_action": "Set GeWe auth env vars or run gewe-api init with the Oracle SSH runner"
    }))
}

fn find_operations(intent: &str, limit: usize) -> Result<Value, String> {
    let catalog = load_catalog()?;
    let mut scored = catalog
        .into_iter()
        .map(|operation| {
            let score = score_operation(intent, &operation);
            (operation, score)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));
    let candidates = scored
        .into_iter()
        .take(limit.clamp(1, 20))
        .map(|(operation, score)| {
            json!({
                "operation_id": operation.id,
                "title": operation.title,
                "category": operation.category,
                "risk": operation.risk,
                "score": (score * 10.0).round() / 10.0,
                "next_command": format!("gewe-api --json describe {}", operation.id)
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "intent": intent,
        "candidates": candidates,
        "guidance": "Select a clear top match. If candidates remain ambiguous, ask for a narrower clue instead of guessing."
    }))
}

fn list_catalog(category: Option<&str>, limit: usize) -> Result<Value, String> {
    let needle = category.map(normalize);
    let operations = load_catalog()?
        .into_iter()
        .filter(|operation| {
            needle
                .as_ref()
                .is_none_or(|value| normalize(&operation.category).contains(value))
        })
        .take(limit.clamp(1, 200))
        .map(|operation| {
            json!({
                "operation_id": operation.id,
                "title": operation.title,
                "category": operation.category,
                "risk": operation.risk
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"ok": true, "count": operations.len(), "operations": operations}))
}

fn describe_value(operation: &Operation, spec: &OperationSpec) -> Value {
    let mut example = spec.example.clone();
    if let Some(object) = example.as_object_mut() {
        object.remove("appId");
    }
    json!({
        "ok": true,
        "operation": {
            "id": operation.id,
            "title": operation.title,
            "category": operation.category,
            "risk": operation.risk,
            "official_doc": operation.doc_url
        },
        "request": {
            "method": spec.method,
            "path": spec.path,
            "required": spec.required.iter().filter(|field| field.as_str() != "appId").collect::<Vec<_>>(),
            "fields": spec.fields.iter().filter(|field| field.get("name").and_then(Value::as_str) != Some("appId")).collect::<Vec<_>>(),
            "example": example,
            "app_id": "injected_by_cli"
        },
        "notes": {
            "summary": spec.summary,
            "description": spec.description
        },
        "execution": {
            "requires_explicit_authorization": operation.risk == "mutation",
            "preview_command": format!("gewe-api --json call {} --body '<json>'", operation.id),
            "execute_flags": if operation.risk == "mutation" {Value::String(format!("--execute --confirm {}", operation.id))} else {Value::Null}
        }
    })
}

async fn call_operation(
    operation_id: &str,
    body_text: &str,
    execute: bool,
    confirm: Option<&str>,
    local_only: bool,
) -> Result<Value, String> {
    let operation = get_operation(operation_id)?;
    let spec = load_operation_spec(&operation, false).await?;
    let body = parse_body(body_text)?;
    validate_body_against_spec(&body, &spec)?;
    if operation.risk == "mutation" && !execute {
        return Ok(preview_value(
            &operation.id,
            &operation.title,
            &operation.risk,
            &spec.method,
            &spec.path,
            body,
        ));
    }
    if operation.risk == "mutation" && confirm != Some(operation.id.as_str()) {
        return Err(format!(
            "mutation confirmation mismatch: rerun with --execute --confirm {} only after explicit user authorization",
            operation.id
        ));
    }
    execute_live(
        &spec.method,
        &spec.path,
        body,
        &operation.id,
        &operation.risk,
        local_only,
    )
    .await
}

async fn raw_request(
    method: &str,
    path: &str,
    body_text: &str,
    execute: bool,
    confirm: Option<&str>,
    local_only: bool,
) -> Result<Value, String> {
    let method = method.to_ascii_uppercase();
    validate_path(path)?;
    let risk = if matches!(method.as_str(), "GET" | "HEAD") {
        "read"
    } else {
        "mutation"
    };
    let body = parse_body(body_text)?;
    if risk == "mutation" && !execute {
        return Ok(preview_value(
            path,
            "raw request",
            risk,
            &method,
            path,
            body,
        ));
    }
    if risk == "mutation" && confirm != Some(path) {
        return Err(format!(
            "raw write confirmation mismatch: rerun with --execute --confirm {path} only after explicit user authorization"
        ));
    }
    execute_live(&method, path, body, path, risk, local_only).await
}

fn preview_value(
    operation_id: &str,
    title: &str,
    risk: &str,
    method: &str,
    path: &str,
    mut body: Value,
) -> Value {
    inject_app_id_placeholder(&mut body);
    json!({
        "ok": true,
        "status": "preview",
        "operation": {"id": operation_id, "title": title, "risk": risk},
        "request": {"method": method, "path": path, "body": body},
        "requires_explicit_authorization": true,
        "execute_flags": format!("--execute --confirm {operation_id}"),
        "warning": "Preview only. No live WeChat state was changed."
    })
}

async fn execute_live(
    method: &str,
    path: &str,
    body: Value,
    operation_id: &str,
    risk: &str,
    local_only: bool,
) -> Result<Value, String> {
    let response = if let Some(auth) = load_auth()? {
        send_gewe(&auth, method, path, body).await?
    } else if !local_only {
        let config = load_runner_config()?.ok_or_else(|| {
            "missing GeWe auth and Oracle SSH runner config; run `gewe-api init ...`".to_string()
        })?;
        return forward_request(
            &config,
            &RemotePayload {
                method: method.to_string(),
                path: path.to_string(),
                body,
                operation_id: operation_id.to_string(),
                risk: risk.to_string(),
            },
        );
    } else {
        return Err("missing GeWe auth on remote runner".to_string());
    };

    response_envelope(operation_id, risk, response)
}

async fn execute_remote_payload(payload_hex: &str) -> Result<Value, String> {
    let bytes =
        hex::decode(payload_hex).map_err(|error| format!("invalid remote payload: {error}"))?;
    let payload: RemotePayload = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid remote payload JSON: {error}"))?;
    validate_path(&payload.path)?;
    let auth = load_auth()?.ok_or_else(|| "missing GeWe auth on remote runner".to_string())?;
    let response = send_gewe(&auth, &payload.method, &payload.path, payload.body).await?;
    response_envelope(&payload.operation_id, &payload.risk, response)
}

fn response_envelope(operation_id: &str, risk: &str, response: Value) -> Result<Value, String> {
    if !api_succeeded(&response) {
        return Err(format!(
            "GeWe API rejected operation {operation_id}: {}",
            compact_api_error(&response)
        ));
    }
    Ok(json!({
        "ok": true,
        "status": "api_acknowledged",
        "operation_id": operation_id,
        "risk": risk,
        "verification": {
            "level": "api_acknowledged",
            "final_state_verified": false,
            "guidance": if risk == "mutation" {"Call the corresponding read operation when final state matters."} else {"The returned data is the read result."}
        },
        "response": response
    }))
}

async fn send_gewe(
    auth: &Auth,
    method: &str,
    path: &str,
    mut body: Value,
) -> Result<Value, String> {
    validate_path(path)?;
    if let Some(object) = body.as_object_mut() {
        object
            .entry("appId".to_string())
            .or_insert_with(|| Value::String(auth.app_id.clone()));
    }
    let url = format!(
        "{}/{}",
        auth.base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let method = Method::from_bytes(method.as_bytes())
        .map_err(|error| format!("invalid HTTP method: {error}"))?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|error| format!("HTTP client setup failed: {error}"))?;
    let request = client
        .request(method.clone(), url)
        .header("X-GEWE-TOKEN", &auth.token)
        .header("Content-Type", "application/json");
    let response = if matches!(method, Method::GET | Method::HEAD) {
        request.send().await
    } else {
        request.json(&body).send().await
    }
    .map_err(|error| format!("GeWe request failed: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("failed to read GeWe response: {error}"))?;
    if !status.is_success() {
        return Err(format!("GeWe HTTP {status}: {}", truncate(&text, 500)));
    }
    serde_json::from_str(&text).map_err(|error| format!("invalid GeWe JSON response: {error}"))
}

fn forward_doctor(config: &RunnerConfig) -> Result<Value, String> {
    let command = format!(
        "{}{} --json --local-only doctor",
        if config.remote_sudo { "sudo " } else { "" },
        shell_quote_trusted_path(&config.remote_binary)?
    );
    let mut result = run_ssh(config, &command)?;
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "runner".to_string(),
            Value::String("oracle_ssh".to_string()),
        );
    }
    Ok(result)
}

fn forward_request(config: &RunnerConfig, payload: &RemotePayload) -> Result<Value, String> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| format!("failed to encode remote payload: {error}"))?;
    let encoded = hex::encode(bytes);
    let command = format!(
        "{}{} --json --local-only __remote {}",
        if config.remote_sudo { "sudo " } else { "" },
        shell_quote_trusted_path(&config.remote_binary)?,
        encoded
    );
    let mut result = run_ssh(config, &command)?;
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "runner".to_string(),
            Value::String("oracle_ssh".to_string()),
        );
    }
    Ok(result)
}

fn run_ssh(config: &RunnerConfig, remote_command: &str) -> Result<Value, String> {
    let output = Command::new("ssh")
        .arg("-i")
        .arg(&config.ssh_identity_file)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(&config.ssh_target)
        .arg(remote_command)
        .output()
        .map_err(|error| format!("failed to start SSH runner: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Oracle runner failed: {}",
            truncate(&String::from_utf8_lossy(&output.stderr), 800)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Oracle runner returned invalid JSON: {error}"))
}

fn shell_quote_trusted_path(path: &str) -> Result<String, String> {
    if path.is_empty()
        || !path
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._-".contains(character))
    {
        return Err("remote binary path contains unsupported characters".to_string());
    }
    Ok(path.to_string())
}

fn load_auth() -> Result<Option<Auth>, String> {
    let mut values = Map::new();
    let env_file = env::var("GEWE_API_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ENV_FILE));
    if env_file.is_file() {
        let text = fs::read_to_string(&env_file)
            .map_err(|error| format!("failed to read GeWe env file: {error}"))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                values.insert(key.trim().to_string(), Value::String(value));
            }
        }
    }

    let lookup = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|key| {
            env::var(key)
                .ok()
                .or_else(|| values.get(*key).and_then(Value::as_str).map(str::to_string))
        })
    };
    let token = lookup(&["GEWE_SKILL_GEWE_TOKEN", "GEWE_TOKEN"]);
    let app_id = lookup(&["GEWE_SKILL_GEWE_APP_ID", "GEWE_APP_ID", "APP_ID"]);
    match (token, app_id) {
        (Some(token), Some(app_id)) if !token.is_empty() && !app_id.is_empty() => Ok(Some(Auth {
            base_url: lookup(&["GEWE_SKILL_GEWE_BASE_URL", "GEWE_API_BASE_URL"])
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            token,
            app_id,
            source: if env::var("GEWE_SKILL_GEWE_TOKEN").is_ok() || env::var("GEWE_TOKEN").is_ok() {
                "env".to_string()
            } else {
                "env_file".to_string()
            },
        })),
        _ => Ok(None),
    }
}

fn init_config(config: RunnerConfig) -> Result<Value, String> {
    if !config.ssh_identity_file.is_file() {
        return Err(format!(
            "SSH identity file does not exist: {}",
            config.ssh_identity_file.display()
        ));
    }
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let data = serde_json::to_vec_pretty(&config)
        .map_err(|error| format!("failed to encode runner config: {error}"))?;
    fs::write(&path, data).map_err(|error| format!("failed to write runner config: {error}"))?;
    Ok(json!({
        "ok": true,
        "config_path": path,
        "ssh_target": config.ssh_target,
        "ssh_identity_file": config.ssh_identity_file,
        "remote_binary": config.remote_binary,
        "secrets_stored": false,
        "next_command": "gewe-api --json doctor"
    }))
}

fn load_runner_config() -> Result<Option<RunnerConfig>, String> {
    let path = config_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("failed to read runner config: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid runner config: {error}"))
}

fn config_path() -> Result<PathBuf, String> {
    home_dir().map(|home| home.join(".config/gewe-api/config.json"))
}

fn cache_dir() -> Result<PathBuf, String> {
    home_dir().map(|home| home.join(".cache/gewe-api"))
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is unavailable".to_string())
}

async fn sync_catalog() -> Result<Value, String> {
    let text = Client::new()
        .get(OFFICIAL_INDEX_URL)
        .send()
        .await
        .map_err(|error| format!("failed to fetch official catalog: {error}"))?
        .error_for_status()
        .map_err(|error| format!("official catalog HTTP error: {error}"))?
        .text()
        .await
        .map_err(|error| format!("failed to read official catalog: {error}"))?;
    let operations = parse_catalog_index(&text)?;
    if operations.len() < 100 {
        return Err(format!(
            "refusing incomplete official catalog with only {} operations",
            operations.len()
        ));
    }
    let path = cache_dir()?.join("operations.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create catalog cache: {error}"))?;
    }
    let mut output = String::new();
    for operation in &operations {
        output.push_str(
            &serde_json::to_string(operation)
                .map_err(|error| format!("failed to encode catalog entry: {error}"))?,
        );
        output.push('\n');
    }
    fs::write(&path, output).map_err(|error| format!("failed to write catalog cache: {error}"))?;
    Ok(json!({
        "ok": true,
        "source": OFFICIAL_INDEX_URL,
        "catalog_path": path,
        "count": operations.len()
    }))
}

fn load_catalog() -> Result<Vec<Operation>, String> {
    let cached = cache_dir()?.join("operations.jsonl");
    if cached.is_file() {
        let text = fs::read_to_string(&cached)
            .map_err(|error| format!("failed to read catalog cache: {error}"))?;
        let operations = parse_catalog_jsonl(&text)?;
        if !operations.is_empty() {
            return Ok(operations);
        }
    }
    parse_catalog_jsonl(BUNDLED_CATALOG)
}

fn parse_catalog_jsonl(text: &str) -> Result<Vec<Operation>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|error| format!("invalid catalog entry: {error}"))
        })
        .collect()
}

fn parse_catalog_index(text: &str) -> Result<Vec<Operation>, String> {
    let pattern = Regex::new(
        r"^-\s+(?P<trail>.*?)\[(?P<title>[^]]+)\]\((?P<url>https://doc\.geweapi\.com/(?P<id>api-[0-9]+)\.md)\)",
    )
    .map_err(|error| format!("catalog parser setup failed: {error}"))?;
    let mut operations = Vec::new();
    for line in text.lines() {
        let Some(captures) = pattern.captures(line) else {
            continue;
        };
        let trail = captures.name("trail").map_or("", |value| value.as_str());
        let category = trail
            .split('>')
            .next_back()
            .unwrap_or("API")
            .trim()
            .to_string();
        let title = captures["title"].trim().to_string();
        operations.push(Operation {
            id: captures["id"].to_string(),
            title: title.clone(),
            category,
            doc_url: captures["url"].to_string(),
            risk: risk_for(&title).to_string(),
            aliases: aliases_for(&title)
                .into_iter()
                .map(str::to_string)
                .collect(),
        });
    }
    Ok(operations)
}

fn get_operation(operation_id: &str) -> Result<Operation, String> {
    load_catalog()?
        .into_iter()
        .find(|operation| operation.id == operation_id)
        .ok_or_else(|| {
            format!("unknown operation id: {operation_id}; run `gewe-api --json find --intent ...`")
        })
}

async fn load_operation_spec(
    operation: &Operation,
    refresh: bool,
) -> Result<OperationSpec, String> {
    let docs_dir = cache_dir()?.join("docs");
    let cache_path = docs_dir.join(format!("{}.md", operation.id));
    let text = if cache_path.is_file() && !refresh {
        fs::read_to_string(&cache_path)
            .map_err(|error| format!("failed to read operation cache: {error}"))?
    } else {
        let text = Client::new()
            .get(&operation.doc_url)
            .send()
            .await
            .map_err(|error| format!("failed to fetch official operation document: {error}"))?
            .error_for_status()
            .map_err(|error| format!("official operation document HTTP error: {error}"))?
            .text()
            .await
            .map_err(|error| format!("failed to read official operation document: {error}"))?;
        fs::create_dir_all(&docs_dir)
            .map_err(|error| format!("failed to create operation cache: {error}"))?;
        fs::write(&cache_path, &text)
            .map_err(|error| format!("failed to cache operation document: {error}"))?;
        text
    };
    parse_operation_spec(&text)
}

fn parse_operation_spec(markdown: &str) -> Result<OperationSpec, String> {
    let yaml = markdown
        .split_once("```yaml")
        .and_then(|(_, rest)| rest.split_once("```").map(|(block, _)| block))
        .ok_or_else(|| "official document does not contain an OpenAPI YAML block".to_string())?;
    let yaml_value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| format!("invalid OpenAPI YAML: {error}"))?;
    let spec = serde_json::to_value(yaml_value)
        .map_err(|error| format!("failed to normalize OpenAPI YAML: {error}"))?;
    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| "OpenAPI document has no paths".to_string())?;
    let (path, path_value) = paths
        .iter()
        .next()
        .ok_or_else(|| "OpenAPI document has an empty paths object".to_string())?;
    let methods = ["get", "post", "put", "patch", "delete", "head"];
    let (method, operation) = methods
        .iter()
        .find_map(|method| {
            path_value
                .get(*method)
                .map(|value| ((*method).to_string(), value))
        })
        .ok_or_else(|| "OpenAPI path has no supported HTTP method".to_string())?;

    let schema = operation
        .pointer("/requestBody/content/application~1json/schema")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let fields = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, value)| {
                    json!({
                        "name": name,
                        "type": schema_type(value),
                        "description": value.get("description").and_then(Value::as_str).unwrap_or(""),
                        "required": required.iter().any(|field| field == name)
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let example = operation
        .pointer("/requestBody/content/application~1json/example")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let description = operation
        .get("description")
        .and_then(Value::as_str)
        .map(clean_description)
        .unwrap_or_default();
    Ok(OperationSpec {
        method: method.to_ascii_uppercase(),
        path: path.to_string(),
        summary: operation
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description,
        required,
        fields,
        example,
    })
}

fn schema_type(value: &Value) -> String {
    let base = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if base == "array" {
        let item = value
            .pointer("/items/type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        format!("array<{item}>")
    } else {
        base.to_string()
    }
}

fn clean_description(text: &str) -> String {
    let without_tags = Regex::new(r"<[^>]+>")
        .expect("static HTML regex must compile")
        .replace_all(text, " ");
    let compact = without_tags
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate(&compact, 1_200)
}

fn parse_body(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("--body must be valid JSON: {error}"))?;
    if !value.is_object() {
        return Err("--body must be a JSON object".to_string());
    }
    Ok(value)
}

fn validate_body_against_spec(body: &Value, spec: &OperationSpec) -> Result<(), String> {
    let object = body
        .as_object()
        .ok_or_else(|| "request body must be a JSON object".to_string())?;
    let missing = spec
        .required
        .iter()
        .filter(|field| field.as_str() != "appId" && !object.contains_key(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "request body is missing required fields: {}",
            missing.join(", ")
        ));
    }

    for field in &spec.fields {
        let Some(name) = field.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = object.get(name) else {
            continue;
        };
        let expected = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let valid = match expected {
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            kind if kind.starts_with("array<") => value.is_array(),
            _ => true,
        };
        if !valid {
            return Err(format!(
                "request field `{name}` must be {expected}, got {}",
                json_type(value)
            ));
        }
    }
    Ok(())
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn inject_app_id_placeholder(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        object
            .entry("appId".to_string())
            .or_insert_with(|| Value::String("<injected>".to_string()));
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/gewe/") || path.contains("..") || path.contains('?') {
        return Err(
            "path must be an absolute /gewe/ API path without query or traversal".to_string(),
        );
    }
    Ok(())
}

fn api_succeeded(response: &Value) -> bool {
    response.get("ret").and_then(Value::as_i64) == Some(200)
}

fn compact_api_error(response: &Value) -> String {
    let ret = response.get("ret").cloned().unwrap_or(Value::Null);
    let msg = response.get("msg").cloned().unwrap_or(Value::Null);
    json!({"ret": ret, "msg": msg}).to_string()
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn score_operation(intent: &str, operation: &Operation) -> f64 {
    let intent = normalize(intent);
    if intent.is_empty() {
        return 0.0;
    }
    let title = normalize(&operation.title);
    let category = normalize(&operation.category);
    let aliases = operation.aliases.iter().map(|alias| normalize(alias));
    let mut score = overlap_score(&intent, &title) * 50.0;
    if intent.contains(&title) || title.contains(&intent) {
        score += 80.0;
    }
    if intent.contains(&category) {
        score += 18.0;
    }
    for alias in aliases {
        score = score.max(overlap_score(&intent, &alias) * 55.0);
        if intent.contains(&alias) || alias.contains(&intent) {
            score += 90.0;
        }
    }
    if ["有哪些", "列出", "列表", "全部", "所有"]
        .iter()
        .any(|hint| intent.contains(hint))
        && operation.title.contains("列表")
    {
        score += 120.0;
    }
    if ["查看", "看看", "查询"]
        .iter()
        .any(|hint| intent.contains(hint))
        && operation.risk == "read"
    {
        score += 15.0;
    }
    score
}

fn overlap_score(left: &str, right: &str) -> f64 {
    let left_parts = ngrams(left);
    let right_parts = ngrams(right);
    if right_parts.is_empty() {
        return 0.0;
    }
    let matches = right_parts.intersection(&left_parts).count();
    matches as f64 / right_parts.len() as f64
}

fn ngrams(text: &str) -> HashSet<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() < 2 {
        return chars
            .into_iter()
            .map(|character| character.to_string())
            .collect();
    }
    chars
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

fn risk_for(title: &str) -> &'static str {
    const READ_PREFIXES: &[&str] = &[
        "获取",
        "搜索",
        "下载",
        "检查",
        "同步",
        "自己的朋友圈列表",
        "联系人的朋友圈列表",
        "某条朋友圈详情",
        "用户主页",
        "关注列表",
        "消息列表",
        "评论列表",
    ];
    const READ_TITLES: &[&str] = &["标签列表", "cdn下载", "我的视频号二维码"];
    const MUTATION_EXCEPTIONS: &[&str] = &["搜索并关注"];
    if !MUTATION_EXCEPTIONS.contains(&title)
        && (READ_TITLES.contains(&title)
            || READ_PREFIXES.iter().any(|prefix| title.starts_with(prefix)))
    {
        "read"
    } else {
        "mutation"
    }
}

fn aliases_for(title: &str) -> Vec<&'static str> {
    match title {
        "修改好友标签" => vec![
            "打标签",
            "加标签",
            "好友打标签",
            "给好友加标签",
            "移除好友标签",
            "标签分组",
        ],
        "标签列表" => vec!["有哪些标签", "查看标签", "好友标签列表", "所有标签"],
        "删除群成员" => vec!["踢人", "移出群聊", "群里删人"],
        "邀请/添加 进群" => vec!["拉人进群", "邀请进群", "群里加人"],
        "发送文字消息" => vec!["发微信", "发文本", "发送消息"],
        "设置好友备注" => vec!["改好友备注", "修改联系人备注"],
        "修改群名称" => vec!["改群名", "群聊改名"],
        "群保存到通讯录" => vec!["保存群聊", "群加入通讯录"],
        "设置消息免打扰" => vec!["群免打扰", "消息静音"],
        "检查是否在线" => vec!["账号在线状态", "微信是否掉线"],
        "退出" => vec!["退出登录", "微信下线"],
        _ => Vec::new(),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut output = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_catalog_line() {
        let text =
            "-  API 参考 > 标签模块 [修改好友标签](https://doc.geweapi.com/api-139908354.md):";
        let operations = parse_catalog_index(text).expect("catalog should parse");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "api-139908354");
        assert_eq!(operations[0].category, "标签模块");
        assert_eq!(operations[0].risk, "mutation");
    }

    #[test]
    fn aliases_rank_label_operation() {
        let operation = Operation {
            id: "api-139908354".to_string(),
            title: "修改好友标签".to_string(),
            category: "标签模块".to_string(),
            doc_url: String::new(),
            risk: "mutation".to_string(),
            aliases: aliases_for("修改好友标签")
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        assert!(score_operation("给这些朋友都打上AI标签", &operation) > 20.0);
    }

    #[test]
    fn parses_operation_request_schema() {
        let markdown = r#"# Test
```yaml
openapi: 3.0.1
paths:
  /gewe/v2/api/example:
    post:
      summary: 示例
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                appId:
                  type: string
                wxIds:
                  type: array
                  items:
                    type: string
              required: [appId, wxIds]
            example:
              appId: test
              wxIds: [one]
```
"#;
        let spec = parse_operation_spec(markdown).expect("spec should parse");
        assert_eq!(spec.path, "/gewe/v2/api/example");
        assert_eq!(spec.method, "POST");
        assert!(spec.required.contains(&"wxIds".to_string()));
    }

    #[test]
    fn raw_path_rejects_traversal() {
        assert!(validate_path("/gewe/v2/api/label/list").is_ok());
        assert!(validate_path("/gewe/../secret").is_err());
        assert!(validate_path("https://example.com").is_err());
    }

    #[test]
    fn risk_classifier_is_conservative() {
        assert_eq!(risk_for("标签列表"), "read");
        assert_eq!(risk_for("搜索好友"), "read");
        assert_eq!(risk_for("搜索并关注"), "mutation");
        assert_eq!(risk_for("发送文字消息"), "mutation");
    }

    #[test]
    fn validates_required_fields_and_types() {
        let spec = OperationSpec {
            method: "POST".to_string(),
            path: "/gewe/v2/api/example".to_string(),
            summary: String::new(),
            description: String::new(),
            required: vec!["appId".to_string(), "wxids".to_string()],
            fields: vec![json!({"name": "wxids", "type": "string"})],
            example: json!({}),
        };
        assert!(validate_body_against_spec(&json!({}), &spec).is_err());
        assert!(validate_body_against_spec(&json!({"wxids": ["one"]}), &spec).is_err());
        assert!(validate_body_against_spec(&json!({"wxids": "one"}), &spec).is_ok());
    }

    #[test]
    fn list_intent_beats_label_mutation() {
        let list = Operation {
            id: "list".to_string(),
            title: "标签列表".to_string(),
            category: "标签模块".to_string(),
            doc_url: String::new(),
            risk: "read".to_string(),
            aliases: aliases_for("标签列表")
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        let modify = Operation {
            id: "modify".to_string(),
            title: "修改好友标签".to_string(),
            category: "标签模块".to_string(),
            doc_url: String::new(),
            risk: "mutation".to_string(),
            aliases: aliases_for("修改好友标签")
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        let intent = "看看目前有哪些好友标签";
        assert!(score_operation(intent, &list) > score_operation(intent, &modify));
    }
}
