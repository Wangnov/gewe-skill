use super::{clamp_limit, safe_attachment_path, ApiError, SharedState};
use axum::{extract::State, Json};
use gewe_skill_core::mime::corrected_mime;
use gewe_skill_types::AttachmentRecord;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::io::Read;
use std::path::Path as FsPath;

#[derive(Deserialize)]
pub(super) struct BackfillRequest {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    #[serde(default)]
    after_id: i64,
    limit: Option<i64>,
}

fn default_dry_run() -> bool {
    true
}

pub(super) fn correct_record(
    root: &FsPath,
    record: &mut AttachmentRecord,
) -> Result<(), std::io::Error> {
    let Some(path) = record
        .object_key
        .as_deref()
        .and_then(|key| safe_attachment_path(root, key))
    else {
        return Ok(());
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut header = [0u8; 64];
    let size = file.read(&mut header)?;
    record.mime_type = Some(corrected_mime(&header[..size], record.mime_type.as_deref()));
    Ok(())
}

pub(super) async fn backfill(
    State(state): State<SharedState>,
    Json(request): Json<BackfillRequest>,
) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, mime_type, attachment_json FROM attachments WHERE id > ? ORDER BY id LIMIT ?",
    )
    .bind(request.after_id)
    .bind(clamp_limit(request.limit))
    .fetch_all(&state.db)
    .await?;
    let mut changes = Vec::new();
    let mut missing_files = 0usize;
    let mut concurrent_updates = 0usize;
    let mut next_after_id = request.after_id;
    let mut tx = state.db.begin().await?;
    for row in &rows {
        let id: i64 = row.get("id");
        next_after_id = id;
        let mut record: AttachmentRecord = serde_json::from_str(row.get("attachment_json"))?;
        let path = record
            .object_key
            .as_deref()
            .and_then(|key| safe_attachment_path(&state.attachment_dir, key));
        if !path.as_ref().is_some_and(|p| p.is_file()) {
            missing_files += 1;
            continue;
        }
        let old_json_mime = record.mime_type.clone();
        let old_column_mime: Option<String> = row.get("mime_type");
        correct_record(&state.attachment_dir, &mut record).map_err(ApiError::Io)?;
        if record.mime_type == old_json_mime && record.mime_type == old_column_mime {
            continue;
        }
        if !request.dry_run
            && !update_if_unchanged(
                &mut tx,
                id,
                row.get("attachment_json"),
                old_column_mime.as_deref(),
                &record,
            )
            .await?
        {
            concurrent_updates += 1;
            continue;
        }
        changes.push(json!({ "id": id, "old_mime_type": old_column_mime, "new_mime_type": record.mime_type }));
    }
    if request.dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(Json(
        json!({ "ok": true, "dry_run": request.dry_run, "scanned": rows.len(), "changed": changes.len(),
        "missing_files": missing_files, "concurrent_updates": concurrent_updates, "changes": changes, "next_after_id": next_after_id }),
    ))
}

async fn update_if_unchanged(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    original_json: &str,
    original_mime: Option<&str>,
    record: &AttachmentRecord,
) -> Result<bool, ApiError> {
    // 现有同步任务可能在读取快照后更新记录；只改仍与快照相同的记录。
    let result = sqlx::query("UPDATE attachments SET mime_type = ?, attachment_json = ? WHERE id = ? AND attachment_json = ? AND mime_type IS ?")
        .bind(&record.mime_type).bind(serde_json::to_string(record)?).bind(id)
        .bind(original_json).bind(original_mime).execute(&mut **tx).await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_db, insert_attachment};

    #[tokio::test]
    async fn backfill_cannot_overwrite_metadata_changed_by_concurrent_sync() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_db(&db).await.unwrap();
        let mut record = AttachmentRecord {
            id: None,
            edge_job_id: Some(1),
            job_key: Some("test-job".to_string()),
            message_key: "v2:test-app:1".to_string(),
            raw_event_dedupe_key: "test-event".to_string(),
            appid: "test-app".to_string(),
            account_wxid: Some("wxid_test_account".to_string()),
            kind: gewe_skill_types::AttachmentKind::Image,
            variant: None,
            source_url: None,
            object_key: Some("test-image".to_string()),
            sha256: Some("test-sha".to_string()),
            size_bytes: Some(10),
            mime_type: Some("application/octst-stream".to_string()),
            created_at: "2026-10-15T00:00:00Z".to_string(),
        };
        insert_attachment(&db, &record).await.unwrap();
        let original = serde_json::to_string(&record).unwrap();
        record.variant = Some("new-sync-metadata".to_string());
        insert_attachment(&db, &record).await.unwrap();
        let updated = serde_json::to_string(&record).unwrap();
        record.mime_type = Some("image/jpeg".to_string());
        let mut tx = db.begin().await.unwrap();
        assert!(!update_if_unchanged(
            &mut tx,
            1,
            &original,
            Some("application/octst-stream"),
            &record
        )
        .await
        .unwrap());
        tx.commit().await.unwrap();
        let row = sqlx::query("SELECT attachment_json FROM attachments WHERE id=1")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("attachment_json"), updated);
    }
}
