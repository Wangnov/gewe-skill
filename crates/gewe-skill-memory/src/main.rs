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
    ApiPage, AttachmentKind, AttachmentRecord, ChatroomEventType, ChatroomEventWriteRequest,
    ChatroomEventWriteResponse, ChatroomMember, ChatroomMemberEvent, ChatroomSnapshot,
    ChatroomSystemEvent, ConversationSummary, IdentityChatroomMemberProfile,
    IdentityContactProfile, IdentityMatch, IdentityProfileResponse, IdentityRefreshRequest,
    IdentityRefreshResponse, IdentityResolveResponse, IngestEventRequest, MessageContextResponse,
    MessageQuery, NormalizedMessage, RawCallbackRequest, VoiceItem, VoiceQuery,
    VoiceTranscribeRequest, VoiceTranscribeResponse, VoiceTranscriptRecord, VoiceWarmRequest,
    VoiceWarmResponse,
};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::{
    collections::BTreeSet,
    env,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
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

#[derive(Debug, Deserialize)]
struct IdentityProfileQuery {
    wxid: String,
    chatroom_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContextQuery {
    before: Option<i64>,
    after: Option<i64>,
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
    backfill_observed_identity_aliases(&db).await?;

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

    maybe_spawn_voice_backfill_worker(state.clone());

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
            "/write/chatroom-events",
            post(write_chatroom_events).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/api/messages",
            get(list_messages).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
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
            "/api/messages/{message_key}/context",
            get(message_context).route_layer(middleware::from_fn_with_state(
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
            "/api/identity/profile",
            get(identity_profile).route_layer(middleware::from_fn_with_state(
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
            "/api/voice",
            get(list_voice).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_read_token,
            )),
        )
        .route(
            "/api/voice/transcribe",
            post(transcribe_voice).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/api/voice/warm",
            post(warm_voice).route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_write_token,
            )),
        )
        .route(
            "/api/maintenance/status",
            get(maintenance_status).route_layer(middleware::from_fn_with_state(
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

fn auto_voice_transcribe_enabled() -> bool {
    matches!(
        env::var("GEWE_SKILL_AUTO_TRANSCRIBE_VOICE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn auto_voice_transcribe_language() -> Option<String> {
    env::var("GEWE_SKILL_ASR_LANGUAGE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone)]
struct VoiceBackfillConfig {
    enabled: bool,
    interval: Duration,
    limit: i64,
    provider: Option<String>,
    language: Option<String>,
    force: bool,
}

fn voice_backfill_config() -> VoiceBackfillConfig {
    VoiceBackfillConfig {
        enabled: env_bool("GEWE_SKILL_ASR_BACKGROUND_ENABLED", false),
        interval: Duration::from_secs(env_u64("GEWE_SKILL_ASR_BACKGROUND_INTERVAL_SECONDS", 300)),
        limit: env_i64("GEWE_SKILL_ASR_BACKGROUND_LIMIT", 20).clamp(1, 200),
        provider: env::var("GEWE_SKILL_ASR_BACKGROUND_PROVIDER")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| env::var("GEWE_SKILL_ASR_PROVIDER").ok())
            .filter(|value| !value.is_empty()),
        language: env::var("GEWE_SKILL_ASR_BACKGROUND_LANGUAGE")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(auto_voice_transcribe_language),
        force: env_bool("GEWE_SKILL_ASR_BACKGROUND_FORCE", false),
    }
}

fn maybe_spawn_voice_backfill_worker(state: SharedState) {
    let config = voice_backfill_config();
    if !config.enabled {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(config.interval);
        loop {
            ticker.tick().await;
            match run_voice_backfill_once(&state, &config).await {
                Ok(summary) => {
                    if summary.scanned > 0 || summary.failed > 0 {
                        info!(
                            scanned = summary.scanned,
                            transcribed = summary.transcribed,
                            skipped = summary.skipped,
                            failed = summary.failed,
                            "voice asr background backfill finished"
                        );
                    }
                }
                Err(error) => {
                    warn!(error = %error, "voice asr background backfill failed");
                }
            }
        }
    });
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name).map_or(default, |value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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

async fn maintenance_status(State(state): State<SharedState>) -> Result<Json<Value>, ApiError> {
    let db = &state.db;
    let voice_backfill = voice_backfill_config();
    Ok(Json(json!({
        "ok": true,
        "service": "gewe-skill-memory",
        "generated_at": now_iso(),
        "messages": {
            "total": scalar_i64(db, "SELECT COUNT(*) AS value FROM messages").await?,
            "conversations": scalar_i64(db, "SELECT COUNT(DISTINCT conversation_id) AS value FROM messages WHERE conversation_id IS NOT NULL AND conversation_id != ''").await?,
            "by_kind": count_map(db, "SELECT lower(kind) AS key, COUNT(*) AS count FROM messages GROUP BY lower(kind) ORDER BY count DESC").await?,
            "oldest_received_at": scalar_text(db, "SELECT MIN(received_at) AS value FROM messages").await?,
            "newest_received_at": scalar_text(db, "SELECT MAX(received_at) AS value FROM messages").await?,
        },
        "attachments": {
            "total": scalar_i64(db, "SELECT COUNT(*) AS value FROM attachments").await?,
            "by_kind": count_map(db, "SELECT lower(kind) AS key, COUNT(*) AS count FROM attachments GROUP BY lower(kind) ORDER BY count DESC").await?,
            "messages_with_attachments": scalar_i64(db, "SELECT COUNT(DISTINCT message_key) AS value FROM attachments").await?,
            "duplicate_sha256_groups": scalar_i64(db, "SELECT COUNT(*) AS value FROM (SELECT sha256 FROM attachments WHERE sha256 IS NOT NULL AND sha256 != '' GROUP BY sha256 HAVING COUNT(*) > 1)").await?,
            "missing_object_key": scalar_i64(db, "SELECT COUNT(*) AS value FROM attachments WHERE object_key IS NULL OR object_key = ''").await?,
            "missing_sha256": scalar_i64(db, "SELECT COUNT(*) AS value FROM attachments WHERE sha256 IS NULL OR sha256 = ''").await?,
            "newest_created_at": scalar_text(db, "SELECT MAX(created_at) AS value FROM attachments").await?,
        },
        "voice": {
            "messages": scalar_i64(db, "SELECT COUNT(*) AS value FROM messages WHERE lower(kind) = 'voice'").await?,
            "messages_with_voice_attachment": scalar_i64(db, "SELECT COUNT(DISTINCT message_key) AS value FROM attachments WHERE lower(kind) = 'voice'").await?,
            "missing_attachments": scalar_i64(db, "SELECT COUNT(*) AS value FROM messages m WHERE lower(m.kind) = 'voice' AND NOT EXISTS (SELECT 1 FROM attachments a WHERE a.message_key = m.message_key AND lower(a.kind) = 'voice')").await?,
            "ready_without_completed_transcript": scalar_i64(db, "SELECT COUNT(*) AS value FROM messages m WHERE lower(m.kind) = 'voice' AND EXISTS (SELECT 1 FROM attachments a WHERE a.message_key = m.message_key AND lower(a.kind) = 'voice') AND NOT EXISTS (SELECT 1 FROM voice_transcripts vt WHERE vt.message_key = m.message_key AND vt.status = 'completed')").await?,
            "transcripts_by_status": count_map(db, "SELECT lower(status) AS key, COUNT(*) AS count FROM voice_transcripts GROUP BY lower(status) ORDER BY count DESC").await?,
            "failed_transcripts": scalar_i64(db, "SELECT COUNT(*) AS value FROM voice_transcripts WHERE status = 'failed'").await?,
            "newest_transcript_at": scalar_text(db, "SELECT MAX(updated_at) AS value FROM voice_transcripts").await?,
        },
        "asr_background": {
            "enabled": voice_backfill.enabled,
            "interval_seconds": voice_backfill.interval.as_secs(),
            "limit": voice_backfill.limit,
            "provider": voice_backfill.provider,
            "language": voice_backfill.language,
            "force": voice_backfill.force,
        },
        "identity": {
            "contacts": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_contacts").await?,
            "chatrooms": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_chatrooms").await?,
            "current_chatroom_members": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_chatroom_members WHERE is_current != 0").await?,
            "all_chatroom_members": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_chatroom_members").await?,
            "aliases": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_aliases").await?,
            "current_aliases": scalar_i64(db, "SELECT COUNT(*) AS value FROM identity_aliases WHERE is_current != 0").await?,
        },
        "chatrooms": {
            "snapshots": scalar_i64(db, "SELECT COUNT(*) AS value FROM chatroom_snapshots").await?,
            "member_events": scalar_i64(db, "SELECT COUNT(*) AS value FROM chatroom_member_events").await?,
            "system_events": scalar_i64(db, "SELECT COUNT(*) AS value FROM chatroom_system_events").await?,
            "member_events_by_type": count_map(db, "SELECT lower(event_type) AS key, COUNT(*) AS count FROM chatroom_member_events GROUP BY lower(event_type) ORDER BY count DESC").await?,
            "system_events_by_type": count_map(db, "SELECT lower(event_type) AS key, COUNT(*) AS count FROM chatroom_system_events GROUP BY lower(event_type) ORDER BY count DESC").await?,
        }
    })))
}

async fn scalar_i64(db: &SqlitePool, sql: &str) -> Result<i64, ApiError> {
    let row = sqlx::query(sql).fetch_one(db).await?;
    Ok(row.get("value"))
}

async fn scalar_text(db: &SqlitePool, sql: &str) -> Result<Option<String>, ApiError> {
    let row = sqlx::query(sql).fetch_one(db).await?;
    Ok(row.get("value"))
}

async fn count_map(db: &SqlitePool, sql: &str) -> Result<Value, ApiError> {
    let rows = sqlx::query(sql).fetch_all(db).await?;
    let mut map = Map::new();
    for row in rows {
        let key = row
            .get::<Option<String>, _>("key")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        map.insert(key, json!(row.get::<i64, _>("count")));
    }
    Ok(Value::Object(map))
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
    let should_auto_transcribe =
        matches!(&record.kind, AttachmentKind::Voice) && auto_voice_transcribe_enabled();
    let message_key = record.message_key.clone();
    let language = auto_voice_transcribe_language();
    insert_attachment(&state.db, &record).await?;
    let auto_transcribe = if should_auto_transcribe {
        match transcribe_voice_impl(
            &state,
            VoiceTranscribeRequest {
                message_key,
                provider: None,
                language,
                force: Some(false),
            },
        )
        .await
        {
            Ok(response) => Some(json!(response)),
            Err(error) => Some(json!({
                "ok": false,
                "status": "auto_transcribe_error",
                "error": error.to_string()
            })),
        }
    } else {
        None
    };
    Ok(Json(json!({
        "ok": true,
        "sha256": record.sha256,
        "auto_transcribe": auto_transcribe
    })))
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

async fn write_chatroom_events(
    State(state): State<SharedState>,
    Json(request): Json<ChatroomEventWriteRequest>,
) -> Result<Json<ChatroomEventWriteResponse>, ApiError> {
    let member_events = request.member_events.len();
    let system_events = request.system_events.len();
    let mut tx = state.db.begin().await?;
    for event in &request.member_events {
        insert_chatroom_member_event(&mut tx, event).await?;
    }
    for event in &request.system_events {
        insert_chatroom_system_event(&mut tx, event).await?;
    }
    tx.commit().await?;

    Ok(Json(ChatroomEventWriteResponse {
        ok: true,
        member_events,
        system_events,
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
    let request = MessageQuery {
        limit: query.limit,
        cursor: query.cursor,
        ..MessageQuery::default()
    };
    list_messages_impl(&state.db, &request).await.map(Json)
}

async fn search_messages(
    State(state): State<SharedState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
    let request = MessageQuery {
        q: Some(query.q),
        limit: query.limit,
        ..MessageQuery::default()
    };
    list_messages_impl(&state.db, &request).await.map(Json)
}

async fn list_messages(
    State(state): State<SharedState>,
    Query(query): Query<MessageQuery>,
) -> Result<Json<ApiPage<NormalizedMessage>>, ApiError> {
    list_messages_impl(&state.db, &query).await.map(Json)
}

async fn list_messages_impl(
    db: &SqlitePool,
    query: &MessageQuery,
) -> Result<ApiPage<NormalizedMessage>, ApiError> {
    let limit = clamp_limit(query.limit);
    let order = query
        .order
        .as_deref()
        .unwrap_or("desc")
        .to_ascii_lowercase();
    let ascending = order == "asc";
    let mut sql = String::from("SELECT message_json FROM messages WHERE 1 = 1");
    let mut args = Vec::<String>::new();

    if let Some(value) = query
        .conversation_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND conversation_id = ?");
        args.push(value.to_string());
    }
    if let Some(value) = query
        .sender_wxid
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND sender_wxid = ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.kind.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND lower(kind) = lower(?)");
        args.push(value.to_string());
    }
    match query
        .direction
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("incoming") => sql.push_str(" AND is_outgoing = 0"),
        Some("outgoing") => sql.push_str(" AND is_outgoing != 0"),
        _ => {}
    }
    if let Some(value) = query.after.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND received_at >= ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.before.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND received_at < ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.cursor.as_deref().filter(|value| !value.is_empty()) {
        if ascending {
            sql.push_str(" AND received_at > ?");
        } else {
            sql.push_str(" AND received_at < ?");
        }
        args.push(value.to_string());
    }
    if let Some(value) = query.q.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND (content_text LIKE ? OR message_json LIKE ?)");
        let pattern = format!("%{value}%");
        args.push(pattern.clone());
        args.push(pattern);
    }
    if ascending {
        sql.push_str(" ORDER BY received_at ASC, id ASC LIMIT ?");
    } else {
        sql.push_str(" ORDER BY received_at DESC, id DESC LIMIT ?");
    }

    let mut statement = sqlx::query(&sql);
    for arg in args {
        statement = statement.bind(arg);
    }
    let rows = statement.bind(limit).fetch_all(db).await?;
    let items = rows
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok()
        })
        .collect::<Vec<_>>();
    let next_cursor = items.last().map(|message| message.received_at.clone());
    Ok(ApiPage { items, next_cursor })
}

async fn message_context(
    State(state): State<SharedState>,
    Path(message_key): Path<String>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<MessageContextResponse>, ApiError> {
    let anchor_row = sqlx::query(
        r#"
        SELECT id, conversation_id, received_at, message_json
        FROM messages
        WHERE message_key = ?
        LIMIT 1
        "#,
    )
    .bind(&message_key)
    .fetch_optional(&state.db)
    .await?;
    let Some(anchor_row) = anchor_row else {
        return Ok(Json(MessageContextResponse {
            conversation_id: None,
            message_key,
            before: Vec::new(),
            anchor: None,
            after: Vec::new(),
        }));
    };
    let anchor_id: i64 = anchor_row.get("id");
    let conversation_id: Option<String> = anchor_row.get("conversation_id");
    let received_at: String = anchor_row.get("received_at");
    let anchor = serde_json::from_str::<NormalizedMessage>(anchor_row.get("message_json")).ok();
    let before = context_rows(
        &state.db,
        conversation_id.as_deref(),
        &received_at,
        anchor_id,
        "before",
        clamp_limit(query.before),
    )
    .await?;
    let after = context_rows(
        &state.db,
        conversation_id.as_deref(),
        &received_at,
        anchor_id,
        "after",
        clamp_limit(query.after),
    )
    .await?;
    Ok(Json(MessageContextResponse {
        conversation_id,
        message_key,
        before,
        anchor,
        after,
    }))
}

async fn context_rows(
    db: &SqlitePool,
    conversation_id: Option<&str>,
    received_at: &str,
    anchor_id: i64,
    side: &str,
    limit: i64,
) -> Result<Vec<NormalizedMessage>, ApiError> {
    let Some(conversation_id) = conversation_id else {
        return Ok(Vec::new());
    };
    let (operator, order) = if side == "after" {
        (">", "ASC")
    } else {
        ("<", "DESC")
    };
    let sql = format!(
        r#"
        SELECT message_json
        FROM messages
        WHERE conversation_id = ?
          AND (received_at {operator} ? OR (received_at = ? AND id {operator} ?))
        ORDER BY received_at {order}, id {order}
        LIMIT ?
        "#
    );
    let rows = sqlx::query(&sql)
        .bind(conversation_id)
        .bind(received_at)
        .bind(received_at)
        .bind(anchor_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
    let mut items = rows
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json")).ok()
        })
        .collect::<Vec<_>>();
    if side == "before" {
        items.reverse();
    }
    Ok(items)
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
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for term in identity_query_terms(&query.q) {
        let matches = query_identity_matches(&state.db, &term, query.q.trim(), limit).await?;
        for item in matches {
            let key = format!(
                "{}:{}:{}:{}",
                item.entity_type,
                item.entity_id,
                item.chatroom_id.as_deref().unwrap_or_default(),
                item.source.as_deref().unwrap_or_default()
            );
            if seen.insert(key) {
                items.push(item);
            }
            if items.len() >= limit as usize {
                break;
            }
        }
        if items.len() >= limit as usize {
            break;
        }
    }
    Ok(Json(IdentityResolveResponse {
        query: query.q,
        items,
    }))
}

async fn query_identity_matches(
    db: &SqlitePool,
    term: &str,
    raw_query: &str,
    limit: i64,
) -> Result<Vec<IdentityMatch>, ApiError> {
    let alias_key = normalize_alias(term);
    let like = format!("%{}%", term.trim());
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
                   WHEN entity_type = 'chatroom_member' THEN COALESCE(
                     (SELECT NULLIF(remark, '')
                      FROM identity_contacts
                      WHERE wxid = entity_id),
                     (SELECT NULLIF(display_name, '')
                      FROM identity_chatroom_members
                      WHERE chatroom_id = scope_key AND member_wxid = entity_id),
                     (SELECT NULLIF(nickname, '')
                      FROM identity_chatroom_members
                      WHERE chatroom_id = scope_key AND member_wxid = entity_id),
                     (SELECT COALESCE(NULLIF(nickname, ''), NULLIF(alias, ''))
                      FROM identity_contacts
                      WHERE wxid = entity_id)
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
    .bind(raw_query)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
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
        .collect())
}

async fn identity_profile(
    State(state): State<SharedState>,
    Query(query): Query<IdentityProfileQuery>,
) -> Result<Json<IdentityProfileResponse>, ApiError> {
    let contact = identity_contact_profile(&state.db, &query.wxid).await?;
    let chatroom_member = if let Some(chatroom_id) = query.chatroom_id.as_deref() {
        identity_chatroom_member_profile(&state.db, chatroom_id, &query.wxid).await?
    } else {
        None
    };
    let aliases =
        identity_alias_profiles(&state.db, &query.wxid, query.chatroom_id.as_deref()).await?;
    let effective_display_name = effective_identity_display_name(&contact, &chatroom_member);

    Ok(Json(IdentityProfileResponse {
        entity_id: query.wxid,
        chatroom_id: query.chatroom_id,
        effective_display_name,
        contact,
        chatroom_member,
        aliases,
    }))
}

async fn identity_contact_profile(
    db: &SqlitePool,
    wxid: &str,
) -> Result<Option<IdentityContactProfile>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT wxid, nickname, remark, alias, raw_json, last_seen_at, updated_at
        FROM identity_contacts
        WHERE wxid = ?
        LIMIT 1
        "#,
    )
    .bind(wxid)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|row| IdentityContactProfile {
        wxid: row.get("wxid"),
        nickname: row.get("nickname"),
        remark: row.get("remark"),
        alias: row.get("alias"),
        raw: parse_json_value(row.get("raw_json")),
        last_seen_at: row.get("last_seen_at"),
        updated_at: row.get("updated_at"),
    }))
}

async fn identity_chatroom_member_profile(
    db: &SqlitePool,
    chatroom_id: &str,
    wxid: &str,
) -> Result<Option<IdentityChatroomMemberProfile>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT chatroom_id, member_wxid, display_name, nickname, is_current, raw_json, last_seen_at
        FROM identity_chatroom_members
        WHERE chatroom_id = ? AND member_wxid = ?
        LIMIT 1
        "#,
    )
    .bind(chatroom_id)
    .bind(wxid)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|row| IdentityChatroomMemberProfile {
        chatroom_id: row.get("chatroom_id"),
        member_wxid: row.get("member_wxid"),
        display_name: row.get("display_name"),
        nickname: row.get("nickname"),
        is_current: row.get::<i64, _>("is_current") != 0,
        raw: parse_json_value(row.get("raw_json")),
        last_seen_at: row.get("last_seen_at"),
    }))
}

async fn identity_alias_profiles(
    db: &SqlitePool,
    wxid: &str,
    chatroom_id: Option<&str>,
) -> Result<Vec<IdentityMatch>, ApiError> {
    let mut sql = String::from(
        r#"
        SELECT entity_type, entity_id, NULLIF(scope_key, '') AS chatroom_id, alias, source,
               is_current, confidence, last_seen_at,
               COALESCE(
                 CASE
                   WHEN entity_type = 'contact' THEN (
                     SELECT COALESCE(NULLIF(remark, ''), NULLIF(nickname, ''), NULLIF(alias, ''))
                     FROM identity_contacts WHERE wxid = entity_id
                   )
                   WHEN entity_type = 'chatroom_member' THEN COALESCE(
                     (SELECT NULLIF(remark, '')
                      FROM identity_contacts
                      WHERE wxid = entity_id),
                     (SELECT NULLIF(display_name, '')
                      FROM identity_chatroom_members
                      WHERE chatroom_id = scope_key AND member_wxid = entity_id),
                     (SELECT NULLIF(nickname, '')
                      FROM identity_chatroom_members
                      WHERE chatroom_id = scope_key AND member_wxid = entity_id),
                     (SELECT COALESCE(NULLIF(nickname, ''), NULLIF(alias, ''))
                      FROM identity_contacts
                      WHERE wxid = entity_id)
                   )
                 END,
                 alias
               ) AS display_name,
               confidence + CASE WHEN is_current != 0 THEN 0.05 ELSE 0 END AS score
        FROM identity_aliases
        WHERE entity_id = ?
        "#,
    );
    if chatroom_id.is_some() {
        sql.push_str(" AND (scope_key = '' OR scope_key = ?)");
    }
    sql.push_str(" ORDER BY is_current DESC, confidence DESC, last_seen_at DESC");

    let mut statement = sqlx::query(&sql).bind(wxid);
    if let Some(chatroom_id) = chatroom_id {
        statement = statement.bind(chatroom_id);
    }
    let rows = statement.fetch_all(db).await?;

    Ok(rows
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
        .collect())
}

fn effective_identity_display_name(
    contact: &Option<IdentityContactProfile>,
    chatroom_member: &Option<IdentityChatroomMemberProfile>,
) -> Option<String> {
    contact
        .as_ref()
        .and_then(|item| non_empty_string(item.remark.as_deref()))
        .or_else(|| {
            chatroom_member
                .as_ref()
                .and_then(|item| non_empty_string(item.display_name.as_deref()))
        })
        .or_else(|| {
            chatroom_member
                .as_ref()
                .and_then(|item| non_empty_string(item.nickname.as_deref()))
        })
        .or_else(|| {
            contact
                .as_ref()
                .and_then(|item| non_empty_string(item.nickname.as_deref()))
        })
        .or_else(|| {
            contact
                .as_ref()
                .and_then(|item| non_empty_string(item.alias.as_deref()))
        })
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_json_value(value: Option<String>) -> Option<Value> {
    serde_json::from_str(value.as_deref()?).ok()
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

    if request.chatroom_id.is_none()
        && request
            .wxids
            .as_ref()
            .map(|wxids| wxids.is_empty())
            .unwrap_or(true)
        && !request.full.unwrap_or(false)
    {
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

async fn list_voice(
    State(state): State<SharedState>,
    Query(query): Query<VoiceQuery>,
) -> Result<Json<ApiPage<VoiceItem>>, ApiError> {
    list_voice_impl(&state.db, &query).await.map(Json)
}

async fn transcribe_voice(
    State(state): State<SharedState>,
    Json(request): Json<VoiceTranscribeRequest>,
) -> Result<Json<VoiceTranscribeResponse>, ApiError> {
    transcribe_voice_impl(&state, request).await.map(Json)
}

async fn warm_voice(
    State(state): State<SharedState>,
    Json(request): Json<VoiceWarmRequest>,
) -> Result<Json<VoiceWarmResponse>, ApiError> {
    run_voice_warm(&state, request).await.map(Json)
}

async fn run_voice_warm(
    state: &SharedState,
    request: VoiceWarmRequest,
) -> Result<VoiceWarmResponse, ApiError> {
    let query = VoiceQuery {
        conversation_id: request.conversation_id.clone(),
        sender_wxid: request.sender_wxid.clone(),
        after: request.after.clone(),
        before: request.before.clone(),
        cursor: None,
        limit: Some(request.limit.unwrap_or(10).clamp(1, 50)),
        order: Some("desc".to_string()),
        missing_only: None,
    };
    let page = list_voice_impl(&state.db, &query).await?;
    let mut items = Vec::new();
    for item in page.items {
        items.push(
            transcribe_voice_impl(
                &state,
                VoiceTranscribeRequest {
                    message_key: item.message.message_key,
                    provider: request.provider.clone(),
                    language: request.language.clone(),
                    force: request.force,
                },
            )
            .await?,
        );
    }
    let transcribed = items
        .iter()
        .filter(|item| {
            matches!(
                item.status.as_str(),
                "completed" | "already_transcribed" | "reused"
            )
        })
        .count();
    let skipped = items
        .iter()
        .filter(|item| item.status == "missing_attachment")
        .count();
    let failed = items
        .iter()
        .filter(|item| {
            !matches!(
                item.status.as_str(),
                "completed" | "already_transcribed" | "reused" | "missing_attachment"
            )
        })
        .count();
    Ok(VoiceWarmResponse {
        ok: failed == 0,
        scanned: items.len(),
        transcribed,
        skipped,
        failed,
        items,
    })
}

async fn run_voice_backfill_once(
    state: &SharedState,
    config: &VoiceBackfillConfig,
) -> Result<VoiceWarmResponse, ApiError> {
    run_voice_warm(
        state,
        VoiceWarmRequest {
            conversation_id: None,
            sender_wxid: None,
            after: None,
            before: None,
            limit: Some(config.limit),
            provider: config.provider.clone(),
            language: config.language.clone(),
            force: Some(config.force),
        },
    )
    .await
}

async fn list_voice_impl(
    db: &SqlitePool,
    query: &VoiceQuery,
) -> Result<ApiPage<VoiceItem>, ApiError> {
    let limit = clamp_limit(query.limit);
    let order = query
        .order
        .as_deref()
        .unwrap_or("desc")
        .to_ascii_lowercase();
    let ascending = order == "asc";
    let mut sql =
        String::from("SELECT message_json FROM messages WHERE lower(kind) = lower('Voice')");
    let mut args = Vec::<String>::new();

    if let Some(value) = query
        .conversation_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND conversation_id = ?");
        args.push(value.to_string());
    }
    if let Some(value) = query
        .sender_wxid
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND sender_wxid = ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.after.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND received_at >= ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.before.as_deref().filter(|value| !value.is_empty()) {
        sql.push_str(" AND received_at < ?");
        args.push(value.to_string());
    }
    if let Some(value) = query.cursor.as_deref().filter(|value| !value.is_empty()) {
        if ascending {
            sql.push_str(" AND received_at > ?");
        } else {
            sql.push_str(" AND received_at < ?");
        }
        args.push(value.to_string());
    }
    if ascending {
        sql.push_str(" ORDER BY received_at ASC, id ASC LIMIT ?");
    } else {
        sql.push_str(" ORDER BY received_at DESC, id DESC LIMIT ?");
    }

    let mut statement = sqlx::query(&sql);
    for arg in args {
        statement = statement.bind(arg);
    }
    let rows = statement.bind(limit).fetch_all(db).await?;
    let mut items = Vec::new();
    for row in rows {
        let Ok(message) =
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json"))
        else {
            continue;
        };
        let item = voice_item_from_message(db, message).await?;
        if query.missing_only.unwrap_or(false) && item.availability != "missing_attachment" {
            continue;
        }
        items.push(item);
    }
    let next_cursor = items.last().map(|item| item.message.received_at.clone());
    Ok(ApiPage { items, next_cursor })
}

async fn voice_item_from_message(
    db: &SqlitePool,
    message: NormalizedMessage,
) -> Result<VoiceItem, ApiError> {
    let attachment = voice_attachment_for_message(db, &message.message_key).await?;
    let transcript = voice_transcript_for_message(db, &message.message_key).await?;
    let (availability, reason) = match (&attachment, &transcript) {
        (_, Some(record)) if record.status == "completed" => ("transcribed", None),
        (_, Some(record)) if record.status == "failed" => (
            "failed",
            record.error.clone().or(Some("asr_failed".to_string())),
        ),
        (Some(record), _) if record.sha256.is_some() && record.object_key.is_some() => {
            ("ready", None)
        }
        (Some(_), _) => (
            "missing_attachment",
            Some("attachment_metadata_incomplete".to_string()),
        ),
        (None, _) => (
            "missing_attachment",
            Some("voice_attachment_not_synced".to_string()),
        ),
    };
    Ok(VoiceItem {
        message,
        attachment,
        transcript,
        availability: availability.to_string(),
        reason,
    })
}

async fn voice_attachment_for_message(
    db: &SqlitePool,
    message_key: &str,
) -> Result<Option<AttachmentRecord>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT id, attachment_json
        FROM attachments
        WHERE message_key = ?
          AND lower(kind) = lower('Voice')
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(message_key)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut record =
        serde_json::from_str::<AttachmentRecord>(row.get::<&str, _>("attachment_json"))?;
    record.id = Some(row.get("id"));
    Ok(Some(record))
}

async fn voice_message_by_key(
    db: &SqlitePool,
    message_key: &str,
) -> Result<Option<NormalizedMessage>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT message_json
        FROM messages
        WHERE message_key = ?
          AND lower(kind) = lower('Voice')
        LIMIT 1
        "#,
    )
    .bind(message_key)
    .fetch_optional(db)
    .await?;
    row.map(|row| serde_json::from_str(row.get::<&str, _>("message_json")))
        .transpose()
        .map_err(ApiError::from)
}

async fn voice_transcript_for_message(
    db: &SqlitePool,
    message_key: &str,
) -> Result<Option<VoiceTranscriptRecord>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT message_key, attachment_sha256, provider, language, text, status,
               error, duration_ms, response_json, created_at, updated_at
        FROM voice_transcripts
        WHERE message_key = ?
        LIMIT 1
        "#,
    )
    .bind(message_key)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let response_json: Option<String> = row.get("response_json");
    Ok(Some(VoiceTranscriptRecord {
        message_key: row.get("message_key"),
        attachment_sha256: row.get("attachment_sha256"),
        provider: row.get("provider"),
        language: row.get("language"),
        text: row.get("text"),
        status: row.get("status"),
        error: row.get("error"),
        duration_ms: row.get("duration_ms"),
        response_json: response_json.and_then(|value| serde_json::from_str(&value).ok()),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

async fn reusable_voice_transcript(
    db: &SqlitePool,
    attachment_sha256: &str,
    provider: &str,
    language: Option<&str>,
    exclude_message_key: &str,
) -> Result<Option<VoiceTranscriptRecord>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT message_key, attachment_sha256, provider, language, text, status,
               error, duration_ms, response_json, created_at, updated_at
        FROM voice_transcripts
        WHERE attachment_sha256 = ?
          AND provider = ?
          AND COALESCE(language, '') = ?
          AND status = 'completed'
          AND text IS NOT NULL
          AND message_key != ?
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(attachment_sha256)
    .bind(provider)
    .bind(language.unwrap_or(""))
    .bind(exclude_message_key)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let response_json: Option<String> = row.get("response_json");
    Ok(Some(VoiceTranscriptRecord {
        message_key: row.get("message_key"),
        attachment_sha256: row.get("attachment_sha256"),
        provider: row.get("provider"),
        language: row.get("language"),
        text: row.get("text"),
        status: row.get("status"),
        error: row.get("error"),
        duration_ms: row.get("duration_ms"),
        response_json: response_json.and_then(|value| serde_json::from_str(&value).ok()),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

async fn transcribe_voice_impl(
    state: &SharedState,
    request: VoiceTranscribeRequest,
) -> Result<VoiceTranscribeResponse, ApiError> {
    let Some(message) = voice_message_by_key(&state.db, &request.message_key).await? else {
        return Ok(VoiceTranscribeResponse {
            ok: false,
            message_key: request.message_key,
            status: "message_not_found".to_string(),
            transcript: None,
            error: Some("voice_message_not_found".to_string()),
        });
    };
    let Some(attachment) = voice_attachment_for_message(&state.db, &message.message_key).await?
    else {
        return Ok(VoiceTranscribeResponse {
            ok: false,
            message_key: message.message_key,
            status: "missing_attachment".to_string(),
            transcript: None,
            error: Some("voice_attachment_not_synced".to_string()),
        });
    };
    let Some(sha256) = attachment.sha256.clone() else {
        return Ok(VoiceTranscribeResponse {
            ok: false,
            message_key: message.message_key,
            status: "missing_attachment".to_string(),
            transcript: None,
            error: Some("attachment_sha256_missing".to_string()),
        });
    };
    let Some(object_key) = attachment.object_key.clone() else {
        return Ok(VoiceTranscribeResponse {
            ok: false,
            message_key: message.message_key,
            status: "missing_attachment".to_string(),
            transcript: None,
            error: Some("attachment_object_key_missing".to_string()),
        });
    };
    let provider = normalize_asr_provider(request.provider.as_deref());
    if !request.force.unwrap_or(false) {
        if let Some(existing) =
            voice_transcript_for_message(&state.db, &message.message_key).await?
        {
            if existing.status == "completed" {
                return Ok(VoiceTranscribeResponse {
                    ok: true,
                    message_key: message.message_key,
                    status: "already_transcribed".to_string(),
                    transcript: Some(existing),
                    error: None,
                });
            }
        }
        if let Some(reusable) = reusable_voice_transcript(
            &state.db,
            &sha256,
            &provider,
            request.language.as_deref(),
            &message.message_key,
        )
        .await?
        {
            let now = now_iso();
            let transcript = VoiceTranscriptRecord {
                message_key: message.message_key.clone(),
                attachment_sha256: Some(sha256),
                provider,
                language: request.language.clone(),
                text: reusable.text,
                status: "completed".to_string(),
                error: None,
                duration_ms: voice_duration_ms(&message),
                response_json: Some(json!({
                    "reused": true,
                    "reused_from_message_key": reusable.message_key,
                    "reused_from_attachment_sha256": reusable.attachment_sha256,
                    "source_provider": reusable.provider,
                    "source_language": reusable.language,
                    "source_updated_at": reusable.updated_at
                })),
                created_at: now.clone(),
                updated_at: now,
            };
            record_voice_transcript(&state.db, &transcript).await?;
            return Ok(VoiceTranscribeResponse {
                ok: true,
                message_key: message.message_key,
                status: "reused".to_string(),
                transcript: Some(transcript),
                error: None,
            });
        }
    }

    let Some(path) = safe_attachment_path(&state.attachment_dir, &object_key) else {
        return Ok(VoiceTranscribeResponse {
            ok: false,
            message_key: message.message_key,
            status: "missing_attachment".to_string(),
            transcript: None,
            error: Some("invalid_attachment_path".to_string()),
        });
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VoiceTranscribeResponse {
                ok: false,
                message_key: message.message_key,
                status: "missing_attachment".to_string(),
                transcript: None,
                error: Some("attachment_file_missing".to_string()),
            });
        }
        Err(error) => return Err(ApiError::Io(error)),
    };

    let filename = format!("{sha256}.silk");
    let asr = match provider.as_str() {
        "codex-asr" => {
            transcribe_with_codex_asr(state, bytes, filename, request.language.as_deref()).await
        }
        "cloudflare" => {
            transcribe_with_cloudflare(state, bytes, filename, request.language.as_deref()).await
        }
        _ => {
            return Ok(VoiceTranscribeResponse {
                ok: false,
                message_key: message.message_key,
                status: "unsupported_provider".to_string(),
                transcript: None,
                error: Some(format!("unsupported_asr_provider:{provider}")),
            });
        }
    };

    let now = now_iso();
    let duration_ms = voice_duration_ms(&message);
    let transcript = match asr {
        Ok(response_json) => VoiceTranscriptRecord {
            message_key: message.message_key.clone(),
            attachment_sha256: Some(sha256),
            provider,
            language: request.language,
            text: asr_text_from_value(&response_json),
            status: "completed".to_string(),
            error: None,
            duration_ms,
            response_json: Some(response_json),
            created_at: now.clone(),
            updated_at: now,
        },
        Err(error) => VoiceTranscriptRecord {
            message_key: message.message_key.clone(),
            attachment_sha256: Some(sha256),
            provider,
            language: request.language,
            text: None,
            status: "failed".to_string(),
            error: Some(error.to_string()),
            duration_ms,
            response_json: None,
            created_at: now.clone(),
            updated_at: now,
        },
    };
    record_voice_transcript(&state.db, &transcript).await?;
    Ok(VoiceTranscribeResponse {
        ok: transcript.status == "completed",
        message_key: message.message_key,
        status: if transcript.status == "completed" {
            "completed".to_string()
        } else {
            "asr_failed".to_string()
        },
        error: transcript.error.clone(),
        transcript: Some(transcript),
    })
}

async fn record_voice_transcript(
    db: &SqlitePool,
    record: &VoiceTranscriptRecord,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO voice_transcripts (
          message_key, attachment_sha256, provider, language, text, status, error,
          duration_ms, response_json, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(message_key) DO UPDATE SET
          attachment_sha256 = excluded.attachment_sha256,
          provider = excluded.provider,
          language = excluded.language,
          text = excluded.text,
          status = excluded.status,
          error = excluded.error,
          duration_ms = excluded.duration_ms,
          response_json = excluded.response_json,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(&record.message_key)
    .bind(&record.attachment_sha256)
    .bind(&record.provider)
    .bind(&record.language)
    .bind(&record.text)
    .bind(&record.status)
    .bind(&record.error)
    .bind(record.duration_ms)
    .bind(record.response_json.as_ref().map(Value::to_string))
    .bind(&record.created_at)
    .bind(&record.updated_at)
    .execute(db)
    .await?;
    Ok(())
}

async fn transcribe_with_codex_asr(
    state: &SharedState,
    bytes: Vec<u8>,
    filename: String,
    language: Option<&str>,
) -> Result<Value, ApiError> {
    let base_url = env_first(&["GEWE_SKILL_CODEX_ASR_URL", "CODEX_ASR_URL"])
        .unwrap_or_else(|| "http://127.0.0.1:18788".to_string());
    let url = format!("{}/v1/audio/transcriptions", base_url.trim_end_matches('/'));
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", "whisper-1")
        .text("response_format", "json");
    if let Some(language) = language.filter(|value| !value.is_empty()) {
        form = form.text("language", language.to_string());
    }
    let mut request = state.http.post(url).multipart(form);
    if let Some(token) = env_first(&[
        "GEWE_SKILL_CODEX_ASR_API_KEY",
        "CODEX_ASR_SERVER_KEY",
        "CODEX_ASR_API_KEY",
    ]) {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    asr_json_response(response).await
}

async fn transcribe_with_cloudflare(
    state: &SharedState,
    bytes: Vec<u8>,
    filename: String,
    language: Option<&str>,
) -> Result<Value, ApiError> {
    let account_id = env_first(&["GEWE_SKILL_CF_ACCOUNT_ID", "CLOUDFLARE_ACCOUNT_ID"])
        .ok_or(ApiError::AsrConfig("missing Cloudflare account id"))?;
    let token = env_first(&["GEWE_SKILL_CF_API_TOKEN", "CLOUDFLARE_API_TOKEN"])
        .ok_or(ApiError::AsrConfig("missing Cloudflare API token"))?;
    let model = env_first(&["GEWE_SKILL_CF_WHISPER_MODEL"])
        .unwrap_or_else(|| "@cf/openai/whisper".to_string());
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/run/{model}");
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
    let mut form = reqwest::multipart::Form::new().part("audio", part);
    if let Some(language) = language.filter(|value| !value.is_empty()) {
        form = form.text("language", language.to_string());
    }
    let response = state
        .http
        .post(url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await?;
    asr_json_response(response).await
}

async fn asr_json_response(response: reqwest::Response) -> Result<Value, ApiError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(ApiError::Asr(format!(
            "asr_http_{}:{body}",
            status.as_u16()
        )));
    }
    Ok(serde_json::from_str(&body)?)
}

fn normalize_asr_provider(provider: Option<&str>) -> String {
    provider
        .map(ToString::to_string)
        .or_else(|| env_first(&["GEWE_SKILL_ASR_PROVIDER"]))
        .unwrap_or_else(|| "codex-asr".to_string())
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn asr_text_from_value(value: &Value) -> Option<String> {
    value
        .get("text")
        .or_else(|| value.pointer("/result/text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn voice_duration_ms(message: &NormalizedMessage) -> Option<i64> {
    let xml = message.content_xml.as_deref()?;
    extract_xml_attr(xml, "voicelength").and_then(|value| value.parse().ok())
}

fn extract_xml_attr(xml: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = xml[start..].find('"')? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then_some(value.to_string())
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
        for observed in observed_member_aliases_from_message(message) {
            upsert_chatroom_member(
                tx,
                &observed.chatroom_id,
                &observed.member_wxid,
                None,
                Some(&observed.display_name),
                None,
                None,
                None,
                true,
                &message.received_at,
                "message_observed_alias",
            )
            .await?;
            upsert_alias(
                tx,
                &observed.display_name,
                "chatroom_member",
                &observed.member_wxid,
                Some(&observed.chatroom_id),
                "message_quote:refermsg_displayname",
                false,
                0.7,
                &message.received_at,
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
    let event_key_suffix = edge_event_key(&event.details).unwrap_or_else(|| {
        event
            .member_wxid
            .as_deref()
            .unwrap_or("chatroom")
            .to_string()
    });
    sqlx::query(
        r#"
        INSERT INTO chatroom_member_events (event_key, event_type, chatroom_id, member_wxid, received_at, event_json)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_key) DO UPDATE SET event_json = excluded.event_json
        "#,
    )
    .bind(format!(
        "{}:{}:{}",
        event.chatroom_id, event.received_at, event_key_suffix
    ))
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
    let event_key_suffix = edge_event_key(&event.details).unwrap_or_else(|| {
        event
            .content_text
            .as_deref()
            .unwrap_or("system")
            .to_string()
    });
    sqlx::query(
        r#"
        INSERT INTO chatroom_system_events (event_key, event_type, chatroom_id, actor_wxid, target_wxid, received_at, event_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_key) DO UPDATE SET event_json = excluded.event_json
        "#,
    )
    .bind(format!(
        "{}:{}:{}",
        event.chatroom_id, event.received_at, event_key_suffix
    ))
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

fn edge_event_key(details: &Value) -> Option<String> {
    details
        .get("edge_event_id")
        .and_then(Value::as_i64)
        .map(|id| format!("edge:{id}"))
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
        ON CONFLICT(edge_job_id) DO UPDATE SET
          job_key = COALESCE(excluded.job_key, attachments.job_key),
          message_key = excluded.message_key,
          raw_event_dedupe_key = excluded.raw_event_dedupe_key,
          appid = excluded.appid,
          account_wxid = excluded.account_wxid,
          kind = excluded.kind,
          variant = excluded.variant,
          object_key = excluded.object_key,
          sha256 = excluded.sha256,
          size_bytes = excluded.size_bytes,
          mime_type = excluded.mime_type,
          source_url = excluded.source_url,
          created_at = excluded.created_at,
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

fn identity_query_terms(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let mut terms = Vec::new();
    push_term(&mut terms, trimmed);
    for separator in ['（', '(', '[', '【'] {
        if let Some((head, tail)) = trimmed.split_once(separator) {
            push_term(&mut terms, head);
            let tail = tail
                .trim_end_matches('）')
                .trim_end_matches(')')
                .trim_end_matches(']')
                .trim_end_matches('】');
            push_term(&mut terms, tail);
        }
    }
    terms
}

fn push_term(terms: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !terms.iter().any(|item| item == value) {
        terms.push(value.to_string());
    }
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

struct ObservedMemberAlias {
    chatroom_id: String,
    member_wxid: String,
    display_name: String,
}

fn observed_member_aliases_from_message(message: &NormalizedMessage) -> Vec<ObservedMemberAlias> {
    let Some(xml) = &message.content_xml else {
        return Vec::new();
    };
    let Some(refermsg) = extract_xml_tag(xml, "refermsg") else {
        return Vec::new();
    };
    let Some(display_name) = extract_xml_tag(&refermsg, "displayname") else {
        return Vec::new();
    };
    let Some(member_wxid) = extract_xml_tag(&refermsg, "chatusr") else {
        return Vec::new();
    };
    let chatroom_id = extract_xml_tag(&refermsg, "fromusr")
        .or_else(|| message.conversation_id.clone())
        .unwrap_or_default();
    if chatroom_id.is_empty() || member_wxid.is_empty() || display_name.is_empty() {
        return Vec::new();
    }
    vec![ObservedMemberAlias {
        chatroom_id,
        member_wxid,
        display_name,
    }]
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let after_open = &xml[start..];
    let close_bracket = after_open.find('>')?;
    let content_start = start + close_bracket + 1;
    let close = format!("</{tag}>");
    let content_end = xml[content_start..].find(&close)? + content_start;
    let value = xml[content_start..content_end].trim();
    (!value.is_empty()).then_some(value.to_string())
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
          sha256 TEXT NOT NULL,
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
    migrate_attachments_allow_duplicate_sha(db).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_key)")
        .execute(db)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_attachments_created ON attachments(created_at DESC)",
    )
    .execute(db)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS voice_transcripts (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          message_key TEXT NOT NULL UNIQUE,
          attachment_sha256 TEXT,
          provider TEXT NOT NULL,
          language TEXT,
          text TEXT,
          status TEXT NOT NULL,
          error TEXT,
          duration_ms INTEGER,
          response_json TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_voice_transcripts_updated ON voice_transcripts(updated_at DESC)",
    )
    .execute(db)
    .await?;
    Ok(())
}

async fn migrate_attachments_allow_duplicate_sha(db: &SqlitePool) -> Result<(), sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT sql
        FROM sqlite_master
        WHERE type = 'table' AND name = 'attachments'
        LIMIT 1
        "#,
    )
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    let sql: String = row.get("sql");
    if !sql
        .to_ascii_lowercase()
        .contains("sha256 text not null unique")
    {
        return Ok(());
    }

    let mut tx = db.begin().await?;
    sqlx::query("DROP INDEX IF EXISTS idx_attachments_message")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DROP INDEX IF EXISTS idx_attachments_created")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE attachments RENAME TO attachments_sha_unique_old")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE attachments (
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
          sha256 TEXT NOT NULL,
          size_bytes INTEGER,
          mime_type TEXT,
          source_url TEXT,
          created_at TEXT NOT NULL,
          attachment_json TEXT NOT NULL
        );
        "#,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO attachments (
          id, edge_job_id, job_key, message_key, raw_event_dedupe_key, appid, account_wxid,
          kind, variant, object_key, sha256, size_bytes, mime_type, source_url,
          created_at, attachment_json
        )
        SELECT id, edge_job_id, job_key, message_key, raw_event_dedupe_key, appid, account_wxid,
               kind, variant, object_key, sha256, size_bytes, mime_type, source_url,
               created_at, attachment_json
        FROM attachments_sha_unique_old
        ORDER BY id
        "#,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DROP TABLE attachments_sha_unique_old")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn backfill_observed_identity_aliases(db: &SqlitePool) -> Result<(), ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT message_json
        FROM messages
        WHERE message_json LIKE '%<refermsg>%'
          AND message_json LIKE '%<displayname>%'
          AND message_json LIKE '%<chatusr>%'
        "#,
    )
    .fetch_all(db)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    for row in rows {
        let Ok(message) =
            serde_json::from_str::<NormalizedMessage>(row.get::<&str, _>("message_json"))
        else {
            continue;
        };
        for observed in observed_member_aliases_from_message(&message) {
            upsert_alias(
                &mut tx,
                &observed.display_name,
                "chatroom_member",
                &observed.member_wxid,
                Some(&observed.chatroom_id),
                "message_quote:refermsg_displayname",
                false,
                0.7,
                &message.received_at,
            )
            .await?;
        }
    }
    tx.commit().await?;
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
    AsrConfig(&'static str),
    Asr(String),
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
            Self::AsrConfig(error) => error.to_string(),
            Self::Asr(error) => error.clone(),
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

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match self {
            Self::Sqlx(error) => error.to_string(),
            Self::Serde(error) => error.to_string(),
            Self::Core(error) => error.to_string(),
            Self::Http(error) => error.to_string(),
            Self::GeweConfig(error) => error.to_string(),
            Self::Gewe(error) => error,
            Self::AsrConfig(error) => error.to_string(),
            Self::Asr(error) => error,
            Self::Io(error) => error.to_string(),
        };
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": message })),
        )
            .into_response()
    }
}
