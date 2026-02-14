use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer};

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WebSearchArgs {
    /// Search query
    pub query: String,
    /// Number of results (Brave: 1-10, Firecrawl: 1-100)
    #[serde(default, deserialize_with = "de_optional_u8")]
    pub count: Option<u8>,
    /// Search sources for Firecrawl provider: web, news, images
    #[serde(default, deserialize_with = "de_optional_string_list")]
    pub sources: Option<Vec<String>>,
    /// Optional categories for Firecrawl provider: github, research, pdf
    #[serde(default, deserialize_with = "de_optional_string_list")]
    pub categories: Option<Vec<String>>,
    /// Optional location for Firecrawl provider
    #[serde(default)]
    pub location: Option<String>,
    /// Optional time filter for Firecrawl provider (e.g. qdr:d, qdr:w)
    #[serde(default)]
    pub tbs: Option<String>,
    /// Enable scrapeOptions on Firecrawl provider
    #[serde(default)]
    pub scrape: Option<bool>,
    /// scrapeOptions.formats for Firecrawl provider (markdown, links, html)
    #[serde(
        default,
        alias = "scrapeFormats",
        deserialize_with = "de_optional_string_list"
    )]
    pub scrape_formats: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WebFetchArgs {
    /// URL to fetch
    pub url: String,
    /// Extract mode: "markdown" or "text"
    #[serde(default, alias = "extractMode")]
    pub extract_mode: Option<String>,
    /// Maximum characters to return (minimum 100)
    #[serde(default, alias = "maxChars", deserialize_with = "de_optional_usize")]
    pub max_chars: Option<usize>,
    /// Firecrawl scrape formats (markdown, html, rawHtml, links, json, images, branding, screenshot, summary)
    #[serde(
        default,
        alias = "format",
        alias = "scrapeFormats",
        deserialize_with = "de_optional_string_list"
    )]
    pub formats: Option<Vec<String>>,
    /// Firecrawl onlyMainContent option
    #[serde(default, alias = "onlyMainContent")]
    pub only_main_content: Option<bool>,
    /// Firecrawl timeout in milliseconds
    #[serde(default, deserialize_with = "de_optional_u64")]
    pub timeout: Option<u64>,
    /// Firecrawl maxAge in milliseconds (0 = always fresh)
    #[serde(default, alias = "maxAge", deserialize_with = "de_optional_u64")]
    pub max_age: Option<u64>,
    /// Firecrawl storeInCache option
    #[serde(default, alias = "storeInCache")]
    pub store_in_cache: Option<bool>,
}

fn de_optional_u8<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| D::Error::custom("count must be an integer between 0 and 255")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u8>()
            .map(Some)
            .map_err(|_| D::Error::custom("count string must be an integer between 0 and 255")),
        Some(_) => Err(D::Error::custom(
            "count must be an integer or integer string",
        )),
    }
}

fn de_optional_string_list<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let parsed = split_csv_like(&s);
            if parsed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(parsed))
            }
        }
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::new();
            for item in arr {
                match item {
                    serde_json::Value::String(s) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                    _ => {
                        return Err(D::Error::custom(
                            "list items must be strings or comma-separated strings",
                        ));
                    }
                }
            }
            if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            }
        }
        Some(_) => Err(D::Error::custom(
            "value must be a string, array of strings, or omitted",
        )),
    }
}

fn de_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .map(|v| Some(v as usize))
            .ok_or_else(|| D::Error::custom("max_chars must be a non-negative integer")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<usize>()
            .map(Some)
            .map_err(|_| D::Error::custom("max_chars string must be an integer")),
        Some(_) => Err(D::Error::custom(
            "max_chars must be an integer or integer string",
        )),
    }
}

fn de_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| D::Error::custom("value must be a non-negative integer")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| D::Error::custom("value string must be an integer")),
        Some(_) => Err(D::Error::custom(
            "value must be an integer or integer string",
        )),
    }
}

fn split_csv_like(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect()
}

pub(crate) fn normalize_list(list: Option<Vec<String>>) -> Option<Vec<String>> {
    let values = list?
        .into_iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

pub(crate) fn normalize_optional_str(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn resolved_firecrawl_formats(args: &WebFetchArgs, extract_mode: &str) -> Vec<String> {
    if let Some(formats) = normalize_firecrawl_formats(args.formats.clone()) {
        return formats;
    }
    match extract_mode {
        "raw" => vec!["rawHtml".to_string()],
        "html" => vec!["html".to_string()],
        "markdown" => vec!["markdown".to_string()],
        "summary" => vec!["summary".to_string()],
        "json" => vec!["json".to_string()],
        _ => vec!["markdown".to_string()],
    }
}

fn normalize_firecrawl_formats(list: Option<Vec<String>>) -> Option<Vec<String>> {
    let raw_values = list?;
    let values = raw_values
        .into_iter()
        .filter_map(|value| canonical_firecrawl_format(&value))
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn canonical_firecrawl_format(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    let canonical = match normalized.as_str() {
        "rawhtml" => "rawHtml",
        "markdown" => "markdown",
        "summary" => "summary",
        "html" => "html",
        "screenshot" => "screenshot",
        "links" => "links",
        "json" => "json",
        "images" => "images",
        "branding" => "branding",
        _ => trimmed,
    };
    Some(canonical.to_string())
}
