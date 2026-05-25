use clap::{Parser, Subcommand};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::normalize_callback;
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = build_client(&cli)?;

    match cli.command {
        Command::Health => print_json(client.healthz().await?)?,
        Command::Recent { limit } => print_json(client.recent_messages(Some(limit)).await?)?,
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

fn print_json(value: impl serde::Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
