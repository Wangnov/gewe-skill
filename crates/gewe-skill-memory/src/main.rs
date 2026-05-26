use axum::{
    extract::{Path, Query, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use gewe_skill_core::{diff_chatroom_snapshots, normalize_callback};
use gewe_skill_types::{
    ApiPage, AttachmentRecord, ChatroomEventType, ChatroomMember, ChatroomMemberEvent,
    ChatroomSnapshot, ChatroomSystemEvent, ConversationSummary, IdentityMatch,
    IdentityRefreshRequest, IdentityRefreshResponse, IdentityResolveResponse, IngestEventRequest,
    NormalizedMessage, RawCallbackRequest,
};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::{
    env,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    attachment_dir: PathBuf,
    read_token: Option<String>,
    write_token: Option<String>,
    gewe_base_url: String,
    gewe_app_id: Option<String>,
    gewe_token: Option<String>,
    http: HttpClient,
}

type SharedState = Arc<AppState>;

#[derive(Debug, Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
}

#[derive(Debug, Serialize)]
struct IngestResponse {
    ok: bool,
    message_key: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = env::var("GEWE_SKILL_DATABASE_URL").unwrap_or_else(|_| {
        "sqlite:/opt/gewe-skill-memory/data/gewe-skill-memory.sqlite?mode=rwc".to_string()
    });
    let listen = env::var("GEWE_SKILL_LISTEN").unwrap_or_else(|_| "127.0.0.1:8788".to_string());
    let attachment_dir = PathBuf::from(
        env::var("GEWE_SKILL_ATTACHMENT_DIR")
            .unwrap_or_else(|_| "/opt/gewe-skill-memory/data/attachments".to_string()),
    );
    ensure_sqlite_parent(&database_url)?;
    std::fs::create_dir_all(&attachment_dir)?;
    let db = SqlitePoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await?;
    init_db(&db).await?;

    let state = Arc::new(AppState {
        db,
        attachment_dir,
        read_token: env::var("GEWE_SKILL_READ_TOKEN").ok(),
        write_token: env::var("GEWE_SKILL_WRITE_TOKEN").ok(),
        gewe_base_url: env_first(&["GEWE_SKILL_GEWE_BASE_URL", "GEWE_API_BASE_URL"])
            .unwrap_or_else(|| "http://api.geweapi.com".to_string()),
        gewe_app_id: env_first(&["GEWE_SKILL_GEWE_APP_ID", "GEWE_APP_ID", "APP_ID"]),
        gewe_token: env_first(&["GEWE_SKILL_GEWE_TOKEN", "GEWE_TOKEN"]),
        http: HttpClient::new(),
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/write/events",
            post(write_event).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/write/raw-events",
            post(write_raw_event).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/write/attachments",
            post(write_attachment).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/api/messages/recent",
            get(recent_messages).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/messages/search",
            get(search_messages).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/conversations",
            get(conversations).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/identity/resolve",
            get(resolve_identity).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/identity/refresh",
            post(refresh_identity).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/attachments/recent",
            get(recent_attachments).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/attachments/{sha256}/download",
            get(download_attachment).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/chatrooms/{chatroom_id}/snapshots",
            get(chatroom_snapshots).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/chatrooms/{chatroom_id}/events",
            get(chatroom_events).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/chatrooms/{chatroom_id}/system-events",
            get(chatroom_system_events).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = listen.parse()?;
    info!(%addr, "starting gewe-skill-memory");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn ensure_sqlite_parent(database_url: &str) -> std::io::Result<()> {
    let Some(path) = database_url.strip_prefix("sqlite:") else {
        return Ok(());
    };
    let path = path.split('?').next().unwrap_or(path);
    if path == ":memory:" {
        return Ok(());
    }
    if let Some(parent) = FsPath::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| env::var(key).ok())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "gewe-skill-memory",
    })
}

async fn require_write_token(
    State(state): State<SharedState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    authorize(headers, state.write_token.as_deref(), request, next).await
}

async fn require_read_token(
    State(state): State<SharedState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    authorize(
        headers,
        state.read_token.as_deref().or(state.write_token.as_deref()),
        request,
        next,
    )
    .await
}

async fn authorize(
    headers: HeaderMap,
    expected: Option<&str>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = expected else {
        return next.run(request).await;
    };
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token == Some(expected) {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "ok": false, "error": "unauthorized" })),
    )
        .into_response()
}

async fn write_event(
    State(state): State<SharedState>,
    Json(request): Json<IngestEventRequest>,
) -> Result<Json<IngestResponse>, ApiError> {
    write_ingest_request(&state.db, request).await
}

async fn write_raw_event(
    State(state): State<SharedState>,
    Json(request): Json<RawCallbackRequest>,
) -> Result<Json<IngestResponse>, ApiError> {
    let normalized = normalize_callback(&request.body, request.received_at)?;
    let mut ingest = normalized.into_ingest_request();
    if let Some(current) = &ingest.chatroom_snapshot {
        if let Some(previous) = latest_chatroom_snapshot(&state.db, &current.chatroom_id).await? {
            ingest.chatroom_member_events = diff_chatroom_snapshots(&previous, current);
        }
    }
    write_ingest_request(&state.db, ingest).await
}

async fn write_attachment(
    State(state): State<SharedState>,
    Json(record): Json<AttachmentRecord>,
) -> Result<Json<serde_json::Value>, ApiError> {
    insert_attachment(&state.db, &record).await?;
    Ok(Json(json!({ "ok": true, "sha256": record.sha256 })))
}

async fn write_ingest_request(
    db: &SqlitePool,
    request: IngestEventRequest,
) -> Result<Json<IngestResponse>, ApiError> {
    let mut tx = db.begin().await?;
    insert_raw_event(&mut tx, &request).await?;
    insert_message(&mut tx, &request.message).await?;
    if let Some(snapshot) = &request.chatroom_snapshot {
        insert_chatroom_snapshot(&mut tx, &request.message.message_key, snapshot).await?;
    }
    for event in &request.chatroom_member_events {
        insert_chatroom_member_event(&mut tx, event).await?;
    }
    for event in &request.chatroom_system_events {
        insert_chatroom_system_event(&mut tx, event).await?;
    }
    project_identity_from_ingest(&mut tx, &request).await?;
    tx.commit().await?;

    Ok(Json(IngestResponse {
        ok: true,
        message_key: request.message.message_key,
    }))
}

async fn latest_chatroom_snapshot(
    db: &SqlitePool,
    chatroom_id: &str,
) -> Result<Option<ChatroomSnapshot>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT snapshot_json
        FROM chatroom_snapshots
        WHERE chatroom_id = ?
        ORDER BY received_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(chatroom_id)
    .fetch_optional(db)
    .await?;
    row.map(|row| serde_json::from_str(row.get::<&str, _>("snapshot_json")))
        .transpose()
        .map_err(ApiError::from)
}

async fn recent_messages(
    State(state): State<SharedState>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let cursor = query
        .cursor
        .unwrap_or_else(|| "9999-12-31T23:59:59.999Z".to_string());
    let rows = sqlx::query(
        r#"
        SELECT message_json
        FROM messages
        WHERE received_at < ?
        ORDER BY received_at DESC, id DESC
        LIMIT ?
        "#,
    )
    .bind(cursor)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok()
        })
        .collect::<Vec<_>>();
    let next_cursor = items.last().map(|message| message.received_at.clone());
    Ok(Json(ApiPage { items, next_cursor }))
}

async fn search_messages(
    State(state): State<SharedState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let pattern = format!("%{}%", query.q);
    let rows = sqlx::query(
        r#"
        SELECT message_json
        FROM messages
        WHERE content_text LIKE ?
        ORDER BY received_at DESC, id DESC
        LIMIT ?
        "#,
    )
    .bind(pattern)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok()
        })
        .collect::<Vec<_>>();
    let next_cursor = items.last().map(|message| message.received_at.clone());
    Ok(Json(ApiPage { items, next_cursor }))
}

async fn conversations(
    State(state): State<SharedState>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<ConversationSummary>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let rows = sqlx::query(
        r#"
        SELECT conversation_id, MAX(is_group) AS is_group, MAX(received_at) AS last_message_at,
               COUNT(*) AS message_count,
               SUBSTR((SELECT content_text FROM messages m2 WHERE m2.conversation_id = messages.conversation_id ORDER BY received_at DESC, id DESC LIMIT 1), 1, 160) AS preview,
               COALESCE(
                 (SELECT display_name FROM identity_chatrooms c WHERE c.chatroom_id = messages.conversation_id),
                 (SELECT COALESCE(NULLIF(remark, ''), NULLIF(nickname, ''), NULLIF(alias, '')) FROM identity_contacts p WHERE p.wxid = messages.conversation_id)
               ) AS display_name
        FROM messages
        WHERE conversation_id IS NOT NULL
        GROUP BY conversation_id
        ORDER BY last_message_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .map(|row| ConversationSummary {
            conversation_id: row.get("conversation_id"),
            display_name: row.get("display_name"),
            is_group: row.get::<i64, _>("is_group") != 0,
            last_message_at: row.get("last_message_at"),
            last_message_preview: row.get("preview"),
            message_count: row.get("message_count"),
        })
        .collect();
    Ok(Json(ApiPage {
        items,
        next_cursor: None,
    }))
}

async fn resolve_identity(
    State(state): State<SharedState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<IdentityResolveResponse>, ApiError> {
    let limit = clamp_limit(query.limit);
    let alias_key = normalize_alias(&query.q);
    let like = format!("%{}%", query.q.trim());
    let rows = sqlx::query(
        r#"
        SELECT entity_type, entity_id, NULLIF(scope_key, '') AS chatroom_id, alias, source,
               is_current, confidence, last_seen_at,
               COALESCE(
                 CASE
                   WHEN entity_type = 'chatroom' THEN (
                     SELECT COALESCE(NULLIF(display_name, ''), NULLIF(remark, ''))
                     FROM identity_chatrooms WHERE chatroom_id = entity_id
                   )
                   WHEN entity_type = 'contact' THEN (
                     SELECT COALESCE(NULLIF(remark, ''), NULLIF(nickname, ''), NULLIF(alias, ''))
                     FROM identity_contacts WHERE wxid = entity_id
                   )
                   WHEN entity_type = 'chatroom_member' THEN (
                     SELECT COALESCE(NULLIF(display_name, ''), NULLIF(nickname, ''))
                     FROM identity_chatroom_members
                     WHERE chatroom_id = scope_key AND member_wxid = entity_id
                   )
                 END,
                 alias
               ) AS display_name,
               ((CASE WHEN alias_key = ? THEN 1.0 ELSE 0.6 END) * confidence)
                 + CASE WHEN is_current != 0 THEN 0.05 ELSE 0 END AS score
        FROM identity_aliases
        WHERE alias_key = ? OR alias LIKE ? OR entity_id = ?
        ORDER BY score DESC, last_seen_at DESC
        LIMIT ?
        "#,
    )
    .bind(&alias_key)
    .bind(&alias_key)
    .bind(&like)
    .bind(query.q.trim())
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .map(|row| IdentityMatch {
            entity_type: row.get("entity_type"),
            entity_id: row.get("entity_id"),
            chatroom_id: row.get("chatroom_id"),
            display_name: row.get("display_name"),
            alias: row.get("alias"),
            source: row.get("source"),
            is_current: row.get::<i64, _>("is_current") != 0,
            score: row.get("score"),
            last_seen_at: row.get("last_seen_at"),
        })
        .collect();
    Ok(Json(IdentityResolveResponse {
        query: query.q,
        items,
    }))
}

async fn refresh_identity(
    State(state): State<SharedState>,
    Json(request): Json<IdentityRefreshRequest>,
) -> Result<Json<IdentityRefreshResponse>, ApiError> {
    let now = now_iso();
    let mut response = IdentityRefreshResponse {
        ok: true,
        refreshed_chatrooms: 0,
        refreshed_contacts: 0,
        refreshed_members: 0,
        errors: Vec::new(),
    };

    if request.full.unwrap_or(false) {
        match refresh_contacts_list(&state, &now).await {
            Ok((chatroom_ids, contact_count)) => {
                response.refreshed_contacts += contact_count;
                for chatroom_id in chatroom_ids {
                    match refresh_chatroom_from_gewe(&state, &chatroom_id, &now).await {
                        Ok(member_count) => {
                            response.refreshed_chatrooms += 1;
                            response.refreshed_members += member_count;
                        }
                        Err(error) => response.errors.push(json!({
                            "chatroom_id": chatroom_id,
                            "error": error.to_string()
                        })),
                    }
                }
            }
            Err(error) => response.errors.push(json!({
                "scope": "contacts_list",
                "error": error.to_string()
            })),
        }
    }

    if let Some(chatroom_id) = &request.chatroom_id {
        match refresh_chatroom_from_gewe(&state, chatroom_id, &now).await {
            Ok(member_count) => {
                response.refreshed_chatrooms += 1;
                response.refreshed_members += member_count;
            }
            Err(error) => response.errors.push(json!({
                "chatroom_id": chatroom_id,
                "error": error.to_string()
            })),
        }
    }

    if let Some(wxids) = &request.wxids {
        match refresh_contacts_detail(&state, wxids, &now, false).await {
            Ok(count) => response.refreshed_contacts += count,
            Err(error) => response.errors.push(json!({
                "scope": "contacts_detail",
                "error": error.to_string()
            })),
        }
    }

    if request.chatroom_id.is_none() && !request.full.unwrap_or(false) {
        let limit = request.recent_chatrooms.unwrap_or(20).clamp(1, 200);
        let chatroom_ids = recent_chatroom_ids(&state.db, limit).await?;
        for chatroom_id in chatroom_ids {
            match refresh_chatroom_from_gewe(&state, &chatroom_id, &now).await {
                Ok(member_count) => {
                    response.refreshed_chatrooms += 1;
                    response.refreshed_members += member_count;
                }
                Err(error) => response.errors.push(json!({
                    "chatroom_id": chatroom_id,
                    "error": error.to_string()
                })),
            }
        }
    }

    response.ok = response.errors.is_empty();
    Ok(Json(response))
}

async fn recent_attachments(
    State(state): State<SharedState>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<AttachmentRecord>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let rows = sqlx::query(
        r#"
        SELECT id, attachment_json
        FROM attachments
        ORDER BY created_at DESC, id DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .filter_map(|row| {
            let mut record =
                serde_json::from_str::<AttachmentRecord>(row.get::<&str, _>("attachment_json"))
                    .ok()?;
            record.id = Some(row.get("id"));
            Some(record)
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiPage {
        items,
        next_cursor: None,
    }))
}

async fn download_attachment(
    State(state): State<SharedState>,
    Path(sha256): Path<String>,
) -> Result<Response, ApiError> {
    if !sha256.chars().all(|ch| ch.is_ascii_hexdigit()) || sha256.len() != 64 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "invalid_sha256" })),
        )
            .into_response());
    }
    let row = sqlx::query(
        r#"
        SELECT object_key, mime_type
        FROM attachments
        WHERE sha256 = ?
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(&sha256)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "attachment_not_found" })),
        )
            .into_response());
    };
    let object_key: String = row.get("object_key");
    let path = safe_attachment_path(&state.attachment_dir, &object_key);
    let Some(path) = path else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "invalid_object_key" })),
        )
            .into_response());
    };
    let bytes = tokio::fs::read(path).await.map_err(ApiError::Io)?;
    let mime_type: Option<String> = row.get("mime_type");
    Ok((
        [(
            "content-type",
            mime_type.unwrap_or_else(|| "application/octet-stream".to_string()),
        )],
        bytes,
    )
        .into_response())
}

async fn chatroom_snapshots(
    State(state): State<SharedState>,
    Path(chatroom_id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<ChatroomSnapshot>>, ApiError> {
    let rows = query_json_rows(
        &state.db,
        "chatroom_snapshots",
        "snapshot_json",
        &chatroom_id,
        clamp_limit(query.limit),
    )
    .await?;
    Ok(Json(ApiPage {
        items: rows,
        next_cursor: None,
    }))
}

async fn chatroom_events(
    State(state): State<SharedState>,
    Path(chatroom_id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<ChatroomMemberEvent>>, ApiError> {
    let rows = query_json_rows(
        &state.db,
        "chatroom_member_events",
        "event_json",
        &chatroom_id,
        clamp_limit(query.limit),
    )
    .await?;
    Ok(Json(ApiPage {
        items: rows,
        next_cursor: None,
    }))
}

async fn chatroom_system_events(
    State(state): State<SharedState>,
    Path(chatroom_id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<ApiPage<ChatroomSystemEvent>>, ApiError> {
    let rows = query_json_rows(
        &state.db,
        "chatroom_system_events",
        "event_json",
        &chatroom_id,
        clamp_limit(query.limit),
    )
    .await?;
    Ok(Json(ApiPage {
        items: rows,
        next_cursor: None,
    }))
}

async fn query_json_rows<T: serde::de::DeserializeOwned>(
    db: &SqlitePool,
    table: &str,
    json_column: &str,
    chatroom_id: &str,
    limit: i64,
) -> Result<Vec<T>, ApiError> {
    let sql = format!("SELECT {json_column} AS payload FROM {table} WHERE chatroom_id = ? ORDER BY received_at DESC, id DESC LIMIT ?");
    let rows = sqlx::query(&sql)
        .bind(chatroom_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| serde_json::from_str::<T>(row.get::<&str, _>("payload")).ok())
        .collect())
}

async fn recent_chatroom_ids(db: &SqlitePool, limit: i64) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT conversation_id
        FROM messages
        WHERE is_group != 0 AND conversation_id IS NOT NULL
        GROUP BY conversation_id
        ORDER BY MAX(received_at) DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|row| row.get("conversation_id")).collect())
}

async fn refresh_contacts_list(
    state: &SharedState,
    seen_at: &str,
) -> Result<(Vec<String>, i64), ApiError> {
    let data = gewe_post(state, "/gewe/v2/api/contacts/fetchContactsList", json!({})).await?;
    let data = if data.is_object() {
        data
    } else {
        gewe_post(
            state,
            "/gewe/v2/api/contacts/fetchContactsListCache",
            json!({}),
        )
        .await?
    };
    let friends = data
        .get("friends")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let chatrooms = data
        .get("chatrooms")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut contact_count = 0;
    for chunk in friends.chunks(50) {
        contact_count += refresh_contacts_detail(state, chunk, seen_at, true).await?;
    }
    Ok((chatrooms, contact_count))
}

async fn refresh_contacts_detail(
    state: &SharedState,
    wxids: &[String],
    seen_at: &str,
    brief: bool,
) -> Result<i64, ApiError> {
    if wxids.is_empty() {
        return Ok(0);
    }
    let path = if brief {
        "/gewe/v2/api/contacts/getBriefInfo"
    } else {
        "/gewe/v2/api/contacts/getDetailInfo"
    };
    let data = gewe_post(state, path, json!({ "wxids": wxids })).await?;
    let items = data.as_array().cloned().unwrap_or_default();
    let mut tx = state.db.begin().await?;
    let mut count = 0;
    for item in items {
        if apply_gewe_contact(&mut tx, &item, seen_at).await? {
            count += 1;
        }
    }
    tx.commit().await?;
    Ok(count)
}

async fn refresh_chatroom_from_gewe(
    state: &SharedState,
    chatroom_id: &str,
    seen_at: &str,
) -> Result<i64, ApiError> {
    let data = gewe_post(
        state,
        "/gewe/v2/api/group/getChatroomInfo",
        json!({ "chatroomId": chatroom_id }),
    )
    .await?;
    let mut tx = state.db.begin().await?;
    let member_count = apply_gewe_chatroom_info(&mut tx, &data, seen_at).await?;
    tx.commit().await?;
    Ok(member_count)
}

async fn gewe_post(state: &SharedState, path: &str, body: Value) -> Result<Value, ApiError> {
    let token = state.gewe_token.as_deref().ok_or(ApiError::GeweConfig(
        "missing GEWE_SKILL_GEWE_TOKEN or GEWE_TOKEN",
    ))?;
    let app_id = state.gewe_app_id.as_deref().ok_or(ApiError::GeweConfig(
        "missing GEWE_SKILL_GEWE_APP_ID or GEWE_APP_ID",
    ))?;
    let mut payload = match body {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    payload.insert("appId".to_string(), Value::String(app_id.to_string()));
    let url = format!(
        "{}/{}",
        state.gewe_base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let value: Value = state
        .http
        .post(url)
        .header("X-GEWE-TOKEN", token)
        .json(&payload)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ret = value.get("ret").and_then(Value::as_i64);
    if ret != Some(200) {
        return Err(ApiError::Gewe(value.to_string()));
    }
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}

async fn project_identity_from_ingest(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &IngestEventRequest,
) -> Result<(), ApiError> {
    project_message_identity(tx, &request.message).await?;
    if let Some(snapshot) = &request.chatroom_snapshot {
        project_chatroom_snapshot(tx, snapshot).await?;
    }
    for event in &request.chatroom_member_events {
        project_chatroom_member_event(tx, event).await?;
    }
    for event in &request.chatroom_system_events {
        project_chatroom_system_event(tx, event).await?;
    }
    Ok(())
}

async fn project_message_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message: &NormalizedMessage,
) -> Result<(), ApiError> {
    let Some(conversation_id) = &message.conversation_id else {
        return Ok(());
    };
    if message.is_group {
        upsert_chatroom(
            tx,
            conversation_id,
            None,
            None,
            None,
            None,
            None,
            &message.received_at,
            "message",
        )
        .await?;
        if let Some(sender_wxid) = &message.sender_wxid {
            let display_name = sender_display_from_push_content(message);
            upsert_chatroom_member(
                tx,
                conversation_id,
                sender_wxid,
                display_name.as_deref(),
                None,
                None,
                None,
                None,
                true,
                &message.received_at,
                "message",
            )
            .await?;
        }
    } else {
        upsert_contact(
            tx,
            conversation_id,
            None,
            None,
            None,
            None,
            &message.received_at,
            "message",
        )
        .await?;
    }
    Ok(())
}

async fn project_chatroom_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    snapshot: &ChatroomSnapshot,
) -> Result<(), ApiError> {
    let raw = serde_json::to_value(snapshot)?;
    upsert_chatroom(
        tx,
        &snapshot.chatroom_id,
        snapshot.chatroom_name.as_deref(),
        None,
        None,
        Some(snapshot.member_count),
        Some(&raw),
        &snapshot.received_at,
        "chatroom_snapshot",
    )
    .await?;
    sqlx::query("UPDATE identity_chatroom_members SET is_current = 0 WHERE chatroom_id = ?")
        .bind(&snapshot.chatroom_id)
        .execute(&mut **tx)
        .await?;
    for member in &snapshot.members {
        upsert_chatroom_member(
            tx,
            &snapshot.chatroom_id,
            &member.wxid,
            member.display_name.as_deref(),
            None,
            None,
            member.flag,
            Some(&serde_json::to_value(member)?),
            true,
            &snapshot.received_at,
            "chatroom_snapshot",
        )
        .await?;
    }
    Ok(())
}

async fn project_chatroom_member_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: &ChatroomMemberEvent,
) -> Result<(), ApiError> {
    if let Some(name) = &event.current_chatroom_name {
        upsert_chatroom(
            tx,
            &event.chatroom_id,
            Some(name),
            None,
            None,
            event.current_member_count,
            None,
            &event.received_at,
            "chatroom_member_event",
        )
        .await?;
    }
    if let Some(member_wxid) = &event.member_wxid {
        let member = event
            .details
            .get("member")
            .and_then(|value| serde_json::from_value::<ChatroomMember>(value.clone()).ok());
        let member_json = member
            .as_ref()
            .and_then(|item| serde_json::to_value(item).ok());
        let is_current = !matches!(
            event.event_type,
            ChatroomEventType::MemberLeft | ChatroomEventType::MemberRemoved
        );
        upsert_chatroom_member(
            tx,
            &event.chatroom_id,
            member_wxid,
            member
                .as_ref()
                .and_then(|item| item.display_name.as_deref()),
            None,
            None,
            member.as_ref().and_then(|item| item.flag),
            member_json.as_ref(),
            is_current,
            &event.received_at,
            "chatroom_member_event",
        )
        .await?;
    }
    Ok(())
}

async fn project_chatroom_system_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: &ChatroomSystemEvent,
) -> Result<(), ApiError> {
    if matches!(event.event_type, ChatroomEventType::ChatroomNameChanged) {
        upsert_chatroom(
            tx,
            &event.chatroom_id,
            event.current_value.as_deref(),
            None,
            None,
            None,
            Some(&event.details),
            &event.received_at,
            "chatroom_system_event",
        )
        .await?;
    }
    if let Some(actor_wxid) = &event.actor_wxid {
        upsert_chatroom_member(
            tx,
            &event.chatroom_id,
            actor_wxid,
            event.actor_name.as_deref(),
            None,
            None,
            None,
            Some(&event.details),
            true,
            &event.received_at,
            "chatroom_system_event",
        )
        .await?;
    }
    for (idx, target_wxid) in event.target_wxids.iter().enumerate() {
        let target_name = event.target_names.get(idx).map(String::as_str);
        let is_current = !matches!(
            event.event_type,
            ChatroomEventType::MemberLeft | ChatroomEventType::MemberRemoved
        );
        upsert_chatroom_member(
            tx,
            &event.chatroom_id,
            target_wxid,
            target_name,
            None,
            None,
            None,
            Some(&event.details),
            is_current,
            &event.received_at,
            "chatroom_system_event",
        )
        .await?;
    }
    Ok(())
}

async fn apply_gewe_contact(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    value: &Value,
    seen_at: &str,
) -> Result<bool, ApiError> {
    let Some(wxid) = json_text(value, "userName") else {
        return Ok(false);
    };
    upsert_contact(
        tx,
        &wxid,
        json_text(value, "nickName").as_deref(),
        json_text(value, "remark").as_deref(),
        json_text(value, "alias").as_deref(),
        Some(value),
        seen_at,
        "gewe_contact_info",
    )
    .await?;
    Ok(true)
}

async fn apply_gewe_chatroom_info(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    value: &Value,
    seen_at: &str,
) -> Result<i64, ApiError> {
    let Some(chatroom_id) = json_text(value, "chatroomId") else {
        return Ok(0);
    };
    let member_list = value
        .get("memberList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let display_name = json_text(value, "remark").or_else(|| json_text(value, "nickName"));
    upsert_chatroom(
        tx,
        &chatroom_id,
        display_name.as_deref(),
        json_text(value, "remark").as_deref(),
        json_text(value, "chatRoomOwner").as_deref(),
        Some(member_list.len() as i64),
        Some(value),
        seen_at,
        "gewe_chatroom_info",
    )
    .await?;
    if let Some(nick_name) = json_text(value, "nickName") {
        upsert_alias(
            tx,
            &nick_name,
            "chatroom",
            &chatroom_id,
            None,
            "gewe_chatroom_nickname",
            true,
            0.95,
            seen_at,
        )
        .await?;
    }
    sqlx::query("UPDATE identity_chatroom_members SET is_current = 0 WHERE chatroom_id = ?")
        .bind(&chatroom_id)
        .execute(&mut **tx)
        .await?;
    for member in &member_list {
        let Some(wxid) = json_text(member, "wxid") else {
            continue;
        };
        upsert_chatroom_member(
            tx,
            &chatroom_id,
            &wxid,
            json_text(member, "displayName").as_deref(),
            json_text(member, "nickName").as_deref(),
            json_text(member, "inviterUserName").as_deref(),
            json_i64(member, "memberFlag"),
            Some(member),
            true,
            seen_at,
            "gewe_chatroom_info",
        )
        .await?;
    }
    Ok(member_list.len() as i64)
}

async fn insert_raw_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &IngestEventRequest,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO raw_events (dedupe_key, schema_version, appid, account_wxid, received_at, body_sha256, body_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(dedupe_key) DO UPDATE SET duplicate_count = duplicate_count + 1, last_seen_at = excluded.received_at
        "#,
    )
    .bind(&request.raw_event.dedupe_key)
    .bind(format!("{:?}", request.raw_event.schema_version))
    .bind(&request.raw_event.appid)
    .bind(&request.raw_event.account_wxid)
    .bind(&request.raw_event.received_at)
    .bind(&request.raw_event.body_sha256)
    .bind(serde_json::to_string(&request.raw_event)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_message(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message: &NormalizedMessage,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO messages (message_key, appid, account_wxid, conversation_id, sender_wxid, kind, is_group, is_outgoing, received_at, content_text, message_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(message_key) DO UPDATE SET duplicate_count = duplicate_count + 1, last_seen_at = excluded.received_at
        "#,
    )
    .bind(&message.message_key)
    .bind(&message.appid)
    .bind(&message.account_wxid)
    .bind(&message.conversation_id)
    .bind(&message.sender_wxid)
    .bind(format!("{:?}", message.kind))
    .bind(i64::from(message.is_group))
    .bind(i64::from(message.is_outgoing))
    .bind(&message.received_at)
    .bind(&message.content_text)
    .bind(serde_json::to_string(message)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_chatroom_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message_key: &str,
    snapshot: &ChatroomSnapshot,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO chatroom_snapshots (message_key, chatroom_id, chatroom_name, member_count, member_hash, received_at, snapshot_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(message_key) DO UPDATE SET snapshot_json = excluded.snapshot_json
        "#,
    )
    .bind(message_key)
    .bind(&snapshot.chatroom_id)
    .bind(&snapshot.chatroom_name)
    .bind(snapshot.member_count)
    .bind(&snapshot.member_hash)
    .bind(&snapshot.received_at)
    .bind(serde_json::to_string(snapshot)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_chatroom_member_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: &ChatroomMemberEvent,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO chatroom_member_events (event_key, event_type, chatroom_id, member_wxid, received_at, event_json)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_key) DO UPDATE SET event_json = excluded.event_json
        "#,
    )
    .bind(format!("{}:{}:{}", event.chatroom_id, event.received_at, event.member_wxid.as_deref().unwrap_or("chatroom")))
    .bind(format!("{:?}", event.event_type))
    .bind(&event.chatroom_id)
    .bind(&event.member_wxid)
    .bind(&event.received_at)
    .bind(serde_json::to_string(event)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_chatroom_system_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: &ChatroomSystemEvent,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO chatroom_system_events (event_key, event_type, chatroom_id, actor_wxid, target_wxid, received_at, event_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_key) DO UPDATE SET event_json = excluded.event_json
        "#,
    )
    .bind(format!("{}:{}:{}", event.chatroom_id, event.received_at, event.content_text.as_deref().unwrap_or("system")))
    .bind(format!("{:?}", event.event_type))
    .bind(&event.chatroom_id)
    .bind(&event.actor_wxid)
    .bind(&event.target_wxid)
    .bind(&event.received_at)
    .bind(serde_json::to_string(event)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upsert_contact(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wxid: &str,
    nickname: Option<&str>,
    remark: Option<&str>,
    alias: Option<&str>,
    raw_json: Option<&Value>,
    seen_at: &str,
    source: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO identity_contacts (wxid, nickname, remark, alias, raw_json, last_seen_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(wxid) DO UPDATE SET
          nickname = COALESCE(NULLIF(excluded.nickname, ''), identity_contacts.nickname),
          remark = COALESCE(NULLIF(excluded.remark, ''), identity_contacts.remark),
          alias = COALESCE(NULLIF(excluded.alias, ''), identity_contacts.alias),
          raw_json = COALESCE(excluded.raw_json, identity_contacts.raw_json),
          last_seen_at = excluded.last_seen_at,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(wxid)
    .bind(nickname)
    .bind(remark)
    .bind(alias)
    .bind(raw_json.map(Value::to_string))
    .bind(seen_at)
    .bind(seen_at)
    .execute(&mut **tx)
    .await?;
    for (value, alias_source, confidence) in [
        (remark, "contact_remark", 0.95),
        (nickname, "contact_nickname", 0.85),
        (alias, "contact_alias", 0.75),
    ] {
        if let Some(value) = value {
            upsert_alias(
                tx,
                value,
                "contact",
                wxid,
                None,
                &format!("{source}:{alias_source}"),
                true,
                confidence,
                seen_at,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upsert_chatroom(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chatroom_id: &str,
    display_name: Option<&str>,
    remark: Option<&str>,
    owner_wxid: Option<&str>,
    member_count: Option<i64>,
    raw_json: Option<&Value>,
    seen_at: &str,
    source: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO identity_chatrooms (
          chatroom_id, display_name, remark, owner_wxid, member_count, raw_json, last_seen_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(chatroom_id) DO UPDATE SET
          display_name = COALESCE(NULLIF(excluded.display_name, ''), identity_chatrooms.display_name),
          remark = COALESCE(NULLIF(excluded.remark, ''), identity_chatrooms.remark),
          owner_wxid = COALESCE(NULLIF(excluded.owner_wxid, ''), identity_chatrooms.owner_wxid),
          member_count = COALESCE(excluded.member_count, identity_chatrooms.member_count),
          raw_json = COALESCE(excluded.raw_json, identity_chatrooms.raw_json),
          last_seen_at = excluded.last_seen_at,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(chatroom_id)
    .bind(display_name)
    .bind(remark)
    .bind(owner_wxid)
    .bind(member_count)
    .bind(raw_json.map(Value::to_string))
    .bind(seen_at)
    .bind(seen_at)
    .execute(&mut **tx)
    .await?;
    if let Some(value) = display_name {
        upsert_alias(
            tx,
            value,
            "chatroom",
            chatroom_id,
            None,
            source,
            true,
            0.95,
            seen_at,
        )
        .await?;
    }
    if let Some(value) = remark {
        upsert_alias(
            tx,
            value,
            "chatroom",
            chatroom_id,
            None,
            &format!("{source}:remark"),
            true,
            0.9,
            seen_at,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upsert_chatroom_member(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chatroom_id: &str,
    member_wxid: &str,
    display_name: Option<&str>,
    nickname: Option<&str>,
    inviter_wxid: Option<&str>,
    member_flag: Option<i64>,
    raw_json: Option<&Value>,
    is_current: bool,
    seen_at: &str,
    source: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO identity_chatroom_members (
          chatroom_id, member_wxid, display_name, nickname, inviter_wxid, member_flag,
          is_current, first_seen_at, last_seen_at, raw_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(chatroom_id, member_wxid) DO UPDATE SET
          display_name = COALESCE(NULLIF(excluded.display_name, ''), identity_chatroom_members.display_name),
          nickname = COALESCE(NULLIF(excluded.nickname, ''), identity_chatroom_members.nickname),
          inviter_wxid = COALESCE(NULLIF(excluded.inviter_wxid, ''), identity_chatroom_members.inviter_wxid),
          member_flag = COALESCE(excluded.member_flag, identity_chatroom_members.member_flag),
          is_current = excluded.is_current,
          last_seen_at = excluded.last_seen_at,
          raw_json = COALESCE(excluded.raw_json, identity_chatroom_members.raw_json)
        "#,
    )
    .bind(chatroom_id)
    .bind(member_wxid)
    .bind(display_name)
    .bind(nickname)
    .bind(inviter_wxid)
    .bind(member_flag)
    .bind(i64::from(is_current))
    .bind(seen_at)
    .bind(seen_at)
    .bind(raw_json.map(Value::to_string))
    .execute(&mut **tx)
    .await?;
    for (value, alias_source, confidence) in [
        (display_name, "member_display_name", 0.95),
        (nickname, "member_nickname", 0.8),
    ] {
        if let Some(value) = value {
            upsert_alias(
                tx,
                value,
                "chatroom_member",
                member_wxid,
                Some(chatroom_id),
                &format!("{source}:{alias_source}"),
                is_current,
                confidence,
                seen_at,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upsert_alias(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    alias: &str,
    entity_type: &str,
    entity_id: &str,
    chatroom_id: Option<&str>,
    source: &str,
    is_current: bool,
    confidence: f64,
    seen_at: &str,
) -> Result<(), ApiError> {
    let alias = alias.trim();
    let alias_key = normalize_alias(alias);
    if alias_key.is_empty() {
        return Ok(());
    }
    let scope_key = chatroom_id.unwrap_or("");
    sqlx::query(
        r#"
        INSERT INTO identity_aliases (
          alias_key, alias, entity_type, entity_id, scope_key, source,
          is_current, confidence, first_seen_at, last_seen_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(alias_key, entity_type, entity_id, scope_key, source) DO UPDATE SET
          alias = excluded.alias,
          is_current = excluded.is_current,
          confidence = MAX(identity_aliases.confidence, excluded.confidence),
          last_seen_at = excluded.last_seen_at
        "#,
    )
    .bind(alias_key)
    .bind(alias)
    .bind(entity_type)
    .bind(entity_id)
    .bind(scope_key)
    .bind(source)
    .bind(i64::from(is_current))
    .bind(confidence)
    .bind(seen_at)
    .bind(seen_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_attachment(db: &SqlitePool, record: &AttachmentRecord) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO attachments (
          edge_job_id, job_key, message_key, raw_event_dedupe_key, appid, account_wxid,
          kind, variant, object_key, sha256, size_bytes, mime_type, source_url,
          created_at, attachment_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(sha256) DO UPDATE SET
          edge_job_id = COALESCE(excluded.edge_job_id, attachments.edge_job_id),
          job_key = COALESCE(excluded.job_key, attachments.job_key),
          message_key = excluded.message_key,
          raw_event_dedupe_key = excluded.raw_event_dedupe_key,
          attachment_json = excluded.attachment_json
        "#,
    )
    .bind(record.edge_job_id)
    .bind(&record.job_key)
    .bind(&record.message_key)
    .bind(&record.raw_event_dedupe_key)
    .bind(&record.appid)
    .bind(&record.account_wxid)
    .bind(format!("{:?}", record.kind))
    .bind(&record.variant)
    .bind(&record.object_key)
    .bind(&record.sha256)
    .bind(record.size_bytes)
    .bind(&record.mime_type)
    .bind(&record.source_url)
    .bind(&record.created_at)
    .bind(serde_json::to_string(record)?)
    .execute(db)
    .await?;
    Ok(())
}

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

fn normalize_alias(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<String>()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn json_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|item| match item {
            Value::String(text) => Some(text.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|item| match item {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    })
}

fn sender_display_from_push_content(message: &NormalizedMessage) -> Option<String> {
    let text = message.push_content.as_deref()?;
    let (name, _) = text.split_once(':').or_else(|| text.split_once('：'))?;
    let name = name.trim();
    (!name.is_empty()).then_some(name.to_string())
}

async fn init_db(db: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA journal_mode = WAL").execute(db).await?;
    sqlx::query("PRAGMA foreign_keys = ON").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS raw_events (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          dedupe_key TEXT NOT NULL UNIQUE,
          schema_version TEXT NOT NULL,
          appid TEXT NOT NULL,
          account_wxid TEXT,
          received_at TEXT NOT NULL,
          body_sha256 TEXT NOT NULL,
          body_json TEXT NOT NULL,
          duplicate_count INTEGER NOT NULL DEFAULT 0,
          last_seen_at TEXT,
          created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS messages (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          message_key TEXT NOT NULL UNIQUE,
          appid TEXT NOT NULL,
          account_wxid TEXT,
          conversation_id TEXT,
          sender_wxid TEXT,
          kind TEXT NOT NULL,
          is_group INTEGER NOT NULL DEFAULT 0,
          is_outgoing INTEGER NOT NULL DEFAULT 0,
          received_at TEXT NOT NULL,
          content_text TEXT,
          message_json TEXT NOT NULL,
          duplicate_count INTEGER NOT NULL DEFAULT 0,
          last_seen_at TEXT,
          created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_conversation_received ON messages(conversation_id, received_at DESC)").execute(db).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_received ON messages(received_at DESC)")
        .execute(db)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chatroom_snapshots (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          message_key TEXT NOT NULL UNIQUE,
          chatroom_id TEXT NOT NULL,
          chatroom_name TEXT,
          member_count INTEGER NOT NULL,
          member_hash TEXT NOT NULL,
          received_at TEXT NOT NULL,
          snapshot_json TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_chatroom_snapshots_chatroom_received ON chatroom_snapshots(chatroom_id, received_at DESC)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chatroom_member_events (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          event_key TEXT NOT NULL UNIQUE,
          event_type TEXT NOT NULL,
          chatroom_id TEXT NOT NULL,
          member_wxid TEXT,
          received_at TEXT NOT NULL,
          event_json TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_chatroom_member_events_chatroom_received ON chatroom_member_events(chatroom_id, received_at DESC)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chatroom_system_events (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          event_key TEXT NOT NULL UNIQUE,
          event_type TEXT NOT NULL,
          chatroom_id TEXT NOT NULL,
          actor_wxid TEXT,
          target_wxid TEXT,
          received_at TEXT NOT NULL,
          event_json TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_chatroom_system_events_chatroom_received ON chatroom_system_events(chatroom_id, received_at DESC)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS identity_contacts (
          wxid TEXT PRIMARY KEY,
          nickname TEXT,
          remark TEXT,
          alias TEXT,
          raw_json TEXT,
          last_seen_at TEXT,
          updated_at TEXT
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS identity_chatrooms (
          chatroom_id TEXT PRIMARY KEY,
          display_name TEXT,
          remark TEXT,
          owner_wxid TEXT,
          member_count INTEGER,
          raw_json TEXT,
          last_seen_at TEXT,
          updated_at TEXT
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_identity_chatrooms_seen ON identity_chatrooms(last_seen_at DESC)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS identity_chatroom_members (
          chatroom_id TEXT NOT NULL,
          member_wxid TEXT NOT NULL,
          display_name TEXT,
          nickname TEXT,
          inviter_wxid TEXT,
          member_flag INTEGER,
          is_current INTEGER NOT NULL DEFAULT 1,
          first_seen_at TEXT,
          last_seen_at TEXT,
          raw_json TEXT,
          PRIMARY KEY(chatroom_id, member_wxid)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_identity_members_member ON identity_chatroom_members(member_wxid, last_seen_at DESC)").execute(db).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_identity_members_chatroom ON identity_chatroom_members(chatroom_id, is_current)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS identity_aliases (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          alias_key TEXT NOT NULL,
          alias TEXT NOT NULL,
          entity_type TEXT NOT NULL,
          entity_id TEXT NOT NULL,
          scope_key TEXT NOT NULL DEFAULT '',
          source TEXT NOT NULL,
          is_current INTEGER NOT NULL DEFAULT 1,
          confidence REAL NOT NULL DEFAULT 0.6,
          first_seen_at TEXT,
          last_seen_at TEXT,
          UNIQUE(alias_key, entity_type, entity_id, scope_key, source)
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_identity_aliases_key ON identity_aliases(alias_key, is_current, confidence)").execute(db).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_identity_aliases_entity ON identity_aliases(entity_type, entity_id)").execute(db).await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS attachments (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          edge_job_id INTEGER UNIQUE,
          job_key TEXT,
          message_key TEXT NOT NULL,
          raw_event_dedupe_key TEXT NOT NULL,
          appid TEXT NOT NULL,
          account_wxid TEXT,
          kind TEXT NOT NULL,
          variant TEXT,
          object_key TEXT NOT NULL,
          sha256 TEXT NOT NULL UNIQUE,
          size_bytes INTEGER,
          mime_type TEXT,
          source_url TEXT,
          created_at TEXT NOT NULL,
          attachment_json TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_key)")
        .execute(db)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_attachments_created ON attachments(created_at DESC)",
    )
    .execute(db)
    .await?;
    Ok(())
}

fn safe_attachment_path(root: &FsPath, object_key: &str) -> Option<PathBuf> {
    let relative = FsPath::new(object_key);
    if relative.is_absolute() || object_key.contains("..") {
        return None;
    }
    Some(root.join(relative))
}

#[derive(Debug)]
enum ApiError {
    Sqlx(sqlx::Error),
    Serde(serde_json::Error),
    Core(gewe_skill_core::CoreError),
    Http(reqwest::Error),
    GeweConfig(&'static str),
    Gewe(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Sqlx(error) => error.to_string(),
            Self::Serde(error) => error.to_string(),
            Self::Core(error) => error.to_string(),
            Self::Http(error) => error.to_string(),
            Self::GeweConfig(error) => error.to_string(),
            Self::Gewe(error) => error.clone(),
            Self::Io(error) => error.to_string(),
        };
        formatter.write_str(&message)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sqlx(error)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serde(error)
    }
}

impl From<gewe_skill_core::CoreError> for ApiError {
    fn from(error: gewe_skill_core::CoreError) -> Self {
        Self::Core(error)
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match self {
            Self::Sqlx(error) => error.to_string(),
            Self::Serde(error) => error.to_string(),
            Self::Core(error) => error.to_string(),
            Self::Http(error) => error.to_string(),
            Self::GeweConfig(error) => error.to_string(),
            Self::Gewe(error) => error,
            Self::Io(error) => error.to_string(),
        };
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": message })),
        )
            .into_response()
    }
}
