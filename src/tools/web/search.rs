use crate::config::WebSearchProvider;
use crate::tools::ToolError;
use serde_json::{json, Value};

use super::args::{normalize_list, normalize_optional_str, WebSearchArgs};
use super::common::first_nonempty;

pub(crate) async fn run_search(
    provider: WebSearchProvider,
    brave_api_key: Option<String>,
    firecrawl_api_key: Option<String>,
    args: WebSearchArgs,
) -> Result<String, ToolError> {
    match provider {
        WebSearchProvider::Brave => {
            let n = args.count.unwrap_or(5).clamp(1, 10);
            let Some(api_key) = brave_api_key else {
                return Ok("Error: BRAVE_API_KEY not configured".to_string());
            };
            let client = reqwest::Client::new();
            let res = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", &args.query), ("count", &n.to_string())])
                .header(reqwest::header::ACCEPT, "application/json")
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
            let Some(api_key) = firecrawl_api_key else {
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
            let scrape_enabled = args.scrape.unwrap_or(false) || args.scrape_formats.is_some();
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
