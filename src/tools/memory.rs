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

fn normalize_memory_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    Some(
        path.strip_prefix("memory/")
            .map(|rest| rest.to_string())
            .unwrap_or_else(|| path.to_string()),
    )
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
    /// Namespace for vector memory in Smart mode (example: telegram_123456)
    #[serde(default)]
    pub namespace: Option<String>,
}

fn default_max_results() -> usize {
    6
}

#[derive(Serialize)]
struct MemorySearchResult {
    path: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_id: Option<String>,
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
                description: "Semantically search memory for prior work, decisions, dates, people, preferences, or todos. In smart mode pass namespace (channel_chat_id style, e.g. telegram_123456) to avoid cross-session recall. Returns snippets with path and score.".to_string(),
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
        let namespace = args.namespace;

        async move {
            if let Some(vs) = &vector_store {
                let namespace =
                    match namespace.as_deref() {
                        Some(ns) if !ns.trim().is_empty() => ns,
                        _ => return Ok(
                            "Error: namespace is required in smart mode (example: telegram_123456)"
                                .to_string(),
                        ),
                    };
                // Smart mode: vector search in the provided namespace.
                match vs
                    .search(&query, max_results, 0.0, Some(namespace), 0.3)
                    .await
                {
                    Ok(pairs) => {
                        let results: Vec<MemorySearchResult> = pairs
                            .into_iter()
                            .map(|(item, score)| MemorySearchResult {
                                path: format!("vector/{}", item.id),
                                snippet: item.content,
                                memory_id: Some(item.id),
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
                                memory_id: None,
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
        let workspace = std::env::temp_dir().join(format!("lightclaw-tooltest-{}", Uuid::new_v4()));
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
                    namespace: None,
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
        let workspace = std::env::temp_dir().join(format!("lightclaw-tooltest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());
        let tool = RememberTool::new_file(store.clone());
        let rt = Runtime::new().expect("runtime");

        let out = rt
            .block_on(async {
                tool.call(RememberArgs {
                    content: "User prefers terminal workflows".to_string(),
                    kind: None,
                    namespace: None,
                    source: None,
                    confidence: None,
                })
                .await
            })
            .expect("tool call");

        assert!(out.contains("Remembered (remembered_fact)"));
        let content = store.read_long_term();
        assert!(content.contains("User prefers terminal workflows"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn memory_get_vector_path_requires_vector_mode() {
        let workspace = std::env::temp_dir().join(format!("lightclaw-tooltest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());
        let tool = MemoryGetTool::new(store, None);
        let rt = Runtime::new().expect("runtime");

        let out = rt
            .block_on(async {
                tool.call(MemoryGetArgs {
                    path: "vector/test-id".to_string(),
                    namespace: None,
                    from: None,
                    lines: None,
                })
                .await
            })
            .expect("tool call");

        assert!(out.contains("vector memory is not enabled"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn memory_get_accepts_memory_prefixed_paths() {
        let workspace = std::env::temp_dir().join(format!("lightclaw-tooltest-{}", Uuid::new_v4()));
        let store = MemoryStore::new(workspace.clone());
        std::fs::write(store.memory_dir().join("MEMORY.md"), "hello memory\n").expect("write");
        let tool = MemoryGetTool::new(store, None);
        let rt = Runtime::new().expect("runtime");

        let out = rt
            .block_on(async {
                tool.call(MemoryGetArgs {
                    path: "memory/MEMORY.md".to_string(),
                    namespace: None,
                    from: None,
                    lines: None,
                })
                .await
            })
            .expect("tool call");

        assert!(out.contains("hello memory"));
        let _ = std::fs::remove_dir_all(workspace);
    }
}

// ---------------------------------------------------------------------------
// memory_get
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MemoryGetTool {
    memory_store: MemoryStore,
    vector_store: Option<VectorMemoryStore>,
}

impl MemoryGetTool {
    pub fn new(memory_store: MemoryStore, vector_store: Option<VectorMemoryStore>) -> Self {
        Self {
            memory_store,
            vector_store,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MemoryGetArgs {
    /// Memory path: MEMORY.md, YYYY-MM-DD.md, or vector/<memory-id>
    pub path: String,
    /// Namespace for vector memory when reading vector/<memory-id>
    #[serde(default)]
    pub namespace: Option<String>,
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
                description: "Read memory by path. Supports MEMORY.md, memory/MEMORY.md, YYYY-MM-DD.md, memory/YYYY-MM-DD.md, and vector/<memory-id>. For vector/<memory-id> in smart mode, provide namespace.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(MemoryGetArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let memory_store = self.memory_store.clone();
        let vector_store = self.vector_store.clone();
        let path = args.path.trim().to_string();
        let namespace = args.namespace;
        let from = args.from;
        let lines = args.lines;

        async move {
            if let Some(memory_id) = path.strip_prefix("vector/") {
                let memory_id = memory_id.trim();
                if memory_id.is_empty() {
                    return Ok("Error: vector path must be vector/<memory-id>".to_string());
                }
                let Some(store) = vector_store else {
                    return Ok("Error: vector memory is not enabled".to_string());
                };
                let namespace = match namespace.as_deref() {
                    Some(ns) if !ns.trim().is_empty() => ns,
                    _ => {
                        return Ok("Error: namespace is required for vector paths in smart mode (example: telegram_123456)".to_string())
                    }
                };
                let item = match store.get(memory_id, Some(namespace)).await {
                    Ok(Some(item)) => item,
                    Ok(None) => return Ok(format!("Error: vector memory not found: {memory_id}")),
                    Err(e) => return Ok(format!("Error: vector memory lookup failed: {e}")),
                };
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "path": path,
                    "text": item.content,
                    "score": Value::Null
                }))
                .unwrap_or_else(|_| item.content));
            }

            let Some(path) = normalize_memory_path(&path) else {
                return Ok("Error: path cannot be empty".to_string());
            };
            if !allowed_memory_path(&path) {
                return Ok(format!(
                    "Error: path must be MEMORY.md, YYYY-MM-DD.md, or vector/<memory-id>, got: {path}"
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
    File(MemoryStore),
    Hybrid {
        vector_store: VectorMemoryStore,
        memory_store: MemoryStore,
    },
}

#[derive(Clone)]
pub struct RememberTool {
    backend: RememberBackend,
}

impl RememberTool {
    pub fn new_file(memory_store: MemoryStore) -> Self {
        Self {
            backend: RememberBackend::File(memory_store),
        }
    }

    pub fn new_hybrid(vector_store: VectorMemoryStore, memory_store: MemoryStore) -> Self {
        Self {
            backend: RememberBackend::Hybrid {
                vector_store,
                memory_store,
            },
        }
    }
}

#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RememberKind {
    RememberedFact,
    ConversationObservation,
    UserObservation,
    GroundedFact,
}

impl Default for RememberKind {
    fn default() -> Self {
        Self::RememberedFact
    }
}

impl RememberKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RememberedFact => "remembered_fact",
            Self::ConversationObservation => "conversation_observation",
            Self::UserObservation => "user_observation",
            Self::GroundedFact => "grounded_fact",
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    /// Fact or information to remember
    pub content: String,
    /// Memory type: remembered_fact, conversation_observation, user_observation, grounded_fact
    #[serde(default)]
    pub kind: Option<RememberKind>,
    /// Namespace for vector memory in Smart mode (example: telegram_123456)
    #[serde(default)]
    pub namespace: Option<String>,
    /// Optional source for grounded facts (tool, URL, file path, API endpoint)
    #[serde(default)]
    pub source: Option<String>,
    /// Confidence score for grounded facts [0.0..1.0]
    #[serde(default)]
    pub confidence: Option<f32>,
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
                description: "Save information to long-term memory. Use kind to classify as remembered_fact, conversation_observation, user_observation, or grounded_fact. In smart mode pass namespace for vector memory isolation; grounded_facts can include source/confidence.".to_string(),
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
        let kind = args.kind.unwrap_or_default();
        let namespace = args.namespace;
        let source = args.source;
        let confidence = args.confidence.unwrap_or(0.7).clamp(0.0, 1.0);

        async move {
            if content.is_empty() {
                return Ok("Error: content cannot be empty".to_string());
            }
            match backend {
                RememberBackend::File(store) => {
                    match kind {
                        RememberKind::RememberedFact => store.append_remembered_fact(&content),
                        RememberKind::ConversationObservation => {
                            store.append_conversation_observation(&content)
                        }
                        RememberKind::UserObservation => store.append_user_observation(&content),
                        RememberKind::GroundedFact => store.append_grounded_fact(
                            &content,
                            source.as_deref().unwrap_or("conversation"),
                            confidence,
                        ),
                    }
                    Ok(format!("Remembered ({})", kind.as_str()))
                }
                RememberBackend::Hybrid {
                    vector_store,
                    memory_store,
                } => {
                    match kind {
                        RememberKind::RememberedFact => {
                            memory_store.append_remembered_fact(&content)
                        }
                        RememberKind::ConversationObservation => {
                            memory_store.append_conversation_observation(&content)
                        }
                        RememberKind::UserObservation => {
                            memory_store.append_user_observation(&content)
                        }
                        RememberKind::GroundedFact => memory_store.append_grounded_fact(
                            &content,
                            source.as_deref().unwrap_or("conversation"),
                            confidence,
                        ),
                    }
                    let namespace = match namespace.as_deref() {
                        Some(ns) if !ns.trim().is_empty() => ns,
                        _ => {
                            return Ok("Remembered in file memory only: namespace is required for vector memory in smart mode (example: telegram_123456)".to_string())
                        }
                    };
                    let mut meta = HashMap::new();
                    meta.insert("importance".to_string(), Value::from(confidence as f64));
                    meta.insert("confidence".to_string(), Value::from(confidence as f64));
                    meta.insert("kind".to_string(), Value::from(kind.as_str()));
                    if let Some(src) = source {
                        if !src.trim().is_empty() {
                            meta.insert("source".to_string(), Value::from(src));
                        }
                    }
                    match vector_store
                        .add(&content, meta, Some(namespace), None)
                        .await
                    {
                        Ok(_) => Ok(format!("Remembered ({})", kind.as_str())),
                        Err(e) => Ok(format!(
                            "Remembered in file memory ({}) (vector add failed: {})",
                            kind.as_str(),
                            e
                        )),
                    }
                }
            }
        }
    }
}
