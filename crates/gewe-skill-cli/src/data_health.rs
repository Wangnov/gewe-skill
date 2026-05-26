use serde_json::Value;

pub(crate) fn maintenance_data_health_report(
    status: Value,
    attachment_queue: Option<Value>,
    voice_issues: Value,
    edge_queue_checked: bool,
    voice_limit: i64,
    edge_queue_limit: u32,
) -> Value {
    let attachment_health = attachment_queue
        .as_ref()
        .and_then(|value| value.get("queue_health"))
        .and_then(Value::as_str)
        .unwrap_or("not_checked");
    let completed_not_ingested_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["completed_not_ingested_count"]))
        .unwrap_or(0);
    let retryable_terminal_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["retryable_terminal_count"]))
        .unwrap_or(0);
    let duplicate_job_key_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["duplicate_job_key_count"]))
        .unwrap_or(0);
    let duplicate_message_key_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["duplicate_message_key_count"]))
        .unwrap_or(0);
    let active_attachment_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["active_count"]))
        .unwrap_or(0);
    let non_retryable_terminal_count = attachment_queue
        .as_ref()
        .map(|value| value_u64_at(value, &["non_retryable_terminal_count"]))
        .unwrap_or(0);
    let voice_issue_count = value_u64_at(&voice_issues, &["issue_count"]);
    let missing_attachment_count =
        value_u64_at(&voice_issues, &["by_issue_type", "missing_attachment"]);
    let retryable_missing_attachment_count =
        count_voice_issues_matching(&voice_issues, Some("missing_attachment"), Some(true), None);
    let known_unavailable_voice_attachment_count = count_voice_issues_matching(
        &voice_issues,
        Some("missing_attachment"),
        Some(false),
        Some("explain_attachment_unavailable"),
    );
    let asr_pending_count = value_u64_at(&voice_issues, &["by_issue_type", "asr_pending"]);
    let asr_failed_count = value_u64_at(&voice_issues, &["by_issue_type", "asr_failed"]);
    let retryable_asr_failed_count =
        count_voice_issues_matching(&voice_issues, Some("asr_failed"), Some(true), None);
    let known_unavailable_asr_count = count_voice_issues_matching(
        &voice_issues,
        Some("asr_failed"),
        Some(false),
        Some("explain_asr_unavailable"),
    );

    let mut next_actions = Vec::<Value>::new();

    if !edge_queue_checked {
        next_actions.push(maintenance_action(
            10,
            "inspect_edge_attachment_queue",
            vec!["maintenance", "data-health", "--with-edge-queue"],
            "edge attachment queue was not checked, so media readiness is only partially known",
            true,
        ));
    }
    if duplicate_job_key_count > 0 {
        next_actions.push(maintenance_action(
            20,
            "investigate_attachment_queue_duplicate_jobs",
            vec!["maintenance", "attachment-queue-health", "--limit", "1000"],
            "duplicate edge attachment job keys can make bulk retries unsafe",
            true,
        ));
    }
    if retryable_terminal_count > 0 {
        next_actions.push(maintenance_action(
            30,
            "retry_failed_attachment_jobs",
            vec![
                "sync",
                "attachment-retry",
                "--status",
                "failed",
                "--limit",
                "20",
            ],
            "failed edge attachment jobs may be retried deliberately before media analysis",
            true,
        ));
    }
    if completed_not_ingested_count > 0 {
        next_actions.push(maintenance_action(
            40,
            "sync_completed_attachments",
            vec!["sync", "attachments", "--limit", "50"],
            "edge has completed attachment downloads that are not yet present in memory",
            true,
        ));
    }
    if active_attachment_count > 0 {
        next_actions.push(maintenance_action(
            50,
            "wait_or_requeue_active_attachment_jobs",
            vec!["sync", "attachment-requeue"],
            "some attachment jobs are still pending, processing, or scheduled for retry",
            false,
        ));
    }
    if retryable_missing_attachment_count > 0 {
        next_actions.push(maintenance_action(
            60,
            "repair_missing_voice_attachments",
            vec!["sync", "attachment-repair", "--asset-type", "voice"],
            "some voice messages still do not have synced audio attachments",
            true,
        ));
    }
    if retryable_asr_failed_count > 0 {
        next_actions.push(maintenance_action(
            70,
            "inspect_failed_asr",
            vec!["maintenance", "voice-issues", "--limit", "50"],
            "some ASR attempts already failed; inspect provider or decoder errors before retrying",
            true,
        ));
    }
    if asr_pending_count > 0 {
        next_actions.push(maintenance_action(
            80,
            "backfill_pending_asr",
            vec!["maintenance", "asr-backfill", "--limit", "50"],
            "some voice attachments are present but not transcribed yet",
            true,
        ));
    }
    if next_actions.is_empty() {
        next_actions.push(maintenance_action(
            100,
            "ready_for_analysis",
            vec!["query", "messages"],
            "no blocking attachment or voice transcript maintenance issue was found in this bounded check",
            false,
        ));
    }

    let blocking_action_count = next_actions
        .iter()
        .filter(|action| {
            action
                .get("blocks_analysis")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let ready_for_analysis = blocking_action_count == 0 && edge_queue_checked;
    let overall_health = if duplicate_job_key_count > 0
        || retryable_terminal_count > 0
        || retryable_asr_failed_count > 0
    {
        "needs_attention"
    } else if completed_not_ingested_count > 0 || retryable_missing_attachment_count > 0 {
        "needs_attachment_sync"
    } else if asr_pending_count > 0 {
        "needs_asr"
    } else if !edge_queue_checked {
        "partial"
    } else if non_retryable_terminal_count > 0
        || known_unavailable_voice_attachment_count > 0
        || known_unavailable_asr_count > 0
    {
        "ready_with_known_gaps"
    } else {
        "healthy"
    };

    serde_json::json!({
        "ok": true,
        "query_mode": "maintenance_data_health",
        "overall_health": overall_health,
        "ready_for_analysis": ready_for_analysis,
        "confidence": if edge_queue_checked { "high" } else { "partial" },
        "limits": {
            "voice_limit": voice_limit,
            "edge_queue_limit": edge_queue_limit,
        },
        "summary": {
            "attachment_queue_checked": edge_queue_checked,
            "attachment_queue_health": attachment_health,
            "completed_not_ingested_count": completed_not_ingested_count,
            "retryable_terminal_count": retryable_terminal_count,
            "non_retryable_terminal_count": non_retryable_terminal_count,
            "duplicate_job_key_count": duplicate_job_key_count,
            "duplicate_message_key_count": duplicate_message_key_count,
            "multi_job_message_key_count": duplicate_message_key_count,
            "active_attachment_count": active_attachment_count,
            "voice_issue_count": voice_issue_count,
            "missing_voice_attachment_count": missing_attachment_count,
            "retryable_missing_voice_attachment_count": retryable_missing_attachment_count,
            "known_unavailable_voice_attachment_count": known_unavailable_voice_attachment_count,
            "asr_pending_count": asr_pending_count,
            "asr_failed_count": asr_failed_count,
            "retryable_asr_failed_count": retryable_asr_failed_count,
            "known_unavailable_asr_count": known_unavailable_asr_count,
            "blocking_action_count": blocking_action_count,
        },
        "next_actions": next_actions,
        "status": status,
        "attachment_queue": {
            "included": edge_queue_checked,
            "health": attachment_queue,
        },
        "voice": voice_issues,
        "agent_notes": [
            "run maintenance data-health before broad media or voice analysis when freshness matters",
            "ready_for_analysis is high-confidence only when --with-edge-queue was used",
            "next_actions are ordered so attachment sync and queue safety are handled before ASR",
            "multi_job_message_key_count can be normal for attachment variants such as image hd/normal/thumb and does not block analysis by itself",
            "non-retryable unavailable or purged media may still leave known gaps even when analysis can continue",
            "non-retryable ASR decoder failures are known transcript gaps and do not block broader analysis"
        ]
    })
}

fn maintenance_action(
    priority: u32,
    action: &str,
    recommended_cli: Vec<&str>,
    reason: &str,
    blocks_analysis: bool,
) -> Value {
    serde_json::json!({
        "priority": priority,
        "action": action,
        "recommended_cli": recommended_cli,
        "reason": reason,
        "blocks_analysis": blocks_analysis,
    })
}

fn value_u64_at(value: &Value, path: &[&str]) -> u64 {
    let mut current = value;
    for segment in path {
        let Some(next) = current.get(*segment) else {
            return 0;
        };
        current = next;
    }
    current
        .as_u64()
        .or_else(|| current.as_i64().and_then(|value| u64::try_from(value).ok()))
        .unwrap_or(0)
}

fn count_voice_issues_matching(
    voice_issues: &Value,
    issue_type: Option<&str>,
    retryable: Option<bool>,
    recommended_action: Option<&str>,
) -> u64 {
    let Some(issues) = voice_issues.get("issues").and_then(Value::as_array) else {
        return 0;
    };
    issues
        .iter()
        .filter(|issue| {
            issue_type
                .map(|expected| issue.get("issue_type").and_then(Value::as_str) == Some(expected))
                .unwrap_or(true)
                && retryable
                    .map(|expected| {
                        issue.get("retryable").and_then(Value::as_bool) == Some(expected)
                    })
                    .unwrap_or(true)
                && recommended_action
                    .map(|expected| {
                        issue.get("recommended_action").and_then(Value::as_str) == Some(expected)
                    })
                    .unwrap_or(true)
        })
        .count() as u64
}
