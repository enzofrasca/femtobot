use crate::config::AppConfig;
use anyhow::{anyhow, Context, Result};
use reqwest::multipart;
use rig::prelude::TranscriptionClient;
use rig::providers::openai;
use rig::transcription::TranscriptionModel;
use serde_json::Value;
use tracing::warn;

#[derive(Clone)]
enum Backend {
    OpenAI(openai::Client),
    Mistral {
        http: reqwest::Client,
        api_key: String,
        base_url: String,
        diarize: bool,
        context_bias: Option<String>,
        timestamp_granularities: Vec<String>,
    },
}

#[derive(Clone)]
pub struct Transcriber {
    backend: Backend,
    model: String,
    language: Option<String>,
    max_bytes: usize,
}

impl Transcriber {
    pub fn from_config(cfg: &AppConfig) -> Option<Self> {
        if !cfg.transcription.enabled {
            return None;
        }
        if cfg.transcription.model.trim().is_empty() {
            warn!("transcription disabled: missing transcription model");
            return None;
        }

        let provider = cfg.transcription.provider.trim().to_ascii_lowercase();
        let backend = match provider.as_str() {
            "" | "openai" => {
                if cfg.providers.openai.api_key.trim().is_empty() {
                    warn!("transcription disabled: missing OpenAI API key");
                    return None;
                }
                Backend::OpenAI(build_openai_client(
                    &cfg.providers.openai.api_key,
                    &cfg.providers.openai.base_url,
                    &cfg.providers.openai.extra_headers,
                ))
            }
            "mistral" => {
                if cfg.providers.mistral.api_key.trim().is_empty() {
                    warn!("transcription disabled: missing MISTRAL_API_KEY");
                    return None;
                }
                Backend::Mistral {
                    http: reqwest::Client::new(),
                    api_key: cfg.providers.mistral.api_key.clone(),
                    base_url: cfg.providers.mistral.base_url.clone(),
                    diarize: cfg.transcription.mistral_diarize,
                    context_bias: cfg.transcription.mistral_context_bias.clone(),
                    timestamp_granularities: cfg
                        .transcription
                        .mistral_timestamp_granularities
                        .clone(),
                }
            }
            other => {
                warn!("transcription disabled: unsupported provider '{other}'");
                return None;
            }
        };

        Some(Self {
            backend,
            model: cfg.transcription.model.clone(),
            language: cfg.transcription.language.clone(),
            max_bytes: cfg.transcription.max_bytes.max(1),
        })
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub async fn transcribe_bytes(&self, filename: String, data: Vec<u8>) -> Result<String> {
        if data.is_empty() {
            return Err(anyhow!("audio payload is empty"));
        }
        if data.len() > self.max_bytes {
            return Err(anyhow!(
                "audio payload too large: {} bytes (max {})",
                data.len(),
                self.max_bytes
            ));
        }

        match &self.backend {
            Backend::OpenAI(client) => {
                let model = client.transcription_model(self.model.clone());
                let mut request = model
                    .transcription_request()
                    .filename(Some(filename))
                    .data(data);
                if let Some(language) = &self.language {
                    request = request.language(language.clone());
                }
                let response = request
                    .send()
                    .await
                    .context("OpenAI transcription request failed")?;
                Ok(response.text.trim().to_string())
            }
            Backend::Mistral {
                http,
                api_key,
                base_url,
                diarize,
                context_bias,
                timestamp_granularities,
            } => {
                let mut form = multipart::Form::new()
                    .text("model", self.model.clone())
                    .part("file", multipart::Part::bytes(data).file_name(filename));

                if let Some(language) = &self.language {
                    form = form.text("language", language.clone());
                }
                if *diarize {
                    form = form.text("diarize", "true");
                }
                if let Some(bias) = context_bias {
                    if !bias.trim().is_empty() {
                        form = form.text("context_bias", bias.clone());
                    }
                }
                for granularity in timestamp_granularities {
                    if !granularity.trim().is_empty() {
                        form = form.text("timestamp_granularities[]", granularity.clone());
                    }
                }

                let endpoint = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
                let response = http
                    .post(endpoint)
                    .bearer_auth(api_key)
                    .multipart(form)
                    .send()
                    .await
                    .context("Mistral transcription request failed")?
                    .error_for_status()
                    .context("Mistral transcription request returned non-success status")?;
                let body: Value = response
                    .json()
                    .await
                    .context("failed to decode Mistral transcription response")?;
                extract_text_from_response(&body).ok_or_else(|| {
                    anyhow!(
                        "Mistral transcription response did not include a recognized text field"
                    )
                })
            }
        }
    }
}

fn build_openai_client(
    api_key: &str,
    base_url: &str,
    extra_headers: &[(String, String)],
) -> openai::Client {
    crate::providers::build_openai_client(api_key, base_url, extra_headers)
}

fn extract_text_from_response(body: &Value) -> Option<String> {
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(segments) = body.get("segments").and_then(Value::as_array) {
        let merged = segments
            .iter()
            .filter_map(|segment| segment.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if !merged.is_empty() {
            return Some(merged);
        }
    }

    None
}
