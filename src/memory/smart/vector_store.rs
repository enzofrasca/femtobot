use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use rig::vector_store::request::{SearchFilter, VectorSearchRequest};
use rig::vector_store::{VectorStoreError, VectorStoreIndex};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::memory::smart::client::LlmClient;
use tokio::sync::Mutex as AsyncMutex;

const MAX_CONTENT_LENGTH: usize = 8192;
const MAX_CACHE_ENTRIES: usize = 512;
/// Maximum number of rows to load during a vector search.
/// Prevents unbounded full-table scans; the highest-priority/most-recent
/// rows are returned first thanks to the composite index.
const MAX_SEARCH_ROWS: usize = 500;

static NAMESPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]{1,64}$").unwrap());

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub content: String,
    #[serde(skip)]
    pub embedding: Vec<f32>,
    pub metadata: HashMap<String, Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub access_count: i64,
    pub priority: f32,
    pub namespace: String,
}

/// Default priority weight used when blending similarity with priority score.
const DEFAULT_PRIORITY_WEIGHT: f32 = 0.3;
/// Default similarity threshold for vector search.
const DEFAULT_THRESHOLD: f32 = 0.0;

#[derive(Clone)]
struct CacheEntry {
    embedding: Vec<f32>,
    insert_order: u64,
}

#[derive(Clone)]
pub struct EmbeddingService {
    client: LlmClient,
    model: String,
    cache: Arc<AsyncMutex<EmbeddingCache>>,
}

#[derive(Clone)]
struct EmbeddingCache {
    entries: HashMap<String, CacheEntry>,
    counter: u64,
}

impl EmbeddingCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            counter: 0,
        }
    }

    fn get(&self, key: &str) -> Option<&Vec<f32>> {
        self.entries.get(key).map(|e| &e.embedding)
    }

    fn insert(&mut self, key: String, embedding: Vec<f32>) {
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            self.evict_oldest_quarter();
        }
        self.counter += 1;
        self.entries.insert(
            key,
            CacheEntry {
                embedding,
                insert_order: self.counter,
            },
        );
    }

    /// Evict the oldest 25% of entries by insertion order.
    fn evict_oldest_quarter(&mut self) {
        let to_remove = self.entries.len() / 4;
        if to_remove == 0 {
            self.entries.clear();
            return;
        }
        let mut by_age: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.insert_order))
            .collect();
        by_age.sort_by_key(|(_, order)| *order);
        for (key, _) in by_age.into_iter().take(to_remove) {
            self.entries.remove(&key);
        }
    }
}

impl EmbeddingService {
    pub fn new(client: LlmClient, model: String) -> Self {
        Self {
            client,
            model,
            cache: Arc::new(AsyncMutex::new(EmbeddingCache::new())),
        }
    }

    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        if text.trim().is_empty() {
            return Err(anyhow!("cannot embed empty text"));
        }
        let cache = self.cache.lock().await;
        if let Some(cached) = cache.get(text) {
            return Ok(cached.clone());
        }
        drop(cache);
        let embedding = self.client.embeddings(&self.model, text).await?;
        let mut cache = self.cache.lock().await;
        cache.insert(text.to_string(), embedding.clone());
        Ok(embedding)
    }
}

#[derive(Clone)]
pub struct VectorMemoryStore {
    conn: Arc<Mutex<Connection>>,
    embedder: EmbeddingService,
    max_memories: usize,
    namespace: String,
}

impl VectorMemoryStore {
    pub fn new(
        db_path: PathBuf,
        embedder: EmbeddingService,
        max_memories: usize,
        namespace: String,
    ) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        init_db(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
            max_memories,
            namespace: validate_namespace(&namespace)?,
        })
    }

    /// Run a blocking closure against the database connection on Tokio's
    /// blocking thread pool, avoiding stalls on the async runtime.
    async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| anyhow!("mutex poisoned: {e}"))?;
            f(&conn)
        })
        .await
        .map_err(|e| anyhow!("blocking task failed: {e}"))?
    }

    pub async fn add(
        &self,
        content: &str,
        metadata: HashMap<String, Value>,
        namespace: Option<&str>,
        precomputed_embedding: Option<Vec<f32>>,
    ) -> Result<MemoryItem> {
        let content = content.trim();
        if content.is_empty() {
            return Err(anyhow!("content cannot be empty"));
        }
        if content.len() > MAX_CONTENT_LENGTH {
            return Err(anyhow!("content exceeds maximum length"));
        }
        let namespace = validate_namespace(namespace.unwrap_or(&self.namespace))?;
        let embedding = match precomputed_embedding {
            Some(e) if !e.is_empty() => e,
            _ => self.embedder.embed(content).await?,
        };
        let now = Utc::now();
        let memory_id = Uuid::new_v4().to_string();
        let embedding_blob = f32s_to_bytes(&embedding);
        let importance = metadata
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let priority = (importance * 0.4 + 0.3).clamp(0.0, 1.0) as f32;

        let content_owned = content.to_string();
        let ns = namespace.clone();
        let mid = memory_id.clone();
        let metadata_json = serde_json::to_string(&metadata)?;
        let now_str = now.to_rfc3339();
        let max_mem = self.max_memories;

        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO memories (id, content, embedding, metadata, created_at, updated_at, access_count, priority, namespace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![mid, content_owned, embedding_blob, metadata_json, now_str, now_str, 0i64, priority, ns],
            )?;
            prune_if_needed(conn, &ns, max_mem)?;
            Ok(())
        }).await?;

        Ok(MemoryItem {
            id: memory_id,
            content: content.to_string(),
            embedding,
            metadata,
            created_at: now,
            updated_at: now,
            access_count: 0,
            priority,
            namespace,
        })
    }

    pub async fn update(
        &self,
        memory_id: &str,
        content: &str,
        metadata: HashMap<String, Value>,
        namespace: Option<&str>,
        precomputed_embedding: Option<Vec<f32>>,
    ) -> Result<Option<MemoryItem>> {
        let content = content.trim();
        if content.is_empty() {
            return Err(anyhow!("content cannot be empty"));
        }
        if content.len() > MAX_CONTENT_LENGTH {
            return Err(anyhow!("content exceeds maximum length"));
        }
        let namespace = validate_namespace(namespace.unwrap_or(&self.namespace))?;

        let existing = self.get(memory_id, Some(&namespace)).await?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let embedding = match precomputed_embedding {
            Some(e) if !e.is_empty() => e,
            _ => {
                if content == existing.content {
                    existing.embedding.clone()
                } else {
                    self.embedder.embed(content).await?
                }
            }
        };
        let embedding_blob = f32s_to_bytes(&embedding);
        let now = Utc::now();
        let importance = metadata
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let age_days = (now - existing.created_at).num_seconds() as f64 / 86400.0;
        let recency = (1.0 - (age_days / 30.0)).clamp(0.0, 1.0);
        let access_score = ((existing.access_count as f64).sqrt() / 10.0).clamp(0.0, 1.0);
        let priority =
            (importance * 0.4 + recency * 0.3 + access_score * 0.3).clamp(0.0, 1.0) as f32;

        let content_owned = content.to_string();
        let ns = namespace.clone();
        let mid = memory_id.to_string();
        let metadata_json = serde_json::to_string(&metadata)?;
        let now_str = now.to_rfc3339();

        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE memories SET content = ?1, embedding = ?2, metadata = ?3, updated_at = ?4, priority = ?5 WHERE id = ?6 AND namespace = ?7",
                params![content_owned, embedding_blob, metadata_json, now_str, priority, mid, ns],
            )?;
            Ok(())
        }).await?;

        Ok(Some(MemoryItem {
            id: memory_id.to_string(),
            content: content.to_string(),
            embedding,
            metadata,
            created_at: existing.created_at,
            updated_at: now,
            access_count: existing.access_count,
            priority,
            namespace,
        }))
    }

    pub async fn delete(&self, memory_id: &str, namespace: Option<&str>) -> Result<bool> {
        let namespace = validate_namespace(namespace.unwrap_or(&self.namespace))?;
        let mid = memory_id.to_string();
        let ns = namespace;

        self.with_conn(move |conn| {
            let rows = conn.execute(
                "DELETE FROM memories WHERE id = ?1 AND namespace = ?2",
                params![mid, ns],
            )?;
            Ok(rows > 0)
        })
        .await
    }

    pub async fn get(
        &self,
        memory_id: &str,
        namespace: Option<&str>,
    ) -> Result<Option<MemoryItem>> {
        let namespace = validate_namespace(namespace.unwrap_or(&self.namespace))?;
        let mid = memory_id.to_string();
        let ns = namespace;

        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content, embedding, metadata, created_at, updated_at, access_count, priority, namespace FROM memories WHERE id = ?1 AND namespace = ?2",
            )?;
            let row = stmt
                .query_row(params![mid, ns], parse_memory_row)
                .optional()?;
            Ok(row)
        }).await
    }

    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
        threshold: f32,
        namespace: Option<&str>,
        priority_weight: f32,
    ) -> Result<Vec<(MemoryItem, f32)>> {
        let (results, _embedding) = self
            .search_with_embedding(query, top_k, threshold, namespace, priority_weight)
            .await?;
        Ok(results)
    }

    /// Like `search`, but also returns the query embedding so callers can
    /// reuse it and avoid a redundant embedding API call.
    pub async fn search_with_embedding(
        &self,
        query: &str,
        top_k: usize,
        threshold: f32,
        namespace: Option<&str>,
        priority_weight: f32,
    ) -> Result<(Vec<(MemoryItem, f32)>, Vec<f32>)> {
        let namespace = validate_namespace(namespace.unwrap_or(&self.namespace))?;
        let query_embedding = self.embedder.embed(query).await?;
        let results = self
            .search_inner(
                query_embedding.clone(),
                top_k,
                threshold,
                namespace,
                priority_weight,
            )
            .await?;
        Ok((results, query_embedding))
    }

    /// Shared search implementation used by both `search()` and `VectorStoreIndex::top_n()`.
    /// Returns `(MemoryItem, similarity_score)` pairs sorted by combined score.
    /// Also bumps `access_count` for the returned memories.
    ///
    /// Rows are capped at `MAX_SEARCH_ROWS` to avoid unbounded full-table scans.
    async fn search_inner(
        &self,
        query_embedding: Vec<f32>,
        top_k: usize,
        threshold: f32,
        namespace: String,
        priority_weight: f32,
    ) -> Result<Vec<(MemoryItem, f32)>> {
        let ns = namespace;
        let row_limit = MAX_SEARCH_ROWS;

        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content, embedding, metadata, created_at, updated_at, access_count, priority, namespace \
                 FROM memories WHERE namespace = ?1 \
                 ORDER BY priority DESC, updated_at DESC \
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![ns, row_limit as i64], parse_memory_row)?;

            let mut results: Vec<(MemoryItem, f32, f32)> = Vec::new();
            for row in rows {
                let item = row?;
                let similarity = cosine_similarity(&query_embedding, &item.embedding);
                if similarity >= threshold {
                    let combined = similarity * (1.0 - priority_weight) + item.priority * priority_weight;
                    results.push((item, similarity, combined));
                }
            }

            results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
            let trimmed: Vec<(MemoryItem, f32)> = results
                .into_iter()
                .take(top_k)
                .map(|(item, sim, _)| (item, sim))
                .collect();

            // Bump access_count for all returned memories.
            for (item, _) in &trimmed {
                let _ = conn.execute(
                    "UPDATE memories SET access_count = access_count + 1 WHERE id = ?1 AND namespace = ?2",
                    params![item.id, item.namespace],
                );
            }

            Ok(trimmed)
        }).await
    }
}

fn parse_memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryItem> {
    let embedding_blob: Vec<u8> = row.get(2)?;
    let embedding = bytes_to_f32s(&embedding_blob);
    let metadata_str: String = row.get(3)?;
    let metadata: HashMap<String, Value> = serde_json::from_str(&metadata_str).unwrap_or_default();
    let created_at: String = row.get(4)?;
    let updated_at: String = row.get(5)?;
    Ok(MemoryItem {
        id: row.get(0)?,
        content: row.get(1)?,
        embedding,
        metadata,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc),
        access_count: row.get(6)?,
        priority: row.get(7)?,
        namespace: row.get(8)?,
    })
}

fn init_db(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS memories (\
            id TEXT PRIMARY KEY,\
            content TEXT NOT NULL,\
            embedding BLOB NOT NULL,\
            metadata TEXT DEFAULT '{}',\
            created_at TEXT NOT NULL,\
            updated_at TEXT NOT NULL,\
            access_count INTEGER DEFAULT 0,\
            priority REAL DEFAULT 0.5,\
            namespace TEXT DEFAULT 'default'\
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_updated ON memories(updated_at DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace)",
        [],
    )?;
    // Composite index used by search_inner (ORDER BY priority DESC, updated_at DESC)
    // and prune_if_needed (ORDER BY priority ASC, updated_at ASC).
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_ns_priority ON memories(namespace, priority DESC, updated_at DESC)",
        [],
    )?;
    Ok(())
}

fn validate_namespace(namespace: &str) -> Result<String> {
    if NAMESPACE_RE.is_match(namespace) {
        return Ok(namespace.to_string());
    }
    let sanitized = namespace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.chars().take(64).collect::<String>();
    if trimmed.is_empty() {
        return Err(anyhow!("invalid namespace"));
    }
    Ok(trimmed)
}

fn prune_if_needed(conn: &Connection, namespace: &str, max_memories: usize) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE namespace = ?1",
        params![namespace],
        |row| row.get(0),
    )?;
    if count as usize > max_memories {
        let excess = count as usize - max_memories;
        let mut stmt = conn.prepare(
            "SELECT id FROM memories WHERE namespace = ?1 ORDER BY priority ASC, updated_at ASC LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(params![namespace, excess as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for id in ids {
            conn.execute(
                "DELETE FROM memories WHERE id = ?1 AND namespace = ?2",
                params![id, namespace],
            )?;
        }
    }
    Ok(())
}

fn f32s_to_bytes(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn bytes_to_f32s(bytes: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
        out.push(f32::from_le_bytes(arr));
    }
    out
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::cosine_similarity;

    #[test]
    fn cosine_similarity_handles_dimension_mismatch() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_is_one_for_identical_vectors() {
        let v = vec![0.2_f32, 0.5, 0.9];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }
}

// ---------------------------------------------------------------------------
// VectorStoreIndex implementation for Rig framework interop
// ---------------------------------------------------------------------------

/// Custom search filter for femtobot's vector store.
///
/// Implements Rig's `SearchFilter` trait so that `VectorMemoryStore` can be
/// used with `AgentBuilder::dynamic_context()`. The canonical `Filter<Value>`
/// operations (`eq`, `gt`, `lt`, `and`, `or`) are mapped to femtobot-specific
/// semantics:
///
/// - `eq("namespace", "value")` — scope the search to a specific namespace
/// - `gt("priority_weight", value)` — set the priority blending weight
///
/// Other filter operations are stored but currently ignored during search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FembotSearchFilter {
    pub namespace: Option<String>,
    pub priority_weight: Option<f32>,
}

impl SearchFilter for FembotSearchFilter {
    type Value = serde_json::Value;

    fn eq(key: impl AsRef<str>, value: Self::Value) -> Self {
        let mut f = FembotSearchFilter {
            namespace: None,
            priority_weight: None,
        };
        match key.as_ref() {
            "namespace" => {
                f.namespace = value.as_str().map(|s| s.to_string());
            }
            "priority_weight" => {
                f.priority_weight = value.as_f64().map(|v| v as f32);
            }
            _ => {}
        }
        f
    }

    fn gt(_key: impl AsRef<str>, _value: Self::Value) -> Self {
        FembotSearchFilter {
            namespace: None,
            priority_weight: None,
        }
    }

    fn lt(_key: impl AsRef<str>, _value: Self::Value) -> Self {
        FembotSearchFilter {
            namespace: None,
            priority_weight: None,
        }
    }

    fn and(self, rhs: Self) -> Self {
        FembotSearchFilter {
            namespace: self.namespace.or(rhs.namespace),
            priority_weight: self.priority_weight.or(rhs.priority_weight),
        }
    }

    fn or(self, rhs: Self) -> Self {
        FembotSearchFilter {
            namespace: self.namespace.or(rhs.namespace),
            priority_weight: self.priority_weight.or(rhs.priority_weight),
        }
    }
}

impl VectorStoreIndex for VectorMemoryStore {
    type Filter = FembotSearchFilter;

    fn top_n<T: for<'a> Deserialize<'a> + Send>(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> impl std::future::Future<Output = Result<Vec<(f64, String, T)>, VectorStoreError>> + Send
    {
        async move {
            let query_text = req.query().to_string();
            let samples = req.samples() as usize;
            let threshold = req
                .threshold()
                .map(|t| t as f32)
                .unwrap_or(DEFAULT_THRESHOLD);
            let (filter_ns, priority_weight) = match req.filter() {
                Some(f) => (
                    f.namespace.clone(),
                    f.priority_weight.unwrap_or(DEFAULT_PRIORITY_WEIGHT),
                ),
                None => (None, DEFAULT_PRIORITY_WEIGHT),
            };

            let namespace = filter_ns.unwrap_or_else(|| self.namespace.clone());
            let namespace = validate_namespace(&namespace)
                .map_err(|e| VectorStoreError::DatastoreError(e.into()))?;

            let query_embedding = match self.embedder.embed(&query_text).await {
                Ok(embedding) => embedding,
                Err(err) => {
                    warn!(
                        "vector memory lookup skipped: failed to embed query namespace={} err={}",
                        namespace, err
                    );
                    return Ok(Vec::new());
                }
            };

            let scored_items = match self
                .search_inner(
                    query_embedding,
                    samples,
                    threshold,
                    namespace.clone(),
                    priority_weight,
                )
                .await
            {
                Ok(items) => items,
                Err(err) => {
                    warn!(
                        "vector memory lookup skipped: failed to query namespace={} err={}",
                        namespace, err
                    );
                    return Ok(Vec::new());
                }
            };

            let mut out = Vec::with_capacity(scored_items.len());
            for (item, score) in scored_items {
                let id = item.id.clone();
                let json_value = serde_json::to_value(&item)?;
                let doc: T = serde_json::from_value(json_value)?;
                out.push((score as f64, id, doc));
            }
            Ok(out)
        }
    }

    fn top_n_ids(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> impl std::future::Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + Send
    {
        async move {
            let results: Vec<(f64, String, serde_json::Value)> = self.top_n(req).await?;
            Ok(results
                .into_iter()
                .map(|(score, id, _)| (score, id))
                .collect())
        }
    }
}
