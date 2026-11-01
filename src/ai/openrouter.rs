//! Minimal `OpenRouter` chat-completions and embeddings client sharing the bot's connection pool.

use std::time::Duration;

use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use futures_util::{Stream, StreamExt, stream};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
        /// Ends a cacheable prompt prefix, for providers that only cache at explicit breakpoints.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ImageUrl {
        image_url: ImageUrl,
    },
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into(), cache_control: None }
    }
}

/// Anthropic's prompt cache marker, which `OpenRouter` passes through. Entries live for five minutes after last use.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CacheControl {
    Ephemeral,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Content,
}

impl Message {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self { role, content: Content::Text(text.into()) }
    }

    /// Marks the end of this message as a prompt cache breakpoint.
    pub fn cache_breakpoint(&mut self) {
        // Only content parts can carry the marker.
        if let Content::Text(text) = &mut self.content {
            let text = std::mem::take(text);
            self.content = Content::Parts(vec![Part::text(text)]);
        }
        let Content::Parts(parts) = &mut self.content else { return };
        if let Some(Part::Text { cache_control, .. }) = parts.iter_mut().rfind(|part| matches!(part, Part::Text { .. }))
        {
            *cache_control = Some(CacheControl::Ephemeral);
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
}

/// `temperature` for `model`, or `None` for Claude models, which reject sampling parameters since Claude Haiku 5.5.
pub fn temperature(model: &str, temperature: f32) -> Option<f32> {
    (!model.starts_with("anthropic/")).then_some(temperature)
}

/// Turns thinking off for short, simple completions, where it would only eat the output budget.
pub fn no_reasoning() -> Value {
    serde_json::json!({ "enabled": false })
}

#[derive(Deserialize)]
struct Completion {
    #[serde(default)]
    choices: Vec<Choice>,
    /// Set instead of `choices` when a stream fails partway.
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(alias = "delta")]
    message: Delta,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    dimensions: u32,
}

#[derive(Deserialize)]
struct Embeddings {
    #[serde(default)]
    data: Vec<Embedding>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Embedding {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Clone, Debug)]
pub struct OpenRouter {
    http: Client,
    endpoint: String,
    embeddings_endpoint: String,
    api_key: String,
    site_url: Option<String>,
    site_name: Option<String>,
}

const ATTEMPTS: u32 = 3;

impl OpenRouter {
    pub fn from_config(http: Client, config: &Config) -> Option<Self> {
        let base_url = config.openrouter_base_url.trim_end_matches('/');
        Some(Self {
            http,
            endpoint: format!("{base_url}/chat/completions"),
            embeddings_endpoint: format!("{base_url}/embeddings"),
            api_key: config.openrouter_api_key.clone()?,
            site_url: config.openrouter_site_url.clone(),
            site_name: config.openrouter_site_name.clone(),
        })
    }

    fn post(&self, url: &str, request: &impl Serialize, timeout: Duration) -> RequestBuilder {
        let mut builder = self.http.post(url).bearer_auth(&self.api_key).timeout(timeout).json(request);
        if let Some(url) = &self.site_url {
            builder = builder.header("HTTP-Referer", url);
        }
        if let Some(name) = &self.site_name {
            builder = builder.header("X-Title", name);
        }
        builder
    }

    /// Sends the request, retrying connection failures, rate limits and server errors with linear backoff.
    async fn send(&self, url: &str, request: &(impl Serialize + Sync), timeout: Duration) -> Result<reqwest::Response> {
        let mut attempt = 1;
        loop {
            let result = self.post(url, request, timeout).send().await;
            let retryable = match &result {
                Ok(response) => {
                    response.status() == StatusCode::TOO_MANY_REQUESTS || response.status().is_server_error()
                }
                Err(error) => error.is_connect() || error.is_timeout(),
            };
            if !retryable || attempt == ATTEMPTS {
                let response = result?;
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    bail!("OpenRouter returned HTTP {status}: {}", body.chars().take(300).collect::<String>());
                }
                return Ok(response);
            }
            tokio::time::sleep(Duration::from_millis(500) * attempt).await;
            attempt += 1;
        }
    }

    /// Returns the trimmed text of the first choice.
    pub async fn complete(&self, request: &ChatRequest, timeout: Duration) -> Result<String> {
        let completion: Completion = self.send(&self.endpoint, request, timeout).await?.json().await?;
        let text = completion
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| eyre!("OpenRouter returned no content"))?;
        match text.trim() {
            "" => bail!("OpenRouter returned empty content"),
            text => Ok(text.to_owned()),
        }
    }

    /// Streams content deltas as they arrive from the server-sent event stream.
    pub async fn stream(
        &self,
        request: &ChatRequest,
        timeout: Duration,
    ) -> Result<impl Stream<Item = Result<String>> + Send + use<>> {
        let request = ChatRequest { stream: true, ..request.clone() };
        let body = self.send(&self.endpoint, &request, timeout).await?.bytes_stream();
        Ok(stream::try_unfold((body, Vec::new()), |(mut body, mut buffer)| async move {
            loop {
                if let Some(end) = buffer.iter().position(|&byte| byte == b'\n') {
                    let event = parse_event(&buffer[..end]);
                    buffer.drain(..=end);
                    match event? {
                        Event::Delta(text) => return Ok(Some((text, (body, buffer)))),
                        Event::Done => return Ok(None),
                        Event::Skip => continue,
                    }
                }
                match body.next().await.transpose()? {
                    Some(chunk) => buffer.extend_from_slice(&chunk),
                    None => return Ok(None),
                }
            }
        }))
    }

    /// Embeds every input with `dimensions` components, returning the vectors in input order.
    pub async fn embed(
        &self,
        model: &str,
        input: &[String],
        dimensions: u32,
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>> {
        let request = EmbeddingRequest { model, input, dimensions };
        let mut response: Embeddings = self.send(&self.embeddings_endpoint, &request, timeout).await?.json().await?;
        if let Some(error) = response.error {
            bail!("OpenRouter embeddings error: {error}");
        }
        response.data.sort_by_key(|embedding| embedding.index);
        if !response.data.iter().map(|embedding| embedding.index).eq(0..input.len()) {
            bail!("OpenRouter returned {} embeddings for {} inputs", response.data.len(), input.len());
        }
        Ok(response.data.into_iter().map(|embedding| embedding.embedding).collect())
    }
}

enum Event {
    Delta(String),
    Done,
    Skip,
}

/// Parses one SSE line. Comments (`OpenRouter`'s keep-alives), blank lines and empty deltas are skipped.
fn parse_event(line: &[u8]) -> Result<Event> {
    let line = std::str::from_utf8(line)?.trim_end();
    let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
        return Ok(Event::Skip);
    };
    if data == "[DONE]" {
        return Ok(Event::Done);
    }
    let chunk: Completion = serde_json::from_str(data)?;
    if let Some(error) = chunk.error {
        bail!("OpenRouter stream error: {error}");
    }
    Ok(match chunk.choices.into_iter().next().and_then(|choice| choice.message.content) {
        Some(text) if !text.is_empty() => Event::Delta(text),
        _ => Event::Skip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::post};
    use serde_json::json;

    async fn serve(body: &'static str) -> OpenRouter {
        client(Router::new().route("/chat/completions", post(move || async move { body }))).await
    }

    async fn client(app: Router) -> OpenRouter {
        let base = format!("http://{}", crate::test_util::serve(app).await);
        let config = Config { openrouter_api_key: Some("key".into()), openrouter_base_url: base, ..Config::default() };
        OpenRouter::from_config(Client::new(), &config).unwrap()
    }

    #[test]
    fn request_serializes_to_the_openai_wire_format() {
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::new(Role::System, "persona"),
                Message {
                    role: Role::User,
                    content: Content::Parts(vec![
                        Part::text("look"),
                        Part::ImageUrl { image_url: ImageUrl { url: "data:x".into() } },
                    ]),
                },
            ],
            max_tokens: 10,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"model": "m", "max_tokens": 10, "messages": [
                {"role": "system", "content": "persona"},
                {"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "data:x"}}
                ]}
            ]})
        );
    }

    #[test]
    fn cache_breakpoints_mark_the_last_text_part() {
        let mut message = Message::new(Role::Assistant, "kek");
        message.cache_breakpoint();
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({"role": "assistant", "content": [{"type": "text", "text": "kek", "cache_control": {"type": "ephemeral"}}]})
        );
    }

    #[tokio::test]
    async fn stream_joins_split_chunks_and_skips_keepalives() {
        let client = serve(concat!(
            ": OPENROUTER PROCESSING\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ignored\"}}]}\n\n",
        ))
        .await;
        let deltas: Vec<_> = client
            .stream(&ChatRequest::default(), Duration::from_secs(5))
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(deltas, ["Hel", "lo"]);
    }

    #[tokio::test]
    async fn stream_surfaces_mid_stream_errors() {
        let client = serve("data: {\"error\":{\"message\":\"overloaded\"}}\n\n").await;
        let mut stream = std::pin::pin!(client.stream(&ChatRequest::default(), Duration::from_secs(5)).await.unwrap());
        assert!(stream.next().await.unwrap().is_err());
    }

    #[tokio::test]
    async fn complete_returns_trimmed_content() {
        let client = serve(r#"{"choices":[{"message":{"content":"  hi there \n"}}]}"#).await;
        assert_eq!(client.complete(&ChatRequest::default(), Duration::from_secs(5)).await.unwrap(), "hi there");
    }

    #[tokio::test]
    async fn embed_orders_vectors_by_index() {
        let app = Router::new().route(
            "/embeddings",
            post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body, json!({"model": "e", "input": ["a", "b"], "dimensions": 2}));
                axum::Json(json!({"data": [
                    {"object": "embedding", "embedding": [0.0, 1.0], "index": 1},
                    {"object": "embedding", "embedding": [1.0, 0.0], "index": 0}
                ]}))
            }),
        );
        let input = ["a".to_owned(), "b".to_owned()];
        let vectors = client(app).await.embed("e", &input, 2, Duration::from_secs(5)).await.unwrap();
        assert_eq!(vectors, [[1.0, 0.0], [0.0, 1.0]]);
    }

    #[tokio::test]
    async fn embed_rejects_errors_and_missing_vectors() {
        let app = Router::new()
            .route("/embeddings", post(|| async { axum::Json(json!({"data": [{"embedding": [1.0], "index": 0}]})) }));
        let input = ["a".to_owned(), "b".to_owned()];
        assert!(client(app).await.embed("e", &input, 1, Duration::from_secs(5)).await.is_err());
        let app = Router::new()
            .route("/embeddings", post(|| async { axum::Json(json!({"error": {"message": "no such model"}})) }));
        assert!(client(app).await.embed("e", &input, 1, Duration::from_secs(5)).await.is_err());
    }
}
