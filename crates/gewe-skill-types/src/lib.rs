//! Shared DTOs and normalized schemas for `gewe-skill`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub type Timestamp = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaVersion {
    V1,
    V2,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NormalizedKind {
    Text,
    Image,
    Voice,
    Video,
    Emoji,
    File,
    FileNotice,
    Link,
    Location,
    ContactCard,
    ChatRecord,
    MiniProgram,
    Quote,
    AppMsg,
    System,
    Status,
    ChatroomContactsUpdate,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Voice,
    Video,
    Emoji,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatroomEventType {
    MemberJoined,
    MemberLeft,
    MemberRemoved,
    MemberInvited,
    MemberCountChanged,
    ChatroomNameChanged,
    SystemUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEventEnvelope {
    pub id: Option<i64>,
    pub event_id: Uuid,
    pub schema_version: SchemaVersion,
    pub appid: String,
    pub account_wxid: Option<String>,
    pub dedupe_key: String,
    pub received_at: Timestamp,
    pub body_sha256: String,
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedMessage {
    pub message_key: String,
    pub schema_version: SchemaVersion,
    pub appid: String,
    pub account_wxid: Option<String>,
    pub kind: NormalizedKind,
    pub type_name: Option<String>,
    pub msg_id: Option<String>,
    pub new_msg_id: Option<String>,
    pub msg_type: Option<i64>,
    pub appmsg_type: Option<i64>,
    pub from_user: Option<String>,
    pub to_user: Option<String>,
    pub conversation_id: Option<String>,
    pub sender_wxid: Option<String>,
    pub is_group: bool,
    pub is_outgoing: bool,
    pub wechat_created_at: Option<i64>,
    pub received_at: Timestamp,
    pub content_text: Option<String>,
    pub content_xml: Option<String>,
    pub push_content: Option<String>,
    pub msg_source: Option<String>,
    pub raw_content: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageQuery {
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub sender_wxid: Option<String>,
    pub kind: Option<String>,
    pub direction: Option<String>,
    pub after: Option<Timestamp>,
    pub before: Option<Timestamp>,
    pub cursor: Option<Timestamp>,
    pub limit: Option<i64>,
    pub order: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContextResponse {
    pub conversation_id: Option<String>,
    pub message_key: String,
    pub before: Vec<NormalizedMessage>,
    pub anchor: Option<NormalizedMessage>,
    pub after: Vec<NormalizedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: Option<i64>,
    pub edge_job_id: Option<i64>,
    pub job_key: Option<String>,
    pub message_key: String,
    pub raw_event_dedupe_key: String,
    pub appid: String,
    pub account_wxid: Option<String>,
    pub kind: AttachmentKind,
    pub variant: Option<String>,
    pub source_url: Option<String>,
    pub object_key: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub mime_type: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceQuery {
    pub conversation_id: Option<String>,
    pub sender_wxid: Option<String>,
    pub after: Option<Timestamp>,
    pub before: Option<Timestamp>,
    pub cursor: Option<Timestamp>,
    pub limit: Option<i64>,
    pub order: Option<String>,
    pub missing_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscriptRecord {
    pub message_key: String,
    pub attachment_sha256: Option<String>,
    pub provider: String,
    pub language: Option<String>,
    pub text: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    pub response_json: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceItem {
    pub message: NormalizedMessage,
    pub attachment: Option<AttachmentRecord>,
    pub transcript: Option<VoiceTranscriptRecord>,
    pub availability: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribeRequest {
    pub message_key: String,
    pub provider: Option<String>,
    pub language: Option<String>,
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribeResponse {
    pub ok: bool,
    pub message_key: String,
    pub status: String,
    pub transcript: Option<VoiceTranscriptRecord>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceWarmRequest {
    pub conversation_id: Option<String>,
    pub sender_wxid: Option<String>,
    pub after: Option<Timestamp>,
    pub before: Option<Timestamp>,
    pub limit: Option<i64>,
    pub provider: Option<String>,
    pub language: Option<String>,
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceWarmResponse {
    pub ok: bool,
    pub scanned: usize,
    pub transcribed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub items: Vec<VoiceTranscribeResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub display_name: Option<String>,
    pub is_group: bool,
    pub last_message_at: Option<Timestamp>,
    pub last_message_preview: Option<String>,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityMatch {
    pub entity_type: String,
    pub entity_id: String,
    pub chatroom_id: Option<String>,
    pub display_name: Option<String>,
    pub alias: Option<String>,
    pub source: Option<String>,
    pub is_current: bool,
    pub score: f64,
    pub last_seen_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityResolveResponse {
    pub query: String,
    pub items: Vec<IdentityMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityContactProfile {
    pub wxid: String,
    pub nickname: Option<String>,
    pub remark: Option<String>,
    pub alias: Option<String>,
    pub raw: Option<Value>,
    pub last_seen_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityChatroomMemberProfile {
    pub chatroom_id: String,
    pub member_wxid: String,
    pub display_name: Option<String>,
    pub nickname: Option<String>,
    pub is_current: bool,
    pub raw: Option<Value>,
    pub last_seen_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityDisplayNameCandidate {
    pub source: String,
    pub value: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityDisplayNameResolution {
    pub selected_source: String,
    pub selected_value: Option<String>,
    pub candidates: Vec<IdentityDisplayNameCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityMemoryRecordStatus {
    pub present: bool,
    pub stale: bool,
    pub refresh_recommended: bool,
    pub stale_after_days: i64,
    pub age_days: Option<i64>,
    pub last_seen_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityMemoryStatus {
    pub refresh_recommended: bool,
    pub reasons: Vec<String>,
    pub stale_after_days: i64,
    pub contact: IdentityMemoryRecordStatus,
    pub chatroom_member: Option<IdentityMemoryRecordStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityProfileResponse {
    pub entity_id: String,
    pub chatroom_id: Option<String>,
    pub effective_display_name: Option<String>,
    pub display_name_source: String,
    pub display_name_resolution: IdentityDisplayNameResolution,
    pub memory_status: IdentityMemoryStatus,
    pub contact: Option<IdentityContactProfile>,
    pub chatroom_member: Option<IdentityChatroomMemberProfile>,
    pub aliases: Vec<IdentityMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityRefreshRequest {
    pub full: Option<bool>,
    pub chatroom_id: Option<String>,
    pub wxids: Option<Vec<String>>,
    pub recent_chatrooms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityRefreshResponse {
    pub ok: bool,
    pub refreshed_chatrooms: i64,
    pub refreshed_contacts: i64,
    pub refreshed_members: i64,
    pub errors: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityEventBackfillRequest {
    pub event_limit: Option<i64>,
    pub max_chatrooms: Option<i64>,
    pub max_wxids: Option<i64>,
    pub contact_detail: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityEventBackfillResponse {
    pub ok: bool,
    pub dry_run: bool,
    pub event_limit: i64,
    pub scanned_candidates: usize,
    pub initially_missing: usize,
    pub refreshed_chatrooms: i64,
    pub refreshed_members: i64,
    pub refreshed_contacts: i64,
    pub unresolved_after: usize,
    pub sample_missing: Vec<Value>,
    pub errors: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomMember {
    pub wxid: String,
    pub display_name: Option<String>,
    pub flag: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomSnapshot {
    pub chatroom_id: String,
    pub chatroom_name: Option<String>,
    pub chatroom_version: Option<i64>,
    pub member_count: i64,
    pub members: Vec<ChatroomMember>,
    pub member_hash: String,
    pub received_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomMemberEvent {
    pub event_type: ChatroomEventType,
    pub chatroom_id: String,
    pub member_wxid: Option<String>,
    pub previous_chatroom_name: Option<String>,
    pub current_chatroom_name: Option<String>,
    pub previous_member_count: Option<i64>,
    pub current_member_count: Option<i64>,
    pub received_at: Timestamp,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomSystemEvent {
    pub event_type: ChatroomEventType,
    pub chatroom_id: String,
    pub actor_wxid: Option<String>,
    pub actor_name: Option<String>,
    pub target_wxid: Option<String>,
    pub target_name: Option<String>,
    pub target_wxids: Vec<String>,
    pub target_names: Vec<String>,
    pub previous_value: Option<String>,
    pub current_value: Option<String>,
    pub template_text: Option<String>,
    pub content_text: Option<String>,
    pub received_at: Timestamp,
    pub details: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatroomEventWriteRequest {
    pub member_events: Vec<ChatroomMemberEvent>,
    pub system_events: Vec<ChatroomSystemEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatroomEventWriteResponse {
    pub ok: bool,
    pub member_events: usize,
    pub system_events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEventRequest {
    pub raw_event: RawEventEnvelope,
    pub message: NormalizedMessage,
    pub attachments: Vec<AttachmentRecord>,
    pub chatroom_snapshot: Option<ChatroomSnapshot>,
    pub chatroom_member_events: Vec<ChatroomMemberEvent>,
    pub chatroom_system_events: Vec<ChatroomSystemEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCallbackRequest {
    pub received_at: Timestamp,
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
