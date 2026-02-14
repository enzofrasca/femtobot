use crate::config::WebSearchProvider;
use crate::tools::ToolError;
use html2text::from_read;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use url::Url;

const DEFAULT_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/537.36";
const MAX_REDIRECTS: usize = 5;

#[derive(Clone)]
pub struct WebSearchTool {
    provider: WebSearchProvider,
    brave_api_key: Option<String>,
    firecrawl_api_key: Option<String>,
}

impl WebSearchTool {
    pub fn new(
        provider: WebSearchProvider,
        brave_api_key: Option<String>,
        firecrawl_api_key: Option<String>,
    ) -> Self {
        Self {
            provider,
            brave_api_key,
            firecrawl_api_key,
        }
    }
}

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

fn split_csv_like(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect()
}

impl Tool for WebSearchTool {
    const NAME: &'static str = "web_search";
    type Args = WebSearchArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Search the web. Returns titles, URLs, and snippets.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(WebSearchArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            match self.provider {
                WebSearchProvider::Brave => {
                    let n = args.count.unwrap_or(5).clamp(1, 10);
                    let Some(api_key) = &self.brave_api_key else {
                        return Ok("Error: BRAVE_API_KEY not configured".to_string());
                    };
                    let client = reqwest::Client::new();
                    let res = client
                        .get("https://api.search.brave.com/res/v1/web/search")
                        .query(&[("q", &args.query), ("count", &n.to_string())])
                        .header(ACCEPT, "application/json")
                        .header("X-Subscription-Token", api_key)
                        .send()
                        .await
                        .map_err(|e| ToolError::msg(e.to_string()))?;
                    let status = res.status();
                    if !status.is_success() {
                        return Ok(format!("Error: Brave search failed with status {status}"));
                    }
                    let body: Value = res
                        .json()
                        .await
                        .map_err(|e| ToolError::msg(e.to_string()))?;
                    let results = body
                        .get("web")
                        .and_then(|w| w.get("results"))
                        .and_then(|r| r.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if results.is_empty() {
                        return Ok(format!("No results for: {}", args.query));
                    }
                    Ok(format_result_block(&args.query, None, &results, n as usize))
                }
                WebSearchProvider::Firecrawl => {
                    let n = args.count.unwrap_or(5).clamp(1, 100);
                    let Some(api_key) = &self.firecrawl_api_key else {
                        return Ok("Error: FIRECRAWL_API_KEY not configured".to_string());
                    };
                    let client = reqwest::Client::new();
                    let mut payload = json!({
                        "query": args.query,
                        "limit": n,
                    });
                    if let Some(sources) = normalize_list(args.sources) {
                        payload["sources"] = json!(sources);
                    }
                    if let Some(categories) = normalize_list(args.categories) {
                        payload["categories"] = json!(categories);
                    }
                    if let Some(location) = normalize_optional_str(args.location) {
                        payload["location"] = json!(location);
                    }
                    if let Some(tbs) = normalize_optional_str(args.tbs) {
                        payload["tbs"] = json!(tbs);
                    }
                    let scrape_enabled =
                        args.scrape.unwrap_or(false) || args.scrape_formats.is_some();
                    if scrape_enabled {
                        let formats = normalize_list(args.scrape_formats)
                            .unwrap_or_else(|| vec!["markdown".to_string()]);
                        payload["scrapeOptions"] = json!({ "formats": formats });
                    }
                    let res = client
                        .post("https://api.firecrawl.dev/v2/search")
                        .bearer_auth(api_key)
                        .json(&payload)
                        .send()
                        .await
                        .map_err(|e| ToolError::msg(e.to_string()))?;
                    let status = res.status();
                    if !status.is_success() {
                        return Ok(format!(
                            "Error: Firecrawl search failed with status {status}"
                        ));
                    }
                    let body: Value = res
                        .json()
                        .await
                        .map_err(|e| ToolError::msg(e.to_string()))?;
                    if body.get("success").and_then(Value::as_bool) == Some(false) {
                        let msg = first_nonempty(
                            body.get("error").and_then(Value::as_str),
                            body.get("message").and_then(Value::as_str),
                        )
                        .unwrap_or("unknown Firecrawl API error");
                        return Ok(format!("Error: Firecrawl search failed: {msg}"));
                    }
                    Ok(format_firecrawl_response(&body, n as usize))
                }
            }
        }
    }
}

fn normalize_list(list: Option<Vec<String>>) -> Option<Vec<String>> {
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

fn normalize_optional_str(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn format_firecrawl_response(body: &Value, limit: usize) -> String {
    let data = body.get("data");
    let Some(data) = data else {
        return "Error: Firecrawl response missing data".to_string();
    };

    if let Some(items) = data.as_array() {
        if items.is_empty() {
            return "No results found.".to_string();
        }
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("Firecrawl search");
        return format_result_block(query, Some("results"), items, limit);
    }

    let Some(data_obj) = data.as_object() else {
        return "Error: Firecrawl response has unsupported format".to_string();
    };

    let query = body
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("Firecrawl search");
    let source_order = ["web", "news", "images"];
    let mut sections = Vec::new();

    for source in source_order {
        if let Some(items) = data_obj.get(source).and_then(Value::as_array) {
            if !items.is_empty() {
                sections.push(format_result_block(query, Some(source), items, limit));
            }
        }
    }

    if sections.is_empty() {
        for (source, value) in data_obj {
            if let Some(items) = value.as_array() {
                if !items.is_empty() {
                    sections.push(format_result_block(query, Some(source), items, limit));
                }
            }
        }
    }

    if sections.is_empty() {
        format!("No results for: {query}")
    } else {
        sections.join("\n\n")
    }
}

fn format_result_block(query: &str, source: Option<&str>, items: &[Value], limit: usize) -> String {
    let mut lines = Vec::new();
    match source {
        Some(source) => lines.push(format!("Results for: {query} ({source})\n")),
        None => lines.push(format!("Results for: {query}\n")),
    }
    for (i, item) in items.iter().take(limit).enumerate() {
        let title = extract_title(item).unwrap_or("Untitled result");
        let url = extract_url(item).unwrap_or("");
        lines.push(format!("{}. {}\n   {}", i + 1, title, url));
        if let Some(extra) = extract_description(item) {
            lines.push(format!("   {extra}"));
        }
    }
    lines.join("\n")
}

fn extract_title(item: &Value) -> Option<&str> {
    first_nonempty(
        item.get("title").and_then(Value::as_str),
        item.get("metadata")
            .and_then(|m| m.get("title"))
            .and_then(Value::as_str),
    )
}

fn extract_url(item: &Value) -> Option<&str> {
    first_nonempty(
        first_nonempty(
            item.get("url").and_then(Value::as_str),
            item.get("imageUrl").and_then(Value::as_str),
        ),
        item.get("metadata")
            .and_then(|m| m.get("sourceURL"))
            .and_then(Value::as_str),
    )
}

fn extract_description(item: &Value) -> Option<String> {
    if let Some(text) = first_nonempty(
        item.get("description").and_then(Value::as_str),
        item.get("snippet").and_then(Value::as_str),
    ) {
        return Some(text.to_string());
    }
    if let Some(markdown) = item.get("markdown").and_then(Value::as_str) {
        let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            let snippet = compact.chars().take(220).collect::<String>();
            if compact.len() > snippet.len() {
                return Some(format!("{snippet}..."));
            }
            return Some(snippet);
        }
    }
    None
}

fn first_nonempty<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match a.map(str::trim).filter(|s| !s.is_empty()) {
        Some(val) => Some(val),
        None => b.map(str::trim).filter(|s| !s.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::{WebFetchArgs, WebSearchArgs};

    #[test]
    fn web_search_args_accept_numeric_count() {
        let args: WebSearchArgs =
            serde_json::from_value(serde_json::json!({"query": "hn", "count": 10})).unwrap();
        assert_eq!(args.count, Some(10));
    }

    #[test]
    fn web_search_args_accept_string_count() {
        let args: WebSearchArgs =
            serde_json::from_value(serde_json::json!({"query": "hn", "count": "10"})).unwrap();
        assert_eq!(args.count, Some(10));
    }

    #[test]
    fn web_search_args_accept_csv_sources() {
        let args: WebSearchArgs =
            serde_json::from_value(serde_json::json!({"query": "hn", "sources": "web,news"}))
                .unwrap();
        assert_eq!(
            args.sources,
            Some(vec!["web".to_string(), "news".to_string()])
        );
    }

    #[test]
    fn web_search_args_accept_array_sources() {
        let args: WebSearchArgs = serde_json::from_value(
            serde_json::json!({"query": "hn", "sources": ["web", "images"]}),
        )
        .unwrap();
        assert_eq!(
            args.sources,
            Some(vec!["web".to_string(), "images".to_string()])
        );
    }

    #[test]
    fn web_fetch_args_accept_csv_formats() {
        let args: WebFetchArgs = serde_json::from_value(
            serde_json::json!({"url": "https://example.com", "formats": "markdown,rawHtml"}),
        )
        .unwrap();
        assert_eq!(
            args.formats,
            Some(vec!["markdown".to_string(), "rawHtml".to_string()])
        );
    }

    #[test]
    fn web_fetch_args_accept_string_timeout_and_max_age() {
        let args: WebFetchArgs = serde_json::from_value(serde_json::json!({
            "url": "https://example.com",
            "timeout": "30000",
            "maxAge": "0"
        }))
        .unwrap();
        assert_eq!(args.timeout, Some(30000));
        assert_eq!(args.max_age, Some(0));
    }
}

#[derive(Clone)]
pub struct WebFetchTool {
    provider: WebSearchProvider,
    firecrawl_api_key: Option<String>,
}

impl WebFetchTool {
    pub fn new(provider: WebSearchProvider, firecrawl_api_key: Option<String>) -> Self {
        Self {
            provider,
            firecrawl_api_key,
        }
    }
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

impl Tool for WebFetchTool {
    const NAME: &'static str = "web_fetch";
    type Args = WebFetchArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Fetch URL and extract readable content (provider-configurable: direct HTTP or Firecrawl scrape).".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(WebFetchArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            if let Err(err) = validate_url(&args.url) {
                return Ok(
                    json!({ "error": format!("URL validation failed: {err}"), "url": args.url })
                        .to_string(),
                );
            }
            let extract_mode = args
                .extract_mode
                .as_deref()
                .map(|m| m.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "text".to_string());
            let max_chars = args.max_chars.unwrap_or(50_000);

            match self.provider {
                WebSearchProvider::Brave => {
                    fetch_direct_http(args.url, extract_mode, max_chars).await
                }
                WebSearchProvider::Firecrawl => {
                    let Some(api_key) = &self.firecrawl_api_key else {
                        return Ok("Error: FIRECRAWL_API_KEY not configured".to_string());
                    };
                    fetch_via_firecrawl(api_key, args, extract_mode, max_chars).await
                }
            }
        }
    }
}

async fn fetch_direct_http(
    url: String,
    extract_mode: String,
    max_chars: usize,
) -> Result<String, ToolError> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_UA));
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .map_err(|e| ToolError::msg(e.to_string()))?;
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ToolError::msg(e.to_string()))?;
    let status = res.status();
    let final_url = res.url().to_string();
    let ctype = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = res
        .text()
        .await
        .map_err(|e| ToolError::msg(e.to_string()))?;
    let mut extractor = "raw";
    let mut out_text = text.clone();
    if extract_mode == "raw" {
        extractor = "raw";
    } else if ctype.contains("application/json") {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
            out_text = serde_json::to_string_pretty(&val).unwrap_or(text);
            extractor = "json";
        }
    } else if ctype.contains("text/html")
        || text.to_ascii_lowercase().starts_with("<!doctype")
        || text.to_ascii_lowercase().starts_with("<html")
    {
        let rendered = from_read(text.as_bytes(), 100);
        out_text = rendered;
        extractor = "html2text";
    }
    let truncated = out_text.len() > max_chars;
    if truncated {
        out_text.truncate(max_chars);
    }
    Ok(json!({
        "url": url,
        "finalUrl": final_url,
        "status": status.as_u16(),
        "extractor": extractor,
        "extractMode": extract_mode,
        "truncated": truncated,
        "length": out_text.len(),
        "text": out_text
    })
    .to_string())
}

async fn fetch_via_firecrawl(
    api_key: &str,
    args: WebFetchArgs,
    extract_mode: String,
    max_chars: usize,
) -> Result<String, ToolError> {
    let client = reqwest::Client::new();
    let mut payload = json!({
        "url": args.url,
        "formats": resolved_firecrawl_formats(&args, &extract_mode),
    });
    if let Some(only_main_content) = args.only_main_content {
        payload["onlyMainContent"] = json!(only_main_content);
    }
    if let Some(timeout) = args.timeout {
        payload["timeout"] = json!(timeout);
    }
    if let Some(max_age) = args.max_age {
        payload["maxAge"] = json!(max_age);
    }
    if let Some(store_in_cache) = args.store_in_cache {
        payload["storeInCache"] = json!(store_in_cache);
    }

    let res = client
        .post("https://api.firecrawl.dev/v2/scrape")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|e| ToolError::msg(e.to_string()))?;
    let status = res.status();
    if !status.is_success() {
        return Ok(format!(
            "Error: Firecrawl scrape failed with status {status}"
        ));
    }
    let body: Value = res
        .json()
        .await
        .map_err(|e| ToolError::msg(e.to_string()))?;
    if body.get("success").and_then(Value::as_bool) == Some(false) {
        let msg = first_nonempty(
            body.get("error").and_then(Value::as_str),
            body.get("message").and_then(Value::as_str),
        )
        .unwrap_or("unknown Firecrawl API error");
        return Ok(format!("Error: Firecrawl scrape failed: {msg}"));
    }
    let Some(data) = body.get("data") else {
        return Ok("Error: Firecrawl scrape response missing data".to_string());
    };

    let (extractor, mut out_text) = select_firecrawl_text(data, &extract_mode);
    let truncated = out_text.len() > max_chars;
    if truncated {
        out_text.truncate(max_chars);
    }

    let final_url = data
        .get("metadata")
        .and_then(|m| m.get("sourceURL"))
        .and_then(Value::as_str)
        .unwrap_or(&args.url)
        .to_string();
    let status_code = data
        .get("metadata")
        .and_then(|m| m.get("statusCode"))
        .and_then(Value::as_u64)
        .unwrap_or(status.as_u16() as u64);
    let extras = firecrawl_extras(data);

    Ok(json!({
        "url": args.url,
        "finalUrl": final_url,
        "status": status_code,
        "extractor": extractor,
        "extractMode": extract_mode,
        "truncated": truncated,
        "length": out_text.len(),
        "text": out_text,
        "metadata": data.get("metadata").cloned().unwrap_or(json!({})),
        "extras": extras
    })
    .to_string())
}

fn resolved_firecrawl_formats(args: &WebFetchArgs, extract_mode: &str) -> Vec<String> {
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

fn select_firecrawl_text(data: &Value, extract_mode: &str) -> (&'static str, String) {
    match extract_mode {
        "raw" => {
            if let Some(raw_html) = data.get("rawHtml").and_then(Value::as_str) {
                return ("firecrawl-rawHtml", raw_html.to_string());
            }
        }
        "html" => {
            if let Some(html) = data.get("html").and_then(Value::as_str) {
                return ("firecrawl-html", html.to_string());
            }
        }
        "markdown" | "text" => {
            if let Some(markdown) = data.get("markdown").and_then(Value::as_str) {
                return ("firecrawl-markdown", markdown.to_string());
            }
        }
        "summary" => {
            if let Some(summary) = data.get("summary").and_then(Value::as_str) {
                return ("firecrawl-summary", summary.to_string());
            }
        }
        "json" => {
            if let Some(val) = data.get("json") {
                return (
                    "firecrawl-json",
                    serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string()),
                );
            }
        }
        _ => {}
    }

    if let Some(markdown) = data.get("markdown").and_then(Value::as_str) {
        return ("firecrawl-markdown", markdown.to_string());
    }
    if let Some(summary) = data.get("summary").and_then(Value::as_str) {
        return ("firecrawl-summary", summary.to_string());
    }
    if let Some(html) = data.get("html").and_then(Value::as_str) {
        return ("firecrawl-html", html.to_string());
    }
    if let Some(raw_html) = data.get("rawHtml").and_then(Value::as_str) {
        return ("firecrawl-rawHtml", raw_html.to_string());
    }
    ("firecrawl-empty", String::new())
}

fn firecrawl_extras(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let Some(obj) = data.as_object() else {
        return Value::Object(out);
    };
    for key in [
        "links",
        "images",
        "json",
        "branding",
        "screenshot",
        "actions",
    ] {
        if let Some(value) = obj.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(out)
}

fn validate_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|e| e.to_string())?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!("only http/https allowed, got '{other}'")),
    }
}
