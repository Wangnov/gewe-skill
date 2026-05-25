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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: Option<i64>,
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
