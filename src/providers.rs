use rig::providers::openai;

/// Build an OpenAI-compatible client (works for OpenAI and Ollama).
pub fn build_openai_client(
    api_key: &str,
    base_url: &str,
    extra_headers: &[(String, String)],
) -> openai::Client {
    use http::{HeaderMap, HeaderValue};

    let mut builder = openai::Client::builder()
        .api_key(api_key)
        .base_url(base_url);

    let mut headers = HeaderMap::new();
    for (key, value) in extra_headers {
        if let Ok(name) = http::header::HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(val) = HeaderValue::from_str(value) {
                headers.insert(name, val);
            }
        }
    }
    if !headers.is_empty() {
        builder = builder.http_headers(headers);
    }

    builder
        .build()
        .expect("failed to build OpenAI-compatible client")
}
