use std::collections::BTreeMap;

use gewe_skill_client::GeweSkillClient;
use gewe_skill_types::{VoiceItem, VoiceQuery, VoiceWarmRequest};
use serde_json::{json, Value};

pub(crate) async fn voice_issues(
    client: &GeweSkillClient,
    query: VoiceQuery,
) -> Result<Value, Box<dyn std::error::Error>> {
    voice_issues_with_edge_queue(client, query, None).await
}

pub(crate) async fn voice_issues_with_edge_queue(
    client: &GeweSkillClient,
    query: VoiceQuery,
    edge_queue: Option<Value>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let page = client.voices(&query).await?;
    let scanned = page.items.len();
    let mut by_issue_type = BTreeMap::<String, usize>::new();
    let mut by_action = BTreeMap::<String, usize>::new();
    let mut issues = Vec::new();
    let edge_jobs = edge_queue_jobs(edge_queue.as_ref());
    let mut edge_queue_matches = 0usize;

    for item in page.items {
        let Some(mut issue) = voice_issue_json(&item) else {
            continue;
        };
        if enrich_missing_attachment_with_edge_queue(&mut issue, &edge_jobs) {
            edge_queue_matches += 1;
        }
        if let Some(issue_type) = issue.get("issue_type").and_then(Value::as_str) {
            *by_issue_type.entry(issue_type.to_string()).or_default() += 1;
        }
        if let Some(action) = issue.get("recommended_action").and_then(Value::as_str) {
            *by_action.entry(action.to_string()).or_default() += 1;
        }
        issues.push(issue);
    }

    Ok(json!({
        "ok": true,
        "query_mode": "maintenance_voice_issues",
        "voice_query": query,
        "scanned": scanned,
        "issue_count": issues.len(),
        "by_issue_type": by_issue_type,
        "by_recommended_action": by_action,
        "edge_queue_checked": edge_queue.is_some(),
        "edge_queue_match_count": edge_queue_matches,
        "issues": issues,
        "agent_hints": [
            "missing_attachment means the message exists but audio bytes are not synced yet; run sync attachments before retrying ASR",
            "when edge_queue_evidence.status is unavailable or purged, the edge queue already proved the upstream attachment cannot currently be downloaded",
            "when edge_queue_evidence.status is failed, use sync attachment-retry intentionally; do not retry unavailable resources in a loop",
            "asr_pending means audio bytes are present and a bounded ASR warm/transcribe pass can fill the transcript",
            "asr_failed means ASR has already failed once; retry only when the provider issue is temporary or the decoder issue has been fixed",
            "asr_failed with retryable=false is a known transcript gap and should be explained instead of retried in a loop",
            "keep message_key, attachment sha256, and transcript status as evidence when explaining voice coverage"
        ]
    }))
}

pub(crate) async fn voice_repair(
    client: &GeweSkillClient,
    query: VoiceQuery,
    provider: Option<String>,
    language: Option<String>,
    force: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let before = voice_issues(client, query.clone()).await?;
    let repair = client
        .warm_voice(&VoiceWarmRequest {
            conversation_id: query.conversation_id.clone(),
            sender_wxid: query.sender_wxid.clone(),
            after: query.after.clone(),
            before: query.before.clone(),
            limit: query.limit,
            provider: provider.clone(),
            language: language.clone(),
            force: Some(force),
        })
        .await?;
    let after = voice_issues(client, query.clone()).await?;
    let delta = voice_issue_delta(&before, &after);

    Ok(json!({
        "ok": true,
        "query_mode": "maintenance_voice_repair",
        "voice_query": query,
        "repair_request": {
            "provider": provider,
            "language": language,
            "force": force
        },
        "before": before,
        "repair": repair,
        "after": after,
        "delta": delta,
        "agent_hints": [
            "voice-repair runs bounded ASR warm; it does not sync missing attachments because that requires edge admin credentials",
            "if missing_attachment remains after repair, run sync attachments first and then rerun voice-repair",
            "if asr_failed remains, inspect transcript_error before retrying repeatedly"
        ]
    }))
}

fn voice_issue_json(item: &VoiceItem) -> Option<Value> {
    let transcript_status = item
        .transcript
        .as_ref()
        .map(|record| record.status.as_str());
    let transcript_error = item
        .transcript
        .as_ref()
        .and_then(|record| record.error.clone());
    let issue = classify_voice_issue(
        &item.availability,
        transcript_status,
        transcript_error.as_deref(),
        item.attachment.is_some(),
    )?;
    let message_key = item.message.message_key.clone();

    Some(json!({
        "issue_type": issue.issue_type,
        "severity": issue.severity,
        "retryable": issue.retryable,
        "recommended_action": issue.recommended_action,
        "recommended_cli": recommended_cli(issue.recommended_action, &message_key),
        "message_key": message_key,
        "availability": item.availability,
        "reason": item.reason,
        "attachment_sha256": item.attachment.as_ref().and_then(|record| record.sha256.clone()),
        "attachment_object_key": item.attachment.as_ref().and_then(|record| record.object_key.clone()),
        "transcript_status": transcript_status,
        "transcript_error": transcript_error,
        "message_evidence": message_evidence(item),
        "attachment_evidence": attachment_evidence(item),
        "transcript_evidence": transcript_evidence(item)
    }))
}

fn enrich_missing_attachment_with_edge_queue(issue: &mut Value, edge_jobs: &[&Value]) -> bool {
    if issue.get("issue_type").and_then(Value::as_str) != Some("missing_attachment") {
        return false;
    }

    let Some(edge_match) = matching_edge_job(issue, edge_jobs) else {
        return false;
    };
    let status = edge_match.job.get("status").and_then(Value::as_str);
    let action = edge_queue_action(status);
    let issue_message_key = issue
        .get("message_key")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if let Some(object) = issue.as_object_mut() {
        object.insert(
            "edge_queue_evidence".to_string(),
            edge_job_evidence(
                edge_match.job,
                edge_match.match_kind,
                edge_match.new_msg_id_delta,
            ),
        );
        object.insert(
            "recommended_action".to_string(),
            Value::String(action.recommended_action.to_string()),
        );
        object.insert(
            "severity".to_string(),
            Value::String(action.severity.to_string()),
        );
        object.insert("retryable".to_string(), Value::Bool(action.retryable));
        object.insert(
            "recommended_cli".to_string(),
            json!(recommended_cli(
                action.recommended_action,
                &issue_message_key
            )),
        );
        object.insert(
            "edge_queue_interpretation".to_string(),
            Value::String(action.interpretation.to_string()),
        );
    }

    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeQueueAction {
    recommended_action: &'static str,
    severity: &'static str,
    retryable: bool,
    interpretation: &'static str,
}

fn edge_queue_action(status: Option<&str>) -> EdgeQueueAction {
    match status.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "completed" => EdgeQueueAction {
            recommended_action: "sync_attachment",
            severity: "warning",
            retryable: true,
            interpretation: "edge queue has a completed attachment; sync completed attachments into memory",
        },
        "failed" => EdgeQueueAction {
            recommended_action: "retry_edge_attachment",
            severity: "warning",
            retryable: true,
            interpretation: "edge queue has a failed attachment job; retry intentionally, then inspect the new terminal status",
        },
        "pending" | "retry_scheduled" | "processing" => EdgeQueueAction {
            recommended_action: "requeue_edge_attachment",
            severity: "info",
            retryable: true,
            interpretation: "edge queue has not reached a terminal result; requeue or wait before judging the attachment missing",
        },
        "unavailable" | "purged" => EdgeQueueAction {
            recommended_action: "explain_attachment_unavailable",
            severity: "warning",
            retryable: false,
            interpretation: "edge queue already reached a terminal unavailable state; explain that the upstream attachment cannot currently be downloaded",
        },
        _ => EdgeQueueAction {
            recommended_action: "inspect_attachment_queue",
            severity: "warning",
            retryable: true,
            interpretation: "edge queue has related evidence; inspect queue status before retrying ASR",
        },
    }
}

struct EdgeJobMatch<'a> {
    job: &'a Value,
    match_kind: &'static str,
    new_msg_id_delta: Option<i128>,
    rank: u8,
    job_id: i64,
}

fn matching_edge_job<'a>(issue: &Value, edge_jobs: &[&'a Value]) -> Option<EdgeJobMatch<'a>> {
    let issue_key = issue.get("message_key").and_then(Value::as_str)?;
    edge_jobs
        .iter()
        .filter_map(|job| match_edge_job(issue_key, job))
        .max_by(|left, right| {
            right
                .rank
                .cmp(&left.rank)
                .then_with(|| {
                    right
                        .new_msg_id_delta
                        .unwrap_or(i128::MAX)
                        .cmp(&left.new_msg_id_delta.unwrap_or(i128::MAX))
                })
                .then_with(|| left.job_id.cmp(&right.job_id))
        })
}

fn match_edge_job<'a>(issue_key: &str, job: &'a Value) -> Option<EdgeJobMatch<'a>> {
    let job_key = job.get("message_key").and_then(Value::as_str)?;
    let job_id = job
        .get("job_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();

    if issue_key == job_key {
        return Some(EdgeJobMatch {
            job,
            match_kind: "exact_message_key",
            new_msg_id_delta: Some(0),
            rank: 0,
            job_id,
        });
    }

    if strip_schema_prefix(issue_key) == strip_schema_prefix(job_key) {
        return Some(EdgeJobMatch {
            job,
            match_kind: "schema_prefix_normalized",
            new_msg_id_delta: Some(0),
            rank: 1,
            job_id,
        });
    }

    let issue_parts = message_key_parts(issue_key)?;
    let job_parts = message_key_parts(job_key)?;
    let delta = (issue_parts.new_msg_id - job_parts.new_msg_id).abs();
    if issue_parts.appid == job_parts.appid && delta <= 1024 {
        return Some(EdgeJobMatch {
            job,
            match_kind: "appid_new_msg_id_approx",
            new_msg_id_delta: Some(delta),
            rank: 2,
            job_id,
        });
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageKeyParts {
    appid: String,
    new_msg_id: i128,
}

fn message_key_parts(message_key: &str) -> Option<MessageKeyParts> {
    let parts = message_key.split(':').collect::<Vec<_>>();
    let (appid, new_msg_id) = match parts.as_slice() {
        [schema, appid, new_msg_id] if *schema == "v1" || *schema == "v2" => (*appid, *new_msg_id),
        [appid, new_msg_id] => (*appid, *new_msg_id),
        _ => return None,
    };
    Some(MessageKeyParts {
        appid: appid.to_string(),
        new_msg_id: new_msg_id.parse().ok()?,
    })
}

fn strip_schema_prefix(message_key: &str) -> &str {
    message_key
        .strip_prefix("v1:")
        .or_else(|| message_key.strip_prefix("v2:"))
        .unwrap_or(message_key)
}

fn edge_queue_jobs(edge_queue: Option<&Value>) -> Vec<&Value> {
    edge_queue
        .and_then(|value| value.get("jobs"))
        .and_then(Value::as_array)
        .map(|jobs| jobs.iter().collect())
        .unwrap_or_default()
}

fn edge_job_evidence(job: &Value, match_kind: &str, new_msg_id_delta: Option<i128>) -> Value {
    json!({
        "job_id": job.get("job_id").cloned().unwrap_or(Value::Null),
        "job_key": job.get("job_key").cloned().unwrap_or(Value::Null),
        "message_key": job.get("message_key").cloned().unwrap_or(Value::Null),
        "status": job.get("status").cloned().unwrap_or(Value::Null),
        "asset_type": job.get("asset_type").cloned().unwrap_or(Value::Null),
        "attempts": job.get("attempts").cloned().unwrap_or(Value::Null),
        "created_at": job.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": job.get("updated_at").cloned().unwrap_or(Value::Null),
        "terminal_at": job.get("terminal_at").cloned().unwrap_or(Value::Null),
        "last_error": job.get("last_error").cloned().unwrap_or(Value::Null),
        "match_kind": match_kind,
        "new_msg_id_delta": new_msg_id_delta
    })
}

fn message_evidence(item: &VoiceItem) -> Value {
    json!({
        "message_key": item.message.message_key,
        "conversation_id": item.message.conversation_id,
        "sender_wxid": item.message.sender_wxid,
        "received_at": item.message.received_at,
        "schema_version": item.message.schema_version,
        "msg_id": item.message.msg_id,
        "new_msg_id": item.message.new_msg_id,
        "msg_type": item.message.msg_type,
        "kind": item.message.kind,
        "is_group": item.message.is_group,
        "is_outgoing": item.message.is_outgoing
    })
}

fn attachment_evidence(item: &VoiceItem) -> Option<Value> {
    item.attachment.as_ref().map(|record| {
        json!({
            "id": record.id,
            "edge_job_id": record.edge_job_id,
            "message_key": record.message_key,
            "kind": record.kind,
            "variant": record.variant,
            "object_key": record.object_key,
            "sha256": record.sha256,
            "size_bytes": record.size_bytes,
            "mime_type": record.mime_type,
            "created_at": record.created_at
        })
    })
}

fn transcript_evidence(item: &VoiceItem) -> Option<Value> {
    item.transcript.as_ref().map(|record| {
        json!({
            "message_key": record.message_key,
            "attachment_sha256": record.attachment_sha256,
            "provider": record.provider,
            "language": record.language,
            "status": record.status,
            "error": record.error,
            "duration_ms": record.duration_ms,
            "created_at": record.created_at,
            "updated_at": record.updated_at
        })
    })
}

fn recommended_cli(action: &str, message_key: &str) -> Vec<String> {
    match action {
        "sync_attachment" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "sync".to_string(),
            "attachments".to_string(),
        ],
        "retry_edge_attachment" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "sync".to_string(),
            "attachment-retry".to_string(),
            "--status".to_string(),
            "failed".to_string(),
            "--asset-type".to_string(),
            "voice".to_string(),
            "--limit".to_string(),
            "20".to_string(),
        ],
        "requeue_edge_attachment" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "sync".to_string(),
            "attachment-requeue".to_string(),
        ],
        "explain_attachment_unavailable" | "inspect_attachment_queue" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "sync".to_string(),
            "attachment-queue".to_string(),
            "--asset-type".to_string(),
            "voice".to_string(),
            "--limit".to_string(),
            "20".to_string(),
        ],
        "retry_asr" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "voice".to_string(),
            "transcribe".to_string(),
            "--message-key".to_string(),
            message_key.to_string(),
            "--provider".to_string(),
            "codex-asr".to_string(),
            "--language".to_string(),
            "zh".to_string(),
            "--force".to_string(),
        ],
        "explain_asr_unavailable" => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "maintenance".to_string(),
            "voice-issues".to_string(),
            "--limit".to_string(),
            "50".to_string(),
        ],
        _ => vec![
            "gewe-skill".to_string(),
            "--json".to_string(),
            "voice".to_string(),
            "transcribe".to_string(),
            "--message-key".to_string(),
            message_key.to_string(),
            "--provider".to_string(),
            "codex-asr".to_string(),
            "--language".to_string(),
            "zh".to_string(),
        ],
    }
}

fn voice_issue_delta(before: &Value, after: &Value) -> Value {
    let before_total = issue_count(before);
    let after_total = issue_count(after);
    let issue_types = ["missing_attachment", "asr_pending", "asr_failed"];
    let by_issue_type = issue_types
        .iter()
        .map(|issue_type| {
            let before_count = issue_type_count(before, issue_type);
            let after_count = issue_type_count(after, issue_type);
            (
                (*issue_type).to_string(),
                json!({
                    "before": before_count,
                    "after": after_count,
                    "resolved": before_count.saturating_sub(after_count),
                    "new": after_count.saturating_sub(before_count)
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    json!({
        "before_issue_count": before_total,
        "after_issue_count": after_total,
        "resolved_issue_count": before_total.saturating_sub(after_total),
        "new_issue_count": after_total.saturating_sub(before_total),
        "by_issue_type": by_issue_type
    })
}

fn issue_count(value: &Value) -> usize {
    value
        .get("issue_count")
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize
}

fn issue_type_count(value: &Value, issue_type: &str) -> usize {
    value
        .get("by_issue_type")
        .and_then(|counts| counts.get(issue_type))
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VoiceIssueClassification {
    issue_type: &'static str,
    recommended_action: &'static str,
    severity: &'static str,
    retryable: bool,
}

fn classify_voice_issue(
    availability: &str,
    transcript_status: Option<&str>,
    transcript_error: Option<&str>,
    has_attachment: bool,
) -> Option<VoiceIssueClassification> {
    let availability = availability.trim().to_ascii_lowercase();
    let transcript_status = transcript_status.map(|status| status.trim().to_ascii_lowercase());

    if availability == "missing_attachment" || !has_attachment {
        return Some(VoiceIssueClassification {
            issue_type: "missing_attachment",
            recommended_action: "sync_attachment",
            severity: "warning",
            retryable: true,
        });
    }

    if transcript_status.as_deref() == Some("completed") || availability == "transcribed" {
        return None;
    }

    if transcript_status.as_deref() == Some("failed")
        || availability == "asr_failed"
        || availability == "failed"
    {
        if is_non_retryable_asr_error(transcript_error) {
            return Some(VoiceIssueClassification {
                issue_type: "asr_failed",
                recommended_action: "explain_asr_unavailable",
                severity: "warning",
                retryable: false,
            });
        }
        return Some(VoiceIssueClassification {
            issue_type: "asr_failed",
            recommended_action: "retry_asr",
            severity: "warning",
            retryable: true,
        });
    }

    if has_attachment {
        return Some(VoiceIssueClassification {
            issue_type: "asr_pending",
            recommended_action: "transcribe_asr",
            severity: "info",
            retryable: true,
        });
    }

    None
}

fn is_non_retryable_asr_error(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let error = error.to_ascii_lowercase();
    error.contains("rust-silk decode failed")
        || error.contains("decode failed")
        || error.contains("invalid audio")
        || error.contains("unsupported audio")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_issue_classifies_missing_attachment_first() {
        assert_eq!(
            classify_voice_issue("missing_attachment", Some("failed"), None, false),
            Some(VoiceIssueClassification {
                issue_type: "missing_attachment",
                recommended_action: "sync_attachment",
                severity: "warning",
                retryable: true,
            })
        );
    }

    #[test]
    fn voice_issue_classifies_failed_transcript_as_retryable_asr() {
        assert_eq!(
            classify_voice_issue("asr_failed", Some("failed"), None, true),
            Some(VoiceIssueClassification {
                issue_type: "asr_failed",
                recommended_action: "retry_asr",
                severity: "warning",
                retryable: true,
            })
        );
    }

    #[test]
    fn voice_issue_classifies_decoder_failure_as_known_asr_gap() {
        assert_eq!(
            classify_voice_issue(
                "asr_failed",
                Some("failed"),
                Some("asr_http_400: rust-silk decode failed with status exit status: 1"),
                true
            ),
            Some(VoiceIssueClassification {
                issue_type: "asr_failed",
                recommended_action: "explain_asr_unavailable",
                severity: "warning",
                retryable: false,
            })
        );
    }

    #[test]
    fn voice_issue_skips_completed_transcripts() {
        assert_eq!(
            classify_voice_issue("transcribed", Some("completed"), None, true),
            None
        );
    }

    #[test]
    fn voice_issue_delta_counts_resolved_by_type() {
        let before = json!({
            "issue_count": 4,
            "by_issue_type": {
                "missing_attachment": 2,
                "asr_pending": 1,
                "asr_failed": 1
            }
        });
        let after = json!({
            "issue_count": 2,
            "by_issue_type": {
                "missing_attachment": 2
            }
        });

        let delta = voice_issue_delta(&before, &after);

        assert_eq!(delta["resolved_issue_count"], 2);
        assert_eq!(delta["by_issue_type"]["asr_pending"]["resolved"], 1);
        assert_eq!(delta["by_issue_type"]["asr_failed"]["resolved"], 1);
        assert_eq!(delta["by_issue_type"]["missing_attachment"]["resolved"], 0);
    }

    #[test]
    fn edge_job_matches_v1_message_key_with_js_precision_delta() {
        let issue = json!({
            "message_key": "v1:wx_XrdYB8DiTE4KlelN4wMjY:1650984009761036000"
        });
        let job = json!({
            "job_id": 7,
            "message_key": "wx_XrdYB8DiTE4KlelN4wMjY:1650984009761035946",
            "status": "unavailable"
        });

        let matched = matching_edge_job(&issue, &[&job]).expect("matched edge job");

        assert_eq!(matched.match_kind, "appid_new_msg_id_approx");
        assert_eq!(matched.new_msg_id_delta, Some(54));
    }

    #[test]
    fn missing_attachment_with_unavailable_edge_job_is_not_retryable() {
        let mut issue = json!({
            "issue_type": "missing_attachment",
            "recommended_action": "sync_attachment",
            "severity": "warning",
            "retryable": true,
            "message_key": "v1:wx_XrdYB8DiTE4KlelN4wMjY:8304142007787887000"
        });
        let job = json!({
            "job_id": 16,
            "job_key": "wx_XrdYB8DiTE4KlelN4wMjY:8304142007787886516:voice:silk:/gewe/v2/api/message/downloadVoice",
            "message_key": "wx_XrdYB8DiTE4KlelN4wMjY:8304142007787886516",
            "asset_type": "voice",
            "status": "unavailable",
            "attempts": 1,
            "last_error": "Gewe API error 200: NullPointerException"
        });

        assert!(enrich_missing_attachment_with_edge_queue(
            &mut issue,
            &[&job]
        ));

        assert_eq!(
            issue["recommended_action"],
            "explain_attachment_unavailable"
        );
        assert_eq!(issue["retryable"], false);
        assert_eq!(issue["edge_queue_evidence"]["status"], "unavailable");
        assert_eq!(
            issue["edge_queue_evidence"]["match_kind"],
            "appid_new_msg_id_approx"
        );
    }
}
