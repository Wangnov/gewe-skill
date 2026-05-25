//! Core normalization and event parsing for GeWe callbacks.

use gewe_skill_types::{
    ChatroomEventType, ChatroomMember, ChatroomMemberEvent, ChatroomSnapshot, ChatroomSystemEvent,
    IngestEventRequest, NormalizedKind, NormalizedMessage, RawEventEnvelope, SchemaVersion,
};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("callback payload does not contain a supported GeWe message identity")]
    UnsupportedPayload,
    #[error("missing GeWe app id")]
    MissingAppId,
}

#[derive(Debug, Clone)]
pub struct NormalizedCallback {
    pub raw_event: RawEventEnvelope,
    pub message: NormalizedMessage,
    pub chatroom_snapshot: Option<ChatroomSnapshot>,
    pub chatroom_system_event: Option<ChatroomSystemEvent>,
}

impl NormalizedCallback {
    #[must_use]
    pub fn into_ingest_request(self) -> IngestEventRequest {
        IngestEventRequest {
            raw_event: self.raw_event,
            message: self.message,
            attachments: Vec::new(),
            chatroom_snapshot: self.chatroom_snapshot,
            chatroom_member_events: Vec::new(),
            chatroom_system_events: self.chatroom_system_event.into_iter().collect(),
        }
    }
}

pub fn normalize_callback(body: &Value, received_at: impl Into<String>) -> Result<NormalizedCallback, CoreError> {
    let received_at = received_at.into();
    if body.get("Appid").is_some() || body.get("TypeName").is_some() {
        normalize_v1(body, received_at)
    } else if body.get("appid").is_some() || body.get("newMsgId").is_some() {
        normalize_v2(body, received_at)
    } else {
        Err(CoreError::UnsupportedPayload)
    }
}

pub fn diff_chatroom_snapshots(previous: &ChatroomSnapshot, current: &ChatroomSnapshot) -> Vec<ChatroomMemberEvent> {
    let previous_members: BTreeMap<_, _> = previous.members.iter().map(|member| (&member.wxid, member)).collect();
    let current_members: BTreeMap<_, _> = current.members.iter().map(|member| (&member.wxid, member)).collect();
    let mut events = Vec::new();

    for member in &current.members {
        if !previous_members.contains_key(&member.wxid) {
            events.push(ChatroomMemberEvent {
                event_type: ChatroomEventType::MemberJoined,
                chatroom_id: current.chatroom_id.clone(),
                member_wxid: Some(member.wxid.clone()),
                previous_chatroom_name: previous.chatroom_name.clone(),
                current_chatroom_name: current.chatroom_name.clone(),
                previous_member_count: Some(previous.member_count),
                current_member_count: Some(current.member_count),
                received_at: current.received_at.clone(),
                details: json!({ "member": member }),
            });
        }
    }

    for member in &previous.members {
        if !current_members.contains_key(&member.wxid) {
            events.push(ChatroomMemberEvent {
                event_type: ChatroomEventType::MemberLeft,
                chatroom_id: current.chatroom_id.clone(),
                member_wxid: Some(member.wxid.clone()),
                previous_chatroom_name: previous.chatroom_name.clone(),
                current_chatroom_name: current.chatroom_name.clone(),
                previous_member_count: Some(previous.member_count),
                current_member_count: Some(current.member_count),
                received_at: current.received_at.clone(),
                details: json!({ "member": member }),
            });
        }
    }

    if previous.chatroom_name != current.chatroom_name {
        events.push(ChatroomMemberEvent {
            event_type: ChatroomEventType::ChatroomNameChanged,
            chatroom_id: current.chatroom_id.clone(),
            member_wxid: None,
            previous_chatroom_name: previous.chatroom_name.clone(),
            current_chatroom_name: current.chatroom_name.clone(),
            previous_member_count: Some(previous.member_count),
            current_member_count: Some(current.member_count),
            received_at: current.received_at.clone(),
            details: json!({
                "previous_name": previous.chatroom_name,
                "current_name": current.chatroom_name,
            }),
        });
    }

    if previous.member_count != current.member_count {
        events.push(ChatroomMemberEvent {
            event_type: ChatroomEventType::MemberCountChanged,
            chatroom_id: current.chatroom_id.clone(),
            member_wxid: None,
            previous_chatroom_name: previous.chatroom_name.clone(),
            current_chatroom_name: current.chatroom_name.clone(),
            previous_member_count: Some(previous.member_count),
            current_member_count: Some(current.member_count),
            received_at: current.received_at.clone(),
            details: json!({
                "previous_count": previous.member_count,
                "current_count": current.member_count,
            }),
        });
    }

    events
}

fn normalize_v1(body: &Value, received_at: String) -> Result<NormalizedCallback, CoreError> {
    let appid = scalar(body.get("Appid")).ok_or(CoreError::MissingAppId)?;
    let account_wxid = scalar(body.get("Wxid"));
    let type_name = scalar(body.get("TypeName")).unwrap_or_else(|| "v1".to_string());
    let data = body.get("Data").unwrap_or(&Value::Null);
    let msg_id = scalar_path(data, &["MsgId"]);
    let new_msg_id = scalar_path(data, &["NewMsgId"]);
    let msg_type = int_path(data, &["MsgType"]);
    let from_user = scalar_unwrapped(data.get("FromUserName"));
    let to_user = scalar_unwrapped(data.get("ToUserName"));
    let raw_content = scalar_unwrapped(data.get("Content"));
    let push_content = scalar_unwrapped(data.get("PushContent"));
    let msg_source = scalar_unwrapped(data.get("MsgSource"));
    let create_time = int_path(data, &["CreateTime"]);
    let appmsg_type = raw_content.as_deref().and_then(parse_appmsg_type);
    let stripped_content = raw_content.as_deref().map(strip_group_speaker_prefix);
    let is_outgoing = account_wxid.as_deref().is_some_and(|wxid| to_user.as_deref() != Some(wxid) && from_user.as_deref() == Some(wxid));
    let conversation_id = choose_conversation_id(from_user.as_deref(), to_user.as_deref(), account_wxid.as_deref(), is_outgoing);
    let sender_wxid = if is_group_conversation(from_user.as_deref()) {
        raw_content.as_deref().and_then(extract_group_sender)
    } else if is_outgoing {
        account_wxid.clone()
    } else {
        from_user.clone()
    };
    let kind = classify_v1(&type_name, msg_type, appmsg_type, stripped_content.as_deref());
    let identity = new_msg_id.clone().or(msg_id.clone()).unwrap_or_else(|| sha256_short(body));
    let dedupe_key = stable_dedupe_key("v1", &appid, &identity);
    let content_xml = if matches!(kind, NormalizedKind::System) || stripped_content.as_deref().is_some_and(is_likely_xml) {
        stripped_content.clone()
    } else {
        None
    };
    let chatroom_snapshot = parse_v1_chatroom_snapshot(data, &received_at);

    let message = NormalizedMessage {
        message_key: dedupe_key.clone(),
        schema_version: SchemaVersion::V1,
        appid: appid.clone(),
        account_wxid: account_wxid.clone(),
        kind: kind.clone(),
        type_name: Some(type_name.clone()),
        msg_id,
        new_msg_id,
        msg_type,
        appmsg_type,
        from_user: from_user.clone(),
        to_user,
        conversation_id: conversation_id.clone(),
        sender_wxid,
        is_group: is_group_conversation(conversation_id.as_deref()) || is_group_conversation(from_user.as_deref()),
        is_outgoing,
        wechat_created_at: create_time,
        received_at: received_at.clone(),
        content_text: stripped_content.clone().filter(|text| !is_likely_xml(text)),
        content_xml,
        push_content,
        msg_source,
        raw_content,
    };
    let chatroom_system_event = parse_chatroom_system_event(&message);
    let raw_event = raw_event(body, SchemaVersion::V1, appid, account_wxid, dedupe_key, received_at);

    Ok(NormalizedCallback { raw_event, message, chatroom_snapshot, chatroom_system_event })
}

fn normalize_v2(body: &Value, received_at: String) -> Result<NormalizedCallback, CoreError> {
    let appid = scalar(body.get("appid")).ok_or(CoreError::MissingAppId)?;
    let account_wxid = scalar(body.get("wxid"));
    let msg_id = scalar(body.get("msgId"));
    let new_msg_id = scalar(body.get("newMsgId"));
    let msg_type = int_scalar(body.get("msgType"));
    let from_user = scalar(body.get("fromUser"));
    let to_user = scalar(body.get("toUser"));
    let from_group = scalar(body.get("fromGroup"));
    let raw_content = scalar(body.get("content"));
    let is_outgoing = body.get("isSelf").and_then(Value::as_bool).unwrap_or(false);
    let appmsg_type = raw_content.as_deref().and_then(parse_appmsg_type);
    let conversation_id = from_group.clone().or_else(|| choose_conversation_id(from_user.as_deref(), to_user.as_deref(), account_wxid.as_deref(), is_outgoing));
    let kind = classify_message(msg_type, appmsg_type, raw_content.as_deref());
    let identity = new_msg_id.clone().or(msg_id.clone()).unwrap_or_else(|| sha256_short(body));
    let dedupe_key = stable_dedupe_key("v2", &appid, &identity);

    let message = NormalizedMessage {
        message_key: dedupe_key.clone(),
        schema_version: SchemaVersion::V2,
        appid: appid.clone(),
        account_wxid: account_wxid.clone(),
        kind: kind.clone(),
        type_name: msg_type.map(|value| value.to_string()),
        msg_id,
        new_msg_id,
        msg_type,
        appmsg_type,
        from_user: from_user.clone(),
        to_user,
        conversation_id: conversation_id.clone(),
        sender_wxid: if is_group_conversation(from_group.as_deref()) { from_user } else { None },
        is_group: is_group_conversation(conversation_id.as_deref()) || body.get("eventCode").and_then(Value::as_str) == Some("group_msg_event"),
        is_outgoing,
        wechat_created_at: int_scalar(body.get("createTime")),
        received_at: received_at.clone(),
        content_text: raw_content.clone().filter(|text| !is_likely_xml(text)),
        content_xml: raw_content.clone().filter(|text| is_likely_xml(text)),
        push_content: scalar(body.get("pushContent")),
        msg_source: scalar(body.get("msgSource")),
        raw_content,
    };
    let chatroom_system_event = parse_chatroom_system_event(&message);
    let raw_event = raw_event(body, SchemaVersion::V2, appid, account_wxid, dedupe_key, received_at);

    Ok(NormalizedCallback { raw_event, message, chatroom_snapshot: None, chatroom_system_event })
}

fn raw_event(body: &Value, schema_version: SchemaVersion, appid: String, account_wxid: Option<String>, dedupe_key: String, received_at: String) -> RawEventEnvelope {
    RawEventEnvelope {
        id: None,
        event_id: Uuid::now_v7(),
        schema_version,
        appid,
        account_wxid,
        dedupe_key,
        received_at,
        body_sha256: sha256_hex(&body.to_string()),
        body: body.clone(),
    }
}

fn parse_v1_chatroom_snapshot(data: &Value, received_at: &str) -> Option<ChatroomSnapshot> {
    let chatroom_id = scalar_unwrapped(data.get("UserName"))?;
    if !is_group_conversation(Some(&chatroom_id)) {
        return None;
    }
    let member_values = data.pointer("/NewChatroomData/ChatRoomMember")?.as_array()?;
    let mut members: Vec<_> = member_values
        .iter()
        .filter_map(|member| {
            let wxid = scalar_unwrapped(member.get("UserName"))?;
            Some(ChatroomMember {
                wxid,
                display_name: scalar_unwrapped(member.get("DisplayName")),
                flag: int_path(member, &["MemberFlag"]),
            })
        })
        .collect();
    members.sort_by(|left, right| left.wxid.cmp(&right.wxid));
    let member_json = serde_json::to_string(&members).unwrap_or_default();

    Some(ChatroomSnapshot {
        chatroom_id,
        chatroom_name: scalar_unwrapped(data.get("NickName")),
        chatroom_version: int_path(data, &["ChatroomVersion"]),
        member_count: int_path(data, &["NewChatroomData", "MemberCount"]).unwrap_or(members.len() as i64),
        member_hash: sha256_hex(&member_json),
        members,
        received_at: received_at.to_string(),
    })
}

fn parse_chatroom_system_event(message: &NormalizedMessage) -> Option<ChatroomSystemEvent> {
    if !matches!(message.kind, NormalizedKind::System) {
        return None;
    }
    let chatroom_id = message.conversation_id.clone()?;
    if !is_group_conversation(Some(&chatroom_id)) {
        return None;
    }
    let xml = message.content_xml.as_deref().or(message.raw_content.as_deref())?;
    if !xml.contains("<sysmsg") {
        return None;
    }

    let template = extract_tag(xml, "template").map(decode_xml_text).unwrap_or_default();
    let links = extract_links(xml);
    let names = links.get("names").cloned().unwrap_or_default();
    let kickout_names = links.get("kickoutname").cloned().unwrap_or_default();
    let username = links.get("username").and_then(|members| members.first()).cloned();
    let remark = links.get("remark").and_then(|members| members.first()).cloned();
    let (event_type, target_members) = if (template.contains("加入了群聊") || template.contains("邀请")) && !names.is_empty() {
        (ChatroomEventType::MemberInvited, names)
    } else if template.contains("移出") {
        (ChatroomEventType::MemberRemoved, if kickout_names.is_empty() { names } else { kickout_names })
    } else if template.contains("退出群聊") {
        (ChatroomEventType::MemberLeft, Vec::new())
    } else if template.contains("修改群名") || template.contains("群名") {
        (ChatroomEventType::ChatroomNameChanged, Vec::new())
    } else {
        (ChatroomEventType::SystemUnknown, Vec::new())
    };
    let target_wxids: Vec<_> = target_members.iter().map(|member| member.wxid.clone()).filter(|value| !value.is_empty()).collect();
    let target_names: Vec<_> = target_members.iter().map(|member| member.display_name.clone().unwrap_or_else(|| member.wxid.clone())).filter(|value| !value.is_empty()).collect();
    let mut actor_wxid = username.as_ref().map(|member| member.wxid.clone()).filter(|value| !value.is_empty());
    let mut actor_name = username.as_ref().and_then(|member| member.display_name.clone()).or_else(|| actor_wxid.clone());
    if actor_wxid.is_none() && template.starts_with('你') {
        actor_wxid = message.account_wxid.clone();
        actor_name = Some("你".to_string());
    }

    Some(ChatroomSystemEvent {
        event_type,
        chatroom_id,
        actor_wxid,
        actor_name,
        target_wxid: target_wxids.first().cloned(),
        target_name: target_names.first().cloned(),
        target_wxids,
        target_names,
        previous_value: None,
        current_value: remark.and_then(|member| member.display_name.or(Some(member.wxid))).filter(|value| !value.is_empty()),
        template_text: (!template.is_empty()).then_some(template.clone()),
        content_text: Some(render_template(&template, &links)),
        received_at: message.received_at.clone(),
        details: json!({ "links": links }),
    })
}

fn classify_v1(type_name: &str, msg_type: Option<i64>, appmsg_type: Option<i64>, content: Option<&str>) -> NormalizedKind {
    if type_name.eq_ignore_ascii_case("ModContacts") {
        return NormalizedKind::ChatroomContactsUpdate;
    }
    classify_message(msg_type, appmsg_type, content)
}

fn classify_message(msg_type: Option<i64>, appmsg_type: Option<i64>, content: Option<&str>) -> NormalizedKind {
    match msg_type.unwrap_or_default() {
        1 => NormalizedKind::Text,
        3 => NormalizedKind::Image,
        34 => NormalizedKind::Voice,
        42 => NormalizedKind::ContactCard,
        43 | 62 => NormalizedKind::Video,
        47 => NormalizedKind::Emoji,
        48 => NormalizedKind::Location,
        49 => classify_appmsg(appmsg_type, content),
        51 => NormalizedKind::Status,
        10000 | 10002 => NormalizedKind::System,
        _ => NormalizedKind::Unknown,
    }
}

fn classify_appmsg(appmsg_type: Option<i64>, content: Option<&str>) -> NormalizedKind {
    match appmsg_type.unwrap_or_default() {
        5 => NormalizedKind::Link,
        6 => {
            if content.is_some_and(|text| text.contains("<appattach>")) {
                NormalizedKind::File
            } else {
                NormalizedKind::FileNotice
            }
        }
        19 => NormalizedKind::ChatRecord,
        33 | 36 => NormalizedKind::MiniProgram,
        57 => NormalizedKind::Quote,
        74 => NormalizedKind::FileNotice,
        _ => NormalizedKind::AppMsg,
    }
}

fn choose_conversation_id(from_user: Option<&str>, to_user: Option<&str>, account_wxid: Option<&str>, is_outgoing: bool) -> Option<String> {
    if is_group_conversation(from_user) {
        return from_user.map(ToOwned::to_owned);
    }
    if is_group_conversation(to_user) {
        return to_user.map(ToOwned::to_owned);
    }
    if is_outgoing {
        to_user.map(ToOwned::to_owned)
    } else if from_user == account_wxid {
        to_user.map(ToOwned::to_owned)
    } else {
        from_user.map(ToOwned::to_owned)
    }
}

fn scalar(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_unwrapped(value: Option<&Value>) -> Option<String> {
    let value = value?;
    scalar(Some(value)).or_else(|| scalar(value.get("string"))).or_else(|| scalar(value.get("String")))
}

fn scalar_path(value: &Value, path: &[&str]) -> Option<String> {
    path.iter().try_fold(value, |current, key| current.get(*key)).and_then(|value| scalar(Some(value)).or_else(|| scalar_unwrapped(Some(value))))
}

fn int_path(value: &Value, path: &[&str]) -> Option<i64> {
    path.iter().try_fold(value, |current, key| current.get(*key)).and_then(int_scalar)
}

fn int_scalar(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        Value::Object(_) => scalar_unwrapped(value).and_then(|text| text.parse().ok()),
        _ => None,
    }
}

fn is_group_conversation(value: Option<&str>) -> bool {
    value.is_some_and(|text| text.ends_with("@chatroom"))
}

fn strip_group_speaker_prefix(text: &str) -> String {
    text.split_once(":\n").map_or_else(|| text.to_string(), |(_, content)| content.to_string())
}

fn extract_group_sender(text: &str) -> Option<String> {
    text.split_once(":\n").map(|(sender, _)| sender.to_string()).filter(|sender| !sender.is_empty())
}

fn is_likely_xml(text: &str) -> bool {
    text.trim_start().starts_with('<')
}

fn parse_appmsg_type(text: &str) -> Option<i64> {
    let re = Regex::new(r"(?is)<type>(\d+)</type>").ok()?;
    re.captures(text)?.get(1)?.as_str().parse().ok()
}

fn stable_dedupe_key(schema: &str, appid: &str, identity: &str) -> String {
    format!("{schema}:{appid}:{identity}")
}

fn sha256_short(value: &Value) -> String {
    sha256_hex(&value.to_string()).chars().take(24).collect()
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?is)<{tag}>(.*?)</{tag}>");
    let re = Regex::new(&pattern).ok()?;
    Some(strip_cdata(re.captures(xml)?.get(1)?.as_str()).trim().to_string())
}

fn extract_links(xml: &str) -> BTreeMap<String, Vec<ChatroomMember>> {
    let link_re = Regex::new(r#"(?is)<link\b[^>]*\bname=["']([^"']+)["'][^>]*>(.*?)</link>"#).expect("valid link regex");
    let member_re = Regex::new(r"(?is)<member>(.*?)</member>").expect("valid member regex");
    let mut links = BTreeMap::new();
    for link in link_re.captures_iter(xml) {
        let name = link.get(1).map(|m| m.as_str()).unwrap_or_default().to_string();
        let body = link.get(2).map(|m| m.as_str()).unwrap_or_default();
        let members = member_re
            .captures_iter(body)
            .map(|member| {
                let member_body = member.get(1).map(|m| m.as_str()).unwrap_or_default();
                ChatroomMember {
                    wxid: extract_tag(member_body, "username").map(|value| decode_xml_text(&value)).unwrap_or_default(),
                    display_name: extract_tag(member_body, "nickname").map(|value| decode_xml_text(&value)).filter(|value| !value.is_empty()),
                    flag: None,
                }
            })
            .collect();
        links.insert(name, members);
    }
    links
}

fn render_template(template: &str, links: &BTreeMap<String, Vec<ChatroomMember>>) -> String {
    let mut output = template.to_string();
    for (name, members) in links {
        let label = members
            .iter()
            .map(|member| member.display_name.clone().unwrap_or_else(|| member.wxid.clone()))
            .collect::<Vec<_>>()
            .join(", ");
        output = output.replace(&format!("${name}$"), &label);
    }
    output.trim().to_string()
}

fn strip_cdata(text: &str) -> String {
    text.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>").to_string()
}

fn decode_xml_text(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn _member_set(snapshot: &ChatroomSnapshot) -> BTreeSet<String> {
    snapshot.members.iter().map(|member| member.wxid.clone()).collect()
}
