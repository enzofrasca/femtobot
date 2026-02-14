use crate::memory::simple::file_store::MemoryStore;
use crate::memory::smart::vector_store::VectorMemoryStore;
use crate::tools::ToolError;
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

fn allowed_memory_path(name: &str) -> bool {
    if name == "MEMORY.md" {
        return true;
    }
    is_daily_memory_file(name)
}

fn is_daily_memory_file(name: &str) -> bool {
    if name.len() != 13 || !name.ends_with(".md") {
        return false;
    }
    let date = name.as_bytes();
    if date[4] != b'-' || date[7] != b'-' {
        return false;
    }
    date[..10].iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        _ => c.is_ascii_digit(),
    })
}

fn collect_memory_file_sources(memory_store: &MemoryStore) -> Vec<(String, String)> {
    let mut sources = Vec::new();

    let long_term = memory_store.read_long_term();
    if !long_term.is_empty() {
        sources.push(("memory/MEMORY.md".to_string(), long_term));
    }

    let mut dated_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(memory_store.memory_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == "MEMORY.md" || !allowed_memory_path(name) {
                continue;
            }
            dated_files.push((name.to_string(), path));
        }
    }

    // Newest date files first because names are YYYY-MM-DD.md.
    dated_files.sort_by(|a, b| b.0.cmp(&a.0));
    for (name, path) in dated_files {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                sources.push((format!("memory/{name}"), content));
            }
        }
    }

    sources
}

// ---------------------------------------------------------------------------
// memory_search
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MemorySearchTool {
    memory_store: MemoryStore,
    vector_store: Option<VectorMemoryStore>,
}

impl MemorySearchTool {
    pub fn new(memory_store: MemoryStore, vector_store: Option<VectorMemoryStore>) -> Self {
        Self {
            memory_store,
            vector_store,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MemorySearchArgs {
    /// Search query (semantic for Smart mode, keyword for Simple)
    pub query: String,
    /// Max results to return
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

fn default_max_results() -> usize {
    6
}

#[derive(Serialize)]
struct MemorySearchResult {
    path: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f32>,
}

impl Tool for MemorySearchTool {
    const NAME: &'static str = "memory_search";
    type Args = MemorySearchArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Semantically search MEMORY.md and memory/*.md for prior work, decisions, dates, people, preferences, or todos. Use before answering questions about past context. Returns snippets with path and score.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(MemorySearchArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let memory_store = self.memory_store.clone();
        let vector_store = self.vector_store.clone();
        let query = args.query;
        let max_results = args.max_results.min(20);

        async move {
            if let Some(vs) = &vector_store {
                // Smart mode: vector search (uses store's default namespace)
                match vs.search(&query, max_results, 0.0, None, 0.3).await {
                    Ok(pairs) => {
                        let results: Vec<MemorySearchResult> = pairs
                            .into_iter()
                            .map(|(item, score)| MemorySearchResult {
                                path: "vector".to_string(),
                                snippet: item.content,
                                score: Some(score),
                            })
                            .collect();
                        Ok(serde_json::to_string_pretty(&serde_json::json!({
                            "results": results,
                            "source": "vector"
                        }))
                        .unwrap_or_else(|_| "[]".to_string()))
                    }
                    Err(e) => Ok(format!("Error: vector search failed: {e}")),
                }
            } else {
                // Simple mode: text search over memory files
                let q_lower = query.to_lowercase();
                let mut results = Vec::new();
                let sources = collect_memory_file_sources(&memory_store);
                for (path, content) in sources {
                    for line in content.lines() {
                        if line.to_lowercase().contains(&q_lower) && !line.trim().is_empty() {
                            results.push(MemorySearchResult {
                                path: path.clone(),
                                snippet: line.trim().to_string(),
                                score: None,
                            });
                            if results.len() >= max_results {
                                break;
                            }
                        }
                    }
                }

                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "results": results,
                    "source": "file"
                }))
                .unwrap_or_else(|_| "[]".to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;
    use uuid::Uuid;

    #[test]
    fn memory_search_simple_scans_historical_daily_files() {
        let workspace = std::env::temp_dir().join(format!("femtobot-tooltest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());
        let memory_dir = store.memory_dir().to_path_buf();

        std::fs::write(memory_dir.join("MEMORY.md"), "General notes\n").expect("write memory");
        std::fs::write(
            memory_dir.join("2025-01-01.md"),
            "Project decision: use rust-analyzer cache\n",
        )
        .expect("write historical");

        let tool = MemorySearchTool::new(store, None);
        let rt = Runtime::new().expect("runtime");
        let out = rt
            .block_on(async {
                tool.call(MemorySearchArgs {
                    query: "rust-analyzer".to_string(),
                    max_results: 5,
                })
                .await
            })
            .expect("tool call");

        let parsed: Value = serde_json::from_str(&out).expect("json output");
        let results = parsed["results"].as_array().expect("results array");
        assert!(results
            .iter()
            .any(|r| r["path"].as_str() == Some("memory/2025-01-01.md")));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn remember_tool_file_backend_persists_fact() {
        let workspace = std::env::temp_dir().join(format!("femtobot-tooltest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());
        let tool = RememberTool::new_file(store.clone());
        let rt = Runtime::new().expect("runtime");

        let out = rt
            .block_on(async {
                tool.call(RememberArgs {
                    content: "User prefers terminal workflows".to_string(),
                })
                .await
            })
            .expect("tool call");

        assert!(out.contains("Remembered:"));
        let content = store.read_long_term();
        assert!(content.contains("User prefers terminal workflows"));

        let _ = std::fs::remove_dir_all(workspace);
    }
}

// ---------------------------------------------------------------------------
// memory_get
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MemoryGetTool {
    memory_store: MemoryStore,
}

impl MemoryGetTool {
    pub fn new(memory_store: MemoryStore) -> Self {
        Self { memory_store }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MemoryGetArgs {
    /// Path relative to memory/ (e.g. MEMORY.md or 2025-02-14.md)
    pub path: String,
    /// Start line (1-based)
    #[serde(default)]
    pub from: Option<usize>,
    /// Number of lines to read
    #[serde(default)]
    pub lines: Option<usize>,
}

impl Tool for MemoryGetTool {
    const NAME: &'static str = "memory_get";
    type Args = MemoryGetArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Read MEMORY.md or memory/YYYY-MM-DD.md by path. Use after memory_search to pull specific lines. Path must be MEMORY.md or a date file like 2025-02-14.md.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(MemoryGetArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let memory_store = self.memory_store.clone();
        let path = args.path.trim().to_string();
        let from = args.from;
        let lines = args.lines;

        async move {
            if !allowed_memory_path(&path) {
                return Ok(format!(
                    "Error: path must be MEMORY.md or YYYY-MM-DD.md, got: {path}"
                ));
            }
            let full_path = memory_store.memory_dir().join(&path);
            if !full_path.exists() {
                return Ok(format!("Error: file not found: {path}"));
            }
            let content = match tokio::fs::read_to_string(&full_path).await {
                Ok(c) => c,
                Err(e) => return Ok(format!("Error reading file: {e}")),
            };
            let out = if let (Some(from_line), Some(n)) = (from, lines) {
                let from_idx = from_line.saturating_sub(1);
                content
                    .lines()
                    .skip(from_idx)
                    .take(n)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content
            };
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "path": path,
                "text": out
            }))
            .unwrap_or_else(|_| out))
        }
    }
}

// ---------------------------------------------------------------------------
// remember (Simple + Smart modes)
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum RememberBackend {
    Vector(VectorMemoryStore),
    File(MemoryStore),
}

#[derive(Clone)]
pub struct RememberTool {
    backend: RememberBackend,
}

impl RememberTool {
    pub fn new_vector(vector_store: VectorMemoryStore) -> Self {
        Self {
            backend: RememberBackend::Vector(vector_store),
        }
    }

    pub fn new_file(memory_store: MemoryStore) -> Self {
        Self {
            backend: RememberBackend::File(memory_store),
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    /// Fact or information to remember
    pub content: String,
}

impl Tool for RememberTool {
    const NAME: &'static str = "remember";
    type Args = RememberArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Save important information to long-term memory. Use for preferences, facts, decisions, dates, people, or anything worth recalling later. For longer notes, use write_file to memory/MEMORY.md instead.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(RememberArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let backend = self.backend.clone();
        let content = args.content.trim().to_string();

        async move {
            if content.is_empty() {
                return Ok("Error: content cannot be empty".to_string());
            }
            match backend {
                RememberBackend::Vector(store) => {
                    let mut meta = HashMap::new();
                    meta.insert("importance".to_string(), Value::from(0.7));
                    match store.add(&content, meta, Some("default"), None).await {
                        Ok(item) => Ok(format!("Remembered: {}", item.content)),
                        Err(e) => Ok(format!("Error: {e}")),
                    }
                }
                RememberBackend::File(store) => {
                    store.append_remembered_fact(&content);
                    Ok(format!("Remembered: {}", content))
                }
            }
        }
    }
}
