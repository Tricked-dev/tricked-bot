//! Minimal `OpenRouter` chat-completions client sharing the bot's connection pool.

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
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
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

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
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

#[derive(Clone, Debug)]
pub struct OpenRouter {
    http: Client,
    endpoint: String,
    api_key: String,
    site_url: Option<String>,
    site_name: Option<String>,
}

const ATTEMPTS: u32 = 3;

impl OpenRouter {
    pub fn from_config(http: Client, config: &Config) -> Option<Self> {
        Some(Self {
            http,
            endpoint: format!("{}/chat/completions", config.openrouter_base_url.trim_end_matches('/')),
            api_key: config.openrouter_api_key.clone()?,
            site_url: config.openrouter_site_url.clone(),
            site_name: config.openrouter_site_name.clone(),
        })
    }

    fn post(&self, request: &ChatRequest, timeout: Duration) -> RequestBuilder {
        let mut builder = self.http.post(&self.endpoint).bearer_auth(&self.api_key).timeout(timeout).json(request);
        if let Some(url) = &self.site_url {
            builder = builder.header("HTTP-Referer", url);
        }
        if let Some(name) = &self.site_name {
            builder = builder.header("X-Title", name);
        }
        builder
    }

    /// Sends the request, retrying connection failures, rate limits and server errors with linear backoff.
    async fn send(&self, request: &ChatRequest, timeout: Duration) -> Result<reqwest::Response> {
        let mut attempt = 1;
        loop {
            let result = self.post(request, timeout).send().await;
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
        let completion: Completion = self.send(request, timeout).await?.json().await?;
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
        let body = self.send(&request, timeout).await?.bytes_stream();
        Ok(stream::try_unfold((body, Vec::new()), |(mut body, mut buffer)| async move {
            loop {
                if let Some(end) = buffer.iter().position(|&byte| byte == b'\n') {
                    let line = buffer.drain(..=end).collect::<Vec<_>>();
                    match parse_event(&line)? {
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
    let chunk: Value = serde_json::from_str(data)?;
    if let Some(error) = chunk.get("error") {
        bail!("OpenRouter stream error: {error}");
    }
    let chunk: Completion = serde_json::from_value(chunk)?;
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
        let app = Router::new().route("/chat/completions", post(move || async move { body }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
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
                        Part::Text { text: "look".into() },
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
}
