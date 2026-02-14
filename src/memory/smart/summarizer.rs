use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::memory::smart::client::{ChatMessage, LlmClient};

const SUMMARY_PROMPT: &str = r#"You are a memory summarizer for an AI assistant.

Summarize the conversation chunk into durable long-term memory that helps future turns.

Keep only information that is likely useful later:
- user preferences, constraints, or recurring habits
- decisions, outcomes, commitments, and important TODOs
- stable facts (projects, tooling choices, identities, timelines)

Do not include:
- greetings, filler, or acknowledgements
- ephemeral tool noise
- sensitive data unless explicitly needed for task continuity

If the chunk has no durable memory value, return an empty summary.

Return ONLY JSON:
{"summary":"...", "importance":"high|medium|low"}
"#;

#[derive(Clone, Debug)]
pub struct ConversationSummary {
    pub content: String,
    pub importance: f32,
    pub source: String,
}

#[derive(Clone)]
pub struct ConversationSummarizer {
    model: String,
    client: LlmClient,
}

impl ConversationSummarizer {
    pub fn new(model: String, client: LlmClient) -> Self {
        Self { model, client }
    }

    pub async fn summarize(&self, messages: &[ChatMessage]) -> Result<Option<ConversationSummary>> {
        if messages.is_empty() {
            return Ok(None);
        }

        let conversation = format_conversation(messages);
        if conversation.trim().len() < 80 {
            return Ok(None);
        }

        let prompt = format!("{SUMMARY_PROMPT}\n\n<conversation>\n{conversation}\n</conversation>");
        let response = self
            .client
            .chat_completion(
                &self.model,
                vec![ChatMessage {
                    role: "user".to_string(),
                    content: prompt,
                }],
                220,
                0.1,
                None,
            )
            .await;

        match response {
            Ok(raw) => parse_summary_response(&raw).or_else(|_| heuristic_summary(messages)),
            Err(_) => heuristic_summary(messages),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SummarySchema {
    #[serde(default)]
    summary: String,
    #[serde(default = "default_importance")]
    importance: String,
}

fn default_importance() -> String {
    "medium".to_string()
}

fn parse_summary_response(raw: &str) -> Result<Option<ConversationSummary>> {
    let cleaned = strip_code_fences(raw).trim().to_string();
    if cleaned.is_empty() {
        return Ok(None);
    }

    if cleaned.starts_with('{') {
        let parsed: SummarySchema =
            serde_json::from_str(&cleaned).map_err(|e| anyhow!("invalid summary response: {e}"))?;
        let content = parsed.summary.trim().to_string();
        if content.is_empty() {
            return Ok(None);
        }
        return Ok(Some(ConversationSummary {
            content,
            importance: importance_to_score(&parsed.importance),
            source: "llm-summary".to_string(),
        }));
    }

    let content = cleaned.lines().next().unwrap_or("").trim().to_string();
    if content.is_empty() {
        return Ok(None);
    }

    Ok(Some(ConversationSummary {
        content,
        importance: 0.6,
        source: "llm-summary-text".to_string(),
    }))
}

fn heuristic_summary(messages: &[ChatMessage]) -> Result<Option<ConversationSummary>> {
    let mut user_bits = Vec::new();
    let mut assistant_bits = Vec::new();

    for msg in messages.iter().rev() {
        let content = first_sentence(msg.content.trim(), 180);
        if content.is_empty() {
            continue;
        }
        if msg.role == "user" {
            if content.len() >= 20 && !user_bits.contains(&content) {
                user_bits.push(content);
            }
        } else if msg.role == "assistant"
            && content.len() >= 25
            && !assistant_bits.contains(&content)
        {
            assistant_bits.push(content);
        }
        if user_bits.len() >= 2 && assistant_bits.len() >= 1 {
            break;
        }
    }

    if user_bits.is_empty() && assistant_bits.is_empty() {
        return Ok(None);
    }

    user_bits.reverse();
    assistant_bits.reverse();

    let mut parts = Vec::new();
    if !user_bits.is_empty() {
        parts.push(format!(
            "User context: {}",
            user_bits.into_iter().take(2).collect::<Vec<_>>().join("; ")
        ));
    }
    if !assistant_bits.is_empty() {
        parts.push(format!(
            "Assistant outcome: {}",
            assistant_bits
                .into_iter()
                .take(1)
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    let content = parts.join(". ");
    if content.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(ConversationSummary {
        content,
        importance: 0.5,
        source: "heuristic-summary".to_string(),
    }))
}

fn format_conversation(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter_map(|m| {
            let content = m.content.trim();
            if content.is_empty() {
                None
            } else if m.role == "user" {
                Some(format!("USER: {content}"))
            } else {
                Some(format!("ASSISTANT: {content}"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_sentence(input: &str, max_chars: usize) -> String {
    let clipped = input.chars().take(max_chars).collect::<String>();
    for sep in ['.', '!', '?', '\n'] {
        if let Some(pos) = clipped.find(sep) {
            let sentence = clipped[..=pos].trim();
            if !sentence.is_empty() {
                return sentence.to_string();
            }
        }
    }
    clipped.trim().to_string()
}

fn importance_to_score(value: &str) -> f32 {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => 0.9,
        "low" => 0.3,
        _ => 0.6,
    }
}

fn strip_code_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(end) = rest.rfind("```") {
            let inner = &rest[..end];
            let inner = inner.trim_start_matches("json").trim();
            return inner.to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_summary_payload() {
        let raw =
            r#"{"summary":"User prefers Rust and asked for concise replies.","importance":"high"}"#;
        let parsed = parse_summary_response(raw).expect("parse");
        let summary = parsed.expect("summary");
        assert!(summary.content.contains("Rust"));
        assert!(summary.importance > 0.8);
    }

    #[test]
    fn accepts_fenced_json_payload() {
        let raw = "```json\n{\"summary\":\"Team decided to keep sqlite vectors.\",\"importance\":\"medium\"}\n```";
        let parsed = parse_summary_response(raw).expect("parse");
        let summary = parsed.expect("summary");
        assert!(summary.content.contains("sqlite"));
    }

    #[test]
    fn returns_none_for_empty_summary() {
        let raw = r#"{"summary":"","importance":"low"}"#;
        let parsed = parse_summary_response(raw).expect("parse");
        assert!(parsed.is_none());
    }

    #[test]
    fn heuristic_generates_summary() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Please remind me that we deploy every Friday afternoon.".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "Understood, I will keep that deployment cadence in mind.".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Also prefer Rust tooling over Python scripts where possible.".to_string(),
            },
        ];

        let summary = heuristic_summary(&messages)
            .expect("heuristic")
            .expect("summary");
        assert!(summary.content.contains("User context"));
    }
}
