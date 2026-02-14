use tracing::debug;

use crate::memory::smart::client::ChatMessage;

const FACT_KEYWORDS: &[&str] = &[
    "my name is",
    "i am",
    "i'm",
    "i work",
    "i live",
    "i prefer",
    "remember that",
    "note that",
    "important:",
    "email:",
    "phone:",
    "address:",
    "birthday:",
    "project uses",
    "using",
    "configured to",
];

#[derive(Clone, Debug)]
pub struct CompactionConfig {
    pub threshold: usize,
    pub recent_turns_keep: usize,
    pub summary_max_turns: usize,
    pub max_facts: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            threshold: 50,
            recent_turns_keep: 8,
            summary_max_turns: 15,
            max_facts: 10,
        }
    }
}

pub struct SessionCompactor {
    pub config: CompactionConfig,
}

impl SessionCompactor {
    pub fn new(config: Option<CompactionConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
        }
    }

    pub fn compact(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        if messages.len() < self.config.threshold {
            debug!(
                "Skipping compaction: {} < {}",
                messages.len(),
                self.config.threshold
            );
            return messages.to_vec();
        }

        let recent_count = self.config.recent_turns_keep * 2;
        let recent_start = messages.len().saturating_sub(recent_count);
        let recent = &messages[recent_start..];

        let middle_count = self.config.summary_max_turns * 2;
        let middle_end = recent_start;
        let middle_start = middle_end.saturating_sub(middle_count);
        let middle = &messages[middle_start..middle_end];

        let old = &messages[..middle_start];

        let mut compacted: Vec<ChatMessage> = Vec::new();
        let mut recall_parts: Vec<String> = Vec::new();

        if !old.is_empty() {
            let facts = self.extract_facts(old);
            if !facts.is_empty() {
                recall_parts.push(format!("Key facts:\n{}", facts));
            }
        }

        if !middle.is_empty() {
            let summary = self.summarize(middle);
            if !summary.is_empty() {
                recall_parts.push(format!("Recent discussion summary:\n{}", summary));
            }
        }

        if !recall_parts.is_empty() {
            let recall = format!(
                "[Recalling from earlier in our conversation]\n\n{}",
                recall_parts.join("\n\n")
            );
            compacted.push(ChatMessage {
                role: "assistant".to_string(),
                content: recall,
            });
        }

        compacted.extend_from_slice(recent);
        compacted
    }

    fn extract_facts(&self, messages: &[ChatMessage]) -> String {
        let facts = extract_facts_from_messages(messages, self.config.max_facts);
        facts
            .into_iter()
            .map(|fact| format!("- {}", fact))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn summarize(&self, messages: &[ChatMessage]) -> String {
        const MIN_QUESTION_LENGTH: usize = 20;
        const MIN_CONTENT_LENGTH: usize = 50;
        const MIN_SENTENCE_LENGTH: usize = 30;
        const MAX_EXTRACT_LENGTH: usize = 150;

        let mut user_questions = Vec::new();
        let mut assistant_conclusions = Vec::new();

        for msg in messages {
            let content = msg.content.trim();
            if content.is_empty() {
                continue;
            }
            if msg.role == "user" {
                for line in content.lines() {
                    let line = line.trim();
                    if line.ends_with('?') && line.len() > MIN_QUESTION_LENGTH {
                        let extracted = line.chars().take(MAX_EXTRACT_LENGTH).collect::<String>();
                        if !user_questions.contains(&extracted) {
                            user_questions.push(extracted);
                        }
                    }
                }
            }
            if msg.role == "assistant" && content.len() > MIN_CONTENT_LENGTH {
                let sentences = content.split('.').take(3);
                for sentence in sentences {
                    let sentence = sentence.trim();
                    if sentence.len() > MIN_SENTENCE_LENGTH {
                        let extracted = sentence
                            .chars()
                            .take(MAX_EXTRACT_LENGTH)
                            .collect::<String>();
                        if !assistant_conclusions.contains(&extracted) {
                            assistant_conclusions.push(extracted);
                        }
                        break;
                    }
                }
            }
        }

        let mut parts = Vec::new();
        if !user_questions.is_empty() {
            parts.push("User asked about:".to_string());
            for q in user_questions.iter().take(3) {
                parts.push(format!("  - {}", q));
            }
        }
        if !assistant_conclusions.is_empty() {
            parts.push("Assistant responses:".to_string());
            for c in assistant_conclusions.iter().take(3) {
                parts.push(format!("  - {}", c));
            }
        }
        if parts.is_empty() {
            "General discussion continued".to_string()
        } else {
            parts.join("\n")
        }
    }
}

fn extract_facts_from_messages(messages: &[ChatMessage], max_facts: usize) -> Vec<String> {
    let mut facts = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for msg in messages {
        if msg.role == "system" {
            continue;
        }
        for line in msg.content.lines() {
            let line = line.trim();
            if line.len() < 10 {
                continue;
            }
            if FACT_KEYWORDS
                .iter()
                .any(|kw| line.to_ascii_lowercase().contains(kw))
            {
                let fact = line.chars().take(200).collect::<String>();
                if seen.insert(fact.clone()) {
                    facts.push(fact);
                    if facts.len() >= max_facts {
                        return facts;
                    }
                }
            }
        }
    }
    facts
}
