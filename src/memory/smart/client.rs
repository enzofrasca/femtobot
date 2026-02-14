use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AppConfig, ProviderKind};

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    headers: HeaderMap,
}

impl LlmClient {
    pub fn new(
        api_key: String,
        base_url: String,
        http_referer: Option<String>,
        app_title: Option<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(anyhow!("missing OpenRouter API key"));
        }
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))?,
        );
        if let Some(referer) = http_referer {
            if !referer.trim().is_empty() {
                headers.insert("HTTP-Referer", HeaderValue::from_str(&referer)?);
            }
        }
        if let Some(title) = app_title {
            if !title.trim().is_empty() {
                headers.insert("X-Title", HeaderValue::from_str(&title)?);
            }
        }
        for (key, value) in extra_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.insert(name, val);
            }
        }

        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
            headers,
        })
    }

    pub fn from_config(cfg: &AppConfig) -> Result<Self> {
        match cfg.provider {
            ProviderKind::OpenRouter => Self::new(
                cfg.providers.openrouter.api_key.clone(),
                cfg.providers.openrouter.base_url.clone(),
                cfg.providers.openrouter.http_referer.clone(),
                cfg.providers.openrouter.app_title.clone(),
                cfg.providers.openrouter.extra_headers.clone(),
            ),
            ProviderKind::OpenAI => Self::new(
                cfg.providers.openai.api_key.clone(),
                cfg.providers.openai.base_url.clone(),
                None,
                None,
                cfg.providers.openai.extra_headers.clone(),
            ),
            ProviderKind::Ollama => Self::new_optional_key(
                cfg.providers.ollama.api_key.clone(),
                cfg.providers.ollama.base_url.clone(),
                None,
                None,
                cfg.providers.ollama.extra_headers.clone(),
            ),
        }
    }

    fn new_optional_key(
        api_key: String,
        base_url: String,
        http_referer: Option<String>,
        app_title: Option<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !api_key.trim().is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {api_key}"))?,
            );
        }
        if let Some(referer) = http_referer {
            if !referer.trim().is_empty() {
                headers.insert("HTTP-Referer", HeaderValue::from_str(&referer)?);
            }
        }
        if let Some(title) = app_title {
            if !title.trim().is_empty() {
                headers.insert("X-Title", HeaderValue::from_str(&title)?);
            }
        }
        for (key, value) in extra_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.insert(name, val);
            }
        }

        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
            headers,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    pub async fn chat_completion(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
        temperature: f32,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        let req = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens,
            temperature,
            response_format,
        };
        let resp = self
            .http
            .post(self.url("/chat/completions"))
            .headers(self.headers.clone())
            .json(&req)
            .send()
            .await?
            .error_for_status()?;
        let body: ChatCompletionResponse = resp.json().await?;
        let content = body
            .choices
            .get(0)
            .and_then(|c| c.message.content.clone())
            .ok_or_else(|| anyhow!("missing response content"))?;
        Ok(content)
    }

    pub async fn embeddings(&self, model: &str, input: &str) -> Result<Vec<f32>> {
        let req = EmbeddingsRequest {
            model: model.to_string(),
            input: vec![input.to_string()],
        };
        let resp = self
            .http
            .post(self.url("/embeddings"))
            .headers(self.headers.clone())
            .json(&req)
            .send()
            .await?
            .error_for_status()?;
        let body: EmbeddingsResponse = resp.json().await?;
        let embedding = body
            .data
            .get(0)
            .map(|d| d.embedding.clone())
            .ok_or_else(|| anyhow!("missing embedding"))?;
        Ok(embedding)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct EmbeddingsRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    #[allow(dead_code)]
    index: Option<u32>,
    #[allow(dead_code)]
    object: Option<Value>,
}
