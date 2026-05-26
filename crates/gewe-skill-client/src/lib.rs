//! Rust SDK for the `gewe-skill-memory` API.

use gewe_skill_types::{
    ApiPage, AttachmentRecord, ChatroomMemberEvent, ChatroomSnapshot, ChatroomSystemEvent,
    ConversationSummary, IdentityRefreshRequest, IdentityRefreshResponse, IdentityResolveResponse,
    IngestEventRequest, MessageContextResponse, MessageQuery, NormalizedMessage,
    RawCallbackRequest, VoiceItem, VoiceQuery, VoiceTranscribeRequest, VoiceTranscribeResponse,
    VoiceWarmRequest, VoiceWarmResponse,
};
use reqwest::{Client as HttpClient, StatusCode, Url};
use serde::de::DeserializeOwned;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(#[from] url::ParseError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api returned {status}: {body}")]
    Api { status: StatusCode, body: String },
}

#[derive(Clone)]
pub struct GeweSkillClient {
    base_url: Url,
    http: HttpClient,
    read_token: Option<String>,
    write_token: Option<String>,
}

impl GeweSkillClient {
    /// Create a client with no bearer tokens.
    ///
    /// Use [`Self::with_read_token`] and [`Self::with_write_token`] when the server requires auth.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, ClientError> {
        Ok(Self {
            base_url: Url::parse(base_url.as_ref())?,
            http: HttpClient::new(),
            read_token: None,
            write_token: None,
        })
    }

    #[must_use]
    pub fn with_read_token(mut self, token: impl Into<String>) -> Self {
        self.read_token = Some(token.into());
        self
    }

    #[must_use]
    pub fn with_write_token(mut self, token: impl Into<String>) -> Self {
        self.write_token = Some(token.into());
        self
    }

    pub async fn healthz(&self) -> Result<serde_json::Value, ClientError> {
        self.get_json("healthz", None).await
    }

    pub async fn write_event(
        &self,
        request: &IngestEventRequest,
    ) -> Result<serde_json::Value, ClientError> {
        self.post_write_json("write/events", request).await
    }

    pub async fn write_raw_event(
        &self,
        request: &RawCallbackRequest,
    ) -> Result<serde_json::Value, ClientError> {
        self.post_write_json("write/raw-events", request).await
    }

    pub async fn write_attachment(
        &self,
        request: &AttachmentRecord,
    ) -> Result<serde_json::Value, ClientError> {
        self.post_write_json("write/attachments", request).await
    }

    async fn post_write_json<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        request: &T,
    ) -> Result<serde_json::Value, ClientError> {
        self.post_write_json_as(path, request).await
    }

    async fn post_write_json_as<T: serde::Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        request: &T,
    ) -> Result<R, ClientError> {
        let response = self
            .http
            .post(self.url(path)?)
            .bearer_auth(self.write_token.as_deref().unwrap_or_default())
            .json(request)
            .send()
            .await?;
        Self::decode_response(response).await
    }

    pub async fn recent_messages(
        &self,
        limit: Option<u32>,
    ) -> Result<ApiPage<NormalizedMessage>, ClientError> {
        self.get_json("api/messages/recent", limit).await
    }

    pub async fn messages(
        &self,
        query: &MessageQuery,
    ) -> Result<ApiPage<NormalizedMessage>, ClientError> {
        let mut url = self.url("api/messages")?;
        append_message_query(&mut url, query);
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    pub async fn search_messages(
        &self,
        query: &str,
        limit: Option<u32>,
    ) -> Result<ApiPage<NormalizedMessage>, ClientError> {
        let mut url = self.url("api/messages/search")?;
        url.query_pairs_mut().append_pair("q", query);
        if let Some(limit) = limit {
            url.query_pairs_mut()
                .append_pair("limit", &limit.to_string());
        }
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    pub async fn message_context(
        &self,
        message_key: &str,
        before: Option<u32>,
        after: Option<u32>,
    ) -> Result<MessageContextResponse, ClientError> {
        let path = format!("api/messages/{message_key}/context");
        let mut url = self.url(&path)?;
        if let Some(before) = before {
            url.query_pairs_mut()
                .append_pair("before", &before.to_string());
        }
        if let Some(after) = after {
            url.query_pairs_mut()
                .append_pair("after", &after.to_string());
        }
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    pub async fn conversations(
        &self,
        limit: Option<u32>,
    ) -> Result<ApiPage<ConversationSummary>, ClientError> {
        self.get_json("api/conversations", limit).await
    }

    pub async fn resolve_identity(
        &self,
        query: &str,
        limit: Option<u32>,
    ) -> Result<IdentityResolveResponse, ClientError> {
        let mut url = self.url("api/identity/resolve")?;
        url.query_pairs_mut().append_pair("q", query);
        if let Some(limit) = limit {
            url.query_pairs_mut()
                .append_pair("limit", &limit.to_string());
        }
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    pub async fn refresh_identity(
        &self,
        request: &IdentityRefreshRequest,
    ) -> Result<IdentityRefreshResponse, ClientError> {
        let response = self
            .http
            .post(self.url("api/identity/refresh")?)
            .bearer_auth(self.read_token.as_deref().unwrap_or_default())
            .json(request)
            .send()
            .await?;
        Self::decode_response(response).await
    }

    pub async fn recent_attachments(
        &self,
        limit: Option<u32>,
    ) -> Result<ApiPage<AttachmentRecord>, ClientError> {
        self.get_json("api/attachments/recent", limit).await
    }

    pub async fn download_attachment(&self, sha256: &str) -> Result<Vec<u8>, ClientError> {
        let path = format!("api/attachments/{sha256}/download");
        let mut request = self.http.get(self.url(&path)?);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.bytes().await?.to_vec());
        }
        let body = response.text().await.unwrap_or_default();
        Err(ClientError::Api { status, body })
    }

    pub async fn voices(&self, query: &VoiceQuery) -> Result<ApiPage<VoiceItem>, ClientError> {
        let mut url = self.url("api/voice")?;
        append_voice_query(&mut url, query);
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    pub async fn transcribe_voice(
        &self,
        request: &VoiceTranscribeRequest,
    ) -> Result<VoiceTranscribeResponse, ClientError> {
        self.post_write_json_as("api/voice/transcribe", request)
            .await
    }

    pub async fn warm_voice(
        &self,
        request: &VoiceWarmRequest,
    ) -> Result<VoiceWarmResponse, ClientError> {
        self.post_write_json_as("api/voice/warm", request).await
    }

    pub async fn chatroom_snapshots(
        &self,
        chatroom_id: &str,
        limit: Option<u32>,
    ) -> Result<ApiPage<ChatroomSnapshot>, ClientError> {
        self.get_json(&format!("api/chatrooms/{chatroom_id}/snapshots"), limit)
            .await
    }

    pub async fn chatroom_events(
        &self,
        chatroom_id: &str,
        limit: Option<u32>,
    ) -> Result<ApiPage<ChatroomMemberEvent>, ClientError> {
        self.get_json(&format!("api/chatrooms/{chatroom_id}/events"), limit)
            .await
    }

    pub async fn chatroom_system_events(
        &self,
        chatroom_id: &str,
        limit: Option<u32>,
    ) -> Result<ApiPage<ChatroomSystemEvent>, ClientError> {
        self.get_json(&format!("api/chatrooms/{chatroom_id}/system-events"), limit)
            .await
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        limit: Option<u32>,
    ) -> Result<T, ClientError> {
        let mut url = self.url(path)?;
        if let Some(limit) = limit {
            url.query_pairs_mut()
                .append_pair("limit", &limit.to_string());
        }
        let mut request = self.http.get(url);
        if let Some(token) = &self.read_token {
            request = request.bearer_auth(token);
        }
        Self::decode_response(request.send().await?).await
    }

    fn url(&self, path: &str) -> Result<Url, ClientError> {
        Ok(self.base_url.join(path.trim_start_matches('/'))?)
    }

    async fn decode_response<T: DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, ClientError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await?);
        }
        let body = response.text().await.unwrap_or_default();
        Err(ClientError::Api { status, body })
    }
}

fn append_message_query(url: &mut Url, query: &MessageQuery) {
    let mut pairs = url.query_pairs_mut();
    if let Some(value) = &query.q {
        pairs.append_pair("q", value);
    }
    if let Some(value) = &query.conversation_id {
        pairs.append_pair("conversation_id", value);
    }
    if let Some(value) = &query.sender_wxid {
        pairs.append_pair("sender_wxid", value);
    }
    if let Some(value) = &query.kind {
        pairs.append_pair("kind", value);
    }
    if let Some(value) = &query.direction {
        pairs.append_pair("direction", value);
    }
    if let Some(value) = &query.after {
        pairs.append_pair("after", value);
    }
    if let Some(value) = &query.before {
        pairs.append_pair("before", value);
    }
    if let Some(value) = &query.cursor {
        pairs.append_pair("cursor", value);
    }
    if let Some(value) = query.limit {
        pairs.append_pair("limit", &value.to_string());
    }
    if let Some(value) = &query.order {
        pairs.append_pair("order", value);
    }
}

fn append_voice_query(url: &mut Url, query: &VoiceQuery) {
    let mut pairs = url.query_pairs_mut();
    if let Some(value) = &query.conversation_id {
        pairs.append_pair("conversation_id", value);
    }
    if let Some(value) = &query.sender_wxid {
        pairs.append_pair("sender_wxid", value);
    }
    if let Some(value) = &query.after {
        pairs.append_pair("after", value);
    }
    if let Some(value) = &query.before {
        pairs.append_pair("before", value);
    }
    if let Some(value) = &query.cursor {
        pairs.append_pair("cursor", value);
    }
    if let Some(value) = query.limit {
        pairs.append_pair("limit", &value.to_string());
    }
    if let Some(value) = &query.order {
        pairs.append_pair("order", value);
    }
    if let Some(value) = query.missing_only {
        pairs.append_pair("missing_only", if value { "true" } else { "false" });
    }
}
