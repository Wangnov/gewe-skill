use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const STATUS_COMPLETED: &str = "completed";
const STATUS_FAILED: &str = "failed";
const STATUS_PENDING: &str = "pending";
const STATUS_PROCESSING: &str = "processing";
const STATUS_RETRY_SCHEDULED: &str = "retry_scheduled";
const STATUS_UNAVAILABLE: &str = "unavailable";
const STATUS_PURGED: &str = "purged";

pub fn queue_health(edge_jobs: &Value) -> Value {
    let jobs = extract_jobs(edge_jobs);
    let mut by_status = BTreeMap::<String, u64>::new();
    let mut by_asset_type = BTreeMap::<String, u64>::new();
    let mut by_asset_status = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut duplicate_job_keys = BTreeMap::<String, u64>::new();
    let mut duplicate_message_keys = BTreeMap::<String, u64>::new();
    let mut job_keys = BTreeMap::<String, u64>::new();
    let mut message_keys = BTreeMap::<String, u64>::new();
    let mut statuses_seen = BTreeSet::<String>::new();
    let mut asset_types_seen = BTreeSet::<String>::new();

    for job in &jobs {
        let status = string_field(job, "status").unwrap_or_else(|| "unknown".to_string());
        let asset_type = string_field(job, "asset_type").unwrap_or_else(|| "unknown".to_string());

        statuses_seen.insert(status.clone());
        asset_types_seen.insert(asset_type.clone());
        *by_status.entry(status.clone()).or_insert(0) += 1;
        *by_asset_type.entry(asset_type.clone()).or_insert(0) += 1;
        *by_asset_status
            .entry(asset_type.clone())
            .or_default()
            .entry(status.clone())
            .or_insert(0) += 1;

        if let Some(job_key) = string_field(job, "job_key") {
            *job_keys.entry(job_key).or_insert(0) += 1;
        }
        if let Some(message_key) = string_field(job, "message_key") {
            *message_keys.entry(message_key).or_insert(0) += 1;
        }
    }

    for (key, count) in job_keys {
        if count > 1 {
            duplicate_job_keys.insert(key, count);
        }
    }
    for (key, count) in message_keys {
        if count > 1 {
            duplicate_message_keys.insert(key, count);
        }
    }

    let failed_count = count_status(&by_status, STATUS_FAILED);
    let pending_count = count_status(&by_status, STATUS_PENDING);
    let processing_count = count_status(&by_status, STATUS_PROCESSING);
    let retry_scheduled_count = count_status(&by_status, STATUS_RETRY_SCHEDULED);
    let completed_count = count_status(&by_status, STATUS_COMPLETED);
    let unavailable_count = count_status(&by_status, STATUS_UNAVAILABLE);
    let purged_count = count_status(&by_status, STATUS_PURGED);
    let active_count = pending_count + processing_count + retry_scheduled_count;
    let non_retryable_terminal_count = unavailable_count + purged_count;
    let duplicate_job_key_count = duplicate_job_keys.len() as u64;
    let duplicate_message_key_count = duplicate_message_keys.len() as u64;

    let health = if failed_count > 0 || duplicate_job_key_count > 0 {
        "needs_attention"
    } else if active_count > 0 {
        "in_progress"
    } else if non_retryable_terminal_count > 0 {
        "has_unavailable"
    } else {
        "healthy"
    };

    json!({
        "ok": true,
        "queue_health": health,
        "job_count": jobs.len(),
        "asset_types_seen": asset_types_seen.into_iter().collect::<Vec<_>>(),
        "statuses_seen": statuses_seen.into_iter().collect::<Vec<_>>(),
        "by_status": by_status,
        "by_asset_type": by_asset_type,
        "by_asset_status": by_asset_status,
        "completed_count": completed_count,
        "active_count": active_count,
        "retryable_terminal_count": failed_count,
        "non_retryable_terminal_count": non_retryable_terminal_count,
        "duplicate_job_key_count": duplicate_job_key_count,
        "duplicate_message_key_count": duplicate_message_key_count,
        "duplicate_job_keys": duplicate_job_keys,
        "duplicate_message_keys": duplicate_message_keys,
        "next_actions": next_actions(
            completed_count,
            active_count,
            failed_count,
            non_retryable_terminal_count,
            duplicate_job_key_count,
        ),
        "agent_notes": [
            "completed jobs should be pulled into memory with sync attachments before analysis",
            "failed jobs are retryable terminal jobs, but retry them intentionally instead of looping forever",
            "unavailable and purged jobs are non-retryable terminal evidence; explain them to the user unless explicitly asked to retry upstream",
            "duplicate job keys indicate queue dedupe drift and should be investigated before bulk retrying"
        ]
    })
}

fn extract_jobs(edge_jobs: &Value) -> Vec<&Value> {
    if let Some(jobs) = edge_jobs.get("jobs").and_then(Value::as_array) {
        return jobs.iter().collect();
    }
    if let Some(jobs) = edge_jobs.get("download_jobs").and_then(Value::as_array) {
        return jobs.iter().collect();
    }
    edge_jobs
        .as_array()
        .map(|jobs| jobs.iter().collect())
        .unwrap_or_default()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn count_status(by_status: &BTreeMap<String, u64>, status: &str) -> u64 {
    by_status.get(status).copied().unwrap_or(0)
}

fn next_actions(
    completed_count: u64,
    active_count: u64,
    failed_count: u64,
    non_retryable_terminal_count: u64,
    duplicate_job_key_count: u64,
) -> Vec<Value> {
    let mut actions = Vec::new();

    if completed_count > 0 {
        actions.push(json!({
            "action": "sync_completed_attachments",
            "priority": "high",
            "recommended_cli": ["sync", "attachments"],
            "reason": "completed edge jobs exist and should be copied into the memory attachment store"
        }));
    }

    if active_count > 0 {
        actions.push(json!({
            "action": "wait_or_requeue_active_jobs",
            "priority": "normal",
            "recommended_cli": ["sync", "attachment-requeue"],
            "reason": "pending, processing, or retry_scheduled jobs still need the edge worker to finish"
        }));
    }

    if failed_count > 0 {
        actions.push(json!({
            "action": "retry_failed_jobs_intentionally",
            "priority": "high",
            "recommended_cli": ["sync", "attachment-retry", "--status", "failed"],
            "reason": "failed jobs are terminal but retryable; retry only as a deliberate repair step"
        }));
    }

    if non_retryable_terminal_count > 0 {
        actions.push(json!({
            "action": "explain_unavailable_or_purged_jobs",
            "priority": "normal",
            "recommended_cli": ["sync", "attachment-queue", "--status", "unavailable"],
            "reason": "unavailable or purged jobs are evidence that upstream media cannot currently be recovered"
        }));
    }

    if duplicate_job_key_count > 0 {
        actions.push(json!({
            "action": "investigate_duplicate_queue_jobs",
            "priority": "high",
            "recommended_cli": ["maintenance", "attachment-queue-health"],
            "reason": "duplicate job keys mean the edge queue may enqueue the same download more than once"
        }));
    }

    if actions.is_empty() {
        actions.push(json!({
            "action": "no_queue_repair_needed",
            "priority": "low",
            "reason": "the inspected attachment queue snapshot has no active, failed, unavailable, or duplicate jobs"
        }));
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_cross_asset_status_counts_and_actions() {
        let input = json!({
            "jobs": [
                {"job_key": "img-1", "message_key": "m1", "asset_type": "image", "status": "completed"},
                {"job_key": "voice-1", "message_key": "m2", "asset_type": "voice", "status": "failed"},
                {"job_key": "file-1", "message_key": "m3", "asset_type": "file", "status": "retry_scheduled"},
                {"job_key": "video-1", "message_key": "m4", "asset_type": "video", "status": "unavailable"}
            ]
        });

        let report = queue_health(&input);

        assert_eq!(report["queue_health"], "needs_attention");
        assert_eq!(report["completed_count"], 1);
        assert_eq!(report["active_count"], 1);
        assert_eq!(report["retryable_terminal_count"], 1);
        assert_eq!(report["non_retryable_terminal_count"], 1);
        assert_eq!(report["by_asset_status"]["voice"]["failed"], 1);
        assert!(report["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["action"] == "retry_failed_jobs_intentionally"));
    }

    #[test]
    fn detects_duplicate_job_keys() {
        let input = json!({
            "jobs": [
                {"job_key": "same", "message_key": "m1", "asset_type": "voice", "status": "pending"},
                {"job_key": "same", "message_key": "m1", "asset_type": "voice", "status": "pending"}
            ]
        });

        let report = queue_health(&input);

        assert_eq!(report["queue_health"], "needs_attention");
        assert_eq!(report["duplicate_job_key_count"], 1);
        assert_eq!(report["duplicate_message_key_count"], 1);
    }
}
