use std::collections::{HashMap, HashSet};

use gewe_skill_client::GeweSkillClient;
use gewe_skill_types::{
    ApiPage, ChatroomEventType, ChatroomMemberEvent, ChatroomSystemEvent, IdentityProfileResponse,
};
use serde_json::Value;

use crate::{
    looks_like_stable_wechat_id, resolve_conversation_for_query, AgentChatroomEventQueryArgs, Order,
};

pub(crate) async fn agent_query_chatroom_events(
    client: &GeweSkillClient,
    args: AgentChatroomEventQueryArgs,
) -> Result<Value, Box<dyn std::error::Error>> {
    let conversation = resolve_conversation_for_query(
        client,
        args.chatroom_id.clone(),
        args.conversation.clone(),
        args.resolve_limit,
    )
    .await?;

    if args.conversation.is_some() && conversation.id.is_none() && !args.allow_unresolved {
        return Ok(serde_json::json!({
            "ok": false,
            "executed": false,
            "error": "chatroom_unresolved",
            "resolved": {
                "conversation": conversation.to_json(),
            }
        }));
    }

    let Some(chatroom_id) = conversation.id.clone() else {
        return Ok(serde_json::json!({
            "ok": false,
            "executed": false,
            "error": "missing_chatroom_id",
            "resolved": {
                "conversation": conversation.to_json(),
            }
        }));
    };

    let fetch_limit = u32::try_from(args.limit.saturating_mul(4).clamp(50, 500)).unwrap_or(500);
    let mut events = Vec::new();
    let mut member_scanned = 0usize;
    let mut system_scanned = 0usize;

    if args.member_events {
        let page: ApiPage<ChatroomMemberEvent> = client
            .chatroom_events(&chatroom_id, Some(fetch_limit))
            .await?;
        member_scanned = page.items.len();
        events.extend(page.items.into_iter().map(member_event_timeline_item));
    }

    if args.system_events {
        let page: ApiPage<ChatroomSystemEvent> = client
            .chatroom_system_events(&chatroom_id, Some(fetch_limit))
            .await?;
        system_scanned = page.items.len();
        events.extend(page.items.into_iter().map(system_event_timeline_item));
    }

    let filters = normalized_event_type_filters(&args.event_type);
    events.retain(|event| {
        let event_type = event
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let received_at = event
            .get("received_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        (filters.is_empty() || filters.iter().any(|filter| filter == event_type))
            && args
                .after
                .as_deref()
                .map(|after| received_at >= after)
                .unwrap_or(true)
            && args
                .before
                .as_deref()
                .map(|before| received_at <= before)
                .unwrap_or(true)
    });
    let mut seen_events = HashSet::new();
    events.retain(|event| seen_events.insert(timeline_event_dedupe_key(event)));

    events.sort_by(|left, right| {
        let left_time = left
            .get("received_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_time = right
            .get("received_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match args.order {
            Order::Asc => left_time.cmp(right_time),
            Order::Desc => right_time.cmp(left_time),
        }
    });
    events.truncate(args.limit);
    let identity_enrichment =
        enrich_timeline_event_identities(client, &chatroom_id, &mut events).await?;

    Ok(serde_json::json!({
        "ok": true,
        "executed": true,
        "query_mode": "agent_chatroom_events",
        "resolved": {
            "conversation": conversation.to_json(),
        },
        "event_query": {
            "chatroom_id": chatroom_id,
            "event_type": filters,
            "after": args.after,
            "before": args.before,
            "limit": args.limit,
            "order": args.order.query_value(),
            "member_events": args.member_events,
            "system_events": args.system_events,
        },
        "scanned": {
            "member_events": member_scanned,
            "system_events": system_scanned,
        },
        "identity_enrichment": identity_enrichment,
        "events": events,
        "agent_hints": [
            "system events usually have better actor/target names when GeWe parsed the system message",
            "member events come from snapshot diffs and are better evidence for actual membership state changes",
            "display names are enriched from memory and keep wxids as stable evidence fields",
            "if system and member events disagree, report the disagreement instead of guessing"
        ]
    }))
}

async fn enrich_timeline_event_identities(
    client: &GeweSkillClient,
    chatroom_id: &str,
    events: &mut [Value],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut wxids = HashSet::new();
    for event in events.iter() {
        collect_event_wxids(event, &mut wxids);
    }

    let mut profiles: HashMap<String, Option<IdentityProfileResponse>> = HashMap::new();
    let mut failed = Vec::new();
    for wxid in wxids.iter() {
        match client.identity_profile(wxid, Some(chatroom_id)).await {
            Ok(profile) => {
                profiles.insert(wxid.clone(), Some(profile));
            }
            Err(error) => {
                profiles.insert(wxid.clone(), None);
                failed.push(serde_json::json!({
                    "wxid": wxid,
                    "error": error.to_string()
                }));
            }
        }
    }

    for event in events.iter_mut() {
        apply_identity_enrichment(event, &profiles);
    }

    let enriched = profiles
        .values()
        .filter(|profile| profile.is_some())
        .count();
    Ok(serde_json::json!({
        "requested": wxids.len(),
        "enriched": enriched,
        "failed_count": failed.len(),
        "failed": failed
    }))
}

fn collect_event_wxids(event: &Value, wxids: &mut HashSet<String>) {
    for key in ["member_wxid", "actor_wxid", "target_wxid"] {
        if let Some(wxid) = event.get(key).and_then(Value::as_str) {
            if looks_like_identity_wxid(wxid) {
                wxids.insert(wxid.to_string());
            }
        }
    }
    if let Some(values) = event.get("target_wxids").and_then(Value::as_array) {
        for value in values {
            if let Some(wxid) = value.as_str() {
                if looks_like_identity_wxid(wxid) {
                    wxids.insert(wxid.to_string());
                }
            }
        }
    }
}

fn apply_identity_enrichment(
    event: &mut Value,
    profiles: &HashMap<String, Option<IdentityProfileResponse>>,
) {
    let mut member_display_name = None;
    let mut actor_display_name = None;
    let mut target_display_name = None;

    if let Some(wxid) = event.get("member_wxid").and_then(Value::as_str) {
        member_display_name = display_name_for_wxid(wxid, profiles);
    }
    if let Some(wxid) = event.get("actor_wxid").and_then(Value::as_str) {
        actor_display_name = display_name_for_wxid(wxid, profiles);
    }
    if let Some(wxid) = event.get("target_wxid").and_then(Value::as_str) {
        target_display_name = display_name_for_wxid(wxid, profiles);
    }

    let target_display_names = event
        .get("target_wxids")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|wxid| {
                    display_name_for_wxid(wxid, profiles).unwrap_or_else(|| wxid.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let summary = enriched_event_summary(
        event,
        member_display_name.as_deref(),
        actor_display_name.as_deref(),
        target_display_name.as_deref(),
        &target_display_names,
    );

    if let Some(object) = event.as_object_mut() {
        if let Some(value) = member_display_name {
            object.insert("member_display_name".to_string(), serde_json::json!(value));
        }
        if let Some(value) = actor_display_name {
            object.insert("actor_display_name".to_string(), serde_json::json!(value));
        }
        if let Some(value) = target_display_name {
            object.insert("target_display_name".to_string(), serde_json::json!(value));
        }
        if !target_display_names.is_empty() {
            object.insert(
                "target_display_names".to_string(),
                serde_json::json!(target_display_names),
            );
        }
        object.insert("summary".to_string(), serde_json::json!(summary));
    }
}

fn display_name_for_wxid(
    wxid: &str,
    profiles: &HashMap<String, Option<IdentityProfileResponse>>,
) -> Option<String> {
    profiles
        .get(wxid)
        .and_then(Option::as_ref)
        .and_then(|profile| profile.effective_display_name.clone())
}

fn enriched_event_summary(
    event: &Value,
    member_display_name: Option<&str>,
    actor_display_name: Option<&str>,
    target_display_name: Option<&str>,
    target_display_names: &[String],
) -> String {
    let event_type = event
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "member_joined" => format!(
            "member joined: {}",
            member_display_name
                .or_else(|| event.get("member_wxid").and_then(Value::as_str))
                .unwrap_or("unknown")
        ),
        "member_left" => format!(
            "member left: {}",
            member_display_name
                .or_else(|| event.get("member_wxid").and_then(Value::as_str))
                .unwrap_or("unknown")
        ),
        "member_removed" => format!(
            "member removed: {}",
            target_display_name
                .or(member_display_name)
                .or_else(|| event.get("target_name").and_then(Value::as_str))
                .or_else(|| event.get("target_wxid").and_then(Value::as_str))
                .or_else(|| event.get("member_wxid").and_then(Value::as_str))
                .unwrap_or("unknown")
        ),
        "member_invited" => {
            let targets = if !target_display_names.is_empty() {
                target_display_names.join(", ")
            } else {
                event
                    .get("target_names")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        target_display_name
                            .or(member_display_name)
                            .or_else(|| event.get("target_wxid").and_then(Value::as_str))
                            .or_else(|| event.get("member_wxid").and_then(Value::as_str))
                            .unwrap_or("unknown")
                            .to_string()
                    })
            };
            let actor = actor_display_name
                .or_else(|| event.get("actor_name").and_then(Value::as_str))
                .or_else(|| event.get("actor_wxid").and_then(Value::as_str));
            if let Some(actor) = actor {
                format!("member invited by {actor}: {targets}")
            } else {
                format!("member invited: {targets}")
            }
        }
        _ => event
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("chatroom event")
            .to_string(),
    }
}

fn looks_like_identity_wxid(value: &str) -> bool {
    looks_like_stable_wechat_id(value)
        || value.starts_with("qq")
        || value.chars().any(|character| character.is_ascii_digit())
}

fn timeline_event_dedupe_key(event: &Value) -> String {
    let read = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let target_names = event
        .get("target_names")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    [
        read("source"),
        read("event_type"),
        read("received_at"),
        read("content_text"),
        read("template_text"),
        read("member_wxid"),
        read("actor_wxid"),
        read("target_wxid"),
        read("current_value"),
        target_names,
    ]
    .join("|")
}

fn member_event_timeline_item(event: ChatroomMemberEvent) -> Value {
    let event_type = chatroom_event_type_value(&event.event_type);
    let raw_event = event.clone();
    serde_json::json!({
        "source": "member_event",
        "event_type": event_type,
        "received_at": event.received_at.clone(),
        "chatroom_id": event.chatroom_id.clone(),
        "summary": summarize_member_event(&event),
        "member_wxid": event.member_wxid.clone(),
        "previous_chatroom_name": event.previous_chatroom_name.clone(),
        "current_chatroom_name": event.current_chatroom_name.clone(),
        "previous_member_count": event.previous_member_count,
        "current_member_count": event.current_member_count,
        "details": event.details.clone(),
        "raw_event": raw_event,
    })
}

fn system_event_timeline_item(event: ChatroomSystemEvent) -> Value {
    let event_type = chatroom_event_type_value(&event.event_type);
    let raw_event = event.clone();
    serde_json::json!({
        "source": "system_event",
        "event_type": event_type,
        "received_at": event.received_at.clone(),
        "chatroom_id": event.chatroom_id.clone(),
        "summary": summarize_system_event(&event),
        "actor_wxid": event.actor_wxid.clone(),
        "actor_name": event.actor_name.clone(),
        "target_wxid": event.target_wxid.clone(),
        "target_name": event.target_name.clone(),
        "target_wxids": event.target_wxids.clone(),
        "target_names": event.target_names.clone(),
        "previous_value": event.previous_value.clone(),
        "current_value": event.current_value.clone(),
        "template_text": event.template_text.clone(),
        "content_text": event.content_text.clone(),
        "details": event.details.clone(),
        "raw_event": raw_event,
    })
}

fn summarize_member_event(event: &ChatroomMemberEvent) -> String {
    match event.event_type {
        ChatroomEventType::MemberJoined => format!(
            "member joined: {}",
            event.member_wxid.as_deref().unwrap_or("unknown")
        ),
        ChatroomEventType::MemberLeft => format!(
            "member left: {}",
            event.member_wxid.as_deref().unwrap_or("unknown")
        ),
        ChatroomEventType::MemberRemoved => format!(
            "member removed: {}",
            event.member_wxid.as_deref().unwrap_or("unknown")
        ),
        ChatroomEventType::MemberInvited => format!(
            "member invited: {}",
            event.member_wxid.as_deref().unwrap_or("unknown")
        ),
        ChatroomEventType::MemberCountChanged => format!(
            "member count changed: {} -> {}",
            event
                .previous_member_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            event
                .current_member_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        ChatroomEventType::ChatroomNameChanged => format!(
            "chatroom name changed: {} -> {}",
            event.previous_chatroom_name.as_deref().unwrap_or("unknown"),
            event.current_chatroom_name.as_deref().unwrap_or("unknown")
        ),
        ChatroomEventType::SystemUnknown => "unknown member event".to_string(),
    }
}

fn summarize_system_event(event: &ChatroomSystemEvent) -> String {
    if let Some(content) = event
        .content_text
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return content.to_string();
    }
    match event.event_type {
        ChatroomEventType::MemberInvited => format!(
            "member invited: {}",
            joined_names_or_ids(&event.target_names, &event.target_wxids)
        ),
        ChatroomEventType::MemberRemoved => format!(
            "member removed: {}",
            event
                .target_name
                .as_deref()
                .or(event.target_wxid.as_deref())
                .unwrap_or("unknown")
        ),
        ChatroomEventType::ChatroomNameChanged => format!(
            "chatroom name changed: {} -> {}",
            event.previous_value.as_deref().unwrap_or("unknown"),
            event.current_value.as_deref().unwrap_or("unknown")
        ),
        _ => format!(
            "{} system event",
            chatroom_event_type_value(&event.event_type)
        ),
    }
}

fn joined_names_or_ids(names: &[String], ids: &[String]) -> String {
    if !names.is_empty() {
        names.join(", ")
    } else if !ids.is_empty() {
        ids.join(", ")
    } else {
        "unknown".to_string()
    }
}

fn normalized_event_type_filters(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase().replace('-', "_"))
        .filter(|value| !value.is_empty())
        .collect()
}

fn chatroom_event_type_value(event_type: &ChatroomEventType) -> String {
    serde_json::to_value(event_type)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{event_type:?}").to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn test_identity_memory_status() -> gewe_skill_types::IdentityMemoryStatus {
        gewe_skill_types::IdentityMemoryStatus {
            refresh_recommended: false,
            reasons: Vec::new(),
            stale_after_days: 1,
            contact: gewe_skill_types::IdentityMemoryRecordStatus {
                present: true,
                stale: false,
                refresh_recommended: false,
                stale_after_days: 1,
                age_days: Some(0),
                last_seen_at: Some("2026-05-27T00:00:00Z".to_string()),
                updated_at: Some("2026-05-27T00:00:00Z".to_string()),
            },
            chatroom_member: Some(gewe_skill_types::IdentityMemoryRecordStatus {
                present: true,
                stale: false,
                refresh_recommended: false,
                stale_after_days: 1,
                age_days: Some(0),
                last_seen_at: Some("2026-05-27T00:00:00Z".to_string()),
                updated_at: None,
            }),
        }
    }

    #[test]
    fn event_type_filters_normalize_spacing_and_hyphens() {
        let filters = normalized_event_type_filters(&[
            " member-joined ".to_string(),
            "member_left".to_string(),
            "".to_string(),
        ]);
        assert_eq!(filters, vec!["member_joined", "member_left"]);
    }

    #[test]
    fn timeline_event_dedupe_key_ignores_edge_event_id_noise() {
        let first = json!({
            "source": "system_event",
            "event_type": "system_unknown",
            "received_at": "2026-05-26T15:05:06.874Z",
            "content_text": "",
            "template_text": null,
            "target_names": [],
            "details": { "edge_event_id": 1 }
        });
        let second = json!({
            "source": "system_event",
            "event_type": "system_unknown",
            "received_at": "2026-05-26T15:05:06.874Z",
            "content_text": "",
            "template_text": null,
            "target_names": [],
            "details": { "edge_event_id": 129 }
        });
        assert_eq!(
            timeline_event_dedupe_key(&first),
            timeline_event_dedupe_key(&second)
        );
    }

    #[test]
    fn identity_enrichment_adds_display_name_but_keeps_stable_wxid() {
        let wxid = "wxid_left_member".to_string();
        let mut profiles = HashMap::new();
        profiles.insert(
            wxid.clone(),
            Some(IdentityProfileResponse {
                entity_id: wxid.clone(),
                chatroom_id: Some("123@chatroom".to_string()),
                effective_display_name: Some("视频怪物".to_string()),
                display_name_source: "chatroom_display_name".to_string(),
                display_name_resolution: gewe_skill_types::IdentityDisplayNameResolution {
                    selected_source: "chatroom_display_name".to_string(),
                    selected_value: Some("视频怪物".to_string()),
                    candidates: vec![gewe_skill_types::IdentityDisplayNameCandidate {
                        source: "chatroom_display_name".to_string(),
                        value: Some("视频怪物".to_string()),
                        selected: true,
                    }],
                },
                memory_status: test_identity_memory_status(),
                contact: None,
                chatroom_member: None,
                aliases: Vec::new(),
            }),
        );
        let mut event = json!({
            "source": "member_event",
            "event_type": "member_left",
            "received_at": "2026-05-26T08:34:51.997Z",
            "chatroom_id": "123@chatroom",
            "member_wxid": wxid,
            "summary": "member left: wxid_left_member"
        });

        apply_identity_enrichment(&mut event, &profiles);

        assert_eq!(event["member_wxid"], "wxid_left_member");
        assert_eq!(event["member_display_name"], "视频怪物");
        assert_eq!(event["summary"], "member left: 视频怪物");
    }
}
