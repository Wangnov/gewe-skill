use clap::{Args, Parser, Subcommand, ValueEnum};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::normalize_callback;
use gewe_skill_types::{
    AttachmentKind, AttachmentRecord, IdentityRefreshRequest, MessageQuery, RawCallbackRequest,
};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
#[command(
    name = "gewe-skill",
    version,
    about = "Agent protocol CLI for GeWe-backed WeChat memory"
)]
struct Cli {
    /// Always emit machine-readable JSON. This is the primary CLI surface.
    #[arg(long, global = true, default_value_t = true)]
    json: bool,

    #[arg(
        long,
        global = true,
        env = "GEWE_SKILL_BASE_URL",
        default_value = "http://127.0.0.1:8788"
    )]
    base_url: String,

    #[arg(
        long,
        global = true,
        env = "GEWE_SKILL_READ_TOKEN",
        hide_env_values = true
    )]
    read_token: Option<String>,

    #[arg(
        long,
        global = true,
        env = "GEWE_SKILL_WRITE_TOKEN",
        hide_env_values = true
    )]
    write_token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check CLI config, auth presence, and memory service reachability.
    Doctor,
    /// Discover and inspect conversations.
    Conversations {
        #[command(subcommand)]
        command: ConversationsCommand,
    },
    /// Resolve and refresh contacts, chatrooms, and room-scoped member aliases.
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
    /// Read message windows for group chats and private chats.
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    /// Inspect or download synced attachments.
    Attachments {
        #[command(subcommand)]
        command: AttachmentsCommand,
    },
    /// Inspect chatroom snapshots and member/system events.
    Chatrooms {
        #[command(subcommand)]
        command: ChatroomsCommand,
    },
    /// Trusted ingest and normalization operations.
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Pull evidence from the Cloudflare edge buffer into memory.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Raw read-only API escape hatch using configured auth.
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConversationsCommand {
    /// List conversations ordered by newest message.
    List {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    /// Resolve human wording into stable chatroom/contact/member ids.
    Resolve {
        #[arg(long)]
        q: String,
        #[arg(long)]
        chatroom_id: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Refresh the local identity memory from GeWe read-only APIs.
    Refresh {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        chatroom_id: Option<String>,
        #[arg(long, value_delimiter = ',')]
        wxids: Vec<String>,
        #[arg(long)]
        recent_chatrooms: Option<i64>,
    },
    /// Prepare identity memory for one chatroom without broad contact polling.
    Warm {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 200)]
        recent_messages: i64,
        #[arg(long, default_value_t = 50)]
        max_contacts: usize,
    },
}

#[derive(Debug, Subcommand)]
enum MessagesCommand {
    /// List a bounded message window with optional filters.
    List {
        #[command(flatten)]
        filters: MessageFilterArgs,
    },
    /// Search message text/XML within an optional scoped window.
    Search {
        #[arg(long)]
        q: String,
        #[command(flatten)]
        filters: MessageFilterArgs,
    },
    /// Read messages around a known message key.
    Context {
        #[arg(long)]
        message_key: String,
        #[arg(long, default_value_t = 5)]
        before: u32,
        #[arg(long, default_value_t = 5)]
        after: u32,
    },
}

#[derive(Debug, Clone, Args)]
struct MessageFilterArgs {
    #[arg(long)]
    conversation_id: Option<String>,
    #[arg(long)]
    sender_wxid: Option<String>,
    #[arg(long)]
    kind: Option<String>,
    #[arg(long, value_enum, default_value = "any")]
    direction: Direction,
    #[arg(long)]
    after: Option<String>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: i64,
    #[arg(long, value_enum, default_value = "desc")]
    order: Order,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
enum Direction {
    Any,
    Incoming,
    Outgoing,
}

impl Direction {
    fn query_value(&self) -> Option<String> {
        match self {
            Self::Any => None,
            Self::Incoming => Some("incoming".to_string()),
            Self::Outgoing => Some("outgoing".to_string()),
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
enum Order {
    Asc,
    Desc,
}

impl Order {
    fn query_value(&self) -> String {
        match self {
            Self::Asc => "asc".to_string(),
            Self::Desc => "desc".to_string(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum AttachmentsCommand {
    /// List recent synced attachment records.
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        message_key: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Download one synced attachment by sha256.
    Download {
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ChatroomsCommand {
    /// List stored membership snapshots for a chatroom.
    Snapshots {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List snapshot-diff member events for a chatroom.
    Events {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List structured system events for a chatroom.
    SystemEvents {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
enum IngestCommand {
    /// Normalize a raw GeWe callback JSON file and print the ingest payload.
    Normalize {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        received_at: Option<String>,
    },
    /// Normalize a raw GeWe callback JSON file and send it to memory.
    File {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        received_at: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Pull raw events from gewe-skill-edge admin export and write them to memory.
    Edge {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long)]
        after_raw_event_id: Option<i64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(
            long,
            env = "GEWE_SKILL_SYNC_CURSOR_FILE",
            default_value = "/opt/gewe-skill-memory/data/edge-sync.cursor"
        )]
        cursor_file: PathBuf,
    },
    /// Pull completed attachments from gewe-skill-edge and store them locally.
    Attachments {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long)]
        after_job_id: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(
            long,
            env = "GEWE_SKILL_ATTACHMENT_CURSOR_FILE",
            default_value = "/opt/gewe-skill-memory/data/edge-attachment-sync.cursor"
        )]
        cursor_file: PathBuf,
        #[arg(
            long,
            env = "GEWE_SKILL_ATTACHMENT_DIR",
            default_value = "/opt/gewe-skill-memory/data/attachments"
        )]
        attachment_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum RequestCommand {
    /// GET a read-only memory API path with configured auth.
    Get {
        #[arg(long)]
        path: String,
        #[arg(long = "query")]
        query_pairs: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
struct EdgeExportResponse {
    events: Vec<EdgeExportEvent>,
    next_after_raw_event_id: i64,
}

#[derive(Debug, Deserialize)]
struct EdgeExportEvent {
    raw_event_id: i64,
    received_at: String,
    body: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct EdgeAttachmentResponse {
    attachments: Vec<EdgeAttachment>,
    next_after_job_id: i64,
}

#[derive(Debug, Deserialize)]
struct EdgeAttachment {
    job_id: i64,
    job_key: Option<String>,
    message_key: Option<String>,
    raw_event_dedupe_key: Option<String>,
    appid: String,
    account_wxid: Option<String>,
    asset_type: String,
    variant: Option<String>,
    size_bytes: Option<i64>,
    mime_type: Option<String>,
    completed_at: Option<String>,
    created_at: Option<String>,
    download_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = build_client(&cli)?;
    let raw_base_url = cli.base_url.clone();
    let raw_read_token = cli.read_token.clone();

    match cli.command {
        Command::Doctor => print_json(doctor(&cli, &client).await?)?,
        Command::Conversations { command } => match command {
            ConversationsCommand::List { limit } => {
                print_json(client.conversations(Some(limit)).await?)?
            }
        },
        Command::Identity { command } => match command {
            IdentityCommand::Resolve {
                q,
                chatroom_id,
                limit,
            } => {
                let mut response = client.resolve_identity(&q, Some(limit)).await?;
                if let Some(chatroom_id) = chatroom_id {
                    let contact_ids = response
                        .items
                        .iter()
                        .filter(|item| item.entity_type == "contact" && item.score >= 0.9)
                        .map(|item| item.entity_id.clone())
                        .collect::<Vec<_>>();
                    response.items.retain(|item| {
                        item.chatroom_id.as_deref() == Some(chatroom_id.as_str())
                            || (item.entity_type == "chatroom" && item.entity_id == chatroom_id)
                    });
                    for contact_id in contact_ids {
                        let member_response = client
                            .resolve_identity(&contact_id, Some(limit.max(50)))
                            .await?;
                        for item in member_response.items {
                            if item.entity_type != "chatroom_member"
                                || item.chatroom_id.as_deref() != Some(chatroom_id.as_str())
                                || response.items.iter().any(|existing| {
                                    existing.entity_type == item.entity_type
                                        && existing.entity_id == item.entity_id
                                        && existing.chatroom_id == item.chatroom_id
                                })
                            {
                                continue;
                            }
                            response.items.push(item);
                        }
                    }
                }
                print_json(response)?;
            }
            IdentityCommand::Refresh {
                full,
                chatroom_id,
                wxids,
                recent_chatrooms,
            } => {
                let request = IdentityRefreshRequest {
                    full: Some(full),
                    chatroom_id,
                    wxids: (!wxids.is_empty()).then_some(wxids),
                    recent_chatrooms,
                };
                print_json(client.refresh_identity(&request).await?)?;
            }
            IdentityCommand::Warm {
                chatroom_id,
                recent_messages,
                max_contacts,
            } => {
                let max_contacts = max_contacts.clamp(1, 500);
                let chatroom_refresh = client
                    .refresh_identity(&IdentityRefreshRequest {
                        full: Some(false),
                        chatroom_id: Some(chatroom_id.clone()),
                        wxids: None,
                        recent_chatrooms: None,
                    })
                    .await?;

                let messages = client
                    .messages(&MessageQuery {
                        q: None,
                        conversation_id: Some(chatroom_id.clone()),
                        sender_wxid: None,
                        kind: None,
                        direction: None,
                        after: None,
                        before: None,
                        cursor: None,
                        limit: Some(recent_messages.clamp(1, 1000)),
                        order: Some("desc".to_string()),
                    })
                    .await?;

                let mut active_wxids = Vec::new();
                for message in &messages.items {
                    let Some(sender_wxid) = message.sender_wxid.as_deref() else {
                        continue;
                    };
                    if sender_wxid.is_empty()
                        || sender_wxid.ends_with("@chatroom")
                        || active_wxids.iter().any(|item| item == sender_wxid)
                    {
                        continue;
                    }
                    active_wxids.push(sender_wxid.to_string());
                    if active_wxids.len() >= max_contacts {
                        break;
                    }
                }

                let contact_refresh = if active_wxids.is_empty() {
                    None
                } else {
                    Some(
                        client
                            .refresh_identity(&IdentityRefreshRequest {
                                full: Some(false),
                                chatroom_id: None,
                                wxids: Some(active_wxids.clone()),
                                recent_chatrooms: None,
                            })
                            .await?,
                    )
                };

                print_json(serde_json::json!({
                    "ok": chatroom_refresh.ok && contact_refresh.as_ref().map(|item| item.ok).unwrap_or(true),
                    "strategy": "chatroom_active_contacts",
                    "chatroom_id": chatroom_id,
                    "recent_messages_scanned": messages.items.len(),
                    "max_contacts": max_contacts,
                    "active_contacts_refreshed": active_wxids.len(),
                    "active_wxids": active_wxids,
                    "chatroom_refresh": chatroom_refresh,
                    "contact_refresh": contact_refresh,
                }))?;
            }
        },
        Command::Messages { command } => match command {
            MessagesCommand::List { filters } => {
                print_json(client.messages(&message_query(None, filters)).await?)?;
            }
            MessagesCommand::Search { q, filters } => {
                print_json(client.messages(&message_query(Some(q), filters)).await?)?;
            }
            MessagesCommand::Context {
                message_key,
                before,
                after,
            } => print_json(
                client
                    .message_context(&message_key, Some(before), Some(after))
                    .await?,
            )?,
        },
        Command::Attachments { command } => match command {
            AttachmentsCommand::List {
                limit,
                message_key,
                kind,
            } => {
                let mut page = client.recent_attachments(Some(limit)).await?;
                if let Some(message_key) = message_key {
                    page.items.retain(|item| item.message_key == message_key);
                }
                if let Some(kind) = kind {
                    page.items
                        .retain(|item| format!("{:?}", item.kind).eq_ignore_ascii_case(&kind));
                }
                print_json(page)?;
            }
            AttachmentsCommand::Download { sha256, output } => {
                let bytes = client.download_attachment(&sha256).await?;
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&output, &bytes)?;
                print_json(serde_json::json!({
                    "ok": true,
                    "sha256": sha256,
                    "output": output,
                    "size_bytes": bytes.len()
                }))?;
            }
        },
        Command::Chatrooms { command } => match command {
            ChatroomsCommand::Snapshots { chatroom_id, limit } => {
                print_json(client.chatroom_snapshots(&chatroom_id, Some(limit)).await?)?
            }
            ChatroomsCommand::Events { chatroom_id, limit } => {
                print_json(client.chatroom_events(&chatroom_id, Some(limit)).await?)?
            }
            ChatroomsCommand::SystemEvents { chatroom_id, limit } => print_json(
                client
                    .chatroom_system_events(&chatroom_id, Some(limit))
                    .await?,
            )?,
        },
        Command::Ingest { command } => match command {
            IngestCommand::Normalize { file, received_at } => {
                let payload = normalize_file(file, received_at)?;
                print_json(payload)?;
            }
            IngestCommand::File { file, received_at } => {
                let payload = normalize_file(file, received_at)?;
                print_json(client.write_event(&payload).await?)?;
            }
        },
        Command::Sync { command } => match command {
            SyncCommand::Edge {
                edge_url,
                admin_token,
                after_raw_event_id,
                limit,
                cursor_file,
            } => {
                let result = sync_edge(
                    &client,
                    &edge_url,
                    &admin_token,
                    after_raw_event_id,
                    limit,
                    cursor_file,
                )
                .await?;
                print_json(result)?;
            }
            SyncCommand::Attachments {
                edge_url,
                admin_token,
                after_job_id,
                limit,
                cursor_file,
                attachment_dir,
            } => {
                let result = sync_edge_attachments(
                    &client,
                    &edge_url,
                    &admin_token,
                    after_job_id,
                    limit,
                    cursor_file,
                    attachment_dir,
                )
                .await?;
                print_json(result)?;
            }
        },
        Command::Request { command } => match command {
            RequestCommand::Get { path, query_pairs } => {
                print_json(
                    raw_get(
                        &raw_base_url,
                        raw_read_token.as_deref(),
                        &path,
                        &query_pairs,
                    )
                    .await?,
                )?;
            }
        },
    }

    Ok(())
}

fn message_query(q: Option<String>, filters: MessageFilterArgs) -> MessageQuery {
    MessageQuery {
        q,
        conversation_id: filters.conversation_id,
        sender_wxid: filters.sender_wxid,
        kind: filters.kind,
        direction: filters.direction.query_value(),
        after: filters.after,
        before: filters.before,
        cursor: filters.cursor,
        limit: Some(filters.limit),
        order: Some(filters.order.query_value()),
    }
}

async fn doctor(cli: &Cli, client: &GeweSkillClient) -> Result<Value, Box<dyn std::error::Error>> {
    let health = client.healthz().await;
    let read_probe = if cli.read_token.is_some() {
        match client.conversations(Some(1)).await {
            Ok(page) => serde_json::json!({ "ok": true, "sample_count": page.items.len() }),
            Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
        }
    } else {
        serde_json::json!({ "ok": false, "error": "missing_read_token" })
    };
    Ok(serde_json::json!({
        "ok": health.is_ok(),
        "cli": {
            "name": "gewe-skill",
            "version": env!("CARGO_PKG_VERSION"),
            "primary_surface": "agent_json"
        },
        "memory": {
            "base_url": cli.base_url.as_str(),
            "health": match health {
                Ok(value) => serde_json::json!({ "ok": true, "response": value }),
                Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
            },
            "read_probe": read_probe
        },
        "auth": {
            "read_token_available": cli.read_token.is_some(),
            "write_token_available": cli.write_token.is_some(),
            "read_token_source": auth_source("GEWE_SKILL_READ_TOKEN", cli.read_token.as_deref()),
            "write_token_source": auth_source("GEWE_SKILL_WRITE_TOKEN", cli.write_token.as_deref())
        }
    }))
}

fn auth_source(env_name: &str, value: Option<&str>) -> &'static str {
    if value.is_none() {
        "missing"
    } else if std::env::var_os(env_name).is_some() {
        "env"
    } else {
        "flag"
    }
}

async fn raw_get(
    base_url: &str,
    read_token: Option<&str>,
    path: &str,
    query_pairs: &[String],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut url = Url::parse(base_url)?.join(path.trim_start_matches('/'))?;
    {
        let mut pairs = url.query_pairs_mut();
        for pair in query_pairs {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            pairs.append_pair(key, value);
        }
    }
    let mut request = reqwest::Client::new().get(url);
    if let Some(token) = read_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await?;
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| Value::String(body));
    Ok(serde_json::json!({
        "ok": status.is_success(),
        "status": status.as_u16(),
        "body": parsed
    }))
}

fn build_client(cli: &Cli) -> Result<GeweSkillClient, Box<dyn std::error::Error>> {
    let mut client = GeweSkillClient::new(&cli.base_url)?;
    if let Some(token) = &cli.read_token {
        client = client.with_read_token(token.clone());
    }
    if let Some(token) = &cli.write_token {
        client = client.with_write_token(token.clone());
    }
    Ok(client)
}

fn normalize_file(
    path: PathBuf,
    received_at: Option<String>,
) -> Result<gewe_skill_types::IngestEventRequest, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let json: Value = serde_json::from_str(&text)?;
    let received_at = received_at.unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());
    Ok(normalize_callback(&json, received_at)?.into_ingest_request())
}

async fn sync_edge(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    after_raw_event_id: Option<i64>,
    limit: u32,
    cursor_file: PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let after_raw_event_id =
        after_raw_event_id.unwrap_or_else(|| read_cursor(&cursor_file).unwrap_or(0));
    let edge_url = edge_url.trim_end_matches('/');
    let export_url =
        format!("{edge_url}/admin/export?after_raw_event_id={after_raw_event_id}&limit={limit}");
    let export: EdgeExportResponse = reqwest::Client::new()
        .get(export_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut written = 0usize;
    let mut failed = Vec::new();
    let mut last_raw_event_id = after_raw_event_id;
    for event in &export.events {
        last_raw_event_id = event.raw_event_id;
        let Some(body) = &event.body else {
            failed.push(serde_json::json!({
                "raw_event_id": event.raw_event_id,
                "error": "missing_body"
            }));
            continue;
        };
        match client
            .write_raw_event(&RawCallbackRequest {
                received_at: event.received_at.clone(),
                body: body.clone(),
            })
            .await
        {
            Ok(_) => written += 1,
            Err(error) => failed.push(serde_json::json!({
                "raw_event_id": event.raw_event_id,
                "error": error.to_string()
            })),
        }
    }

    write_cursor(&cursor_file, export.next_after_raw_event_id)?;

    Ok(serde_json::json!({
        "ok": true,
        "scanned": export.events.len(),
        "written": written,
        "failed": failed,
        "failed_count": failed.len(),
        "after_raw_event_id": after_raw_event_id,
        "last_raw_event_id": last_raw_event_id,
        "next_after_raw_event_id": export.next_after_raw_event_id
    }))
}

fn read_cursor(path: &PathBuf) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_cursor(path: &PathBuf, value: i64) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))
}

async fn sync_edge_attachments(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    after_job_id: Option<i64>,
    limit: u32,
    cursor_file: PathBuf,
    attachment_dir: PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let after_job_id = after_job_id.unwrap_or_else(|| read_cursor(&cursor_file).unwrap_or(0));
    let edge_url = edge_url.trim_end_matches('/');
    let manifest_url =
        format!("{edge_url}/admin/attachments?after_job_id={after_job_id}&limit={limit}");
    let http = reqwest::Client::new();
    let manifest: EdgeAttachmentResponse = http
        .get(manifest_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut written = 0usize;
    let mut failed = Vec::new();
    let mut last_job_id = after_job_id;
    for item in &manifest.attachments {
        last_job_id = item.job_id;
        match sync_one_attachment(client, &http, edge_url, admin_token, item, &attachment_dir).await
        {
            Ok(_) => written += 1,
            Err(error) => failed.push(serde_json::json!({
                "job_id": item.job_id,
                "error": error.to_string()
            })),
        }
    }
    write_cursor(&cursor_file, manifest.next_after_job_id)?;

    Ok(serde_json::json!({
        "ok": true,
        "scanned": manifest.attachments.len(),
        "written": written,
        "failed": failed,
        "failed_count": failed.len(),
        "after_job_id": after_job_id,
        "last_job_id": last_job_id,
        "next_after_job_id": manifest.next_after_job_id
    }))
}

async fn sync_one_attachment(
    client: &GeweSkillClient,
    http: &reqwest::Client,
    edge_url: &str,
    admin_token: &str,
    item: &EdgeAttachment,
    attachment_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let download_url = format!("{edge_url}{}", item.download_path);
    let response = http
        .get(download_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let object_key = format!("sha256/{}/{}", &sha256[..2], sha256);
    let path = attachment_dir.join(&object_key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        fs::write(&path, &bytes)?;
    }

    let kind = parse_attachment_kind(&item.asset_type);
    let message_key = item.message_key.clone().unwrap_or_else(|| {
        item.job_key
            .clone()
            .unwrap_or_else(|| format!("edge-job:{}", item.job_id))
    });
    let raw_event_dedupe_key = item
        .raw_event_dedupe_key
        .clone()
        .unwrap_or_else(|| message_key.clone());
    let record = AttachmentRecord {
        id: None,
        edge_job_id: Some(item.job_id),
        job_key: item.job_key.clone(),
        message_key,
        raw_event_dedupe_key,
        appid: item.appid.clone(),
        account_wxid: item.account_wxid.clone(),
        kind,
        variant: item.variant.clone(),
        source_url: Some(format!("{edge_url}{}", item.download_path)),
        object_key: Some(object_key),
        sha256: Some(sha256),
        size_bytes: Some(item.size_bytes.unwrap_or(bytes.len() as i64)),
        mime_type: item.mime_type.clone(),
        created_at: item
            .completed_at
            .clone()
            .or_else(|| item.created_at.clone())
            .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string()),
    };
    client.write_attachment(&record).await?;
    Ok(())
}

fn parse_attachment_kind(value: &str) -> AttachmentKind {
    match value {
        "image" => AttachmentKind::Image,
        "voice" => AttachmentKind::Voice,
        "video" => AttachmentKind::Video,
        "emoji" => AttachmentKind::Emoji,
        "file" => AttachmentKind::File,
        _ => AttachmentKind::File,
    }
}

fn print_json(value: impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
