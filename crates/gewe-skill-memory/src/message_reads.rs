use super::{clamp_limit, ApiError};
use chrono::DateTime;
use gewe_skill_types::{ApiPage, MessagePagination, MessageQuery, NormalizedMessage};
use sqlx::{Row, SqlitePool};
use std::fmt::Write;

fn filter_sql(query: &MessageQuery) -> Result<(String, Vec<String>), ApiError> {
    let mut sql = String::from("FROM messages WHERE 1 = 1");
    let mut args = Vec::new();
    for (field, value) in [
        ("conversation_id", &query.conversation_id),
        ("sender_wxid", &query.sender_wxid),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            let _ = write!(sql, " AND {field} = ?");
            args.push(value.to_string());
        }
    }
    if query.dm == Some(true) {
        sql.push_str(" AND is_group = 0");
    }
    if let Some(kind) = query.kind.as_deref().filter(|v| !v.is_empty()) {
        sql.push_str(" AND lower(kind) = lower(?)");
        args.push(kind.to_string());
    }
    match query.direction.as_deref() {
        Some("incoming") => sql.push_str(" AND is_outgoing = 0"),
        Some("outgoing") => sql.push_str(" AND is_outgoing != 0"),
        _ => {}
    }
    for (operator, value) in [(">=", &query.after), ("<", &query.before)] {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            validate_time(value)?;
            let _ = write!(sql, " AND julianday(received_at) {operator} julianday(?)");
            args.push(value.to_string());
        }
    }
    if let Some(q) = query.q.as_deref().filter(|v| !v.is_empty()) {
        sql.push_str(" AND (content_text LIKE ? OR message_json LIKE ?)");
        args.extend([format!("%{q}%"), format!("%{q}%")]);
    }
    Ok((sql, args))
}

fn validate_time(value: &str) -> Result<(), ApiError> {
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| ApiError::BadRequest("时间须为带时区的 RFC3339 格式".to_string()))
}

fn cursor_position(cursor: &str) -> Result<(&str, Option<i64>), ApiError> {
    if let Some((time, id)) = cursor.rsplit_once('|') {
        validate_time(time)?;
        let id = id
            .parse::<i64>()
            .map_err(|_| ApiError::BadRequest("invalid_message_cursor".to_string()))?;
        Ok((time, Some(id)))
    } else {
        validate_time(cursor)?;
        Ok((cursor, None))
    }
}

fn append_position(
    sql: &mut String,
    args: &mut Vec<String>,
    time: &str,
    id: Option<i64>,
    asc: bool,
) {
    let op = if asc { ">" } else { "<" };
    if let Some(id) = id {
        let _ = write!(sql, " AND (julianday(received_at) {op} julianday(?) OR (julianday(received_at) = julianday(?) AND id {op} ?))");
        args.extend([time.to_string(), time.to_string(), id.to_string()]);
    } else {
        let _ = write!(sql, " AND julianday(received_at) {op} julianday(?)");
        args.push(time.to_string());
    }
}

async fn exists_beyond(
    db: &SqlitePool,
    query: &MessageQuery,
    time: &str,
    id: i64,
    asc: bool,
) -> Result<bool, ApiError> {
    let (mut sql, mut args) = filter_sql(query)?;
    append_position(&mut sql, &mut args, time, Some(id), asc);
    let statement = format!("SELECT EXISTS(SELECT 1 {sql}) AS present");
    let mut statement = sqlx::query(&statement);
    for arg in args {
        statement = statement.bind(arg);
    }
    Ok(statement.fetch_one(db).await?.get::<i64, _>("present") != 0)
}

pub(super) async fn pagination_for_positions(
    db: &SqlitePool,
    query: &MessageQuery,
    oldest: Option<(String, i64)>,
    newest: Option<(String, i64)>,
    returned: usize,
) -> Result<MessagePagination, ApiError> {
    let asc = query
        .order
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("asc"));
    let has_earlier = if let Some((time, id)) = &oldest {
        exists_beyond(db, query, time, *id, false).await?
    } else {
        false
    };
    let has_later = if let Some((time, id)) = &newest {
        exists_beyond(db, query, time, *id, true).await?
    } else {
        false
    };
    Ok(MessagePagination {
        order: if asc { "asc" } else { "desc" }.to_string(),
        selection: if asc { "earliest" } else { "latest" }.to_string(),
        time_basis: "received_at".to_string(),
        returned,
        start_at: oldest.as_ref().map(|(time, _)| time.clone()),
        end_at: newest.as_ref().map(|(time, _)| time.clone()),
        has_more: if asc { has_later } else { has_earlier },
        has_earlier,
        has_later,
        earlier_cursor: oldest
            .filter(|_| has_earlier)
            .map(|(time, id)| format!("{time}|{id}")),
        later_cursor: newest
            .filter(|_| has_later)
            .map(|(time, id)| format!("{time}|{id}")),
    })
}

pub(super) async fn list_messages_impl(
    db: &SqlitePool,
    query: &MessageQuery,
) -> Result<ApiPage<NormalizedMessage>, ApiError> {
    let limit = clamp_limit(query.limit);
    let asc = query
        .order
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("asc"));
    let (mut sql, mut args) = filter_sql(query)?;
    if let Some(cursor) = query.cursor.as_deref().filter(|v| !v.is_empty()) {
        let (time, id) = cursor_position(cursor)?;
        append_position(&mut sql, &mut args, time, id, asc);
    }
    let order = if asc { "ASC" } else { "DESC" };
    let sql = format!("SELECT id, received_at, message_json {sql} ORDER BY julianday(received_at) {order}, id {order} LIMIT ?");
    let mut statement = sqlx::query(&sql);
    for arg in args {
        statement = statement.bind(arg);
    }
    let rows = statement.bind(limit).fetch_all(db).await?;
    let items = rows
        .iter()
        .map(|row| serde_json::from_str::<NormalizedMessage>(row.get("message_json")))
        .collect::<Result<Vec<_>, _>>()?;
    let position = |row: &sqlx::sqlite::SqliteRow| {
        (row.get::<String, _>("received_at"), row.get::<i64, _>("id"))
    };
    let first = rows.first().map(position);
    let last = rows.last().map(position);
    let (oldest, newest) = if asc { (first, last) } else { (last, first) };
    let pagination = pagination_for_positions(db, query, oldest, newest, items.len()).await?;
    let next_cursor = if asc {
        pagination.later_cursor.clone()
    } else {
        pagination.earlier_cursor.clone()
    };
    Ok(ApiPage {
        items,
        next_cursor,
        pagination: Some(pagination),
    })
}

pub(super) async fn context_pagination(
    db: &SqlitePool,
    conversation_id: Option<String>,
    messages: &[&NormalizedMessage],
) -> Result<Option<MessagePagination>, ApiError> {
    let mut positions = Vec::new();
    for message in [messages.first(), messages.last()].into_iter().flatten() {
        let row = sqlx::query("SELECT id, received_at FROM messages WHERE message_key = ?")
            .bind(&message.message_key)
            .fetch_one(db)
            .await?;
        positions.push((row.get::<String, _>("received_at"), row.get::<i64, _>("id")));
    }
    if positions.is_empty() {
        return Ok(None);
    }
    let query = MessageQuery {
        conversation_id,
        order: Some("asc".to_string()),
        ..MessageQuery::default()
    };
    let mut page = pagination_for_positions(
        db,
        &query,
        positions.first().cloned(),
        positions.last().cloned(),
        messages.len(),
    )
    .await?;
    page.selection = "context".to_string();
    Ok(Some(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_db, normalize_callback, write_ingest_request};
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fixture() -> SqlitePool {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_db(&db).await.unwrap();
        for (id, time) in [
            (1, "2026-10-12T15:00:00Z"),
            (2, "2026-10-12T16:00:00Z"),
            (3, "2026-10-12T17:00:00Z"),
            (4, "2026-10-12T17:00:00Z"),
            (5, "2026-10-12T18:00:00Z"),
        ] {
            let callback = json!({ "appid": "test-app", "wxid": "wxid_test_account", "newMsgId": id, "msgType": 1,
                "fromUser": "wxid_test_friend", "toUser": "wxid_test_account", "content": format!("消息{id}") });
            let request = normalize_callback(&callback, time)
                .unwrap()
                .into_ingest_request();
            let _ = write_ingest_request(&db, request).await.unwrap();
        }
        db
    }

    #[tokio::test]
    async fn after_selects_latest_by_default_and_same_timestamp_cursor_loses_nothing() {
        let db = fixture().await;
        let mut query = MessageQuery {
            after: Some("2026-10-13T00:00:00+08:00".to_string()),
            limit: Some(2),
            ..MessageQuery::default()
        };
        let page = list_messages_impl(&db, &query).await.unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|m| m.content_text.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["消息5", "消息4"]
        );
        let meta = page.pagination.unwrap();
        assert_eq!(meta.selection, "latest");
        assert!(meta.has_earlier);
        assert!(!meta.has_later);
        query.cursor = page.next_cursor;
        let page = list_messages_impl(&db, &query).await.unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|m| m.content_text.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["消息3", "消息2"]
        );
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn ascending_selects_earliest_and_both_sides_report_remaining_messages() {
        let db = fixture().await;
        let mut query = MessageQuery {
            after: Some("2026-10-13T00:00:00+08:00".to_string()),
            limit: Some(2),
            order: Some("asc".to_string()),
            ..MessageQuery::default()
        };
        let page = list_messages_impl(&db, &query).await.unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|m| m.content_text.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["消息2", "消息3"]
        );
        assert_eq!(page.pagination.as_ref().unwrap().selection, "earliest");
        query.cursor = page.next_cursor;
        let page = list_messages_impl(&db, &query).await.unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|m| m.content_text.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["消息4", "消息5"]
        );
        let meta = page.pagination.unwrap();
        assert!(meta.has_earlier);
        assert!(!meta.has_later);
    }

    #[tokio::test]
    async fn invalid_time_is_an_error_instead_of_a_broad_query() {
        let db = fixture().await;
        let result = list_messages_impl(
            &db,
            &MessageQuery {
                after: Some("not-a-time".to_string()),
                ..MessageQuery::default()
            },
        )
        .await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }
}
