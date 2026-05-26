mod attachment_maintenance;
mod chatroom_events;
mod voice_maintenance;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::normalize_callback;
use gewe_skill_types::{
    ApiPage, AttachmentKind, AttachmentRecord, IdentityEventBackfillRequest, IdentityMatch,
    IdentityProfileResponse, IdentityRefreshRequest, MessageQuery, NormalizedKind,
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
    time::Duration,
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
    /// Resolve a group name, then read member/system events as one bounded timeline.
    ChatroomEvents {
        #[command(flatten)]
        args: AgentChatroomEventQueryArgs,
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
    /// Disable exact attachment lookup for the returned message window.
    #[arg(long = "no-attachments", action = ArgAction::SetFalse, default_value_t = true)]
    attachments: bool,
    /// Disable speaker identity lookup for the returned message window.
    #[arg(long = "no-speakers", action = ArgAction::SetFalse, default_value_t = true)]
    speakers: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct AgentChatroomEventQueryArgs {
    /// Human chatroom wording. The CLI resolves it to chatroom_id before reading.
    #[arg(long)]
    pub(crate) conversation: Option<String>,
    /// Exact chatroom_id escape hatch. Takes precedence over --conversation.
    #[arg(long)]
    pub(crate) chatroom_id: Option<String>,
    /// Filter event types such as member_joined, member_left, member_removed, member_invited, chatroom_name_changed.
    #[arg(long, value_delimiter = ',')]
    pub(crate) event_type: Vec<String>,
    #[arg(long)]
    pub(crate) after: Option<String>,
    #[arg(long)]
    pub(crate) before: Option<String>,
    #[arg(long, default_value_t = 50)]
    pub(crate) limit: usize,
    #[arg(long, value_enum, default_value = "desc")]
    pub(crate) order: Order,
    #[arg(long, default_value_t = 20)]
    pub(crate) resolve_limit: u32,
    /// Continue with an empty result if a provided human group name cannot be resolved.
    #[arg(long)]
    pub(crate) allow_unresolved: bool,
    /// Disable snapshot-diff member events.
    #[arg(long = "no-member-events", action = ArgAction::SetFalse, default_value_t = true)]
    pub(crate) member_events: bool,
    /// Disable structured system events.
    #[arg(long = "no-system-events", action = ArgAction::SetFalse, default_value_t = true)]
    pub(crate) system_events: bool,
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
pub(crate) enum Order {
    Asc,
    Desc,
}

impl Order {
    pub(crate) fn query_value(&self) -> String {
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
    /// Inspect the edge attachment download queue.
    AttachmentQueue {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        asset_type: Option<String>,
        #[arg(long)]
        after_job_id: Option<i64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Backfill missing edge attachment download jobs from stored messages.
    AttachmentBackfill {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long, default_value = "voice")]
        asset_type: String,
        #[arg(long)]
        schema_version: Option<String>,
        #[arg(long, default_value_t = false)]
        include_existing: bool,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Requeue pending/retryable/stale edge attachment download jobs.
    AttachmentRequeue {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
    },
    /// Reset terminal edge attachment jobs, then send them back to the queue.
    AttachmentRetry {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long, default_value = "failed")]
        status: String,
        #[arg(long)]
        asset_type: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Run one bounded attachment queue repair sweep: backfill, requeue, then sync completed files.
    AttachmentRepair {
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        #[arg(long, default_value = "voice")]
        asset_type: String,
        #[arg(long)]
        schema_version: Option<String>,
        #[arg(long, default_value_t = false)]
        include_existing: bool,
        #[arg(long, default_value_t = 100)]
        backfill_limit: u32,
        #[arg(long, default_value_t = 50)]
        sync_limit: u32,
        #[arg(long, default_value_t = 2_000)]
        settle_ms: u64,
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
    /// Agent-oriented data readiness report with ordered maintenance actions.
    DataHealth {
        /// Include Cloudflare edge attachment queue evidence. Recommended before media-heavy analysis.
        #[arg(long, default_value_t = false)]
        with_edge_queue: bool,
        /// Edge worker base URL.
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        /// Edge admin token.
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        edge_admin_token: Option<String>,
        /// Maximum voice rows to inspect for ASR and attachment readiness.
        #[arg(long, default_value_t = 200)]
        voice_limit: i64,
        /// Maximum edge attachment jobs to inspect when --with-edge-queue is enabled.
        #[arg(long, default_value_t = 1000)]
        edge_queue_limit: u32,
    },
    /// Inspect edge attachment queue health across all asset types.
    AttachmentQueueHealth {
        /// Edge worker base URL.
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        /// Edge admin token.
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        edge_admin_token: Option<String>,
        /// Optional edge asset type filter, such as image, voice, video, emoji, or file.
        #[arg(long)]
        asset_type: Option<String>,
        /// Maximum edge jobs to inspect.
        #[arg(long, default_value_t = 1000)]
        limit: u32,
    },
    /// List actionable voice attachment and ASR maintenance issues.
    VoiceIssues {
        #[command(flatten)]
        filters: VoiceFilterArgs,
        #[arg(long, default_value_t = false)]
        with_edge_queue: bool,
        #[arg(
            long,
            env = "GEWE_SKILL_EDGE_URL",
            default_value = "https://gewe-agent.wangnov-ai.com"
        )]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN", hide_env_values = true)]
        edge_admin_token: Option<String>,
        #[arg(long, default_value_t = 500)]
        edge_queue_limit: u32,
    },
    /// Run a bounded ASR repair pass and return before/after voice issue counts.
    VoiceRepair {
        #[command(flatten)]
        filters: VoiceFilterArgs,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Refresh missing display memory for wxids seen in recent chatroom events.
    IdentityBackfill {
        #[arg(long, default_value_t = 500)]
        event_limit: i64,
        #[arg(long, default_value_t = 10)]
        max_chatrooms: i64,
        #[arg(long, default_value_t = 100)]
        max_wxids: i64,
        #[arg(long = "no-contact-detail", action = ArgAction::SetFalse, default_value_t = true)]
        contact_detail: bool,
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
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
            QueryCommand::ChatroomEvents { args } => {
                print_json(chatroom_events::agent_query_chatroom_events(&client, args).await?)?;
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
            SyncCommand::AttachmentQueue {
                edge_url,
                admin_token,
                status,
                asset_type,
                after_job_id,
                limit,
            } => {
                let result = edge_download_jobs(
                    &edge_url,
                    &admin_token,
                    status,
                    asset_type,
                    after_job_id,
                    limit,
                )
                .await?;
                print_json(result)?;
            }
            SyncCommand::AttachmentBackfill {
                edge_url,
                admin_token,
                asset_type,
                schema_version,
                include_existing,
                limit,
            } => {
                let result = backfill_edge_download_jobs(
                    &edge_url,
                    &admin_token,
                    asset_type,
                    schema_version,
                    include_existing,
                    limit,
                )
                .await?;
                print_json(result)?;
            }
            SyncCommand::AttachmentRequeue {
                edge_url,
                admin_token,
            } => {
                let result = requeue_edge_download_jobs(&edge_url, &admin_token).await?;
                print_json(result)?;
            }
            SyncCommand::AttachmentRetry {
                edge_url,
                admin_token,
                status,
                asset_type,
                limit,
            } => {
                let result =
                    retry_edge_download_jobs(&edge_url, &admin_token, status, asset_type, limit)
                        .await?;
                print_json(result)?;
            }
            SyncCommand::AttachmentRepair {
                edge_url,
                admin_token,
                asset_type,
                schema_version,
                include_existing,
                backfill_limit,
                sync_limit,
                settle_ms,
                cursor_file,
                attachment_dir,
            } => {
                let result = repair_edge_attachment_queue(
                    &client,
                    &edge_url,
                    &admin_token,
                    asset_type,
                    schema_version,
                    include_existing,
                    backfill_limit,
                    sync_limit,
                    settle_ms,
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
            MaintenanceCommand::DataHealth {
                with_edge_queue,
                edge_url,
                edge_admin_token,
                voice_limit,
                edge_queue_limit,
            } => {
                let status = client.maintenance_status().await?;
                let edge_queue = if with_edge_queue {
                    let Some(admin_token) = edge_admin_token else {
                        return Err(
                            "missing GEWE_SKILL_EDGE_ADMIN_TOKEN for --with-edge-queue".into()
                        );
                    };
                    Some(
                        edge_download_jobs(
                            &edge_url,
                            &admin_token,
                            None,
                            None,
                            None,
                            edge_queue_limit,
                        )
                        .await?,
                    )
                } else {
                    None
                };
                let attachment_queue = if let Some(jobs) = edge_queue.as_ref() {
                    let completed_job_keys = attachment_maintenance::completed_job_keys(jobs);
                    let memory_attachments =
                        client.attachments_by_job_keys(&completed_job_keys).await?;
                    let memory_attachments = serde_json::to_value(memory_attachments)?;
                    Some(attachment_maintenance::queue_health_with_memory(
                        jobs,
                        &memory_attachments,
                    ))
                } else {
                    None
                };
                let voice_issues = voice_maintenance::voice_issues_with_edge_queue(
                    &client,
                    VoiceQuery {
                        conversation_id: None,
                        sender_wxid: None,
                        after: None,
                        before: None,
                        cursor: None,
                        limit: Some(voice_limit),
                        order: Some("desc".to_string()),
                        missing_only: None,
                    },
                    edge_queue,
                )
                .await?;
                print_json(maintenance_data_health_report(
                    status,
                    attachment_queue,
                    voice_issues,
                    with_edge_queue,
                    voice_limit,
                    edge_queue_limit,
                ))?;
            }
            MaintenanceCommand::AttachmentQueueHealth {
                edge_url,
                edge_admin_token,
                asset_type,
                limit,
            } => {
                let admin_token = edge_admin_token.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "--edge-admin-token or GEWE_SKILL_EDGE_ADMIN_TOKEN is required for attachment queue health"
                    )
                })?;
                let jobs =
                    edge_download_jobs(&edge_url, &admin_token, None, asset_type, None, limit)
                        .await?;
                let completed_job_keys = attachment_maintenance::completed_job_keys(&jobs);
                let memory_attachments =
                    client.attachments_by_job_keys(&completed_job_keys).await?;
                let memory_attachments = serde_json::to_value(memory_attachments)?;
                let result =
                    attachment_maintenance::queue_health_with_memory(&jobs, &memory_attachments);
                print_json(&result)?;
            }

            MaintenanceCommand::VoiceIssues {
                filters,
                with_edge_queue,
                edge_url,
                edge_admin_token,
                edge_queue_limit,
            } => {
                let edge_queue = if with_edge_queue {
                    let Some(admin_token) = edge_admin_token else {
                        return Err(
                            "missing GEWE_SKILL_EDGE_ADMIN_TOKEN for --with-edge-queue".into()
                        );
                    };
                    Some(
                        edge_download_jobs(
                            &edge_url,
                            &admin_token,
                            None,
                            Some("voice".to_string()),
                            None,
                            edge_queue_limit,
                        )
                        .await?,
                    )
                } else {
                    None
                };
                print_json(
                    voice_maintenance::voice_issues_with_edge_queue(
                        &client,
                        voice_query(filters, None),
                        edge_queue,
                    )
                    .await?,
                )?;
            }
            MaintenanceCommand::VoiceRepair {
                filters,
                provider,
                language,
                force,
            } => {
                print_json(
                    voice_maintenance::voice_repair(
                        &client,
                        voice_query(filters, None),
                        provider,
                        language,
                        force,
                    )
                    .await?,
                )?;
            }
            MaintenanceCommand::IdentityBackfill {
                event_limit,
                max_chatrooms,
                max_wxids,
                contact_detail,
                dry_run,
            } => {
                print_json(
                    client
                        .maintenance_identity_event_backfill(&IdentityEventBackfillRequest {
                            event_limit: Some(event_limit),
                            max_chatrooms: Some(max_chatrooms),
                            max_wxids: Some(max_wxids),
                            contact_detail: Some(contact_detail),
                            dry_run: Some(dry_run),
                        })
                        .await?,
                )?;
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
    let speaker_chatroom_id = conversation
        .id
        .as_deref()
        .filter(|conversation_id| conversation_id.ends_with("@chatroom"));
    let speaker_block =
        agent_speaker_block(client, args.speakers, &messages.items, speaker_chatroom_id).await?;
    let message_keys = messages
        .items
        .iter()
        .map(|message| message.message_key.clone())
        .collect::<Vec<_>>();
    let attachments: Option<ApiPage<AttachmentRecord>> = if args.attachments {
        Some(client.attachments_by_message_keys(&message_keys).await?)
    } else {
        None
    };
    let attachment_block = agent_attachment_block(
        args.attachments,
        &messages.items,
        attachments.as_ref().map(|page| page.items.as_slice()),
    );
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
        "speakers": speaker_block,
        "attachments": attachment_block,
        "voice": {
            "included": args.voice_transcripts,
            "items": voice.map(|page| page.items).unwrap_or_default()
        }
    }))
}

async fn agent_speaker_block(
    client: &GeweSkillClient,
    included: bool,
    messages: &[NormalizedMessage],
    chatroom_id: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let speaker_wxids = collect_message_speaker_wxids(messages);
    let messages_missing_sender_count = messages
        .iter()
        .filter(|message| {
            message
                .sender_wxid
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        })
        .count();

    if !included {
        return Ok(serde_json::json!({
            "included": false,
            "by_wxid": {},
            "lookup_errors": [],
            "summary": {
                "message_count": messages.len(),
                "speaker_count": speaker_wxids.len(),
                "messages_missing_sender_count": messages_missing_sender_count,
            }
        }));
    }

    let mut by_wxid = serde_json::Map::new();
    let mut lookup_errors = Vec::new();
    let mut missing_effective_display_count = 0usize;

    for wxid in &speaker_wxids {
        match client.identity_profile(wxid, chatroom_id).await {
            Ok(profile) => {
                if profile
                    .effective_display_name
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    missing_effective_display_count += 1;
                }
                let value = agent_speaker_profile_value(&profile);
                by_wxid.insert(profile.entity_id.clone(), value);
            }
            Err(error) => {
                lookup_errors.push(serde_json::json!({
                    "wxid": wxid,
                    "error": error.to_string(),
                }));
            }
        }
    }

    let resolved_count = by_wxid.len();
    let lookup_error_count = lookup_errors.len();

    Ok(serde_json::json!({
        "included": true,
        "by_wxid": by_wxid,
        "lookup_errors": lookup_errors,
        "summary": {
            "message_count": messages.len(),
            "speaker_count": speaker_wxids.len(),
            "resolved_count": resolved_count,
            "lookup_error_count": lookup_error_count,
            "messages_missing_sender_count": messages_missing_sender_count,
            "missing_effective_display_count": missing_effective_display_count,
            "chatroom_id": chatroom_id,
            "chatroom_scoped": chatroom_id.is_some(),
            "agent_notes": [
                "use speakers.by_wxid[message.sender_wxid].effective_display_name for human-facing names",
                "keep message.sender_wxid as the stable evidence id when explaining who said something",
                "for chatrooms, display names prefer contact remark, then room-scoped card/display names, then nicknames"
            ]
        }
    }))
}

fn agent_speaker_profile_value(profile: &IdentityProfileResponse) -> Value {
    let contact = profile.contact.as_ref().map(|contact| {
        serde_json::json!({
            "wxid": contact.wxid.clone(),
            "nickname": contact.nickname.clone(),
            "remark": contact.remark.clone(),
            "alias": contact.alias.clone(),
            "last_seen_at": contact.last_seen_at.clone(),
            "updated_at": contact.updated_at.clone(),
        })
    });
    let chatroom_member = profile.chatroom_member.as_ref().map(|member| {
        serde_json::json!({
            "chatroom_id": member.chatroom_id.clone(),
            "member_wxid": member.member_wxid.clone(),
            "display_name": member.display_name.clone(),
            "nickname": member.nickname.clone(),
            "is_current": member.is_current,
            "last_seen_at": member.last_seen_at.clone(),
        })
    });

    serde_json::json!({
        "entity_id": profile.entity_id.clone(),
        "chatroom_id": profile.chatroom_id.clone(),
        "effective_display_name": profile.effective_display_name.clone(),
        "display_name_source": speaker_display_source(profile),
        "contact": contact,
        "chatroom_member": chatroom_member,
        "aliases": profile.aliases.clone(),
    })
}

fn collect_message_speaker_wxids(messages: &[NormalizedMessage]) -> Vec<String> {
    let mut wxids = std::collections::BTreeSet::new();
    for message in messages {
        if let Some(wxid) = message.sender_wxid.as_deref() {
            let wxid = wxid.trim();
            if !wxid.is_empty() {
                wxids.insert(wxid.to_string());
            }
        }
    }
    wxids.into_iter().collect()
}

fn speaker_display_source(profile: &IdentityProfileResponse) -> &'static str {
    if profile
        .contact
        .as_ref()
        .and_then(|contact| contact.remark.as_deref())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        "contact_remark"
    } else if profile
        .chatroom_member
        .as_ref()
        .and_then(|member| member.display_name.as_deref())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        "chatroom_display_name"
    } else if profile
        .chatroom_member
        .as_ref()
        .and_then(|member| member.nickname.as_deref())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        "chatroom_nickname"
    } else if profile
        .contact
        .as_ref()
        .and_then(|contact| contact.nickname.as_deref())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        "contact_nickname"
    } else if profile
        .effective_display_name
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        "effective_display_name"
    } else {
        "unresolved"
    }
}

fn agent_attachment_block(
    included: bool,
    messages: &[NormalizedMessage],
    attachments: Option<&[AttachmentRecord]>,
) -> Value {
    if !included {
        return serde_json::json!({
            "included": false,
            "items": [],
            "by_message_key": {},
            "summary": {
                "message_count": messages.len(),
                "attachment_count": 0
            }
        });
    }

    let attachments = attachments.unwrap_or_default();
    let mut by_message = std::collections::BTreeMap::<String, Vec<&AttachmentRecord>>::new();
    let mut by_kind = std::collections::BTreeMap::<String, usize>::new();
    for attachment in attachments {
        by_message
            .entry(attachment.message_key.clone())
            .or_default()
            .push(attachment);
        *by_kind
            .entry(attachment_kind_value(&attachment.kind).to_string())
            .or_default() += 1;
    }

    let mut by_message_json = serde_json::Map::new();
    for (message_key, items) in &by_message {
        let mut kinds = std::collections::BTreeSet::<String>::new();
        let mut sha256s = Vec::<String>::new();
        for item in items {
            kinds.insert(attachment_kind_value(&item.kind).to_string());
            if let Some(sha256) = item.sha256.as_deref() {
                if !sha256.is_empty() {
                    sha256s.push(sha256.to_string());
                }
            }
        }
        sha256s.sort();
        sha256s.dedup();
        by_message_json.insert(
            message_key.clone(),
            serde_json::json!({
                "count": items.len(),
                "kinds": kinds.into_iter().collect::<Vec<_>>(),
                "sha256s": sha256s,
            }),
        );
    }

    let attachment_expected_message_keys = messages
        .iter()
        .filter(|message| message_kind_expects_attachment(&message.kind))
        .map(|message| message.message_key.clone())
        .collect::<Vec<_>>();
    let attachment_expected_missing_message_keys = attachment_expected_message_keys
        .iter()
        .filter(|message_key| !by_message.contains_key(*message_key))
        .take(20)
        .cloned()
        .collect::<Vec<_>>();

    serde_json::json!({
        "included": true,
        "items": attachments,
        "by_message_key": by_message_json,
        "summary": {
            "message_count": messages.len(),
            "attachment_count": attachments.len(),
            "messages_with_attachments": by_message.len(),
            "attachment_expected_message_count": attachment_expected_message_keys.len(),
            "attachment_expected_missing_count": attachment_expected_message_keys
                .iter()
                .filter(|message_key| !by_message.contains_key(*message_key))
                .count(),
            "attachment_expected_missing_message_keys": attachment_expected_missing_message_keys,
            "by_kind": by_kind,
            "agent_notes": [
                "attachments are looked up exactly by message_key for the returned message window",
                "attachment_expected_missing_count means media-like messages in this window currently have no stored attachment record",
                "voice transcript availability is reported separately under voice.items"
            ]
        }
    })
}

fn attachment_kind_value(kind: &AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Voice => "voice",
        AttachmentKind::Video => "video",
        AttachmentKind::Emoji => "emoji",
        AttachmentKind::File => "file",
    }
}

fn message_kind_expects_attachment(kind: &NormalizedKind) -> bool {
    matches!(
        kind,
        NormalizedKind::Image
            | NormalizedKind::Voice
            | NormalizedKind::Video
            | NormalizedKind::Emoji
            | NormalizedKind::File
    )
}

fn maintenance_data_health_report(
    status: Value,
    attachment_queue: Option<Value>,
    voice_issues: Value,
    edge_queue_checked: bool,
    voice_limit: i64,
    edge_queue_limit: u32,
) -> Value {
    let attachment_health = attachment_queue
        .as_ref()
        .and_then(|value| value.get("queue_health"))
        .and_then(Value::as_str)
        .unwrap_or("not_checked");
    let completed_not_ingested_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["completed_not_ingested_count"]))
        .unwrap_or(0);
    let retryable_terminal_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["retryable_terminal_count"]))
        .unwrap_or(0);
    let duplicate_job_key_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["duplicate_job_key_count"]))
        .unwrap_or(0);
    let duplicate_message_key_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["duplicate_message_key_count"]))
        .unwrap_or(0);
    let active_attachment_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["active_count"]))
        .unwrap_or(0);
    let non_retryable_terminal_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["non_retryable_terminal_count"]))
        .unwrap_or(0);
    let voice_issue_count = value_u64_at(&voice_issues, &["issue_count"]);
    let missing_attachment_count =
        value_u64_at(&voice_issues, &["by_issue_type", "missing_attachment"]);
    let retryable_missing_attachment_count =
        count_voice_issues_matching(&voice_issues, Some("missing_attachment"), Some(true), None);
    let known_unavailable_voice_attachment_count = count_voice_issues_matching(
        &voice_issues,
        Some("missing_attachment"),
        Some(false),
        Some("explain_attachment_unavailable"),
    );
    let asr_pending_count = value_u64_at(&voice_issues, &["by_issue_type", "asr_pending"]);
    let asr_failed_count = value_u64_at(&voice_issues, &["by_issue_type", "asr_failed"]);
    let retryable_asr_failed_count =
        count_voice_issues_matching(&voice_issues, Some("asr_failed"), Some(true), None);
    let known_unavailable_asr_count = count_voice_issues_matching(
        &voice_issues,
        Some("asr_failed"),
        Some(false),
        Some("explain_asr_unavailable"),
    );

    let mut next_actions = Vec::<Value>::new();

    if !edge_queue_checked {
        next_actions.push(maintenance_action(
            10,
            "inspect_edge_attachment_queue",
            vec!["maintenance", "data-health", "--with-edge-queue"],
            "edge attachment queue was not checked, so media readiness is only partially known",
            true,
        ));
    }
    if duplicate_job_key_count > 0 {
        next_actions.push(maintenance_action(
            20,
            "investigate_attachment_queue_duplicate_jobs",
            vec!["maintenance", "attachment-queue-health", "--limit", "1000"],
            "duplicate edge attachment job keys can make bulk retries unsafe",
            true,
        ));
    }
    if retryable_terminal_count > 0 {
        next_actions.push(maintenance_action(
            30,
            "retry_failed_attachment_jobs",
            vec![
                "sync",
                "attachment-retry",
                "--status",
                "failed",
                "--limit",
                "20",
            ],
            "failed edge attachment jobs may be retried deliberately before media analysis",
            true,
        ));
    }
    if completed_not_ingested_count > 0 {
        next_actions.push(maintenance_action(
            40,
            "sync_completed_attachments",
            vec!["sync", "attachments", "--limit", "50"],
            "edge has completed attachment downloads that are not yet present in memory",
            true,
        ));
    }
    if active_attachment_count > 0 {
        next_actions.push(maintenance_action(
            50,
            "wait_or_requeue_active_attachment_jobs",
            vec!["sync", "attachment-requeue"],
            "some attachment jobs are still pending, processing, or scheduled for retry",
            false,
        ));
    }
    if retryable_missing_attachment_count > 0 {
        next_actions.push(maintenance_action(
            60,
            "repair_missing_voice_attachments",
            vec!["sync", "attachment-repair", "--asset-type", "voice"],
            "some voice messages still do not have synced audio attachments",
            true,
        ));
    }
    if retryable_asr_failed_count > 0 {
        next_actions.push(maintenance_action(
            70,
            "inspect_failed_asr",
            vec!["maintenance", "voice-issues", "--limit", "50"],
            "some ASR attempts already failed; inspect provider or decoder errors before retrying",
            true,
        ));
    }
    if asr_pending_count > 0 {
        next_actions.push(maintenance_action(
            80,
            "backfill_pending_asr",
            vec!["maintenance", "asr-backfill", "--limit", "50"],
            "some voice attachments are present but not transcribed yet",
            true,
        ));
    }
    if next_actions.is_empty() {
        next_actions.push(maintenance_action(
            100,
            "ready_for_analysis",
            vec!["query", "messages"],
            "no blocking attachment or voice transcript maintenance issue was found in this bounded check",
            false,
        ));
    }

    let blocking_action_count = next_actions
        .iter()
        .filter(|action| {
            action
                .get("blocks_analysis")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let ready_for_analysis = blocking_action_count == 0 && edge_queue_checked;
    let overall_health = if duplicate_job_key_count > 0
        || retryable_terminal_count > 0
        || retryable_asr_failed_count > 0
    {
        "needs_attention"
    } else if completed_not_ingested_count > 0 || retryable_missing_attachment_count > 0 {
        "needs_attachment_sync"
    } else if asr_pending_count > 0 {
        "needs_asr"
    } else if !edge_queue_checked {
        "partial"
    } else if non_retryable_terminal_count > 0
        || known_unavailable_voice_attachment_count > 0
        || known_unavailable_asr_count > 0
    {
        "ready_with_known_gaps"
    } else {
        "healthy"
    };

    serde_json::json!({
        "ok": true,
        "query_mode": "maintenance_data_health",
        "overall_health": overall_health,
        "ready_for_analysis": ready_for_analysis,
        "confidence": if edge_queue_checked { "high" } else { "partial" },
        "limits": {
            "voice_limit": voice_limit,
            "edge_queue_limit": edge_queue_limit,
        },
        "summary": {
            "attachment_queue_checked": edge_queue_checked,
            "attachment_queue_health": attachment_health,
            "completed_not_ingested_count": completed_not_ingested_count,
            "retryable_terminal_count": retryable_terminal_count,
            "non_retryable_terminal_count": non_retryable_terminal_count,
            "duplicate_job_key_count": duplicate_job_key_count,
            "duplicate_message_key_count": duplicate_message_key_count,
            "multi_job_message_key_count": duplicate_message_key_count,
            "active_attachment_count": active_attachment_count,
            "voice_issue_count": voice_issue_count,
            "missing_voice_attachment_count": missing_attachment_count,
            "retryable_missing_voice_attachment_count": retryable_missing_attachment_count,
            "known_unavailable_voice_attachment_count": known_unavailable_voice_attachment_count,
            "asr_pending_count": asr_pending_count,
            "asr_failed_count": asr_failed_count,
            "retryable_asr_failed_count": retryable_asr_failed_count,
            "known_unavailable_asr_count": known_unavailable_asr_count,
            "blocking_action_count": blocking_action_count,
        },
        "next_actions": next_actions,
        "status": status,
        "attachment_queue": {
            "included": edge_queue_checked,
            "health": attachment_queue,
        },
        "voice": voice_issues,
        "agent_notes": [
            "run maintenance data-health before broad media or voice analysis when freshness matters",
            "ready_for_analysis is high-confidence only when --with-edge-queue was used",
            "next_actions are ordered so attachment sync and queue safety are handled before ASR",
            "multi_job_message_key_count can be normal for attachment variants such as image hd/normal/thumb and does not block analysis by itself",
            "non-retryable unavailable or purged media may still leave known gaps even when analysis can continue",
            "non-retryable ASR decoder failures are known transcript gaps and do not block broader analysis"
        ]
    })
}

fn maintenance_action(
    priority: u32,
    action: &str,
    recommended_cli: Vec<&str>,
    reason: &str,
    blocks_analysis: bool,
) -> Value {
    serde_json::json!({
        "priority": priority,
        "action": action,
        "recommended_cli": recommended_cli,
        "reason": reason,
        "blocks_analysis": blocks_analysis,
    })
}

fn value_u64_at(value: &Value, path: &[&str]) -> u64 {
    let mut current = value;
    for segment in path {
        let Some(next) = current.get(*segment) else {
            return 0;
        };
        current = next;
    }
    current
        .as_u64()
        .or_else(|| current.as_i64().and_then(|value| u64::try_from(value).ok()))
        .unwrap_or(0)
}

fn count_voice_issues_matching(
    voice_issues: &Value,
    issue_type: Option<&str>,
    retryable: Option<bool>,
    recommended_action: Option<&str>,
) -> u64 {
    let Some(issues) = voice_issues.get("issues").and_then(Value::as_array) else {
        return 0;
    };
    issues
        .iter()
        .filter(|issue| {
            issue_type
                .map(|expected| issue.get("issue_type").and_then(Value::as_str) == Some(expected))
                .unwrap_or(true)
                && retryable
                    .map(|expected| {
                        issue.get("retryable").and_then(Value::as_bool) == Some(expected)
                    })
                    .unwrap_or(true)
                && recommended_action
                    .map(|expected| {
                        issue.get("recommended_action").and_then(Value::as_str) == Some(expected)
                    })
                    .unwrap_or(true)
        })
        .count() as u64
}

#[derive(Debug, Clone)]
pub(crate) struct QueryResolution {
    input: Option<String>,
    pub(crate) id: Option<String>,
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

    pub(crate) fn to_json(&self) -> Value {
        serde_json::json!({
            "input": self.input.clone(),
            "id": self.id.clone(),
            "source": self.source,
            "selected": self.selected.clone(),
            "candidates": self.candidates.clone(),
        })
    }
}

pub(crate) async fn resolve_conversation_for_query(
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

pub(crate) fn looks_like_stable_wechat_id(value: &str) -> bool {
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
    let mut item_results = Vec::new();
    let mut last_job_id = after_job_id;
    for item in &manifest.attachments {
        last_job_id = item.job_id;
        match sync_one_attachment(client, &http, edge_url, admin_token, item, &attachment_dir).await
        {
            Ok(_) => {
                written += 1;
                item_results.push((item.job_id, true));
            }
            Err(error) => {
                item_results.push((item.job_id, false));
                failed.push(serde_json::json!({
                    "job_id": item.job_id,
                    "error": error.to_string()
                }));
            }
        }
    }
    let cursor_written =
        attachment_sync_cursor_after_batch(after_job_id, manifest.next_after_job_id, &item_results);
    write_cursor(&cursor_file, cursor_written)?;
    let cursor_blocked_by_failed_job_id = item_results
        .iter()
        .find_map(|(job_id, ok)| (!ok).then_some(*job_id));

    Ok(serde_json::json!({
        "ok": true,
        "scanned": manifest.attachments.len(),
        "written": written,
        "failed": failed,
        "failed_count": failed.len(),
        "after_job_id": after_job_id,
        "last_job_id": last_job_id,
        "next_after_job_id": manifest.next_after_job_id,
        "cursor_written": cursor_written,
        "cursor_blocked_by_failed_job_id": cursor_blocked_by_failed_job_id,
        "cursor_overlap": cursor_overlap,
        "configured_cursor_overlap": configured_cursor_overlap
    }))
}

fn attachment_sync_cursor_after_batch(
    after_job_id: i64,
    manifest_next_after_job_id: i64,
    item_results: &[(i64, bool)],
) -> i64 {
    let mut cursor = after_job_id;
    for (job_id, ok) in item_results {
        if !ok {
            return cursor;
        }
        cursor = *job_id;
    }
    manifest_next_after_job_id
}

fn attachment_cursor_overlap() -> i64 {
    std::env::var("GEWE_SKILL_ATTACHMENT_CURSOR_OVERLAP")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(500)
        .clamp(0, 5000)
}

async fn edge_download_jobs(
    edge_url: &str,
    admin_token: &str,
    status: Option<String>,
    asset_type: Option<String>,
    after_job_id: Option<i64>,
    limit: u32,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    edge_admin_get_json(
        &http,
        edge_url,
        admin_token,
        "/admin/download-jobs",
        vec![
            ("limit", Some(limit.to_string())),
            ("after_job_id", after_job_id.map(|value| value.to_string())),
            ("status", status),
            ("asset_type", asset_type),
        ],
    )
    .await
}

async fn backfill_edge_download_jobs(
    edge_url: &str,
    admin_token: &str,
    asset_type: String,
    schema_version: Option<String>,
    include_existing: bool,
    limit: u32,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    edge_admin_post_json(
        &http,
        edge_url,
        admin_token,
        "/admin/backfill-download-jobs",
        vec![
            ("limit", Some(limit.to_string())),
            ("asset_type", Some(asset_type)),
            ("schema_version", schema_version),
            (
                "include_existing",
                include_existing.then(|| "true".to_string()),
            ),
        ],
    )
    .await
}

async fn requeue_edge_download_jobs(
    edge_url: &str,
    admin_token: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    edge_admin_post_json(
        &http,
        edge_url,
        admin_token,
        "/admin/requeue-downloads",
        vec![],
    )
    .await
}

async fn retry_edge_download_jobs(
    edge_url: &str,
    admin_token: &str,
    status: String,
    asset_type: Option<String>,
    limit: u32,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    edge_admin_post_json(
        &http,
        edge_url,
        admin_token,
        "/admin/retry-downloads",
        vec![
            ("limit", Some(limit.to_string())),
            ("status", Some(status)),
            ("asset_type", asset_type),
        ],
    )
    .await
}

async fn repair_edge_attachment_queue(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    asset_type: String,
    schema_version: Option<String>,
    include_existing: bool,
    backfill_limit: u32,
    sync_limit: u32,
    settle_ms: u64,
    cursor_file: PathBuf,
    attachment_dir: PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    let before = edge_admin_get_json(&http, edge_url, admin_token, "/admin/stats", vec![]).await?;
    let queue_limit = backfill_limit.max(sync_limit).max(100);
    let before_queue = edge_download_jobs(
        edge_url,
        admin_token,
        None,
        Some(asset_type.clone()),
        None,
        queue_limit,
    )
    .await?;
    let before_completed_job_keys = attachment_maintenance::completed_job_keys(&before_queue);
    let before_memory_attachments = client
        .attachments_by_job_keys(&before_completed_job_keys)
        .await?;
    let before_memory_attachments = serde_json::to_value(before_memory_attachments)?;
    let before_queue_health =
        attachment_maintenance::queue_health_with_memory(&before_queue, &before_memory_attachments);
    let backfill = backfill_edge_download_jobs(
        edge_url,
        admin_token,
        asset_type.clone(),
        schema_version.clone(),
        include_existing,
        backfill_limit,
    )
    .await?;
    let requeue = requeue_edge_download_jobs(edge_url, admin_token).await?;

    if settle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
    }

    let sync = sync_edge_attachments(
        client,
        edge_url,
        admin_token,
        None,
        sync_limit,
        cursor_file,
        attachment_dir,
    )
    .await?;
    let after = edge_admin_get_json(&http, edge_url, admin_token, "/admin/stats", vec![]).await?;
    let after_queue = edge_download_jobs(
        edge_url,
        admin_token,
        None,
        Some(asset_type.clone()),
        None,
        queue_limit,
    )
    .await?;
    let after_completed_job_keys = attachment_maintenance::completed_job_keys(&after_queue);
    let after_memory_attachments = client
        .attachments_by_job_keys(&after_completed_job_keys)
        .await?;
    let after_memory_attachments = serde_json::to_value(after_memory_attachments)?;
    let after_queue_health =
        attachment_maintenance::queue_health_with_memory(&after_queue, &after_memory_attachments);

    Ok(serde_json::json!({
        "ok": true,
        "query_mode": "sync_attachment_queue_repair",
        "asset_type": asset_type,
        "schema_version": schema_version,
        "include_existing": include_existing,
        "settle_ms": settle_ms,
        "before": before,
        "before_queue_health": before_queue_health,
        "backfill": backfill,
        "requeue": requeue,
        "sync": sync,
        "after": after,
        "after_queue_health": after_queue_health,
        "agent_hints": [
            "attachment-repair backfills missing edge jobs, requeues ready jobs, then pulls completed files into memory",
            "after_queue_health is the authoritative repair outcome summary for Agent follow-up decisions",
            "if backfill queued jobs but sync wrote zero files, run attachment-repair again after the edge queue finishes processing",
            "use sync attachment-queue to inspect failed, unavailable, pending, retry_scheduled, and completed jobs before retrying terminal failures"
        ]
    }))
}

async fn edge_admin_get_json(
    http: &reqwest::Client,
    edge_url: &str,
    admin_token: &str,
    path: &str,
    params: Vec<(&str, Option<String>)>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let url = edge_admin_url(edge_url, path, params)?;
    Ok(http
        .get(url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn edge_admin_post_json(
    http: &reqwest::Client,
    edge_url: &str,
    admin_token: &str,
    path: &str,
    params: Vec<(&str, Option<String>)>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let url = edge_admin_url(edge_url, path, params)?;
    Ok(http
        .post(url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn edge_admin_url(
    edge_url: &str,
    path: &str,
    params: Vec<(&str, Option<String>)>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut url = Url::parse(&format!(
        "{}/{}",
        edge_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    ))?;
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in params {
            if let Some(value) = value.map(|item| item.trim().to_string()) {
                if !value.is_empty() {
                    query.append_pair(key, &value);
                }
            }
        }
    }
    Ok(url.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_cursor_stops_before_first_failed_item() {
        let cursor = attachment_sync_cursor_after_batch(
            100,
            250,
            &[(120, true), (140, true), (160, false), (180, true)],
        );
        assert_eq!(cursor, 140);
    }

    #[test]
    fn attachment_cursor_uses_manifest_cursor_when_batch_succeeds() {
        let cursor =
            attachment_sync_cursor_after_batch(100, 250, &[(120, true), (140, true), (160, true)]);
        assert_eq!(cursor, 250);
    }

    #[test]
    fn attachment_cursor_does_not_advance_when_first_item_fails() {
        let cursor = attachment_sync_cursor_after_batch(100, 250, &[(120, false), (140, true)]);
        assert_eq!(cursor, 100);
    }

    #[test]
    fn edge_admin_url_builds_filtered_queue_url() {
        let url = edge_admin_url(
            "https://example.com/",
            "/admin/download-jobs",
            vec![
                ("limit", Some("20".to_string())),
                ("status", Some("failed".to_string())),
                ("asset_type", Some("voice".to_string())),
                ("schema_version", None),
            ],
        )
        .unwrap();

        assert_eq!(
            url,
            "https://example.com/admin/download-jobs?limit=20&status=failed&asset_type=voice"
        );
    }

    #[test]
    fn agent_attachment_block_reports_exact_window_summary() {
        let messages = vec![
            test_message("m1", gewe_skill_types::NormalizedKind::Image),
            test_message("m2", gewe_skill_types::NormalizedKind::Voice),
            test_message("m3", gewe_skill_types::NormalizedKind::Text),
        ];
        let attachments = vec![AttachmentRecord {
            id: Some(1),
            edge_job_id: Some(10),
            job_key: Some("job:m1:image".to_string()),
            message_key: "m1".to_string(),
            raw_event_dedupe_key: "raw:m1".to_string(),
            appid: "app".to_string(),
            account_wxid: None,
            kind: AttachmentKind::Image,
            variant: None,
            source_url: None,
            object_key: Some("attachments/image.bin".to_string()),
            sha256: Some("sha".to_string()),
            size_bytes: Some(3),
            mime_type: Some("image/jpeg".to_string()),
            created_at: "2026-05-26T00:00:00Z".to_string(),
        }];

        let block = agent_attachment_block(true, &messages, Some(&attachments));

        assert_eq!(block["included"], true);
        assert_eq!(block["summary"]["attachment_count"], 1);
        assert_eq!(block["summary"]["messages_with_attachments"], 1);
        assert_eq!(block["summary"]["attachment_expected_message_count"], 2);
        assert_eq!(block["summary"]["attachment_expected_missing_count"], 1);
        assert_eq!(
            block["summary"]["attachment_expected_missing_message_keys"][0],
            "m2"
        );
        assert_eq!(block["by_message_key"]["m1"]["kinds"][0], "image");
    }

    #[test]
    fn collect_message_speaker_wxids_deduplicates_and_ignores_empty_values() {
        let mut first = test_message("m1", gewe_skill_types::NormalizedKind::Text);
        first.sender_wxid = Some(" wxid_b ".to_string());
        let mut second = test_message("m2", gewe_skill_types::NormalizedKind::Text);
        second.sender_wxid = Some("wxid_a".to_string());
        let mut duplicate = test_message("m3", gewe_skill_types::NormalizedKind::Text);
        duplicate.sender_wxid = Some("wxid_b".to_string());
        let mut missing = test_message("m4", gewe_skill_types::NormalizedKind::Text);
        missing.sender_wxid = Some(" ".to_string());

        let wxids = collect_message_speaker_wxids(&[first, second, duplicate, missing]);

        assert_eq!(wxids, vec!["wxid_a".to_string(), "wxid_b".to_string()]);
    }

    #[test]
    fn speaker_display_source_prefers_contact_remark_before_group_card() {
        let profile = IdentityProfileResponse {
            entity_id: "wxid_left".to_string(),
            chatroom_id: Some("room@chatroom".to_string()),
            effective_display_name: Some("左备注".to_string()),
            contact: Some(gewe_skill_types::IdentityContactProfile {
                wxid: "wxid_left".to_string(),
                nickname: Some("左".to_string()),
                remark: Some("左备注".to_string()),
                alias: None,
                raw: Some(serde_json::json!({"phoneNumList": ["secret"]})),
                last_seen_at: None,
                updated_at: None,
            }),
            chatroom_member: Some(gewe_skill_types::IdentityChatroomMemberProfile {
                chatroom_id: "room@chatroom".to_string(),
                member_wxid: "wxid_left".to_string(),
                display_name: Some("左（今天你喝水了吗）".to_string()),
                nickname: Some("左".to_string()),
                is_current: true,
                raw: Some(serde_json::json!({"bigHeadImgUrl": "secret"})),
                last_seen_at: None,
            }),
            aliases: Vec::new(),
        };

        assert_eq!(speaker_display_source(&profile), "contact_remark");

        let value = agent_speaker_profile_value(&profile);
        assert_eq!(value["display_name_source"], "contact_remark");
        assert!(value["contact"].get("raw").is_none());
        assert!(value["chatroom_member"].get("raw").is_none());
    }

    #[test]
    fn maintenance_data_health_prioritizes_attachment_sync_before_asr() {
        let report = maintenance_data_health_report(
            serde_json::json!({"ok": true}),
            Some(serde_json::json!({
                "queue_health": "needs_sync",
                "completed_not_ingested_count": 2,
                "retryable_terminal_count": 0,
                "non_retryable_terminal_count": 0,
                "duplicate_job_key_count": 0,
                "duplicate_message_key_count": 0,
                "active_count": 0,
            })),
            serde_json::json!({
                "issue_count": 3,
                "by_issue_type": {
                    "asr_pending": 3
                }
            }),
            true,
            200,
            1000,
        );

        assert_eq!(report["overall_health"], "needs_attachment_sync");
        assert_eq!(report["ready_for_analysis"], false);
        assert_eq!(
            report["next_actions"][0]["action"],
            "sync_completed_attachments"
        );
        assert_eq!(report["next_actions"][1]["action"], "backfill_pending_asr");
    }

    #[test]
    fn maintenance_data_health_marks_partial_without_edge_queue() {
        let report = maintenance_data_health_report(
            serde_json::json!({"ok": true}),
            None,
            serde_json::json!({
                "issue_count": 0,
                "by_issue_type": {}
            }),
            false,
            200,
            1000,
        );

        assert_eq!(report["overall_health"], "partial");
        assert_eq!(report["ready_for_analysis"], false);
        assert_eq!(
            report["next_actions"][0]["action"],
            "inspect_edge_attachment_queue"
        );
    }

    #[test]
    fn maintenance_data_health_allows_message_key_repeats_for_attachment_variants() {
        let report = maintenance_data_health_report(
            serde_json::json!({"ok": true}),
            Some(serde_json::json!({
                "queue_health": "has_unavailable",
                "completed_not_ingested_count": 0,
                "retryable_terminal_count": 0,
                "non_retryable_terminal_count": 2,
                "duplicate_job_key_count": 0,
                "duplicate_message_key_count": 2,
                "active_count": 0,
            })),
            serde_json::json!({
                "issue_count": 0,
                "by_issue_type": {}
            }),
            true,
            200,
            1000,
        );

        assert_eq!(report["overall_health"], "ready_with_known_gaps");
        assert_eq!(report["ready_for_analysis"], true);
        assert_eq!(report["summary"]["multi_job_message_key_count"], 2);
        assert_eq!(report["next_actions"][0]["action"], "ready_for_analysis");
    }

    #[test]
    fn maintenance_data_health_treats_unavailable_voice_as_known_gap() {
        let report = maintenance_data_health_report(
            serde_json::json!({"ok": true}),
            Some(serde_json::json!({
                "queue_health": "has_unavailable",
                "completed_not_ingested_count": 0,
                "retryable_terminal_count": 0,
                "non_retryable_terminal_count": 2,
                "duplicate_job_key_count": 0,
                "duplicate_message_key_count": 0,
                "active_count": 0,
            })),
            serde_json::json!({
                "issue_count": 2,
                "by_issue_type": {
                    "missing_attachment": 2
                },
                "issues": [
                    {
                        "issue_type": "missing_attachment",
                        "retryable": false,
                        "recommended_action": "explain_attachment_unavailable"
                    },
                    {
                        "issue_type": "missing_attachment",
                        "retryable": false,
                        "recommended_action": "explain_attachment_unavailable"
                    }
                ]
            }),
            true,
            200,
            1000,
        );

        assert_eq!(report["overall_health"], "ready_with_known_gaps");
        assert_eq!(report["ready_for_analysis"], true);
        assert_eq!(
            report["summary"]["known_unavailable_voice_attachment_count"],
            2
        );
        assert_eq!(
            report["summary"]["retryable_missing_voice_attachment_count"],
            0
        );
        assert_eq!(report["next_actions"][0]["action"], "ready_for_analysis");
    }

    #[test]
    fn maintenance_data_health_treats_decoder_asr_failure_as_known_gap() {
        let report = maintenance_data_health_report(
            serde_json::json!({"ok": true}),
            Some(serde_json::json!({
                "queue_health": "healthy",
                "completed_not_ingested_count": 0,
                "retryable_terminal_count": 0,
                "non_retryable_terminal_count": 0,
                "duplicate_job_key_count": 0,
                "duplicate_message_key_count": 0,
                "active_count": 0,
            })),
            serde_json::json!({
                "issue_count": 1,
                "by_issue_type": {
                    "asr_failed": 1
                },
                "issues": [
                    {
                        "issue_type": "asr_failed",
                        "retryable": false,
                        "recommended_action": "explain_asr_unavailable"
                    }
                ]
            }),
            true,
            200,
            1000,
        );

        assert_eq!(report["overall_health"], "ready_with_known_gaps");
        assert_eq!(report["ready_for_analysis"], true);
        assert_eq!(report["summary"]["asr_failed_count"], 1);
        assert_eq!(report["summary"]["retryable_asr_failed_count"], 0);
        assert_eq!(report["summary"]["known_unavailable_asr_count"], 1);
        assert_eq!(report["next_actions"][0]["action"], "ready_for_analysis");
    }

    fn test_message(
        message_key: &str,
        kind: gewe_skill_types::NormalizedKind,
    ) -> NormalizedMessage {
        NormalizedMessage {
            message_key: message_key.to_string(),
            schema_version: gewe_skill_types::SchemaVersion::V1,
            appid: "app".to_string(),
            account_wxid: None,
            kind,
            type_name: None,
            msg_id: None,
            new_msg_id: None,
            msg_type: None,
            appmsg_type: None,
            from_user: None,
            to_user: None,
            conversation_id: Some("room".to_string()),
            sender_wxid: Some("sender".to_string()),
            is_group: true,
            is_outgoing: false,
            wechat_created_at: None,
            received_at: "2026-05-26T00:00:00Z".to_string(),
            content_text: None,
            content_xml: None,
            push_content: None,
            msg_source: None,
            raw_content: None,
        }
    }
}
