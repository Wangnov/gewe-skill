use std::collections::BTreeMap;

use gewe_skill_client::GeweSkillClient;
use gewe_skill_types::{VoiceItem, VoiceQuery};
use serde_json::{json, Value};

pub(crate) async fn voice_issues(
    client: &GeweSkillClient,
    query: VoiceQuery,
) -> Result<Value, Box<dyn std::error::Error>> {
    let page = client.voices(&query).await?;
    let scanned = page.items.len();
    let mut by_issue_type = BTreeMap::<String, usize>::new();
    let mut by_action = BTreeMap::<String, usize>::new();
    let mut issues = Vec::new();

    for item in page.items {
        let Some(issue) = voice_issue_json(&item) else {
            continue;
        };
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
        "issues": issues,
        "agent_hints": [
            "missing_attachment means the message exists but audio bytes are not synced yet; run sync attachments before retrying ASR",
            "asr_pending means audio bytes are present and a bounded ASR warm/transcribe pass can fill the transcript",
            "asr_failed means ASR has already failed once; retry only when the provider or decoder issue has been fixed",
            "keep message_key, attachment sha256, and transcript status as evidence when explaining voice coverage"
        ]
    }))
}

fn voice_issue_json(item: &VoiceItem) -> Option<Value> {
    let transcript_status = item
        .transcript
        .as_ref()
        .map(|record| record.status.as_str());
    let issue = classify_voice_issue(
        &item.availability,
        transcript_status,
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
        "transcript_error": item.transcript.as_ref().and_then(|record| record.error.clone()),
        "message_evidence": message_evidence(item),
        "attachment_evidence": attachment_evidence(item),
        "transcript_evidence": transcript_evidence(item)
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_issue_classifies_missing_attachment_first() {
        assert_eq!(
            classify_voice_issue("missing_attachment", Some("failed"), false),
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
            classify_voice_issue("asr_failed", Some("failed"), true),
            Some(VoiceIssueClassification {
                issue_type: "asr_failed",
                recommended_action: "retry_asr",
                severity: "warning",
                retryable: true,
            })
        );
    }

    #[test]
    fn voice_issue_skips_completed_transcripts() {
        assert_eq!(
            classify_voice_issue("transcribed", Some("completed"), true),
            None
        );
    }
}
