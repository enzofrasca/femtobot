use chrono::{Datelike, Local};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const MAX_CONTEXT_TOKENS: usize = 2000;
pub const CHARS_PER_TOKEN: usize = 4;
pub const MAX_CONTEXT_CHARS: usize = MAX_CONTEXT_TOKENS * CHARS_PER_TOKEN;

/// Maximum size of the Extracted Notes section before trimming oldest entries.
const MAX_EXTRACTED_NOTES_CHARS: usize = 8000;
const EXTRACTED_SECTION_HEADER: &str = "## Extracted Notes";
const REMEMBERED_FACTS_SECTION_HEADER: &str = "## Remembered Facts";
const CONVERSATION_OBSERVATIONS_SECTION_HEADER: &str = "## Conversation Observations";
const USER_OBSERVATIONS_SECTION_HEADER: &str = "## User Observations";
const GROUNDED_FACTS_SECTION_HEADER: &str = "## Grounded Facts";
static MEMORY_FILE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone)]
pub struct MemoryStore {
    workspace: PathBuf,
    memory_dir: PathBuf,
    memory_file: PathBuf,
}

impl MemoryStore {
    pub fn new(workspace: PathBuf) -> Self {
        let memory_dir = ensure_dir(&workspace.join("memory"));
        let memory_file = memory_dir.join("MEMORY.md");
        Self {
            workspace,
            memory_dir,
            memory_file,
        }
    }

    pub fn get_today_file(&self) -> PathBuf {
        self.memory_dir.join(format!("{}.md", today_date()))
    }

    pub fn read_today(&self) -> String {
        let today_file = self.get_today_file();
        fs::read_to_string(today_file).unwrap_or_default()
    }

    pub fn read_long_term(&self) -> String {
        fs::read_to_string(&self.memory_file).unwrap_or_default()
    }

    pub fn get_memory_context(&self, max_chars: usize) -> String {
        let mut parts = Vec::new();
        let mut remaining = max_chars;

        let long_term_budget = (max_chars as f64 * 0.6) as usize;
        let long_term = self.read_long_term();
        if !long_term.is_empty() {
            let truncated = truncate(&long_term, long_term_budget);
            parts.push(format!("## Long-term Memory\n{}", truncated));
            remaining = remaining.saturating_sub(truncated.len());
        }

        let today = self.read_today();
        if !today.is_empty() && remaining > 100 {
            let truncated = truncate(&today, remaining);
            parts.push(format!("## Today's Notes\n{}", truncated));
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n\n")
        }
    }

    /// Append auto-extracted facts to the `## Extracted Notes` section of
    /// MEMORY.md. If the section grows past `MAX_EXTRACTED_NOTES_CHARS`, the
    /// oldest bullet points are trimmed from the top.
    pub fn append_extracted_facts(&self, facts: &[String]) {
        let today = today_date();
        let entries: Vec<String> = facts.iter().map(|f| format!("- [{today}] {f}")).collect();
        self.append_section_entries(
            EXTRACTED_SECTION_HEADER,
            &entries,
            Some(MAX_EXTRACTED_NOTES_CHARS),
        );
    }

    pub fn append_remembered_fact(&self, fact: &str) {
        let fact = fact.trim();
        if fact.is_empty() {
            return;
        }
        let today = today_date();
        self.append_section_entries(
            REMEMBERED_FACTS_SECTION_HEADER,
            &[format!("- [{today}] {fact}")],
            None,
        );
    }

    pub fn append_conversation_observation(&self, observation: &str) {
        let observation = observation.trim();
        if observation.is_empty() {
            return;
        }
        let today = today_date();
        self.append_section_entries(
            CONVERSATION_OBSERVATIONS_SECTION_HEADER,
            &[format!("- [{today}] {observation}")],
            None,
        );
    }

    pub fn append_user_observation(&self, observation: &str) {
        let observation = observation.trim();
        if observation.is_empty() {
            return;
        }
        let today = today_date();
        self.append_section_entries(
            USER_OBSERVATIONS_SECTION_HEADER,
            &[format!("- [{today}] {observation}")],
            None,
        );
    }

    pub fn append_grounded_fact(&self, fact: &str, source: &str, confidence: f32) {
        let fact = fact.trim();
        if fact.is_empty() {
            return;
        }
        let source = if source.trim().is_empty() {
            "unknown"
        } else {
            source.trim()
        };
        let confidence = confidence.clamp(0.0, 1.0);
        let today = today_date();
        self.append_section_entries(
            GROUNDED_FACTS_SECTION_HEADER,
            &[format!(
                "- [{today}] {fact} (source: {source}, confidence: {confidence:.2})"
            )],
            None,
        );
    }

    fn append_section_entries(
        &self,
        section_header: &str,
        entries: &[String],
        max_section_chars: Option<usize>,
    ) {
        if entries.is_empty() {
            return;
        }

        let _guard = match MEMORY_FILE_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let new_lines = entries.join("\n");
        let existing = fs::read_to_string(&self.memory_file).unwrap_or_default();
        let updated = if let Some(section_start) = existing.find(section_header) {
            let after_header = section_start + section_header.len();
            let mut before = existing[..after_header].to_string();
            let rest = &existing[after_header..];
            let section_end = rest.find("\n## ").unwrap_or(rest.len());
            let section_body = rest[..section_end].trim_start_matches('\n');
            let after_section = &rest[section_end..];
            let mut combined = if section_body.is_empty() {
                new_lines
            } else {
                format!("{section_body}\n{new_lines}")
            };

            if let Some(limit) = max_section_chars {
                while combined.len() > limit {
                    if let Some(newline_pos) = combined.find('\n') {
                        combined = combined[newline_pos + 1..].to_string();
                    } else {
                        break;
                    }
                }
            }

            before.push('\n');
            before.push_str(&combined);
            before.push_str(after_section);
            before
        } else {
            let mut content = existing;
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&format!("\n{section_header}\n{new_lines}\n"));
            content
        };

        if let Ok(mut file) = fs::File::create(&self.memory_file) {
            let _ = file.write_all(updated.as_bytes());
        }
    }

    #[allow(dead_code)]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }
}

fn ensure_dir(path: &Path) -> PathBuf {
    if let Err(err) = fs::create_dir_all(path) {
        eprintln!("Failed to create dir {}: {}", path.display(), err);
    }
    path.to_path_buf()
}

fn today_date() -> String {
    let now = Local::now().date_naive();
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

fn truncate(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }

    let truncate_at = max_chars.saturating_sub(20);
    for sep in ["\n\n", ".\n", ". ", "\n"] {
        if let Some(pos) = content[..truncate_at].rfind(sep) {
            if pos > truncate_at / 2 {
                return format!("{}{}\n... (truncated)", &content[..pos + sep.len()], "");
            }
        }
    }

    if let Some(pos) = content[..truncate_at].rfind(' ') {
        if pos > truncate_at / 2 {
            return format!("{} ... (truncated)", &content[..pos]);
        }
    }

    format!("{}... (truncated)", &content[..truncate_at])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use uuid::Uuid;

    #[test]
    fn append_extracted_facts_keeps_all_concurrent_writes() {
        let workspace = std::env::temp_dir().join(format!("lightclaw-memtest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());

        let mut handles = Vec::new();
        for i in 0..20 {
            let s = store.clone();
            handles.push(thread::spawn(move || {
                s.append_extracted_facts(&[format!("fact-{i}")]);
            }));
        }
        for handle in handles {
            handle.join().expect("thread join");
        }

        let content = store.read_long_term();
        for i in 0..20 {
            assert!(content.contains(&format!("fact-{i}")));
        }

        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn append_remembered_fact_creates_and_appends_section() {
        let workspace = std::env::temp_dir().join(format!("lightclaw-memtest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());

        store.append_remembered_fact("User prefers concise responses");
        store.append_remembered_fact("User uses Rust");

        let content = store.read_long_term();
        assert!(content.contains(REMEMBERED_FACTS_SECTION_HEADER));
        assert!(content.contains("User prefers concise responses"));
        assert!(content.contains("User uses Rust"));

        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn appends_user_observation_and_grounded_fact_sections() {
        let workspace = std::env::temp_dir().join(format!("lightclaw-memtest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());

        store.append_user_observation("I prefer concise replies.");
        store.append_grounded_fact("Build succeeded in 2m31s", "cargo build", 0.92);

        let content = store.read_long_term();
        assert!(content.contains(USER_OBSERVATIONS_SECTION_HEADER));
        assert!(content.contains("I prefer concise replies."));
        assert!(content.contains(GROUNDED_FACTS_SECTION_HEADER));
        assert!(content.contains("source: cargo build"));
        assert!(content.contains("confidence: 0.92"));

        let _ = fs::remove_dir_all(workspace);
    }
}
