//! Adapter-managed web search backends.

use reqwest::Client;
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;
use tracing::debug;

use crate::config::CustomSearchConfig;
use crate::config::WebSearchBackend;
use crate::config::WebSearchConfig;
use crate::error::AdapterError;

pub const ADAPTER_WEB_SEARCH_TOOL_NAME: &str = "__adapter_web_search";

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub max_results: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResponse {
    pub query: String,
    pub provider: String,
    pub results: Vec<SearchResultItem>,
}

pub struct PreparedSearchRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub async fn search(
    client: &Client,
    config: &WebSearchConfig,
    req: SearchRequest,
) -> Result<SearchResponse, AdapterError> {
    let backend = config.backend.ok_or_else(|| {
        AdapterError::UnsupportedFeature(
            "web_search backend is not configured for adapter-managed search".to_string(),
        )
    })?;

    debug!(
        "web_search backend selected: backend='{}' query='{}' max_results={}",
        backend_name(backend),
        req.query,
        req.max_results
    );

    match backend {
        WebSearchBackend::Tavily => search_tavily(client, config, req).await,
        WebSearchBackend::Brave => search_brave(client, config, req).await,
        WebSearchBackend::Custom => search_custom(client, config, req).await,
    }
}

async fn search_tavily(
    client: &Client,
    config: &WebSearchConfig,
    req: SearchRequest,
) -> Result<SearchResponse, AdapterError> {
    let started_at = Instant::now();
    let api_key = config.tavily.resolve_api_key().ok_or_else(|| {
        AdapterError::TransportError(
            "web_search backend 'tavily' is configured but no API key was found".to_string(),
        )
    })?;

    let resp = client
        .post("https://api.tavily.com/search")
        .timeout(std::time::Duration::from_secs(config.timeout_seconds))
        .json(&serde_json::json!({
            "api_key": api_key,
            "query": req.query,
            "max_results": req.max_results,
        }))
        .send()
        .await
        .map_err(|err| AdapterError::TransportError(format!("tavily search failed: {err}")))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|err| {
        AdapterError::TransportError(format!("failed to read tavily response: {err}"))
    })?;

    if !status.is_success() {
        return Err(AdapterError::UpstreamError {
            status: status.as_u16(),
            body,
        });
    }

    let result = parse_tavily_response(&body, req.query)?;
    debug!(
        "web_search backend success: backend='tavily' results={} duration_ms={}",
        result.results.len(),
        started_at.elapsed().as_millis()
    );
    Ok(result)
}

async fn search_brave(
    client: &Client,
    config: &WebSearchConfig,
    req: SearchRequest,
) -> Result<SearchResponse, AdapterError> {
    let started_at = Instant::now();
    let api_key = config.brave.resolve_api_key().ok_or_else(|| {
        AdapterError::TransportError(
            "web_search backend 'brave' is configured but no API key was found".to_string(),
        )
    })?;

    let resp = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .timeout(std::time::Duration::from_secs(config.timeout_seconds))
        .query(&[
            ("q", req.query.as_str()),
            ("count", &req.max_results.to_string()),
        ])
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|err| AdapterError::TransportError(format!("brave search failed: {err}")))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|err| {
        AdapterError::TransportError(format!("failed to read brave response: {err}"))
    })?;

    if !status.is_success() {
        return Err(AdapterError::UpstreamError {
            status: status.as_u16(),
            body,
        });
    }

    let result = parse_brave_response(&body, req.query)?;
    debug!(
        "web_search backend success: backend='brave' results={} duration_ms={}",
        result.results.len(),
        started_at.elapsed().as_millis()
    );
    Ok(result)
}

async fn search_custom(
    client: &Client,
    config: &WebSearchConfig,
    req: SearchRequest,
) -> Result<SearchResponse, AdapterError> {
    let custom = &config.custom;
    let started_at = Instant::now();
    let prepared = prepare_custom_search_request(custom, &req)?;

    let mut builder = client
        .request(prepared.method, prepared.url)
        .timeout(std::time::Duration::from_secs(config.timeout_seconds));

    for (key, value) in prepared.headers {
        builder = builder.header(key, value);
    }

    if let Some(body) = prepared.body {
        builder = builder
            .header("Content-Type", "application/json")
            .body(body);
    }

    let resp = builder
        .send()
        .await
        .map_err(|err| AdapterError::TransportError(format!("custom search failed: {err}")))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|err| {
        AdapterError::TransportError(format!("failed to read custom search response: {err}"))
    })?;

    if !status.is_success() {
        return Err(AdapterError::UpstreamError {
            status: status.as_u16(),
            body,
        });
    }

    let result = parse_custom_response(&body, custom, req.query)?;
    debug!(
        "web_search backend success: backend='custom' results={} duration_ms={}",
        result.results.len(),
        started_at.elapsed().as_millis()
    );
    Ok(result)
}

fn backend_name(backend: WebSearchBackend) -> &'static str {
    match backend {
        WebSearchBackend::Tavily => "tavily",
        WebSearchBackend::Brave => "brave",
        WebSearchBackend::Custom => "custom",
    }
}

pub fn prepare_custom_search_request(
    custom: &CustomSearchConfig,
    req: &SearchRequest,
) -> Result<PreparedSearchRequest, AdapterError> {
    let url = custom.url.clone().ok_or_else(|| {
        AdapterError::UnsupportedFeature(
            "web_search backend 'custom' is configured but [web_search.custom].url is missing"
                .to_string(),
        )
    })?;
    let method = Method::from_bytes(custom.method.as_bytes()).map_err(|err| {
        AdapterError::ParseError(format!(
            "invalid custom web_search method '{}': {err}",
            custom.method
        ))
    })?;

    let headers = custom
        .headers
        .iter()
        .map(|(key, value)| (key.clone(), expand_env_and_templates(value, req)))
        .collect();

    let body = if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        custom
            .body_template
            .as_ref()
            .map(|body| expand_env_and_templates(body, req))
    } else {
        None
    };

    Ok(PreparedSearchRequest {
        method,
        url,
        headers,
        body,
    })
}

pub fn parse_tavily_response(body: &str, query: String) -> Result<SearchResponse, AdapterError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|err| AdapterError::ParseError(format!("invalid tavily response JSON: {err}")))?;

    let results = value
        .get("results")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            AdapterError::ParseError("tavily response missing 'results' array".to_string())
        })?
        .iter()
        .map(|item| SearchResultItem {
            title: item
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            url: item
                .get("url")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            snippet: truncate_snippet(
                item.get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
            ),
        })
        .collect();

    Ok(SearchResponse {
        query,
        provider: "tavily".to_string(),
        results,
    })
}

pub fn parse_brave_response(body: &str, query: String) -> Result<SearchResponse, AdapterError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|err| AdapterError::ParseError(format!("invalid brave response JSON: {err}")))?;

    let results = value
        .get("web")
        .and_then(|value| value.get("results"))
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            AdapterError::ParseError("brave response missing 'web.results' array".to_string())
        })?
        .iter()
        .map(|item| SearchResultItem {
            title: item
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            url: item
                .get("url")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            snippet: truncate_snippet(
                item.get("description")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
            ),
        })
        .collect();

    Ok(SearchResponse {
        query,
        provider: "brave".to_string(),
        results,
    })
}

pub fn format_search_results(resp: &SearchResponse) -> String {
    let mut out = format!(
        "Web search results for query: {}\nProvider: {}\n",
        resp.query, resp.provider
    );

    if resp.results.is_empty() {
        out.push_str("No results found.");
        return out;
    }

    for (idx, result) in resp.results.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {}\nURL: {}\nSnippet: {}\n",
            idx + 1,
            result.title,
            result.url,
            result.snippet
        ));
    }

    out
}

pub fn parse_custom_response(
    body: &str,
    custom: &CustomSearchConfig,
    query: String,
) -> Result<SearchResponse, AdapterError> {
    let value: Value = serde_json::from_str(body).map_err(|err| {
        AdapterError::ParseError(format!("invalid custom search response JSON: {err}"))
    })?;

    let results_path = custom.results_path.as_deref().ok_or_else(|| {
        AdapterError::ParseError(
            "custom web_search config missing required results_path".to_string(),
        )
    })?;
    let title_path = custom.title_path.as_deref().unwrap_or("title");
    let url_path = custom.url_path.as_deref().unwrap_or("url");
    let snippet_path = custom.snippet_path.as_deref().unwrap_or("snippet");

    let results = get_dotted_path(&value, results_path)
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            AdapterError::ParseError(format!(
                "custom search response missing array at results_path '{results_path}'"
            ))
        })?
        .iter()
        .map(|item| SearchResultItem {
            title: get_dotted_path(item, title_path)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            url: get_dotted_path(item, url_path)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            snippet: truncate_snippet(
                get_dotted_path(item, snippet_path)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
            ),
        })
        .collect();

    Ok(SearchResponse {
        query,
        provider: "custom".to_string(),
        results,
    })
}

fn truncate_snippet(input: &str) -> String {
    let limit = 500;
    let mut chars = input.chars();
    let truncated: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn get_dotted_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

fn expand_env_and_templates(input: &str, req: &SearchRequest) -> String {
    let mut output = input
        .replace("{{query}}", &req.query)
        .replace("{{max_results}}", &req.max_results.to_string());

    while let Some(start) = output.find("${") {
        let Some(end_rel) = output[start + 2..].find('}') else {
            break;
        };
        let end = start + 2 + end_rel;
        let var_name = &output[start + 2..end];
        let replacement = std::env::var(var_name).unwrap_or_default();
        output.replace_range(start..=end, &replacement);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CustomSearchConfig;

    #[test]
    fn parse_tavily_results() {
        let body = r#"{
          "results": [
            { "title": "A", "url": "https://a.test", "content": "alpha" },
            { "title": "B", "url": "https://b.test", "content": "beta" }
          ]
        }"#;
        let resp = parse_tavily_response(body, "query".to_string()).unwrap();
        assert_eq!(resp.provider, "tavily");
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].title, "A");
        assert_eq!(resp.results[1].snippet, "beta");
    }

    #[test]
    fn parse_brave_results() {
        let body = r#"{
          "web": {
            "results": [
              { "title": "A", "url": "https://a.test", "description": "alpha" }
            ]
          }
        }"#;
        let resp = parse_brave_response(body, "query".to_string()).unwrap();
        assert_eq!(resp.provider, "brave");
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].url, "https://a.test");
    }

    #[test]
    fn formats_search_results_text() {
        let text = format_search_results(&SearchResponse {
            query: "query".to_string(),
            provider: "tavily".to_string(),
            results: vec![SearchResultItem {
                title: "A".to_string(),
                url: "https://a.test".to_string(),
                snippet: "alpha".to_string(),
            }],
        });
        assert!(text.contains("Web search results for query: query"));
        assert!(text.contains("1. A"));
    }

    #[test]
    fn parse_custom_results_with_dotted_paths() {
        let body = r#"{
          "data": {
            "items": [
              { "meta": { "title": "A" }, "link": "https://a.test", "summary": "alpha" }
            ]
          }
        }"#;
        let custom = CustomSearchConfig {
            url: Some("https://search.example.com/query".to_string()),
            method: "POST".to_string(),
            headers: Default::default(),
            body_template: None,
            results_path: Some("data.items".to_string()),
            title_path: Some("meta.title".to_string()),
            url_path: Some("link".to_string()),
            snippet_path: Some("summary".to_string()),
        };
        let resp = parse_custom_response(body, &custom, "query".to_string()).unwrap();
        assert_eq!(resp.provider, "custom");
        assert_eq!(resp.results[0].title, "A");
        assert_eq!(resp.results[0].url, "https://a.test");
    }

    #[test]
    fn expands_templates_and_env() {
        let req = SearchRequest {
            query: "hello".to_string(),
            max_results: 7,
        };
        std::env::set_var("SEARCH_API_KEY_TEST", "secret");
        let rendered = expand_env_and_templates(
            "Bearer ${SEARCH_API_KEY_TEST} q={{query}} n={{max_results}}",
            &req,
        );
        assert_eq!(rendered, "Bearer secret q=hello n=7");
    }

    #[test]
    fn prepares_custom_request_from_templates() {
        let custom = CustomSearchConfig {
            url: Some("https://search.example.com/query".to_string()),
            method: "POST".to_string(),
            headers: [(
                "Authorization".to_string(),
                "Bearer ${SEARCH_API_KEY_TEST}".to_string(),
            )]
            .into_iter()
            .collect(),
            body_template: Some("{\"query\":\"{{query}}\",\"limit\":{{max_results}}}".to_string()),
            results_path: Some("data.items".to_string()),
            title_path: Some("title".to_string()),
            url_path: Some("url".to_string()),
            snippet_path: Some("snippet".to_string()),
        };
        std::env::set_var("SEARCH_API_KEY_TEST", "secret");
        let prepared = prepare_custom_search_request(
            &custom,
            &SearchRequest {
                query: "hello".to_string(),
                max_results: 7,
            },
        )
        .unwrap();

        assert_eq!(prepared.method, Method::POST);
        assert_eq!(prepared.url, "https://search.example.com/query");
        assert_eq!(
            prepared.headers,
            vec![("Authorization".to_string(), "Bearer secret".to_string())]
        );
        assert_eq!(
            prepared.body.as_deref(),
            Some("{\"query\":\"hello\",\"limit\":7}")
        );
    }
}
