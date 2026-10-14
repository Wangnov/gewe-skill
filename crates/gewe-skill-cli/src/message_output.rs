use super::{agent_attachment_block, agent_speaker_block, message_sender_id, print_json};
use chrono::DateTime;
use chrono_tz::Tz;
use clap::{Args, ValueEnum};
use gewe_skill_client::GeweSkillClient;
use gewe_skill_core::{extract_tag, parse_chatroom_system_event};
use gewe_skill_types::{ApiPage, MessageQuery, NormalizedKind, NormalizedMessage};
use regex::Regex;
use serde_json::Value;
use std::fmt::Write;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(super) enum MessageFormat {
    #[default]
    Json,
    Text,
}

#[derive(Debug, Clone, Args)]
pub(super) struct MessageOutputArgs {
    #[arg(long, value_enum, default_value = "json")]
    pub format: MessageFormat,
    /// 文本时间的 IANA 时区，默认读取 TZ，否则 `Asia/Shanghai`。
    #[arg(long, env = "TZ", default_value = "Asia/Shanghai")]
    pub timezone: String,
    /// JSON 消息省略 `raw_content` 和 `msg_source`。
    #[arg(long)]
    pub compact: bool,
}

fn compact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.contains_key("message_key") && object.contains_key("received_at") {
                object.remove("raw_content");
                object.remove("msg_source");
            }
            for value in object.values_mut() {
                compact(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                compact(value);
            }
        }
        _ => {}
    }
}

pub(super) fn print_agent(
    result: &Value,
    output: &MessageOutputArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    if output.format == MessageFormat::Text {
        print!("{}", render(result, &output.timezone)?);
    } else {
        let mut result = result.clone();
        if output.compact {
            compact(&mut result);
        }
        print_json(result)?;
    }
    Ok(())
}

async fn enrich(
    client: &GeweSkillClient,
    page: &ApiPage<NormalizedMessage>,
    query: &MessageQuery,
) -> Result<Value, Box<dyn std::error::Error>> {
    let keys = page
        .items
        .iter()
        .map(|m| m.message_key.clone())
        .collect::<Vec<_>>();
    let attachments = client.attachments_by_message_keys(&keys).await?;
    let speakers = agent_speaker_block(
        client,
        true,
        &page.items,
        query
            .conversation_id
            .as_deref()
            .filter(|id| id.ends_with("@chatroom")),
    )
    .await?;
    let voice = client.voices_by_message_keys(&keys).await?;
    Ok(
        serde_json::json!({ "ok": true, "messages": page, "message_query": query, "speakers": speakers,
        "attachments": agent_attachment_block(true, &page.items, Some(&attachments.items)), "voice": { "items": voice.items } }),
    )
}

pub(super) async fn print_page(
    client: &GeweSkillClient,
    query: &MessageQuery,
    output: &MessageOutputArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let page = client.messages(query).await?;
    let result = if output.format == MessageFormat::Text {
        enrich(client, &page, query).await?
    } else {
        serde_json::to_value(page)?
    };
    print_agent(&result, output)
}

pub(super) async fn print_context(
    client: &GeweSkillClient,
    key: &str,
    before: u32,
    after: u32,
    output: &MessageOutputArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = client
        .message_context(key, Some(before), Some(after))
        .await?;
    if output.format == MessageFormat::Json {
        return print_agent(&serde_json::to_value(context)?, output);
    }
    if context.anchor.is_none() {
        println!("未找到消息，返回 0 条。");
        return Ok(());
    }
    let items: Vec<_> = context
        .before
        .into_iter()
        .chain(context.anchor)
        .chain(context.after)
        .collect();
    let query = MessageQuery {
        conversation_id: context.conversation_id,
        order: Some("asc".to_string()),
        after: items.first().map(|m| m.received_at.clone()),
        before: items
            .last()
            .and_then(|m| DateTime::parse_from_rfc3339(&m.received_at).ok())
            .map(|time| (time + chrono::Duration::milliseconds(1)).to_rfc3339()),
        ..MessageQuery::default()
    };
    let page = ApiPage {
        items,
        next_cursor: None,
        pagination: context.pagination,
    };
    print_agent(&enrich(client, &page, &query).await?, output)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn xml_text(xml: &str) -> String {
    static TAGS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("valid XML display regex"));
    static NUMERIC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").expect("valid entity regex"));
    let text = xml
        .replace("<![CDATA[", "")
        .replace("]]>", "")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let text = NUMERIC.replace_all(&text, |caps: &regex::Captures| {
        let entity = &caps[1];
        let code = entity
            .strip_prefix('x')
            .map_or_else(|| entity.parse::<u32>(), |hex| u32::from_str_radix(hex, 16));
        code.ok()
            .and_then(char::from_u32)
            .map_or_else(|| caps[0].to_string(), |ch| ch.to_string())
    });
    one_line(&TAGS.replace_all(&text, " "))
}

fn tag(xml: &str, name: &str) -> String {
    extract_tag(xml, name)
        .map(|s| xml_text(&s))
        .unwrap_or_default()
}
fn excerpt(text: &str) -> String {
    let mut result: String = text.chars().take(80).collect();
    if text.chars().count() > 80 {
        result.push('…');
    }
    result
}

fn body(message: &NormalizedMessage, result: &Value) -> String {
    let xml = message
        .content_xml
        .as_deref()
        .or(message.raw_content.as_deref())
        .unwrap_or_default();
    match message.kind {
        NormalizedKind::Image => {
            let sha = result["attachments"]["by_message_key"][&message.message_key]["sha256s"][0]
                .as_str()
                .map_or_else(
                    || "附件缺失".to_string(),
                    |sha| sha.chars().take(8).collect::<String>(),
                );
            format!("[图片 {sha}]")
        }
        NormalizedKind::Voice => {
            let transcript = result["voice"]["items"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item["message"]["message_key"] == message.message_key)
                })
                .filter(|item| item["transcript"]["status"] == "completed")
                .and_then(|item| item["transcript"]["text"].as_str())
                .filter(|s| !s.trim().is_empty());
            format!("[语音 {}]", one_line(transcript.unwrap_or("未转写")))
        }
        NormalizedKind::File | NormalizedKind::FileNotice => {
            format!("[文件 {}]", tag(xml, "title"))
        }
        NormalizedKind::Quote => {
            let referenced = extract_tag(xml, "refermsg").unwrap_or_default();
            let content = tag(&referenced, "content");
            let summary = if content.is_empty() {
                "非文本消息".to_string()
            } else {
                excerpt(&content)
            };
            format!("[引用 {summary}] {}", tag(xml, "title"))
        }
        NormalizedKind::Link => format!("[链接 {}]", tag(xml, "title")),
        NormalizedKind::Emoji => "[表情]".to_string(),
        NormalizedKind::Video => "[视频]".to_string(),
        NormalizedKind::System => parse_chatroom_system_event(message)
            .and_then(|event| event.content_text)
            .or_else(|| message.content_text.clone())
            .map_or_else(
                || {
                    let text = ["replacemsg", "plain", "content", "template"]
                        .iter()
                        .map(|name| tag(xml, name))
                        .find(|s| !s.is_empty())
                        .unwrap_or_default();
                    if text.is_empty() {
                        "[系统消息]".to_string()
                    } else {
                        text
                    }
                },
                |s| xml_text(&s),
            ),
        NormalizedKind::Text => one_line(message.content_text.as_deref().unwrap_or_default()),
        _ => {
            let title = tag(xml, "title");
            if title.is_empty() {
                format!(
                    "[{}]",
                    match message.kind {
                        NormalizedKind::Location => "位置",
                        NormalizedKind::ContactCard => "名片",
                        NormalizedKind::ChatRecord => "聊天记录",
                        NormalizedKind::MiniProgram => "小程序",
                        _ => "非文本消息",
                    }
                )
            } else {
                format!("[分享 {title}]")
            }
        }
    }
}

pub(super) fn render(result: &Value, timezone: &str) -> Result<String, Box<dyn std::error::Error>> {
    let timezone: Tz = timezone
        .trim_start_matches(':')
        .parse()
        .map_err(|_| "无法识别时区，请使用 Asia/Shanghai 或 UTC 等 IANA 名称")?;
    if result["ok"] == false {
        return Ok(format!(
            "{}\n",
            result["hint"]
                .as_str()
                .unwrap_or("未解析到会话或发送者，请检查名称或使用精确 ID。")
        ));
    }
    let messages: Vec<NormalizedMessage> =
        serde_json::from_value(result["messages"]["items"].clone())?;
    let local_time = |value: &str, format: &str| {
        DateTime::parse_from_rfc3339(value)
            .map(|t| t.with_timezone(&timezone).format(format).to_string())
    };
    let mut rendered = String::new();
    for message in &messages {
        let name = if message.is_outgoing
            || message_sender_id(message)
                .is_some_and(|id| message.account_wxid.as_deref() == Some(id))
        {
            "我"
        } else if message.kind == NormalizedKind::System {
            "系统"
        } else {
            message_sender_id(message)
                .and_then(|id| {
                    message
                        .conversation_id
                        .as_deref()
                        .and_then(|room| {
                            result["speakers"]["by_conversation"][room][id]
                                ["effective_display_name"]
                                .as_str()
                        })
                        .or_else(|| {
                            result["speakers"]["by_wxid"][id]["effective_display_name"].as_str()
                        })
                })
                .unwrap_or("未知成员")
        };
        writeln!(
            rendered,
            "{} {}: {}",
            local_time(&message.received_at, "%m-%d %H:%M")?,
            one_line(name),
            body(message, result)
        )?;
    }
    let page = &result["messages"]["pagination"];
    let range = match (
        page["start_at"]
            .as_str()
            .or_else(|| messages.iter().map(|m| m.received_at.as_str()).min()),
        page["end_at"]
            .as_str()
            .or_else(|| messages.iter().map(|m| m.received_at.as_str()).max()),
    ) {
        (Some(start), Some(end)) => format!(
            "{} ～ {}",
            local_time(start, "%Y-%m-%d %H:%M")?,
            local_time(end, "%Y-%m-%d %H:%M")?
        ),
        _ => "无".to_string(),
    };
    let order = page["order"]
        .as_str()
        .or(result["message_query"]["order"].as_str())
        .unwrap_or("desc");
    let selection = if page["selection"] == "context" {
        "消息上下文"
    } else if order == "asc" {
        "范围内最早 N 条，正序"
    } else {
        "范围内最新 N 条，倒序"
    };
    let more = |side: &str, order: &str| {
        let has = &page[format!("has_{side}")];
        let cursor = &page[format!("{side}_cursor")];
        match (has.as_bool(), cursor.as_str()) {
            (Some(true), Some(cursor)) => {
                format!("有（--order {order} --cursor '{cursor}'，保留原筛选条件）")
            }
            (Some(false), _) => "无".to_string(),
            _ => "未知（服务端需升级以提供分页统计）".to_string(),
        }
    };
    let missing = if result["attachments"]["included"] == true {
        result["attachments"]["summary"]["attachment_expected_missing_count"].to_string()
    } else {
        "未检查".to_string()
    };
    writeln!(rendered, "共 {} 条；接收时间 {} ({timezone})；{selection}；更早：{}；更晚：{}；附件缺失 {missing} 条。", messages.len(), range, more("earlier", "desc"), more("later", "asc"))?;
    Ok(rendered)
}
