use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use etcetera::{choose_base_strategy, BaseStrategy};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    OpenRouter,
    OpenAI,
    Ollama,
}

impl ProviderKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openrouter" => Some(Self::OpenRouter),
            "openai" => Some(Self::OpenAI),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::OpenAI => "openai",
            Self::Ollama => "ollama",
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-config structs
// ---------------------------------------------------------------------------

/// Generic provider credentials (api key, base URL, extra headers).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub api_key: String,
    pub base_url: String,
    pub extra_headers: Vec<(String, String)>,
}

/// OpenRouter-specific provider entry (adds referer and app title headers).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterEntry {
    pub api_key: String,
    pub base_url: String,
    pub http_referer: Option<String>,
    pub app_title: Option<String>,
    pub extra_headers: Vec<(String, String)>,
}

/// Mistral provider entry (api key + base URL only).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MistralEntry {
    pub api_key: String,
    pub base_url: String,
}

/// All provider credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub openrouter: OpenRouterEntry,
    pub openai: ProviderEntry,
    pub ollama: ProviderEntry,
    pub mistral: MistralEntry,
}

/// Model selection & agent configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelConfig {
    pub model: String,
    pub fallbacks: Vec<String>,
    pub max_tool_turns: usize,
}

/// Telegram channel settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allow_from: Vec<String>,
}

/// Discord channel settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    pub allow_from: Vec<String>,
    pub allowed_channels: Vec<String>,
}

/// All channel settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelsConfig {
    pub telegram: TelegramConfig,
    pub discord: DiscordConfig,
}

/// Transcription (speech-to-text) settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub language: Option<String>,
    pub max_bytes: usize,
    pub mistral_diarize: bool,
    pub mistral_context_bias: Option<String>,
    pub mistral_timestamp_granularities: Vec<String>,
}

/// Memory mode: none, simple (file-based), or smart (vector + file).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryMode {
    /// No memory at all.
    None,
    /// File-based memory (MEMORY.md + daily notes) with auto-extraction.
    /// No embeddings required.
    Simple,
    /// Rig-style long-term memory: file + local vector store with periodic
    /// conversation summaries and semantic retrieval. Requires embeddings.
    Smart,
}

impl MemoryMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "disabled" => Some(Self::None),
            "simple" | "file" => Some(Self::Simple),
            "smart" | "vector" | "mem0" => Some(Self::Smart),
            _ => Option::None,
        }
    }
}

/// Memory (vector store for Smart mode) settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub mode: MemoryMode,
    pub embedding_model: String,

    pub max_memories: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Brave,
    Firecrawl,
}

impl WebSearchProvider {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "brave" => Some(Self::Brave),
            "firecrawl" => Some(Self::Firecrawl),
            _ => None,
        }
    }
}

/// Tool-related settings (exec timeout, workspace restriction, web search).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolsConfig {
    pub exec_timeout_secs: u64,
    pub restrict_to_workspace: bool,
    pub web_search_provider: WebSearchProvider,
    pub brave_api_key: Option<String>,
    pub firecrawl_api_key: Option<String>,
}

// ---------------------------------------------------------------------------
// AppConfig – composed of sub-configs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub provider: ProviderKind,
    pub providers: ProvidersConfig,
    pub model: ModelConfig,
    pub channels: ChannelsConfig,
    pub transcription: TranscriptionConfig,
    pub memory: MemoryConfig,
    pub tools: ToolsConfig,
    pub data_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let cfg = Self::load_relaxed();

        if cfg.provider_requires_api_key() && cfg.provider_api_key().trim().is_empty() {
            return Err(anyhow!(
                "missing API key for provider '{}' (set env var or providers.{}.apiKey in ~/.femtobot/config.json)",
                cfg.provider.as_str(),
                cfg.provider.as_str()
            ));
        }

        Ok(cfg)
    }

    pub fn load_relaxed() -> Self {
        let mut cfg = Self::defaults();

        if let Some(femtobot) = load_femtobot_config() {
            apply_femtobot_config(&mut cfg, &femtobot);
        }

        apply_env_overrides(&mut cfg);
        cfg
    }

    fn defaults() -> Self {
        Self {
            provider: ProviderKind::OpenRouter,
            providers: ProvidersConfig {
                openrouter: OpenRouterEntry {
                    api_key: String::new(),
                    base_url: "https://openrouter.ai/api/v1".to_string(),
                    http_referer: None,
                    app_title: None,
                    extra_headers: Vec::new(),
                },
                openai: ProviderEntry {
                    api_key: String::new(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    extra_headers: Vec::new(),
                },
                ollama: ProviderEntry {
                    api_key: String::new(),
                    base_url: "http://127.0.0.1:11434/v1".to_string(),
                    extra_headers: Vec::new(),
                },
                mistral: MistralEntry {
                    api_key: String::new(),
                    base_url: "https://api.mistral.ai/v1".to_string(),
                },
            },
            model: ModelConfig {
                model: "anthropic/claude-opus-4-5".to_string(),
                fallbacks: Vec::new(),
                max_tool_turns: 20,
            },
            channels: ChannelsConfig {
                telegram: TelegramConfig {
                    bot_token: String::new(),
                    allow_from: Vec::new(),
                },
                discord: DiscordConfig {
                    bot_token: String::new(),
                    allow_from: Vec::new(),
                    allowed_channels: Vec::new(),
                },
            },
            transcription: TranscriptionConfig {
                enabled: true,
                provider: "openai".to_string(),
                model: "whisper-1".to_string(),
                language: None,
                max_bytes: 20 * 1024 * 1024,
                mistral_diarize: false,
                mistral_context_bias: None,
                mistral_timestamp_granularities: Vec::new(),
            },
            memory: MemoryConfig {
                mode: MemoryMode::Simple,
                embedding_model: "text-embedding-3-small".to_string(),
                max_memories: 1000,
            },
            tools: ToolsConfig {
                exec_timeout_secs: 60,
                restrict_to_workspace: false,
                web_search_provider: WebSearchProvider::Brave,
                brave_api_key: None,
                firecrawl_api_key: None,
            },
            data_dir: default_data_dir(),
            workspace_dir: default_workspace_dir(),
        }
    }

    pub fn provider_api_key(&self) -> &str {
        match self.provider {
            ProviderKind::OpenRouter => &self.providers.openrouter.api_key,
            ProviderKind::OpenAI => &self.providers.openai.api_key,
            ProviderKind::Ollama => &self.providers.ollama.api_key,
        }
    }

    pub fn provider_requires_api_key(&self) -> bool {
        match self.provider {
            ProviderKind::OpenRouter | ProviderKind::OpenAI => true,
            ProviderKind::Ollama => false,
        }
    }

    pub fn telegram_enabled(&self) -> bool {
        !self.channels.telegram.bot_token.trim().is_empty()
    }

    pub fn discord_enabled(&self) -> bool {
        !self.channels.discord.bot_token.trim().is_empty()
    }

    pub fn model_routes(&self) -> Vec<ModelRoute> {
        let mut routes = Vec::new();
        let mut seen = HashSet::new();

        let primary = ModelRoute {
            provider: self.provider.clone(),
            model: self.model.model.trim().to_string(),
        };
        if !primary.model.is_empty() {
            let key = format!("{}/{}", primary.provider.as_str(), primary.model);
            seen.insert(key);
            routes.push(primary);
        }

        for raw in &self.model.fallbacks {
            if let Some(route) = parse_model_route(raw, &self.provider) {
                let key = format!("{}/{}", route.provider.as_str(), route.model);
                if seen.insert(key) {
                    routes.push(route);
                }
            }
        }

        routes
    }
}

#[derive(Clone, Debug)]
pub struct ModelRoute {
    pub provider: ProviderKind,
    pub model: String,
}

pub fn config_path() -> PathBuf {
    default_config_path().unwrap_or_else(|| PathBuf::from(".femtobot/config.json"))
}

fn default_config_path() -> Option<PathBuf> {
    let legacy = dirs::home_dir().map(|p| p.join(".femtobot").join("config.json"));
    if let Some(ref p) = legacy {
        if p.exists() {
            return legacy;
        }
    }

    if let Ok(strategy) = choose_base_strategy() {
        return Some(strategy.config_dir().join("femtobot").join("config.json"));
    }

    legacy
}

fn default_data_dir() -> PathBuf {
    let legacy = dirs::home_dir().map(|p| p.join(".femtobot").join("data"));
    if let Some(ref p) = legacy {
        if p.exists() {
            return p.clone();
        }
    }

    if let Ok(strategy) = choose_base_strategy() {
        return strategy.data_dir().join("femtobot");
    }

    legacy.unwrap_or_else(|| PathBuf::from(".").join(".femtobot").join("data"))
}

fn default_workspace_dir() -> PathBuf {
    let legacy = dirs::home_dir().map(|p| p.join(".femtobot").join("workspace"));
    if let Some(ref p) = legacy {
        if p.exists() {
            return p.clone();
        }
    }

    if let Ok(strategy) = choose_base_strategy() {
        return strategy.data_dir().join("femtobot").join("workspace");
    }

    legacy.unwrap_or_else(|| PathBuf::from(".").join(".femtobot").join("workspace"))
}

fn load_femtobot_config() -> Option<Value> {
    let path = default_config_path()?;
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&content).ok()
}

fn apply_femtobot_config(cfg: &mut AppConfig, value: &Value) {
    if let Some(provider) = get_str(value, &["agents", "defaults", "provider"])
        .or_else(|| get_str(value, &["llm", "provider"]))
    {
        if let Some(parsed) = ProviderKind::parse(provider) {
            cfg.provider = parsed;
        }
    }

    apply_provider_config(cfg, value, &["openrouter"], ProviderKind::OpenRouter);
    apply_provider_config(cfg, value, &["openai"], ProviderKind::OpenAI);
    apply_provider_config(cfg, value, &["ollama"], ProviderKind::Ollama);
    if let Some(obj) = get_provider_object(value, &["mistral"]) {
        if let Some(v) = obj
            .get("apiKey")
            .and_then(Value::as_str)
            .or_else(|| obj.get("api_key").and_then(Value::as_str))
        {
            cfg.providers.mistral.api_key = v.to_string();
        }
        if let Some(v) = obj
            .get("apiBase")
            .and_then(Value::as_str)
            .or_else(|| obj.get("api_base").and_then(Value::as_str))
        {
            cfg.providers.mistral.base_url = v.to_string();
        }
    }

    if let Some(model) = get_str(value, &["agents", "defaults", "model"]) {
        cfg.model.model = model.to_string();
    }
    if let Some(fallbacks) = get_array(value, &["agents", "defaults", "model_fallbacks"])
        .or_else(|| get_array(value, &["agents", "defaults", "fallbacks"]))
    {
        cfg.model.fallbacks = fallbacks;
    }
    if let Some(ws) = get_str(value, &["agents", "defaults", "workspace"]) {
        cfg.workspace_dir = PathBuf::from(ws);
    }
    if let Some(timeout) = get_u64(value, &["tools", "exec", "timeout"]) {
        cfg.tools.exec_timeout_secs = timeout;
    }
    if let Some(restrict) = get_bool(value, &["tools", "restrict_to_workspace"]) {
        cfg.tools.restrict_to_workspace = restrict;
    }
    if let Some(provider) = get_str(value, &["tools", "web", "search", "provider"]) {
        if let Some(parsed) = WebSearchProvider::parse(provider) {
            cfg.tools.web_search_provider = parsed;
        }
    }
    if let Some(legacy_key) = get_str(value, &["tools", "web", "search", "api_key"])
        .or_else(|| get_str(value, &["tools", "web", "search", "apiKey"]))
    {
        let legacy_key = legacy_key.to_string();
        cfg.tools.brave_api_key = Some(legacy_key.clone());
        if cfg.tools.firecrawl_api_key.is_none()
            && matches!(cfg.tools.web_search_provider, WebSearchProvider::Firecrawl)
        {
            cfg.tools.firecrawl_api_key = Some(legacy_key);
        }
    }
    if let Some(brave) = get_str(value, &["tools", "web", "search", "brave_api_key"])
        .or_else(|| get_str(value, &["tools", "web", "search", "braveApiKey"]))
    {
        cfg.tools.brave_api_key = Some(brave.to_string());
    }
    if let Some(firecrawl) = get_str(value, &["tools", "web", "search", "firecrawl_api_key"])
        .or_else(|| get_str(value, &["tools", "web", "search", "firecrawlApiKey"]))
    {
        cfg.tools.firecrawl_api_key = Some(firecrawl.to_string());
    }
    if let Some(token) = get_str(value, &["channels", "telegram", "token"]) {
        cfg.channels.telegram.bot_token = token.to_string();
    }
    if let Some(list) = get_array(value, &["channels", "telegram", "allow_from"]) {
        cfg.channels.telegram.allow_from = list;
    }
    if let Some(token) = get_str(value, &["channels", "discord", "token"]) {
        cfg.channels.discord.bot_token = token.to_string();
    }
    if let Some(list) = get_array(value, &["channels", "discord", "allow_from"]) {
        cfg.channels.discord.allow_from = list;
    }
    if let Some(list) = get_array(value, &["channels", "discord", "allowed_channels"]) {
        cfg.channels.discord.allowed_channels = list;
    }
    if let Some(enabled) = get_bool(value, &["channels", "telegram", "transcription", "enabled"]) {
        cfg.transcription.enabled = enabled;
    }
    if let Some(provider) = get_str(
        value,
        &["channels", "telegram", "transcription", "provider"],
    ) {
        if !provider.trim().is_empty() {
            cfg.transcription.provider = provider.to_string();
        }
    }
    if let Some(model) = get_str(value, &["channels", "telegram", "transcription", "model"]) {
        if !model.trim().is_empty() {
            cfg.transcription.model = model.to_string();
        }
    }
    if let Some(language) = get_str(
        value,
        &["channels", "telegram", "transcription", "language"],
    ) {
        if language.trim().is_empty() {
            cfg.transcription.language = None;
        } else {
            cfg.transcription.language = Some(language.to_string());
        }
    }
    if let Some(max_bytes) = get_u64(
        value,
        &["channels", "telegram", "transcription", "max_bytes"],
    ) {
        cfg.transcription.max_bytes = max_bytes as usize;
    }
    if let Some(diarize) = get_bool(value, &["channels", "telegram", "transcription", "diarize"]) {
        cfg.transcription.mistral_diarize = diarize;
    }
    if let Some(context_bias) = get_str(
        value,
        &["channels", "telegram", "transcription", "context_bias"],
    ) {
        if context_bias.trim().is_empty() {
            cfg.transcription.mistral_context_bias = None;
        } else {
            cfg.transcription.mistral_context_bias = Some(context_bias.to_string());
        }
    }
    if let Some(grans) = get_array(
        value,
        &[
            "channels",
            "telegram",
            "transcription",
            "timestamp_granularities",
        ],
    ) {
        cfg.transcription.mistral_timestamp_granularities = grans;
    }
    if let Some(turns) = get_u64(value, &["agents", "defaults", "max_tool_iterations"]) {
        cfg.model.max_tool_turns = turns as usize;
    }
    // New "mode" key takes priority over legacy booleans.
    if let Some(mode_str) = get_str(value, &["memory", "mode"]) {
        if let Some(mode) = MemoryMode::parse(mode_str) {
            cfg.memory.mode = mode;
        }
    } else {
        // Backward compat: map legacy enabled / vector_enabled booleans.
        let enabled = get_bool(value, &["memory", "enabled"]);
        let vector = get_bool(value, &["memory", "vector_enabled"]);
        match (enabled, vector) {
            (Some(false), _) => cfg.memory.mode = MemoryMode::None,
            (Some(true), Some(true)) => cfg.memory.mode = MemoryMode::Smart,
            (Some(true), Some(false)) | (Some(true), Option::None) => {
                cfg.memory.mode = MemoryMode::Simple
            }
            _ => {} // keep default
        }
    }
    if let Some(model) = get_str(value, &["memory", "embedding_model"]) {
        cfg.memory.embedding_model = model.to_string();
    }

    if let Some(max) = get_u64(value, &["memory", "max_memories"]) {
        cfg.memory.max_memories = max as usize;
    }
}

fn apply_provider_config(
    cfg: &mut AppConfig,
    value: &Value,
    provider_names: &[&str],
    provider_kind: ProviderKind,
) {
    let Some(provider_obj) = get_provider_object(value, provider_names) else {
        return;
    };

    let api_key = provider_obj
        .get("apiKey")
        .and_then(Value::as_str)
        .or_else(|| provider_obj.get("api_key").and_then(Value::as_str));
    let base_url = provider_obj
        .get("apiBase")
        .and_then(Value::as_str)
        .or_else(|| provider_obj.get("api_base").and_then(Value::as_str));
    let extra_headers = provider_obj
        .get("extra_headers")
        .and_then(Value::as_object)
        .map(object_to_pairs);

    match provider_kind {
        ProviderKind::OpenRouter => {
            if let Some(v) = api_key {
                cfg.providers.openrouter.api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.providers.openrouter.base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.providers.openrouter.extra_headers = v;
            }
        }
        ProviderKind::OpenAI => {
            if let Some(v) = api_key {
                cfg.providers.openai.api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.providers.openai.base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.providers.openai.extra_headers = v;
            }
        }
        ProviderKind::Ollama => {
            if let Some(v) = api_key {
                cfg.providers.ollama.api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.providers.ollama.base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.providers.ollama.extra_headers = v;
            }
        }
    }
}

fn object_to_pairs(obj: &Map<String, Value>) -> Vec<(String, String)> {
    obj.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

fn get_provider_object<'a>(
    value: &'a Value,
    provider_names: &[&str],
) -> Option<&'a Map<String, Value>> {
    let providers = value.get("providers")?.as_object()?;
    for name in provider_names {
        if let Some(obj) = providers.get(*name).and_then(Value::as_object) {
            return Some(obj);
        }
    }
    None
}

fn apply_env_overrides(cfg: &mut AppConfig) {
    if let Ok(provider) =
        std::env::var("FEMTOBOT_PROVIDER").or_else(|_| std::env::var("LLM_PROVIDER"))
    {
        if let Some(parsed) = ProviderKind::parse(&provider) {
            cfg.provider = parsed;
        }
    }

    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        cfg.providers.openrouter.api_key = key;
    }
    if let Ok(base) = std::env::var("OPENROUTER_BASE_URL") {
        cfg.providers.openrouter.base_url = base;
    }
    if let Ok(referer) = std::env::var("OPENROUTER_HTTP_REFERER") {
        if !referer.trim().is_empty() {
            cfg.providers.openrouter.http_referer = Some(referer);
        }
    }
    if let Ok(title) = std::env::var("OPENROUTER_APP_TITLE") {
        if !title.trim().is_empty() {
            cfg.providers.openrouter.app_title = Some(title);
        }
    }

    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        cfg.providers.openai.api_key = key;
    }
    if let Ok(base) = std::env::var("OPENAI_BASE_URL") {
        cfg.providers.openai.base_url = base;
    }
    if let Ok(key) = std::env::var("OLLAMA_API_KEY") {
        cfg.providers.ollama.api_key = key;
    }
    if let Ok(base) = std::env::var("OLLAMA_BASE_URL") {
        cfg.providers.ollama.base_url = base;
    }
    if let Ok(key) = std::env::var("MISTRAL_API_KEY") {
        cfg.providers.mistral.api_key = key;
    }
    if let Ok(base) = std::env::var("MISTRAL_BASE_URL") {
        cfg.providers.mistral.base_url = base;
    }

    if let Ok(token) =
        std::env::var("TELOXIDE_TOKEN").or_else(|_| std::env::var("TELEGRAM_BOT_TOKEN"))
    {
        cfg.channels.telegram.bot_token = token;
    }
    if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
        cfg.channels.discord.bot_token = token;
    }
    if let Ok(val) = std::env::var("FEMTOBOT_DISCORD_ALLOW_FROM") {
        cfg.channels.discord.allow_from = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    }
    if let Ok(val) = std::env::var("FEMTOBOT_DISCORD_ALLOWED_CHANNELS") {
        cfg.channels.discord.allowed_channels = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    }
    if let Ok(provider) = std::env::var("FEMTOBOT_WEB_SEARCH_PROVIDER") {
        if let Some(parsed) = WebSearchProvider::parse(&provider) {
            cfg.tools.web_search_provider = parsed;
        }
    }
    if let Ok(brave) = std::env::var("FEMTOBOT_BRAVE_API_KEY") {
        cfg.tools.brave_api_key = Some(brave);
    }
    if let Ok(brave) = std::env::var("BRAVE_API_KEY") {
        cfg.tools.brave_api_key = Some(brave);
    }
    if let Ok(firecrawl) = std::env::var("FEMTOBOT_FIRECRAWL_API_KEY") {
        cfg.tools.firecrawl_api_key = Some(firecrawl);
    }
    if let Ok(firecrawl) = std::env::var("FIRECRAWL_API_KEY") {
        cfg.tools.firecrawl_api_key = Some(firecrawl);
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_ENABLED") {
        if let Some(flag) = parse_bool(&val) {
            cfg.transcription.enabled = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_MODEL") {
        if !val.trim().is_empty() {
            cfg.transcription.model = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_PROVIDER") {
        if !val.trim().is_empty() {
            cfg.transcription.provider = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_LANGUAGE") {
        if val.trim().is_empty() {
            cfg.transcription.language = None;
        } else {
            cfg.transcription.language = Some(val);
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_MAX_BYTES") {
        if let Ok(num) = val.parse::<usize>() {
            cfg.transcription.max_bytes = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_DIARIZE") {
        if let Some(flag) = parse_bool(&val) {
            cfg.transcription.mistral_diarize = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_CONTEXT_BIAS") {
        if val.trim().is_empty() {
            cfg.transcription.mistral_context_bias = None;
        } else {
            cfg.transcription.mistral_context_bias = Some(val);
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_TIMESTAMP_GRANULARITIES") {
        let parsed = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        cfg.transcription.mistral_timestamp_granularities = parsed;
    }
    if let Ok(path) =
        std::env::var("FEMTOBOT_DATA_DIR").or_else(|_| std::env::var("RUSTBOT_DATA_DIR"))
    {
        cfg.data_dir = PathBuf::from(path);
    }
    if let Ok(path) =
        std::env::var("FEMTOBOT_WORKSPACE_DIR").or_else(|_| std::env::var("RUSTBOT_WORKSPACE_DIR"))
    {
        cfg.workspace_dir = PathBuf::from(path);
    }
    if let Ok(val) = std::env::var("FEMTOBOT_RESTRICT_TO_WORKSPACE")
        .or_else(|_| std::env::var("RUSTBOT_RESTRICT_TO_WORKSPACE"))
    {
        cfg.tools.restrict_to_workspace =
            parse_bool(&val).unwrap_or(cfg.tools.restrict_to_workspace);
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EXEC_TIMEOUT_SECS")
        .or_else(|_| std::env::var("RUSTBOT_EXEC_TIMEOUT_SECS"))
    {
        if let Ok(num) = val.parse::<u64>() {
            cfg.tools.exec_timeout_secs = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_MAX_TOOL_TURNS")
        .or_else(|_| std::env::var("RUSTBOT_MAX_TOOL_TURNS"))
    {
        if let Ok(num) = val.parse::<usize>() {
            cfg.model.max_tool_turns = num;
        }
    }
    // New env var takes priority.
    if let Ok(val) = std::env::var("FEMTOBOT_MEMORY_MODE") {
        if let Some(mode) = MemoryMode::parse(&val) {
            cfg.memory.mode = mode;
        }
    } else {
        // Backward compat: map legacy env vars.
        let enabled = std::env::var("FEMTOBOT_MEMORY_ENABLED")
            .ok()
            .and_then(|v| parse_bool(&v));
        let vector = std::env::var("FEMTOBOT_VECTOR_MEMORY_ENABLED")
            .ok()
            .and_then(|v| parse_bool(&v));
        match (enabled, vector) {
            (Some(false), _) => cfg.memory.mode = MemoryMode::None,
            (Some(true), Some(true)) => cfg.memory.mode = MemoryMode::Smart,
            (Some(true), Some(false)) => cfg.memory.mode = MemoryMode::Simple,
            _ => {} // keep current
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EMBEDDING_MODEL") {
        if !val.trim().is_empty() {
            cfg.memory.embedding_model = val;
        }
    }

    if let Ok(val) = std::env::var("FEMTOBOT_MAX_MEMORIES") {
        if let Ok(num) = val.parse::<usize>() {
            cfg.memory.max_memories = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_MODEL_FALLBACKS") {
        let parsed = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            cfg.model.fallbacks = parsed;
        }
    }
}

fn get_str<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str()
}

fn get_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_u64()
}

fn get_bool(value: &Value, path: &[&str]) -> Option<bool> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_bool()
}

fn get_array(value: &Value, path: &[&str]) -> Option<Vec<String>> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    let arr = cur.as_array()?;
    let mut out = Vec::new();
    for v in arr {
        if let Some(s) = v.as_str() {
            out.push(s.to_string());
        }
    }
    Some(out)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" => Some(true),
        "0" | "false" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn parse_model_route(raw: &str, default_provider: &ProviderKind) -> Option<ModelRoute> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((provider_raw, model_raw)) = trimmed.split_once('/') {
        if let Some(provider) = ProviderKind::parse(provider_raw) {
            let model = model_raw.trim();
            if model.is_empty() {
                return None;
            }
            return Some(ModelRoute {
                provider,
                model: model.to_string(),
            });
        }
    }

    Some(ModelRoute {
        provider: default_provider.clone(),
        model: trimmed.to_string(),
    })
}
