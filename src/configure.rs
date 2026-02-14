use anyhow::{anyhow, Result};
use cliclack::{confirm, input, intro, log, outro, outro_cancel, password, select};
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;
use std::process;

pub fn run() -> Result<()> {
    let path = crate::config::config_path();
    let mut root = load_config_value(&path)?;
    let initial_root = root.clone();
    let mut dirty = false;

    intro("femtobot configure")?;
    log::info(&format!("Config path: {}", path.display()))?;

    loop {
        let action = select("What would you like to configure?")
            .item(
                MenuAction::Provider,
                "Provider",
                "LLM API provider and keys",
            )
            .item(MenuAction::Model, "Model", "Default model and fallbacks")
            .item(MenuAction::Channels, "Channels", "Telegram, Discord")
            .item(
                MenuAction::WebSearch,
                "Web search (Brave)",
                "Brave Search API key",
            )
            .item(
                MenuAction::Transcription,
                "Transcription",
                "Voice/audio transcription settings",
            )
            .item(
                MenuAction::Memory,
                "Memory",
                "Memory mode and extraction settings",
            )
            .item(MenuAction::ShowPath, "Show config path", "")
            .item(MenuAction::SaveAndExit, "Save and exit", "")
            .item(MenuAction::ExitWithoutSaving, "Exit without saving", "")
            .interact()?;

        match action {
            MenuAction::Provider => {
                configure_provider(&mut root)?;
                dirty = root != initial_root;
            }
            MenuAction::Model => {
                configure_model(&mut root)?;
                dirty = root != initial_root;
            }
            MenuAction::Channels => {
                let channel = select("Which channel?")
                    .item(
                        ChannelChoice::Telegram,
                        "Telegram",
                        "Bot token and allowed users",
                    )
                    .item(ChannelChoice::Discord, "Discord", "Bot token and channels")
                    .interact()?;
                match channel {
                    ChannelChoice::Telegram => configure_telegram(&mut root),
                    ChannelChoice::Discord => configure_discord(&mut root),
                }?;
                dirty = root != initial_root;
            }
            MenuAction::WebSearch => {
                configure_web_search(&mut root)?;
                dirty = root != initial_root;
            }
            MenuAction::Transcription => {
                configure_transcription(&mut root)?;
                dirty = root != initial_root;
            }
            MenuAction::Memory => {
                configure_memory(&mut root)?;
                dirty = root != initial_root;
            }
            MenuAction::ShowPath => {
                log::info(&format!("Config path: {}", path.display()))?;
            }
            MenuAction::SaveAndExit => {
                if dirty {
                    print_change_summary(&initial_root, &root);
                    save_config_value(&path, &root)?;
                    outro("Configuration saved.")?;
                } else {
                    outro("No changes to save.")?;
                }
                break;
            }
            MenuAction::ExitWithoutSaving => {
                if dirty {
                    outro_cancel("Exited without saving.")?;
                } else {
                    outro("No changes to save.")?;
                }
                break;
            }
        }
    }

    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Provider,
    Model,
    Channels,
    WebSearch,
    Transcription,
    Memory,
    ShowPath,
    SaveAndExit,
    ExitWithoutSaving,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelChoice {
    Telegram,
    Discord,
}

fn prompt_str(label: &str, current: &str) -> Result<String> {
    let mut prompt = input(label);
    if !current.trim().is_empty() {
        prompt = prompt.default_input(current).required(false);
    }
    Ok(prompt.interact()?)
}

fn prompt_secret(label: &str, current: &str) -> Result<String> {
    let prompt_label = if current.trim().is_empty() {
        label.to_string()
    } else {
        format!("{label} (press Enter to keep current)")
    };
    let mut p = password(&prompt_label);
    if !current.trim().is_empty() {
        p = p.allow_empty();
    }
    let result = p.interact()?;
    Ok(if result.trim().is_empty() && !current.trim().is_empty() {
        current.to_string()
    } else {
        result
    })
}

fn configure_provider(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_provider = get_str_at(root, &["agents", "defaults", "provider"])
        .unwrap_or("openrouter")
        .to_string();

    let provider = select("Active provider")
        .item("openrouter", "OpenRouter", "openrouter.ai")
        .item("openai", "OpenAI", "api.openai.com")
        .item("ollama", "Ollama", "local")
        .initial_value(&current_provider)
        .interact()?;

    set_path(
        root,
        &["agents", "defaults", "provider"],
        Value::String(provider.to_string()),
    )?;

    match provider {
        "openrouter" => {
            let current_key =
                get_str_at(root, &["providers", "openrouter", "apiKey"]).unwrap_or("");
            let current_base = get_str_at(root, &["providers", "openrouter", "apiBase"])
                .unwrap_or("https://openrouter.ai/api/v1");
            let key = prompt_secret("OpenRouter API key", current_key)?;
            let base = prompt_str("OpenRouter base URL", current_base)?;
            set_path(
                root,
                &["providers", "openrouter", "apiKey"],
                Value::String(key),
            )?;
            set_path(
                root,
                &["providers", "openrouter", "apiBase"],
                Value::String(base),
            )?;
        }
        "openai" => {
            let current_key = get_str_at(root, &["providers", "openai", "apiKey"]).unwrap_or("");
            let current_base = get_str_at(root, &["providers", "openai", "apiBase"])
                .unwrap_or("https://api.openai.com/v1");
            let key = prompt_secret("OpenAI API key", current_key)?;
            let base = prompt_str("OpenAI base URL", current_base)?;
            set_path(root, &["providers", "openai", "apiKey"], Value::String(key))?;
            set_path(
                root,
                &["providers", "openai", "apiBase"],
                Value::String(base),
            )?;
        }
        "ollama" => {
            let current_key = get_str_at(root, &["providers", "ollama", "apiKey"]).unwrap_or("");
            let current_base = get_str_at(root, &["providers", "ollama", "apiBase"])
                .unwrap_or("http://127.0.0.1:11434/v1");
            let key = prompt_secret("Ollama API key (optional)", current_key)?;
            let base = prompt_str("Ollama base URL", current_base)?;
            set_path(root, &["providers", "ollama", "apiKey"], Value::String(key))?;
            set_path(
                root,
                &["providers", "ollama", "apiBase"],
                Value::String(base),
            )?;
        }
        _ => {}
    }

    Ok(root != &before)
}

fn configure_telegram(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_token = get_str_at(root, &["channels", "telegram", "token"]).unwrap_or("");
    let current_allow = get_array_at(root, &["channels", "telegram", "allow_from"]);
    let current_allow_str = current_allow.join(",");

    let token = prompt_secret("Telegram bot token", current_token)?;
    let allow_from = prompt_str(
        "Allowed Telegram user IDs (comma separated)",
        &current_allow_str,
    )?;

    let allow_list = parse_comma_list(&allow_from, &current_allow);

    set_path(
        root,
        &["channels", "telegram", "token"],
        Value::String(token),
    )?;
    set_path(
        root,
        &["channels", "telegram", "allow_from"],
        Value::Array(allow_list.into_iter().map(Value::String).collect()),
    )?;

    Ok(root != &before)
}

fn configure_discord(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_token = get_str_at(root, &["channels", "discord", "token"]).unwrap_or("");
    let current_allow = get_array_at(root, &["channels", "discord", "allow_from"]);
    let current_allow_str = current_allow.join(",");
    let current_channels = get_array_at(root, &["channels", "discord", "allowed_channels"]);
    let current_channels_str = current_channels.join(",");

    let token = prompt_secret("Discord bot token", current_token)?;
    let allow_from = prompt_str(
        "Allowed Discord users (IDs/usernames, comma separated)",
        &current_allow_str,
    )?;
    let allowed_channels = prompt_str(
        "Allowed Discord channel IDs (comma separated, blank = all)",
        &current_channels_str,
    )?;

    let allow_list = parse_comma_list(&allow_from, &current_allow);
    let channel_list = parse_comma_list(&allowed_channels, &current_channels);

    set_path(
        root,
        &["channels", "discord", "token"],
        Value::String(token),
    )?;
    set_path(
        root,
        &["channels", "discord", "allow_from"],
        Value::Array(allow_list.into_iter().map(Value::String).collect()),
    )?;
    set_path(
        root,
        &["channels", "discord", "allowed_channels"],
        Value::Array(channel_list.into_iter().map(Value::String).collect()),
    )?;

    Ok(root != &before)
}

fn configure_model(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_model =
        get_str_at(root, &["agents", "defaults", "model"]).unwrap_or("anthropic/claude-opus-4-5");
    let current_fallbacks = get_array_at(root, &["agents", "defaults", "model_fallbacks"]);
    let current_fallbacks_str = current_fallbacks.join(",");

    let model = prompt_str("Default model", current_model)?;
    let fallbacks = prompt_str("Fallback models (comma separated)", &current_fallbacks_str)?;
    let fallback_list = parse_comma_list(&fallbacks, &current_fallbacks);

    set_path(root, &["agents", "defaults", "model"], Value::String(model))?;
    set_path(
        root,
        &["agents", "defaults", "model_fallbacks"],
        Value::Array(fallback_list.into_iter().map(Value::String).collect()),
    )?;

    Ok(root != &before)
}

fn configure_web_search(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current = get_str_at(root, &["tools", "web", "search", "apiKey"]).unwrap_or("");
    let key = prompt_secret("Brave API key", current)?;
    set_path(
        root,
        &["tools", "web", "search", "apiKey"],
        Value::String(key),
    )?;
    Ok(root != &before)
}

fn configure_transcription(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_enabled =
        get_bool_at(root, &["channels", "telegram", "transcription", "enabled"]).unwrap_or(true);
    let current_provider = get_str_at(root, &["channels", "telegram", "transcription", "provider"])
        .unwrap_or("openai")
        .to_string();
    let current_model = get_str_at(root, &["channels", "telegram", "transcription", "model"])
        .unwrap_or("whisper-1")
        .to_string();
    let current_language = get_str_at(root, &["channels", "telegram", "transcription", "language"])
        .unwrap_or("")
        .to_string();
    let current_max_bytes = get_u64_at(
        root,
        &["channels", "telegram", "transcription", "max_bytes"],
    )
    .unwrap_or(20 * 1024 * 1024);
    let current_diarize =
        get_bool_at(root, &["channels", "telegram", "transcription", "diarize"]).unwrap_or(false);
    let current_context_bias = get_str_at(
        root,
        &["channels", "telegram", "transcription", "context_bias"],
    )
    .unwrap_or("")
    .to_string();
    let current_grans = get_array_at(
        root,
        &[
            "channels",
            "telegram",
            "transcription",
            "timestamp_granularities",
        ],
    );
    let current_grans_str = current_grans.join(",");

    let enabled = confirm("Enable transcription")
        .initial_value(current_enabled)
        .interact()?;
    let provider = select("Transcription provider")
        .item("openai", "OpenAI", "whisper")
        .item("mistral", "Mistral", "")
        .initial_value(&current_provider)
        .interact()?;
    let model = prompt_str("Transcription model", &current_model)?;
    let language = prompt_str("Language (empty = auto-detect)", &current_language)?;
    let max_bytes: u64 = input("Max audio bytes")
        .default_input(&current_max_bytes.to_string())
        .required(false)
        .validate(|s: &String| {
            s.parse::<u64>()
                .map_err(|_| "Enter a non-negative integer".to_string())
                .map(|_| ())
        })
        .interact()?;

    set_path(
        root,
        &["channels", "telegram", "transcription", "enabled"],
        Value::Bool(enabled),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "provider"],
        Value::String(provider.to_string()),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "model"],
        Value::String(model),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "language"],
        Value::String(language),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "max_bytes"],
        Value::Number(serde_json::Number::from(max_bytes)),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "diarize"],
        Value::Bool(current_diarize),
    )?;
    set_path(
        root,
        &["channels", "telegram", "transcription", "context_bias"],
        Value::String(current_context_bias.clone()),
    )?;
    set_path(
        root,
        &[
            "channels",
            "telegram",
            "transcription",
            "timestamp_granularities",
        ],
        Value::Array(current_grans.iter().cloned().map(Value::String).collect()),
    )?;

    if provider == "mistral" {
        let diarize = confirm("Enable diarization")
            .initial_value(current_diarize)
            .interact()?;
        let context_bias = prompt_str(
            "Context bias (comma-separated terms)",
            &current_context_bias,
        )?;
        let grans_raw = prompt_str(
            "Timestamp granularities (e.g. segment,word)",
            &current_grans_str,
        )?;
        let grans = parse_comma_list(&grans_raw, &current_grans);
        set_path(
            root,
            &["channels", "telegram", "transcription", "diarize"],
            Value::Bool(diarize),
        )?;
        set_path(
            root,
            &["channels", "telegram", "transcription", "context_bias"],
            Value::String(context_bias),
        )?;
        set_path(
            root,
            &[
                "channels",
                "telegram",
                "transcription",
                "timestamp_granularities",
            ],
            Value::Array(grans.into_iter().map(Value::String).collect()),
        )?;

        let current_key = get_str_at(root, &["providers", "mistral", "apiKey"]).unwrap_or("");
        let current_base = get_str_at(root, &["providers", "mistral", "apiBase"])
            .unwrap_or("https://api.mistral.ai/v1");
        let key = prompt_secret("Mistral API key", current_key)?;
        let base = prompt_str("Mistral base URL", current_base)?;
        set_path(
            root,
            &["providers", "mistral", "apiKey"],
            Value::String(key),
        )?;
        set_path(
            root,
            &["providers", "mistral", "apiBase"],
            Value::String(base),
        )?;
    }

    Ok(root != &before)
}

fn configure_memory(root: &mut Value) -> Result<bool> {
    let before = root.clone();
    let current_mode = get_str_at(root, &["memory", "mode"])
        .unwrap_or("simple")
        .to_string();

    let current_embedding_model = get_str_at(root, &["memory", "embedding_model"])
        .unwrap_or("text-embedding-3-small")
        .to_string();
    let current_max_memories = get_u64_at(root, &["memory", "max_memories"]).unwrap_or(1000);
    let mode: &str = select("Memory mode")
        .item("none", "Disabled", "No memory")
        .item(
            "simple",
            "Simple",
            "File-based (MEMORY.md), no embeddings needed",
        )
        .item(
            "smart",
            "Smart (Rig-style)",
            "Periodic summary memory + vector retrieval, requires embeddings",
        )
        .initial_value(&current_mode)
        .interact()?;

    set_path(root, &["memory", "mode"], Value::String(mode.to_string()))?;

    if mode == "smart" {
        let embedding_model = prompt_str("Embedding model", &current_embedding_model)?;
        set_path(
            root,
            &["memory", "embedding_model"],
            Value::String(embedding_model),
        )?;

        let max_memories: u64 = input("Max memories in vector store")
            .default_input(&current_max_memories.to_string())
            .required(false)
            .validate(|s: &String| {
                s.parse::<u64>()
                    .map_err(|_| "Enter a positive integer".to_string())
                    .and_then(|n| {
                        if n == 0 {
                            Err("Must be at least 1".to_string())
                        } else {
                            Ok(())
                        }
                    })
            })
            .interact()?;
        set_path(
            root,
            &["memory", "max_memories"],
            Value::Number(serde_json::Number::from(max_memories)),
        )?;
    }

    Ok(root != &before)
}

fn parse_comma_list(input: &str, fallback: &[String]) -> Vec<String> {
    if input.trim().is_empty() {
        return fallback.to_vec();
    }
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn load_config_value(path: &PathBuf) -> Result<Value> {
    if path.exists() {
        let content = fs::read_to_string(path)?;
        let parsed: Value = serde_json::from_str(&content)
            .map_err(|e| anyhow!("failed to parse config at {}: {e}", path.display()))?;
        if !parsed.is_object() {
            return Err(anyhow!(
                "invalid config at {}: root must be a JSON object",
                path.display()
            ));
        }
        Ok(parsed)
    } else {
        Ok(Value::Object(Map::new()))
    }
}

fn save_config_value(path: &PathBuf, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(value)?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid config file path: {}", path.display()))?;
    let tmp_path = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::write(&tmp_path, content)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn set_path(value: &mut Value, path: &[&str], new_value: Value) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    if !value.is_object() {
        return Err(anyhow!("invalid config: root must be a JSON object"));
    }
    let mut cur = value;
    for (idx, key) in path[..path.len() - 1].iter().enumerate() {
        let parent_path = if idx == 0 {
            "<root>".to_string()
        } else {
            path[..idx].join(".")
        };
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| anyhow!("invalid config: '{parent_path}' must be an object"))?;
        cur = obj
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !cur.is_object() {
            return Err(anyhow!(
                "invalid config: '{}' must be an object",
                path[..=idx].join(".")
            ));
        }
    }
    let obj = cur.as_object_mut().ok_or_else(|| {
        anyhow!(
            "invalid config: '{}' must be an object",
            path[..path.len() - 1].join(".")
        )
    })?;
    obj.insert(path[path.len() - 1].to_string(), new_value);
    Ok(())
}

fn print_change_summary(before: &Value, after: &Value) {
    let mut changed = Vec::new();
    collect_changed_paths(before, after, String::new(), &mut changed);
    if changed.is_empty() {
        return;
    }
    log::step("Changes to save").ok();
    for path in changed {
        log::info(&format!("  - {path}")).ok();
    }
}

fn collect_changed_paths(before: &Value, after: &Value, prefix: String, out: &mut Vec<String>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(bm), Value::Object(am)) => {
            let mut keys: Vec<&str> = bm
                .keys()
                .chain(am.keys())
                .map(String::as_str)
                .collect::<Vec<_>>();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let child_prefix = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                match (bm.get(key), am.get(key)) {
                    (Some(bv), Some(av)) => collect_changed_paths(bv, av, child_prefix, out),
                    _ => out.push(child_prefix),
                }
            }
        }
        _ => {
            out.push(if prefix.is_empty() {
                "<root>".to_string()
            } else {
                prefix
            });
        }
    }
}

fn get_str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str()
}

fn get_array_at(value: &Value, path: &[&str]) -> Vec<String> {
    let mut cur = value;
    for key in path {
        match cur.get(*key) {
            Some(v) => cur = v,
            None => return Vec::new(),
        }
    }
    match cur.as_array() {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        None => Vec::new(),
    }
}

fn get_bool_at(value: &Value, path: &[&str]) -> Option<bool> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_bool()
}

fn get_u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_u64()
}
