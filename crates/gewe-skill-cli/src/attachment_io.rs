use super::{
    edge_admin_get_json, edge_admin_post_json, edge_admin_url, print_json, sync_one_attachment,
    EdgeAttachmentResponse,
};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_types::AttachmentRecord;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn write_file_or_stdout(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if path == Path::new("-") {
        std::io::stdout().lock().write_all(bytes)?;
    } else {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}

pub(super) fn select_attachment(
    items: &[AttachmentRecord],
    index: Option<u32>,
) -> Result<&AttachmentRecord, Box<dyn std::error::Error>> {
    if items.is_empty() {
        return Err("该消息尚无附件记录；可运行 gewe sync attachment-repair --message-key '<key>' 定向补拉。".into());
    }
    if items.len() > 1 && index.is_none() {
        let choices = items.iter().enumerate().map(|(i, a)| serde_json::json!({ "index": i+1, "kind": a.kind, "variant": a.variant, "sha256": a.sha256, "mime_type": a.mime_type })).collect::<Vec<_>>();
        return Err(format!(
            "该消息有多个附件，请添加 --index N 选择一个：{}",
            serde_json::to_string(&choices)?
        )
        .into());
    }
    let index = usize::try_from(index.unwrap_or(1))?
        .checked_sub(1)
        .ok_or("--index 从 1 开始")?;
    items
        .get(index)
        .ok_or_else(|| format!("附件 --index 超出范围，共 {} 个", items.len()).into())
}

pub(super) async fn download_selected_attachment(
    client: &GeweSkillClient,
    sha256: Option<String>,
    message_key: Option<String>,
    index: Option<u32>,
    output: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let sha256 = if let Some(sha256) = sha256 {
        sha256
    } else {
        let key = message_key
            .as_ref()
            .ok_or("请指定 --message-key 或 --sha256")?;
        let records = client
            .attachments_by_message_keys(std::slice::from_ref(key))
            .await?;
        select_attachment(&records.items, index)?
            .sha256
            .clone()
            .ok_or("附件尚无 sha256，无法下载")?
    };
    let bytes = client.download_attachment(&sha256).await?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&sha256) {
        return Err("附件校验失败：下载内容的 SHA-256 与记录不符，未写入输出文件".into());
    }
    write_file_or_stdout(&output, &bytes)?;
    if output != Path::new("-") {
        print_json(
            serde_json::json!({ "ok": true, "sha256": sha256, "message_key": message_key, "output": output, "size_bytes": bytes.len() }),
        )?;
    }
    Ok(())
}

pub(super) async fn repair_one_message_attachment(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    message_key: &str,
    wait_seconds: u64,
    attachment_dir: &Path,
) -> Result<Value, Box<dyn std::error::Error>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let repair = async {
        let queued = edge_admin_post_json(
            &http,
            edge_url,
            admin_token,
            "/admin/repair-attachment",
            vec![("message_key", Some(message_key.to_string()))],
        )
        .await?;
        if queued["ok"] == false {
            return Ok(queued);
        }
        let mut synced = std::collections::BTreeSet::new();
        loop {
            let manifest_url = edge_admin_url(
                edge_url,
                "/admin/attachments",
                vec![
                    ("message_key", Some(message_key.to_string())),
                    ("limit", Some("500".to_string())),
                ],
            )?;
            let manifest: EdgeAttachmentResponse = http
                .get(manifest_url)
                .bearer_auth(admin_token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            // Reject an unfiltered old edge response; never write unrelated attachments.
            if manifest
                .attachments
                .iter()
                .any(|item| item.message_key.as_deref() != Some(message_key))
            {
                return Err("edge 未按消息过滤，请升级 edge 后再使用定向补拉".into());
            }
            for item in &manifest.attachments {
                if synced.contains(&item.job_id) {
                    continue;
                }
                sync_one_attachment(
                    client,
                    &http,
                    edge_url.trim_end_matches('/'),
                    admin_token,
                    item,
                    attachment_dir,
                )
                .await?;
                synced.insert(item.job_id);
            }
            let jobs = scoped_edge_jobs(&http, edge_url, admin_token, message_key).await?;
            let active = jobs.iter().any(|job| {
                matches!(
                    job["status"].as_str(),
                    Some("pending" | "processing" | "retry_scheduled")
                )
            });
            if !active {
                // A job can finish between reading the manifest and reading the queue.
                if jobs.iter().any(|job| {
                    job["status"] == "completed"
                        && job["job_id"]
                            .as_i64()
                            .is_some_and(|id| !synced.contains(&id))
                }) {
                    continue;
                }
                let records = client
                    .attachments_by_message_keys(&[message_key.to_string()])
                    .await?;
                let ok = !records.items.is_empty()
                    && jobs.iter().all(|job| job["status"] == "completed");
                return Ok(
                    serde_json::json!({ "ok": ok, "query_mode": "message_attachment_repair", "message_key": message_key,
                    "queued": queued, "synced": manifest.attachments.len(), "attachments": records.items, "jobs": jobs,
                    "hint": if ok { "附件已同步回记忆层，可按消息下载" } else { "下载任务已结束但附件不完整，请查看 jobs 的状态与 last_error" } }),
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };
    match tokio::time::timeout(Duration::from_secs(wait_seconds), repair).await {
        Ok(result) => result,
        Err(_) => Ok(
            serde_json::json!({ "ok": false, "status": "timeout", "message_key": message_key, "wait_seconds": wait_seconds,
            "hint": "定向下载仍在 edge 进行，已完成的附件已同步；稍后用相同 message-key 重试。全局同步游标未改变。" }),
        ),
    }
}

async fn scoped_edge_jobs(
    http: &reqwest::Client,
    edge_url: &str,
    admin_token: &str,
    message_key: &str,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let response = edge_admin_get_json(
        http,
        edge_url,
        admin_token,
        "/admin/download-jobs",
        vec![
            ("message_key", Some(message_key.to_string())),
            ("limit", Some("500".to_string())),
        ],
    )
    .await?;
    let jobs = response["jobs"].as_array().ok_or("edge 下载任务响应无效")?;
    if jobs
        .iter()
        .any(|job| job["message_key"].as_str() != Some(message_key))
    {
        return Err("edge 未按消息过滤下载任务，请先升级 edge".into());
    }
    Ok(jobs.clone())
}
