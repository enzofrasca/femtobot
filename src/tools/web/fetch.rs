use crate::config::WebFetchProvider;
use crate::tools::ToolError;
use html2text::from_read;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{json, Value};

use super::args::{resolved_firecrawl_formats, WebFetchArgs};
use super::common::{first_nonempty, validate_url};

const DEFAULT_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/537.36";
const MAX_REDIRECTS: usize = 5;

pub(crate) async fn run_fetch(
    provider: WebFetchProvider,
    firecrawl_api_key: Option<String>,
    args: WebFetchArgs,
) -> Result<String, ToolError> {
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

    match provider {
        WebFetchProvider::Native => fetch_direct_http(args.url, extract_mode, max_chars).await,
        WebFetchProvider::Firecrawl => {
            let Some(api_key) = firecrawl_api_key else {
                return Ok("Error: FIRECRAWL_API_KEY not configured".to_string());
            };
            fetch_via_firecrawl(&api_key, args, extract_mode, max_chars).await
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
