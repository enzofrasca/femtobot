use crate::config::{WebFetchProvider, WebSearchProvider};
use crate::tools::ToolError;
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;

mod args;
mod common;
mod fetch;
mod search;

pub use args::{WebFetchArgs, WebSearchArgs};

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
        let provider = self.provider.clone();
        let brave_api_key = self.brave_api_key.clone();
        let firecrawl_api_key = self.firecrawl_api_key.clone();

        async move { search::run_search(provider, brave_api_key, firecrawl_api_key, args).await }
    }
}

#[derive(Clone)]
pub struct WebFetchTool {
    provider: WebFetchProvider,
    firecrawl_api_key: Option<String>,
}

impl WebFetchTool {
    pub fn new(provider: WebFetchProvider, firecrawl_api_key: Option<String>) -> Self {
        Self {
            provider,
            firecrawl_api_key,
        }
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
        let provider = self.provider.clone();
        let firecrawl_api_key = self.firecrawl_api_key.clone();

        async move { fetch::run_fetch(provider, firecrawl_api_key, args).await }
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
