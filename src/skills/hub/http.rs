use anyhow::{bail, Context, Result};
use reqwest::blocking::Response;
use reqwest::Url;
use serde::de::DeserializeOwned;

pub(super) fn normalize_limit(limit: usize) -> usize {
    limit.clamp(1, super::MAX_SEARCH_LIMIT)
}

pub(super) fn parse_json_response<T: DeserializeOwned>(response: Response, url: &Url) -> Result<T> {
    let checked = ensure_success(response, url)?;
    checked
        .json::<T>()
        .with_context(|| format!("failed to parse JSON response: {}", url))
}

pub(super) fn ensure_success(response: Response, url: &Url) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_else(|_| String::new());
    let snippet = body.trim();
    if snippet.is_empty() {
        bail!("request failed ({}): {}", status, url);
    }
    bail!("request failed ({}): {} -> {}", status, url, snippet)
}
