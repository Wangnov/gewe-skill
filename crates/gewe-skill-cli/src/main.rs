use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::normalize_callback;
use gewe_skill_types::{
    ApiPage, AttachmentKind, AttachmentRecord, IdentityMatch, IdentityRefreshRequest, MessageQuery,
    NormalizedMessage, RawCallbackRequest, VoiceItem, VoiceQuery, VoiceTranscribeRequest,
    VoiceWarmRequest,
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
    /// Agent-friendly composed read queries that resolve names before fetching evidence.
    Query {
        #[command(subcommand)]
        command: QueryCommand,
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
    /// Inspect and transcribe voice messages that have synced audio attachments.
    Voice {
        #[command(subcommand)]
        command: VoiceCommand,
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
    /// Inspect local memory health and backlog before running repair actions.
    Maintenance {
        #[command(subcommand)]
        command: MaintenanceCommand,
    },
    /// Raw read-only API escape hatch using configured auth.
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
}

#[derive(Debug, Subcommand)]
enum QueryCommand {
    /// Resolve human names, then read a bounded message window with optional voice transcript evidence.
    Messages {
        #[command(flatten)]
        args: AgentMessageQueryArgs,
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
    /// Inspect current contact/member display memory and historical aliases for one wxid.
    Inspect {
        #[arg(long)]
        wxid: String,
        #[arg(long)]
        chatroom_id: Option<String>,
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

#[derive(Debug, Clone, Args)]
struct AgentMessageQueryArgs {
    /// Human chatroom/contact wording. The CLI resolves it to conversation_id before reading.
    #[arg(long)]
    conversation: Option<String>,
    /// Exact conversation_id escape hatch. Takes precedence over --conversation.
    #[arg(long)]
    conversation_id: Option<String>,
    /// Human sender/member wording. If conversation is a group, room-scoped member aliases are preferred.
    #[arg(long)]
    sender: Option<String>,
    /// Exact sender_wxid escape hatch. Takes precedence over --sender.
    #[arg(long)]
    sender_wxid: Option<String>,
    /// Optional text/XML search term.
    #[arg(long)]
    q: Option<String>,
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
    #[arg(long, default_value_t = 10)]
    resolve_limit: u32,
    /// Continue with a broad read if a provided human conversation/sender cannot be resolved.
    #[arg(long)]
    allow_unresolved: bool,
    /// Disable the companion voice query that returns transcript availability for the same scope.
    #[arg(long = "no-voice-transcripts", action = ArgAction::SetFalse, default_value_t = true)]
    voice_transcripts: bool,
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
enum VoiceCommand {
    /// List voice messages with attachment/transcript availability.
    List {
        #[command(flatten)]
        filters: VoiceFilterArgs,
        #[arg(long)]
        missing_only: bool,
    },
    /// Transcribe one voice message if its audio attachment is available.
    Transcribe {
        #[arg(long)]
        message_key: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Transcribe a bounded window of voice messages.
    Warm {
        #[command(flatten)]
        filters: VoiceFilterArgs,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Args)]
struct VoiceFilterArgs {
    #[arg(long)]
    conversation_id: Option<String>,
    #[arg(long)]
    sender_wxid: Option<String>,
    #[arg(long)]
    after: Option<String>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long, default_value_t = 10)]
    limit: i64,
    #[arg(long, value_enum, default_value = "desc")]
    order: Order,
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
    /// Pull chatroom member/system events from gewe-skill-edge and write them to memory.
    ChatroomEvents {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long)]
        chatroom_id: Option<String>,
        #[arg(long)]
        after_member_event_id: Option<i64>,
        #[arg(long)]
        after_system_event_id: Option<i64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(
            long,
            env = "GEWE_SKILL_CHATROOM_MEMBER_EVENT_CURSOR_FILE",
            default_value = "/opt/gewe-skill-memory/data/edge-chatroom-member-events.cursor"
        )]
        member_cursor_file: PathBuf,
        #[arg(
            long,
            env = "GEWE_SKILL_CHATROOM_SYSTEM_EVENT_CURSOR_FILE",
            default_value = "/opt/gewe-skill-memory/data/edge-chatroom-system-events.cursor"
        )]
        system_cursor_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum MaintenanceCommand {
    /// Summarize memory, attachment, voice transcript, identity, and chatroom event health.
    Status,
    /// Backfill ready voice messages that do not have completed transcripts yet.
    AsrBackfill {
        #[command(flatten)]
        filters: VoiceFilterArgs,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
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
struct EdgeChatroomMemberEventResponse {
    events: Vec<EdgeChatroomMemberEvent>,
    next_after_event_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EdgeChatroomMemberEvent {
    id: i64,
    received_at: String,
    event_type: String,
    chatroom_id: String,
    member_wxid: Option<String>,
    previous_chatroom_name: Option<String>,
    current_chatroom_name: Option<String>,
    previous_member_count: Option<i64>,
    current_member_count: Option<i64>,
    previous_snapshot_id: Option<i64>,
    current_snapshot_id: Option<i64>,
    details_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EdgeChatroomSystemEventResponse {
    events: Vec<EdgeChatroomSystemEvent>,
    next_after_event_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EdgeChatroomSystemEvent {
    id: i64,
    received_at: String,
    event_type: String,
    chatroom_id: String,
    actor_wxid: Option<String>,
    actor_name: Option<String>,
    target_wxid: Option<String>,
    target_name: Option<String>,
    target_wxids_json: Option<String>,
    target_names_json: Option<String>,
    previous_value: Option<String>,
    current_value: Option<String>,
    template_text: Option<String>,
    content_text: Option<String>,
    raw_event_id: Option<i64>,
    message_id: Option<i64>,
    details_json: Option<String>,
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
            IdentityCommand::Inspect { wxid, chatroom_id } => {
                print_json(
                    client
                        .identity_profile(&wxid, chatroom_id.as_deref())
                        .await?,
                )?;
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
        Command::Query { command } => match command {
            QueryCommand::Messages { args } => {
                print_json(agent_query_messages(&client, args).await?)?;
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
        Command::Voice { command } => match command {
            VoiceCommand::List {
                filters,
                missing_only,
            } => {
                print_json(
                    client
                        .voices(&voice_query(filters, Some(missing_only)))
                        .await?,
                )?;
            }
            VoiceCommand::Transcribe {
                message_key,
                provider,
                language,
                force,
            } => {
                print_json(
                    client
                        .transcribe_voice(&VoiceTranscribeRequest {
                            message_key,
                            provider,
                            language,
                            force: Some(force),
                        })
                        .await?,
                )?;
            }
            VoiceCommand::Warm {
                filters,
                provider,
                language,
                force,
            } => {
                print_json(
                    client
                        .warm_voice(&VoiceWarmRequest {
                            conversation_id: filters.conversation_id,
                            sender_wxid: filters.sender_wxid,
                            after: filters.after,
                            before: filters.before,
                            limit: Some(filters.limit),
                            provider,
                            language,
                            force: Some(force),
                        })
                        .await?,
                )?;
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
            SyncCommand::ChatroomEvents {
                edge_url,
                admin_token,
                chatroom_id,
                after_member_event_id,
                after_system_event_id,
                limit,
                member_cursor_file,
                system_cursor_file,
            } => {
                let result = sync_chatroom_events(
                    &client,
                    &edge_url,
                    &admin_token,
                    chatroom_id,
                    after_member_event_id,
                    after_system_event_id,
                    limit,
                    member_cursor_file,
                    system_cursor_file,
                )
                .await?;
                print_json(result)?;
            }
        },
        Command::Maintenance { command } => match command {
            MaintenanceCommand::Status => {
                print_json(client.maintenance_status().await?)?;
            }
            MaintenanceCommand::AsrBackfill {
                filters,
                provider,
                language,
                force,
            } => {
                print_json(
                    client
                        .warm_voice(&VoiceWarmRequest {
                            conversation_id: filters.conversation_id,
                            sender_wxid: filters.sender_wxid,
                            after: filters.after,
                            before: filters.before,
                            limit: Some(filters.limit),
                            provider,
                            language,
                            force: Some(force),
                        })
                        .await?,
                )?;
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

async fn agent_query_messages(
    client: &GeweSkillClient,
    args: AgentMessageQueryArgs,
) -> Result<Value, Box<dyn std::error::Error>> {
    let conversation = resolve_conversation_for_query(
        client,
        args.conversation_id.clone(),
        args.conversation.clone(),
        args.resolve_limit,
    )
    .await?;
    let sender = resolve_sender_for_query(
        client,
        args.sender_wxid.clone(),
        args.sender.clone(),
        conversation.id.as_deref(),
        args.resolve_limit,
    )
    .await?;

    if args.conversation.is_some() && conversation.id.is_none() && !args.allow_unresolved {
        return Ok(serde_json::json!({
            "ok": false,
            "executed": false,
            "error": "conversation_unresolved",
            "resolved": {
                "conversation": conversation.to_json(),
                "sender": sender.to_json(),
            }
        }));
    }
    if args.sender.is_some() && sender.id.is_none() && !args.allow_unresolved {
        return Ok(serde_json::json!({
            "ok": false,
            "executed": false,
            "error": "sender_unresolved",
            "resolved": {
                "conversation": conversation.to_json(),
                "sender": sender.to_json(),
            }
        }));
    }

    let query = MessageQuery {
        q: args.q.clone(),
        conversation_id: conversation.id.clone(),
        sender_wxid: sender.id.clone(),
        kind: args.kind.clone(),
        direction: args.direction.query_value(),
        after: args.after.clone(),
        before: args.before.clone(),
        cursor: args.cursor.clone(),
        limit: Some(args.limit),
        order: Some(args.order.query_value()),
    };
    let messages: ApiPage<NormalizedMessage> = client.messages(&query).await?;
    let voice: Option<ApiPage<VoiceItem>> = if args.voice_transcripts {
        Some(
            client
                .voices(&VoiceQuery {
                    conversation_id: query.conversation_id.clone(),
                    sender_wxid: query.sender_wxid.clone(),
                    after: query.after.clone(),
                    before: query.before.clone(),
                    cursor: query.cursor.clone(),
                    limit: query.limit,
                    order: query.order.clone(),
                    missing_only: None,
                })
                .await?,
        )
    } else {
        None
    };

    Ok(serde_json::json!({
        "ok": true,
        "executed": true,
        "query_mode": "agent_messages",
        "resolved": {
            "conversation": conversation.to_json(),
            "sender": sender.to_json(),
        },
        "message_query": query,
        "messages": messages,
        "voice": {
            "included": args.voice_transcripts,
            "items": voice.map(|page| page.items).unwrap_or_default()
        }
    }))
}

#[derive(Debug, Clone)]
struct QueryResolution {
    input: Option<String>,
    id: Option<String>,
    selected: Option<IdentityMatch>,
    candidates: Vec<IdentityMatch>,
    source: &'static str,
}

impl QueryResolution {
    fn exact(id: Option<String>) -> Self {
        Self {
            input: id.clone(),
            id,
            selected: None,
            candidates: Vec::new(),
            source: "exact",
        }
    }

    fn missing(input: Option<String>, candidates: Vec<IdentityMatch>) -> Self {
        Self {
            input,
            id: None,
            selected: None,
            candidates,
            source: "unresolved",
        }
    }

    fn from_identity(
        input: Option<String>,
        id: Option<String>,
        selected: Option<IdentityMatch>,
        candidates: Vec<IdentityMatch>,
    ) -> Self {
        Self {
            input,
            id,
            selected,
            candidates,
            source: "identity",
        }
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "input": self.input.clone(),
            "id": self.id.clone(),
            "source": self.source,
            "selected": self.selected.clone(),
            "candidates": self.candidates.clone(),
        })
    }
}

async fn resolve_conversation_for_query(
    client: &GeweSkillClient,
    exact: Option<String>,
    input: Option<String>,
    limit: u32,
) -> Result<QueryResolution, Box<dyn std::error::Error>> {
    if exact
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(QueryResolution::exact(exact));
    }
    let Some(input) = input.filter(|value| !value.trim().is_empty()) else {
        return Ok(QueryResolution::missing(None, Vec::new()));
    };
    if looks_like_stable_wechat_id(&input) {
        return Ok(QueryResolution::exact(Some(input)));
    }

    let response = client.resolve_identity(&input, Some(limit)).await?;
    let selected = response
        .items
        .iter()
        .find(|item| item.entity_type == "chatroom")
        .or_else(|| {
            response
                .items
                .iter()
                .find(|item| item.entity_type == "contact")
        })
        .or_else(|| {
            response
                .items
                .iter()
                .find(|item| item.entity_type == "chatroom_member")
        })
        .cloned();
    let id = selected
        .as_ref()
        .and_then(conversation_id_from_identity_match);
    Ok(QueryResolution::from_identity(
        Some(input),
        id,
        selected,
        response.items,
    ))
}

async fn resolve_sender_for_query(
    client: &GeweSkillClient,
    exact: Option<String>,
    input: Option<String>,
    conversation_id: Option<&str>,
    limit: u32,
) -> Result<QueryResolution, Box<dyn std::error::Error>> {
    if exact
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(QueryResolution::exact(exact));
    }
    let Some(input) = input.filter(|value| !value.trim().is_empty()) else {
        return Ok(QueryResolution::missing(None, Vec::new()));
    };
    if looks_like_stable_wechat_id(&input) {
        return Ok(QueryResolution::exact(Some(input)));
    }

    let response = client.resolve_identity(&input, Some(limit.max(20))).await?;
    let selected = response
        .items
        .iter()
        .find(|item| {
            item.entity_type == "chatroom_member" && item.chatroom_id.as_deref() == conversation_id
        })
        .or_else(|| {
            response
                .items
                .iter()
                .find(|item| item.entity_type == "contact")
        })
        .or_else(|| {
            response
                .items
                .iter()
                .find(|item| item.entity_type == "chatroom_member")
        })
        .cloned();
    let id = selected.as_ref().map(|item| item.entity_id.clone());
    Ok(QueryResolution::from_identity(
        Some(input),
        id,
        selected,
        response.items,
    ))
}

fn conversation_id_from_identity_match(item: &IdentityMatch) -> Option<String> {
    match item.entity_type.as_str() {
        "chatroom" | "contact" => Some(item.entity_id.clone()),
        "chatroom_member" => item.chatroom_id.clone(),
        _ => None,
    }
}

fn looks_like_stable_wechat_id(value: &str) -> bool {
    let value = value.trim();
    value.ends_with("@chatroom")
        || value.starts_with("wxid_")
        || value.starts_with("gh_")
        || value.starts_with("openim_")
}

fn voice_query(filters: VoiceFilterArgs, missing_only: Option<bool>) -> VoiceQuery {
    VoiceQuery {
        conversation_id: filters.conversation_id,
        sender_wxid: filters.sender_wxid,
        after: filters.after,
        before: filters.before,
        cursor: filters.cursor,
        limit: Some(filters.limit),
        order: Some(filters.order.query_value()),
        missing_only,
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

async fn sync_chatroom_events(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    chatroom_id: Option<String>,
    after_member_event_id: Option<i64>,
    after_system_event_id: Option<i64>,
    limit: u32,
    member_cursor_file: PathBuf,
    system_cursor_file: PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let after_member_event_id =
        after_member_event_id.unwrap_or_else(|| read_cursor(&member_cursor_file).unwrap_or(0));
    let after_system_event_id =
        after_system_event_id.unwrap_or_else(|| read_cursor(&system_cursor_file).unwrap_or(0));
    let edge_url = edge_url.trim_end_matches('/');
    let http = reqwest::Client::new();

    let mut member_url = reqwest::Url::parse(&format!("{edge_url}/admin/chatroom-events"))?;
    member_url
        .query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("after_id", &after_member_event_id.to_string());
    if let Some(chatroom_id) = chatroom_id.as_deref() {
        member_url
            .query_pairs_mut()
            .append_pair("chatroom_id", chatroom_id);
    }
    let member_response: EdgeChatroomMemberEventResponse = http
        .get(member_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut system_url = reqwest::Url::parse(&format!("{edge_url}/admin/chatroom-system-events"))?;
    system_url
        .query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("after_id", &after_system_event_id.to_string());
    if let Some(chatroom_id) = chatroom_id.as_deref() {
        system_url
            .query_pairs_mut()
            .append_pair("chatroom_id", chatroom_id);
    }
    let system_response: EdgeChatroomSystemEventResponse = http
        .get(system_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut failed = Vec::new();
    let mut member_events = Vec::new();
    for event in member_response.events {
        match parse_chatroom_event_type(&event.event_type) {
            Ok(event_type) => member_events.push(gewe_skill_types::ChatroomMemberEvent {
                event_type,
                chatroom_id: event.chatroom_id,
                member_wxid: event.member_wxid,
                previous_chatroom_name: event.previous_chatroom_name,
                current_chatroom_name: event.current_chatroom_name,
                previous_member_count: event.previous_member_count,
                current_member_count: event.current_member_count,
                received_at: event.received_at,
                details: edge_event_details(
                    event.details_json.as_deref(),
                    event.id,
                    &[
                        ("previous_snapshot_id", event.previous_snapshot_id),
                        ("current_snapshot_id", event.current_snapshot_id),
                    ],
                ),
            }),
            Err(error) => failed.push(serde_json::json!({
                "kind": "member_event",
                "edge_event_id": event.id,
                "event_type": event.event_type,
                "error": error.to_string()
            })),
        }
    }

    let mut system_events = Vec::new();
    for event in system_response.events {
        match parse_chatroom_event_type(&event.event_type) {
            Ok(event_type) => system_events.push(gewe_skill_types::ChatroomSystemEvent {
                event_type,
                chatroom_id: event.chatroom_id,
                actor_wxid: event.actor_wxid,
                actor_name: event.actor_name,
                target_wxid: event.target_wxid,
                target_name: event.target_name,
                target_wxids: parse_edge_string_vec(event.target_wxids_json.as_deref()),
                target_names: parse_edge_string_vec(event.target_names_json.as_deref()),
                previous_value: event.previous_value,
                current_value: event.current_value,
                template_text: event.template_text,
                content_text: event.content_text,
                received_at: event.received_at,
                details: edge_event_details(
                    event.details_json.as_deref(),
                    event.id,
                    &[
                        ("raw_event_id", event.raw_event_id),
                        ("message_id", event.message_id),
                    ],
                ),
            }),
            Err(error) => failed.push(serde_json::json!({
                "kind": "system_event",
                "edge_event_id": event.id,
                "event_type": event.event_type,
                "error": error.to_string()
            })),
        }
    }

    let member_event_count = member_events.len();
    let system_event_count = system_events.len();
    let write_response = client
        .write_chatroom_events(&gewe_skill_types::ChatroomEventWriteRequest {
            member_events,
            system_events,
        })
        .await?;

    let next_after_member_event_id = member_response
        .next_after_event_id
        .unwrap_or(after_member_event_id);
    let next_after_system_event_id = system_response
        .next_after_event_id
        .unwrap_or(after_system_event_id);
    if failed.is_empty() {
        write_cursor(&member_cursor_file, next_after_member_event_id)?;
        write_cursor(&system_cursor_file, next_after_system_event_id)?;
    }

    Ok(serde_json::json!({
        "ok": failed.is_empty(),
        "member_events_scanned": member_event_count + failed.iter().filter(|item| item.get("kind").and_then(Value::as_str) == Some("member_event")).count(),
        "system_events_scanned": system_event_count + failed.iter().filter(|item| item.get("kind").and_then(Value::as_str) == Some("system_event")).count(),
        "member_events_written": member_event_count,
        "system_events_written": system_event_count,
        "failed_count": failed.len(),
        "failed": failed,
        "after_member_event_id": after_member_event_id,
        "after_system_event_id": after_system_event_id,
        "next_after_member_event_id": next_after_member_event_id,
        "next_after_system_event_id": next_after_system_event_id,
        "write_response": write_response
    }))
}

fn parse_chatroom_event_type(
    event_type: &str,
) -> Result<gewe_skill_types::ChatroomEventType, serde_json::Error> {
    serde_json::from_value(Value::String(event_type.to_string()))
}

fn edge_event_details(
    details_json: Option<&str>,
    edge_event_id: i64,
    ids: &[(&str, Option<i64>)],
) -> Value {
    let details = parse_edge_json_value(details_json);
    let mut object = match details {
        Value::Object(object) => object,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut object = serde_json::Map::new();
            object.insert("edge_original_details".to_string(), other);
            object
        }
    };
    object.insert(
        "edge_event_id".to_string(),
        serde_json::json!(edge_event_id),
    );
    for (key, value) in ids {
        if let Some(value) = value {
            object.insert((*key).to_string(), serde_json::json!(value));
        }
    }
    Value::Object(object)
}

fn parse_edge_json_value(raw: Option<&str>) -> Value {
    raw.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or(Value::Null)
}

fn parse_edge_string_vec(raw: Option<&str>) -> Vec<String> {
    match parse_edge_json_value(raw) {
        Value::Array(values) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect(),
        Value::String(value) if !value.is_empty() => vec![value],
        _ => Vec::new(),
    }
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
    let configured_cursor_overlap = attachment_cursor_overlap();
    let cursor_overlap = configured_cursor_overlap.min(i64::from(limit) / 2);
    let after_job_id = after_job_id
        .unwrap_or_else(|| (read_cursor(&cursor_file).unwrap_or(0) - cursor_overlap).max(0));
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
        "next_after_job_id": manifest.next_after_job_id,
        "cursor_overlap": cursor_overlap,
        "configured_cursor_overlap": configured_cursor_overlap
    }))
}

fn attachment_cursor_overlap() -> i64 {
    std::env::var("GEWE_SKILL_ATTACHMENT_CURSOR_OVERLAP")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(500)
        .clamp(0, 5000)
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
