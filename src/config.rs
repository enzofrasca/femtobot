use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub provider: ProviderKind,

    pub openrouter_api_key: String,
    pub openrouter_base_url: String,
    pub openrouter_http_referer: Option<String>,
    pub openrouter_app_title: Option<String>,
    pub openrouter_extra_headers: Vec<(String, String)>,

    pub openai_api_key: String,
    pub openai_base_url: String,
    pub openai_extra_headers: Vec<(String, String)>,
    pub ollama_api_key: String,
    pub ollama_base_url: String,
    pub ollama_extra_headers: Vec<(String, String)>,
    pub mistral_api_key: String,
    pub mistral_base_url: String,

    pub model: String,
    pub model_fallbacks: Vec<String>,
    pub brave_api_key: Option<String>,
    pub telegram_bot_token: String,
    pub telegram_allow_from: Vec<String>,
    pub transcription_enabled: bool,
    pub transcription_provider: String,
    pub transcription_model: String,
    pub transcription_language: Option<String>,
    pub transcription_max_bytes: usize,
    pub transcription_mistral_diarize: bool,
    pub transcription_mistral_context_bias: Option<String>,
    pub transcription_mistral_timestamp_granularities: Vec<String>,
    pub data_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub exec_timeout_secs: u64,
    pub restrict_to_workspace: bool,
    pub max_tool_turns: usize,
    pub memory_enabled: bool,
    pub memory_vector_enabled: bool,
    pub memory_embedding_model: String,
    pub memory_extraction_model: String,
    pub memory_max_memories: usize,
    pub memory_extraction_interval: usize,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let mut cfg = Self::defaults();

        if let Some(femtobot) = load_femtobot_config() {
            apply_femtobot_config(&mut cfg, &femtobot);
        }

        apply_env_overrides(&mut cfg);

        if cfg.provider_requires_api_key() && cfg.provider_api_key().trim().is_empty() {
            return Err(anyhow!(
                "missing API key for provider '{}' (set env var or providers.{}.apiKey in ~/.femtobot/config.json)",
                cfg.provider.as_str(),
                cfg.provider.as_str()
            ));
        }

        Ok(cfg)
    }

    fn defaults() -> Self {
        Self {
            provider: ProviderKind::OpenRouter,

            openrouter_api_key: String::new(),
            openrouter_base_url: "https://openrouter.ai/api/v1".to_string(),
            openrouter_http_referer: None,
            openrouter_app_title: None,
            openrouter_extra_headers: Vec::new(),

            openai_api_key: String::new(),
            openai_base_url: "https://api.openai.com/v1".to_string(),
            openai_extra_headers: Vec::new(),
            ollama_api_key: String::new(),
            ollama_base_url: "http://127.0.0.1:11434/v1".to_string(),
            ollama_extra_headers: Vec::new(),
            mistral_api_key: String::new(),
            mistral_base_url: "https://api.mistral.ai/v1".to_string(),

            model: "anthropic/claude-opus-4-5".to_string(),
            model_fallbacks: Vec::new(),
            brave_api_key: None,
            telegram_bot_token: String::new(),
            telegram_allow_from: Vec::new(),
            transcription_enabled: true,
            transcription_provider: "openai".to_string(),
            transcription_model: "whisper-1".to_string(),
            transcription_language: None,
            transcription_max_bytes: 20 * 1024 * 1024,
            transcription_mistral_diarize: false,
            transcription_mistral_context_bias: None,
            transcription_mistral_timestamp_granularities: Vec::new(),
            data_dir: default_data_dir(),
            workspace_dir: default_workspace_dir(),
            exec_timeout_secs: 60,
            restrict_to_workspace: false,
            max_tool_turns: 20,
            memory_enabled: true,
            memory_vector_enabled: true,
            memory_embedding_model: "text-embedding-3-small".to_string(),
            memory_extraction_model: "gpt-4o-mini".to_string(),
            memory_max_memories: 1000,
            memory_extraction_interval: 10,
        }
    }

    pub fn provider_api_key(&self) -> &str {
        match self.provider {
            ProviderKind::OpenRouter => &self.openrouter_api_key,
            ProviderKind::OpenAI => &self.openai_api_key,
            ProviderKind::Ollama => &self.ollama_api_key,
        }
    }

    pub fn provider_requires_api_key(&self) -> bool {
        match self.provider {
            ProviderKind::OpenRouter | ProviderKind::OpenAI => true,
            ProviderKind::Ollama => false,
        }
    }

    pub fn telegram_enabled(&self) -> bool {
        !self.telegram_bot_token.trim().is_empty()
    }

    pub fn model_routes(&self) -> Vec<ModelRoute> {
        let mut routes = Vec::new();
        let mut seen = HashSet::new();

        let primary = ModelRoute {
            provider: self.provider.clone(),
            model: self.model.trim().to_string(),
        };
        if !primary.model.is_empty() {
            let key = format!("{}/{}", primary.provider.as_str(), primary.model);
            seen.insert(key);
            routes.push(primary);
        }

        for raw in &self.model_fallbacks {
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
    dirs::home_dir().map(|p| p.join(".femtobot").join("config.json"))
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".femtobot")
        .join("data")
}

fn default_workspace_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".femtobot")
        .join("workspace")
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
            cfg.mistral_api_key = v.to_string();
        }
        if let Some(v) = obj
            .get("apiBase")
            .and_then(Value::as_str)
            .or_else(|| obj.get("api_base").and_then(Value::as_str))
        {
            cfg.mistral_base_url = v.to_string();
        }
    }

    if let Some(model) = get_str(value, &["agents", "defaults", "model"]) {
        cfg.model = model.to_string();
    }
    if let Some(fallbacks) = get_array(value, &["agents", "defaults", "model_fallbacks"])
        .or_else(|| get_array(value, &["agents", "defaults", "fallbacks"]))
    {
        cfg.model_fallbacks = fallbacks;
    }
    if let Some(ws) = get_str(value, &["agents", "defaults", "workspace"]) {
        cfg.workspace_dir = PathBuf::from(ws);
    }
    if let Some(timeout) = get_u64(value, &["tools", "exec", "timeout"]) {
        cfg.exec_timeout_secs = timeout;
    }
    if let Some(restrict) = get_bool(value, &["tools", "restrict_to_workspace"]) {
        cfg.restrict_to_workspace = restrict;
    }
    if let Some(brave) = get_str(value, &["tools", "web", "search", "api_key"])
        .or_else(|| get_str(value, &["tools", "web", "search", "apiKey"]))
    {
        cfg.brave_api_key = Some(brave.to_string());
    }
    if let Some(token) = get_str(value, &["channels", "telegram", "token"]) {
        cfg.telegram_bot_token = token.to_string();
    }
    if let Some(list) = get_array(value, &["channels", "telegram", "allow_from"]) {
        cfg.telegram_allow_from = list;
    }
    if let Some(enabled) = get_bool(value, &["channels", "telegram", "transcription", "enabled"]) {
        cfg.transcription_enabled = enabled;
    }
    if let Some(provider) = get_str(
        value,
        &["channels", "telegram", "transcription", "provider"],
    ) {
        if !provider.trim().is_empty() {
            cfg.transcription_provider = provider.to_string();
        }
    }
    if let Some(model) = get_str(value, &["channels", "telegram", "transcription", "model"]) {
        if !model.trim().is_empty() {
            cfg.transcription_model = model.to_string();
        }
    }
    if let Some(language) = get_str(
        value,
        &["channels", "telegram", "transcription", "language"],
    ) {
        if language.trim().is_empty() {
            cfg.transcription_language = None;
        } else {
            cfg.transcription_language = Some(language.to_string());
        }
    }
    if let Some(max_bytes) = get_u64(
        value,
        &["channels", "telegram", "transcription", "max_bytes"],
    ) {
        cfg.transcription_max_bytes = max_bytes as usize;
    }
    if let Some(diarize) = get_bool(value, &["channels", "telegram", "transcription", "diarize"]) {
        cfg.transcription_mistral_diarize = diarize;
    }
    if let Some(context_bias) = get_str(
        value,
        &["channels", "telegram", "transcription", "context_bias"],
    ) {
        if context_bias.trim().is_empty() {
            cfg.transcription_mistral_context_bias = None;
        } else {
            cfg.transcription_mistral_context_bias = Some(context_bias.to_string());
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
        cfg.transcription_mistral_timestamp_granularities = grans;
    }
    if let Some(turns) = get_u64(value, &["agents", "defaults", "max_tool_iterations"]) {
        cfg.max_tool_turns = turns as usize;
    }
    if let Some(enabled) = get_bool(value, &["memory", "enabled"]) {
        cfg.memory_enabled = enabled;
    }
    if let Some(enabled) = get_bool(value, &["memory", "vector_enabled"]) {
        cfg.memory_vector_enabled = enabled;
    }
    if let Some(model) = get_str(value, &["memory", "embedding_model"]) {
        cfg.memory_embedding_model = model.to_string();
    }
    if let Some(model) = get_str(value, &["memory", "extraction_model"]) {
        cfg.memory_extraction_model = model.to_string();
    }
    if let Some(max) = get_u64(value, &["memory", "max_memories"]) {
        cfg.memory_max_memories = max as usize;
    }
    if let Some(interval) = get_u64(value, &["memory", "extraction_interval"]) {
        cfg.memory_extraction_interval = interval as usize;
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
                cfg.openrouter_api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.openrouter_base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.openrouter_extra_headers = v;
            }
        }
        ProviderKind::OpenAI => {
            if let Some(v) = api_key {
                cfg.openai_api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.openai_base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.openai_extra_headers = v;
            }
        }
        ProviderKind::Ollama => {
            if let Some(v) = api_key {
                cfg.ollama_api_key = v.to_string();
            }
            if let Some(v) = base_url {
                cfg.ollama_base_url = v.to_string();
            }
            if let Some(v) = extra_headers {
                cfg.ollama_extra_headers = v;
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
        cfg.openrouter_api_key = key;
    }
    if let Ok(base) = std::env::var("OPENROUTER_BASE_URL") {
        cfg.openrouter_base_url = base;
    }
    if let Ok(referer) = std::env::var("OPENROUTER_HTTP_REFERER") {
        if !referer.trim().is_empty() {
            cfg.openrouter_http_referer = Some(referer);
        }
    }
    if let Ok(title) = std::env::var("OPENROUTER_APP_TITLE") {
        if !title.trim().is_empty() {
            cfg.openrouter_app_title = Some(title);
        }
    }

    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        cfg.openai_api_key = key;
    }
    if let Ok(base) = std::env::var("OPENAI_BASE_URL") {
        cfg.openai_base_url = base;
    }
    if let Ok(key) = std::env::var("OLLAMA_API_KEY") {
        cfg.ollama_api_key = key;
    }
    if let Ok(base) = std::env::var("OLLAMA_BASE_URL") {
        cfg.ollama_base_url = base;
    }
    if let Ok(key) = std::env::var("MISTRAL_API_KEY") {
        cfg.mistral_api_key = key;
    }
    if let Ok(base) = std::env::var("MISTRAL_BASE_URL") {
        cfg.mistral_base_url = base;
    }

    if let Ok(token) =
        std::env::var("TELOXIDE_TOKEN").or_else(|_| std::env::var("TELEGRAM_BOT_TOKEN"))
    {
        cfg.telegram_bot_token = token;
    }
    if let Ok(brave) = std::env::var("BRAVE_API_KEY") {
        cfg.brave_api_key = Some(brave);
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_ENABLED") {
        if let Some(flag) = parse_bool(&val) {
            cfg.transcription_enabled = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_MODEL") {
        if !val.trim().is_empty() {
            cfg.transcription_model = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_PROVIDER") {
        if !val.trim().is_empty() {
            cfg.transcription_provider = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_LANGUAGE") {
        if val.trim().is_empty() {
            cfg.transcription_language = None;
        } else {
            cfg.transcription_language = Some(val);
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_MAX_BYTES") {
        if let Ok(num) = val.parse::<usize>() {
            cfg.transcription_max_bytes = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_DIARIZE") {
        if let Some(flag) = parse_bool(&val) {
            cfg.transcription_mistral_diarize = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_CONTEXT_BIAS") {
        if val.trim().is_empty() {
            cfg.transcription_mistral_context_bias = None;
        } else {
            cfg.transcription_mistral_context_bias = Some(val);
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_TRANSCRIPTION_TIMESTAMP_GRANULARITIES") {
        let parsed = val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        cfg.transcription_mistral_timestamp_granularities = parsed;
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
        cfg.restrict_to_workspace = parse_bool(&val).unwrap_or(cfg.restrict_to_workspace);
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EXEC_TIMEOUT_SECS")
        .or_else(|_| std::env::var("RUSTBOT_EXEC_TIMEOUT_SECS"))
    {
        if let Ok(num) = val.parse::<u64>() {
            cfg.exec_timeout_secs = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_MAX_TOOL_TURNS")
        .or_else(|_| std::env::var("RUSTBOT_MAX_TOOL_TURNS"))
    {
        if let Ok(num) = val.parse::<usize>() {
            cfg.max_tool_turns = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_MEMORY_ENABLED") {
        if let Some(flag) = parse_bool(&val) {
            cfg.memory_enabled = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_VECTOR_MEMORY_ENABLED") {
        if let Some(flag) = parse_bool(&val) {
            cfg.memory_vector_enabled = flag;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EMBEDDING_MODEL") {
        if !val.trim().is_empty() {
            cfg.memory_embedding_model = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EXTRACTION_MODEL") {
        if !val.trim().is_empty() {
            cfg.memory_extraction_model = val;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_MAX_MEMORIES") {
        if let Ok(num) = val.parse::<usize>() {
            cfg.memory_max_memories = num;
        }
    }
    if let Ok(val) = std::env::var("FEMTOBOT_EXTRACTION_INTERVAL") {
        if let Ok(num) = val.parse::<usize>() {
            cfg.memory_extraction_interval = num;
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
            cfg.model_fallbacks = parsed;
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
