use axum::{
    extract::{Path, Query, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use gewe_skill_core::{diff_chatroom_snapshots, normalize_callback};
use gewe_skill_types::{ApiPage, AttachmentRecord, ChatroomMemberEvent, ChatroomSnapshot, ChatroomSystemEvent, ConversationSummary, IngestEventRequest, NormalizedMessage, RawCallbackRequest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::{env, net::SocketAddr, path::{Path as FsPath, PathBuf}, sync::Arc};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    attachment_dir: PathBuf,
    read_token: Option<String>,
    write_token: Option<String>,
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
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let database_url = env::var("GEWE_SKILL_DATABASE_URL").unwrap_or_else(|_| "sqlite:/opt/gewe-skill-memory/data/gewe-skill-memory.sqlite?mode=rwc".to_string());
    let listen = env::var("GEWE_SKILL_LISTEN").unwrap_or_else(|_| "127.0.0.1:8788".to_string());
    let attachment_dir = PathBuf::from(env::var("GEWE_SKILL_ATTACHMENT_DIR").unwrap_or_else(|_| "/opt/gewe-skill-memory/data/attachments".to_string()));
    ensure_sqlite_parent(&database_url)?;
    std::fs::create_dir_all(&attachment_dir)?;
    let db = SqlitePoolOptions::new().max_connections(8).connect(&database_url).await?;
    init_db(&db).await?;

    let state = Arc::new(AppState {
        db,
        attachment_dir,
        read_token: env::var("GEWE_SKILL_READ_TOKEN").ok(),
        write_token: env::var("GEWE_SKILL_WRITE_TOKEN").ok(),
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/write/events", post(write_event).route_layer(middleware::from_fn_with_state(state.clone(), require_write_token)))
        .route("/write/raw-events", post(write_raw_event).route_layer(middleware::from_fn_with_state(state.clone(), require_write_token)))
        .route("/write/attachments", post(write_attachment).route_layer(middleware::from_fn_with_state(state.clone(), require_write_token)))
        .route("/api/messages/recent", get(recent_messages).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/messages/search", get(search_messages).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/conversations", get(conversations).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/attachments/recent", get(recent_attachments).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/attachments/{sha256}/download", get(download_attachment).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/chatrooms/{chatroom_id}/snapshots", get(chatroom_snapshots).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/chatrooms/{chatroom_id}/events", get(chatroom_events).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .route("/api/chatrooms/{chatroom_id}/system-events", get(chatroom_system_events).route_layer(middleware::from_fn_with_state(state.clone(), require_read_token)))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = listen.parse()?;
    info!(%addr, "starting gewe-skill-memory");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
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

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to listen for shutdown signal");
    }
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true, service: "gewe-skill-memory" })
}

async fn require_write_token(State(state): State<SharedState>, headers: HeaderMap, request: axum::extract::Request, next: Next) -> Response {
    authorize(headers, state.write_token.as_deref(), request, next).await
}

async fn require_read_token(State(state): State<SharedState>, headers: HeaderMap, request: axum::extract::Request, next: Next) -> Response {
    authorize(headers, state.read_token.as_deref().or(state.write_token.as_deref()), request, next).await
}

async fn authorize(headers: HeaderMap, expected: Option<&str>, request: axum::extract::Request, next: Next) -> Response {
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
    (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error": "unauthorized" }))).into_response()
}

async fn write_event(State(state): State<SharedState>, Json(request): Json<IngestEventRequest>) -> Result<Json<IngestResponse>, ApiError> {
    write_ingest_request(&state.db, request).await
}

async fn write_raw_event(State(state): State<SharedState>, Json(request): Json<RawCallbackRequest>) -> Result<Json<IngestResponse>, ApiError> {
    let normalized = normalize_callback(&request.body, request.received_at)?;
    let mut ingest = normalized.into_ingest_request();
    if let Some(current) = &ingest.chatroom_snapshot {
        if let Some(previous) = latest_chatroom_snapshot(&state.db, &current.chatroom_id).await? {
            ingest.chatroom_member_events = diff_chatroom_snapshots(&previous, current);
        }
    }
    write_ingest_request(&state.db, ingest).await
}

async fn write_attachment(State(state): State<SharedState>, Json(record): Json<AttachmentRecord>) -> Result<Json<serde_json::Value>, ApiError> {
    insert_attachment(&state.db, &record).await?;
    Ok(Json(json!({ "ok": true, "sha256": record.sha256 })))
}

async fn write_ingest_request(db: &SqlitePool, request: IngestEventRequest) -> Result<Json<IngestResponse>, ApiError> {
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
    tx.commit().await?;

    Ok(Json(IngestResponse { ok: true, message_key: request.message.message_key }))
}

async fn latest_chatroom_snapshot(db: &SqlitePool, chatroom_id: &str) -> Result<Option<ChatroomSnapshot>, ApiError> {
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

async fn recent_messages(State(state): State<SharedState>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let cursor = query.cursor.unwrap_or_else(|| "9999-12-31T23:59:59.999Z".to_string());
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
        .filter_map(|row| serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok())
        .collect::<Vec<_>>();
    let next_cursor = items.last().map(|message| message.received_at.clone());
    Ok(Json(ApiPage { items, next_cursor }))
}

async fn search_messages(State(state): State<SharedState>, Query(query): Query<SearchQuery>) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
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
        .filter_map(|row| serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok())
        .collect::<Vec<_>>();
    let next_cursor = items.last().map(|message| message.received_at.clone());
    Ok(Json(ApiPage { items, next_cursor }))
}

async fn conversations(State(state): State<SharedState>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<ConversationSummary>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let rows = sqlx::query(
        r#"
        SELECT conversation_id, MAX(is_group) AS is_group, MAX(received_at) AS last_message_at,
               COUNT(*) AS message_count,
               SUBSTR((SELECT content_text FROM messages m2 WHERE m2.conversation_id = messages.conversation_id ORDER BY received_at DESC, id DESC LIMIT 1), 1, 160) AS preview
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
            display_name: None,
            is_group: row.get::<i64, _>("is_group") != 0,
            last_message_at: row.get("last_message_at"),
            last_message_preview: row.get("preview"),
            message_count: row.get("message_count"),
        })
        .collect();
    Ok(Json(ApiPage { items, next_cursor: None }))
}

async fn recent_attachments(State(state): State<SharedState>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<AttachmentRecord>>, ApiError> {
    let limit = clamp_limit(query.limit);
    let rows = sqlx::query(
        r#"
        SELECT attachment_json
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
        .filter_map(|row| serde_json::from_str::<AttachmentRecord>(row.get::<&str, _>("attachment_json")).ok())
        .collect::<Vec<_>>();
    Ok(Json(ApiPage { items, next_cursor: None }))
}

async fn download_attachment(State(state): State<SharedState>, Path(sha256): Path<String>) -> Result<Response, ApiError> {
    if !sha256.chars().all(|ch| ch.is_ascii_hexdigit()) || sha256.len() != 64 {
        return Ok((StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "invalid_sha256" }))).into_response());
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
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "attachment_not_found" }))).into_response());
    };
    let object_key: String = row.get("object_key");
    let path = safe_attachment_path(&state.attachment_dir, &object_key);
    let Some(path) = path else {
        return Ok((StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "invalid_object_key" }))).into_response());
    };
    let bytes = tokio::fs::read(path).await.map_err(ApiError::Io)?;
    let mime_type: Option<String> = row.get("mime_type");
    Ok((
        [("content-type", mime_type.unwrap_or_else(|| "application/octet-stream".to_string()))],
        bytes,
    ).into_response())
}

async fn chatroom_snapshots(State(state): State<SharedState>, Path(chatroom_id): Path<String>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<ChatroomSnapshot>>, ApiError> {
    let rows = query_json_rows(&state.db, "chatroom_snapshots", "snapshot_json", &chatroom_id, clamp_limit(query.limit)).await?;
    Ok(Json(ApiPage { items: rows, next_cursor: None }))
}

async fn chatroom_events(State(state): State<SharedState>, Path(chatroom_id): Path<String>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<ChatroomMemberEvent>>, ApiError> {
    let rows = query_json_rows(&state.db, "chatroom_member_events", "event_json", &chatroom_id, clamp_limit(query.limit)).await?;
    Ok(Json(ApiPage { items: rows, next_cursor: None }))
}

async fn chatroom_system_events(State(state): State<SharedState>, Path(chatroom_id): Path<String>, Query(query): Query<LimitQuery>) -> Result<Json<ApiPage<ChatroomSystemEvent>>, ApiError> {
    let rows = query_json_rows(&state.db, "chatroom_system_events", "event_json", &chatroom_id, clamp_limit(query.limit)).await?;
    Ok(Json(ApiPage { items: rows, next_cursor: None }))
}

async fn query_json_rows<T: serde::de::DeserializeOwned>(db: &SqlitePool, table: &str, json_column: &str, chatroom_id: &str, limit: i64) -> Result<Vec<T>, ApiError> {
    let sql = format!("SELECT {json_column} AS payload FROM {table} WHERE chatroom_id = ? ORDER BY received_at DESC, id DESC LIMIT ?");
    let rows = sqlx::query(&sql).bind(chatroom_id).bind(limit).fetch_all(db).await?;
    Ok(rows
        .iter()
        .filter_map(|row| serde_json::from_str::<T>(row.get::<&str, _>("payload")).ok())
        .collect())
}

async fn insert_raw_event(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, request: &IngestEventRequest) -> Result<(), ApiError> {
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

async fn insert_message(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, message: &NormalizedMessage) -> Result<(), ApiError> {
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

async fn insert_chatroom_snapshot(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, message_key: &str, snapshot: &ChatroomSnapshot) -> Result<(), ApiError> {
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

async fn insert_chatroom_member_event(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, event: &ChatroomMemberEvent) -> Result<(), ApiError> {
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

async fn insert_chatroom_system_event(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, event: &ChatroomSystemEvent) -> Result<(), ApiError> {
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
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_received ON messages(received_at DESC)").execute(db).await?;
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
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_key)").execute(db).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_attachments_created ON attachments(created_at DESC)").execute(db).await?;
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
    Io(std::io::Error),
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
            Self::Io(error) => error.to_string(),
        };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": message }))).into_response()
    }
}
