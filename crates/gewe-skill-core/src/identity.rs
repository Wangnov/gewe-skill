use gewe_skill_types::IdentityMatch;
use std::collections::BTreeMap;

/// Keep one candidate per person or chatroom, preserving all matched sources and room scopes.
#[must_use]
pub fn merge_identity_matches(items: Vec<IdentityMatch>) -> Vec<IdentityMatch> {
    let mut entities = BTreeMap::<(bool, String), IdentityMatch>::new();
    for mut item in items {
        if let Some(source) = &item.source {
            item.sources.push(source.clone());
        }
        if let Some(room) = &item.chatroom_id {
            item.chatroom_ids.push(room.clone());
        }
        if let Some(alias) = &item.alias {
            item.matched_aliases.push(alias.clone());
        }
        let key = (item.entity_type == "chatroom", item.entity_id.clone());
        if let Some(existing) = entities.get_mut(&key) {
            let prefer_item = (item.entity_type == "contact" && existing.entity_type != "contact")
                || (item.entity_type == existing.entity_type && item.score > existing.score);
            if prefer_item {
                std::mem::swap(existing, &mut item);
            }
            existing.sources.extend(item.sources);
            existing.chatroom_ids.extend(item.chatroom_ids);
            existing.matched_aliases.extend(item.matched_aliases);
        } else {
            entities.insert(key, item);
        }
    }
    let mut items: Vec<_> = entities.into_values().collect();
    for item in &mut items {
        item.sources.sort();
        item.sources.dedup();
        item.chatroom_ids.sort();
        item.chatroom_ids.dedup();
        item.matched_aliases.sort();
        item.matched_aliases.dedup();
    }
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.is_current.cmp(&a.is_current))
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(kind: &str, room: Option<&str>, source: &str) -> IdentityMatch {
        IdentityMatch {
            entity_type: kind.to_string(),
            entity_id: "wxid_test_friend".to_string(),
            chatroom_id: room.map(str::to_string),
            display_name: Some("测试好友".to_string()),
            alias: Some("测试好友".to_string()),
            source: Some(source.to_string()),
            is_current: true,
            score: 1.0,
            last_seen_at: None,
            sources: Vec::new(),
            chatroom_ids: Vec::new(),
            matched_aliases: Vec::new(),
        }
    }

    #[test]
    fn person_is_one_candidate_with_contact_priority_and_all_sources_and_rooms() {
        let candidates = merge_identity_matches(vec![
            candidate(
                "chatroom_member",
                Some("test_room_a@chatroom"),
                "member_card",
            ),
            candidate(
                "chatroom_member",
                Some("test_room_b@chatroom"),
                "member_nickname",
            ),
            candidate("contact", None, "contact_remark"),
            candidate("contact", None, "contact_remark"),
        ]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].entity_type, "contact");
        assert_eq!(
            candidates[0].sources,
            ["contact_remark", "member_card", "member_nickname"]
        );
        assert_eq!(
            candidates[0].chatroom_ids,
            ["test_room_a@chatroom", "test_room_b@chatroom"]
        );
    }
}
