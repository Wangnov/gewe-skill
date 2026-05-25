use clap::{Parser, Subcommand};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::normalize_callback;
use gewe_skill_types::RawCallbackRequest;
use serde::Deserialize;
use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(Debug, Parser)]
#[command(name = "gewe-skill", version, about = "Operate and query gewe-skill memory")]
struct Cli {
    #[arg(long, env = "GEWE_SKILL_BASE_URL", default_value = "http://127.0.0.1:8788")]
    base_url: String,

    #[arg(long, env = "GEWE_SKILL_READ_TOKEN")]
    read_token: Option<String>,

    #[arg(long, env = "GEWE_SKILL_WRITE_TOKEN")]
    write_token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check memory service health.
    Health,
    /// List recent normalized messages.
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Search messages by keyword.
    Search {
        #[arg(long)]
        q: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// List conversations ordered by last message time.
    Conversations {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List chatroom member diff events.
    ChatroomEvents {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List structured chatroom system events.
    ChatroomSystemEvents {
        #[arg(long)]
        chatroom_id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Normalize a raw GeWe callback JSON file and print the ingest payload.
    Normalize {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        received_at: Option<String>,
    },
    /// Normalize a raw GeWe callback JSON file and send it to memory.
    IngestFile {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        received_at: Option<String>,
    },
    /// Pull raw events from gewe-skill-edge admin export and write them to memory.
    SyncEdge {
        #[arg(long, env = "GEWE_SKILL_EDGE_URL", default_value = "https://gewe-agent.wangnov-ai.com")]
        edge_url: String,
        #[arg(long, env = "GEWE_SKILL_EDGE_ADMIN_TOKEN")]
        admin_token: String,
        #[arg(long)]
        after_raw_event_id: Option<i64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long, env = "GEWE_SKILL_SYNC_CURSOR_FILE", default_value = "/opt/gewe-skill-memory/data/edge-sync.cursor")]
        cursor_file: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct EdgeExportResponse {
    events: Vec<EdgeExportEvent>,
    next_after_raw_event_id: i64,
}

#[derive(Debug, Deserialize)]
struct EdgeExportEvent {
    raw_event_id: i64,
    received_at: String,
    body: Option<Value>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = build_client(&cli)?;

    match cli.command {
        Command::Health => print_json(client.healthz().await?)?,
        Command::Recent { limit } => print_json(client.recent_messages(Some(limit)).await?)?,
        Command::Search { q, limit } => print_json(client.search_messages(&q, Some(limit)).await?)?,
        Command::Conversations { limit } => print_json(client.conversations(Some(limit)).await?)?,
        Command::ChatroomEvents { chatroom_id, limit } => print_json(client.chatroom_events(&chatroom_id, Some(limit)).await?)?,
        Command::ChatroomSystemEvents { chatroom_id, limit } => print_json(client.chatroom_system_events(&chatroom_id, Some(limit)).await?)?,
        Command::Normalize { file, received_at } => {
            let payload = normalize_file(file, received_at)?;
            print_json(payload)?;
        }
        Command::IngestFile { file, received_at } => {
            let payload = normalize_file(file, received_at)?;
            print_json(client.write_event(&payload).await?)?;
        }
        Command::SyncEdge { edge_url, admin_token, after_raw_event_id, limit, cursor_file } => {
            let result = sync_edge(&client, &edge_url, &admin_token, after_raw_event_id, limit, cursor_file).await?;
            print_json(result)?;
        }
    }

    Ok(())
}

fn build_client(cli: &Cli) -> Result<GeweSkillClient, Box<dyn std::error::Error>> {
    let mut client = GeweSkillClient::new(&cli.base_url)?;
    if let Some(token) = &cli.read_token {
        client = client.with_read_token(token.clone());
    }
    if let Some(token) = &cli.write_token {
        client = client.with_write_token(token.clone());
    }
    Ok(client)
}

fn normalize_file(path: PathBuf, received_at: Option<String>) -> Result<gewe_skill_types::IngestEventRequest, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let json: Value = serde_json::from_str(&text)?;
    let received_at = received_at.unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());
    Ok(normalize_callback(&json, received_at)?.into_ingest_request())
}

async fn sync_edge(
    client: &GeweSkillClient,
    edge_url: &str,
    admin_token: &str,
    after_raw_event_id: Option<i64>,
    limit: u32,
    cursor_file: PathBuf,
) -> Result<Value, Box<dyn std::error::Error>> {
    let after_raw_event_id = after_raw_event_id.unwrap_or_else(|| read_cursor(&cursor_file).unwrap_or(0));
    let edge_url = edge_url.trim_end_matches('/');
    let export_url = format!("{edge_url}/admin/export?after_raw_event_id={after_raw_event_id}&limit={limit}");
    let export: EdgeExportResponse = reqwest::Client::new()
        .get(export_url)
        .bearer_auth(admin_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut written = 0usize;
    let mut failed = Vec::new();
    let mut last_raw_event_id = after_raw_event_id;
    for event in &export.events {
        last_raw_event_id = event.raw_event_id;
        let Some(body) = &event.body else {
            failed.push(serde_json::json!({
                "raw_event_id": event.raw_event_id,
                "error": "missing_body"
            }));
            continue;
        };
        match client
            .write_raw_event(&RawCallbackRequest {
                received_at: event.received_at.clone(),
                body: body.clone(),
            })
            .await
        {
            Ok(_) => written += 1,
            Err(error) => failed.push(serde_json::json!({
                "raw_event_id": event.raw_event_id,
                "error": error.to_string()
            })),
        }
    }

    write_cursor(&cursor_file, export.next_after_raw_event_id)?;

    Ok(serde_json::json!({
        "ok": true,
        "scanned": export.events.len(),
        "written": written,
        "failed": failed,
        "failed_count": failed.len(),
        "after_raw_event_id": after_raw_event_id,
        "last_raw_event_id": last_raw_event_id,
        "next_after_raw_event_id": export.next_after_raw_event_id
    }))
}

fn read_cursor(path: &PathBuf) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_cursor(path: &PathBuf, value: i64) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))
}

fn print_json(value: impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
