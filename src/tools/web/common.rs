use url::Url;

pub(crate) fn first_nonempty<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match a.map(str::trim).filter(|s| !s.is_empty()) {
        Some(val) => Some(val),
        None => b.map(str::trim).filter(|s| !s.is_empty()),
    }
}

pub(crate) fn validate_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|e| e.to_string())?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!("only http/https allowed, got '{other}'")),
    }
}
