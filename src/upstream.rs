//! HTTP client module for upstream provider communication.
//!
//! Provides a trait-based port for testability with InMemoryClient
//! for unit tests and ReqwestClient for production use.

use crate::error::AdapterError;
use crate::providers::ProviderCapabilities;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A simple HTTP request structure.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

/// A simple HTTP response structure.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn with_json_body(mut self, json: &serde_json::Value) -> Self {
        self.body = json.to_string().into_bytes();
        self.headers.insert("content-type".to_string(), "application/json".to_string());
        self
    }
}

/// HTTP client trait - the testing seam.
#[async_trait::async_trait]
pub trait HttpClient: Send + Sync {
    /// Send an HTTP request and return the response.
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, AdapterError>;
}

/// In-memory HTTP client for testing.
/// Records all requests and returns pre-configured responses.
pub struct InMemoryClient {
    recorded_requests: Arc<Mutex<Vec<HttpRequest>>>,
    responses: Arc<Mutex<Vec<HttpResponse>>>,
}

impl InMemoryClient {
    pub fn new() -> Self {
        Self {
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue a response to be returned on the next request.
    pub fn enqueue_response(&self, response: HttpResponse) {
        self.responses.lock().unwrap().push(response);
    }

    /// Get all recorded requests.
    pub fn recorded_requests(&self) -> Vec<HttpRequest> {
        self.recorded_requests.lock().unwrap().clone()
    }

    /// Get the number of recorded requests.
    pub fn request_count(&self) -> usize {
        self.recorded_requests.lock().unwrap().len()
    }
}

impl Default for InMemoryClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl HttpClient for InMemoryClient {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, AdapterError> {
        self.recorded_requests.lock().unwrap().push(request);
        
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(AdapterError::TransportError(
                "No more responses queued".to_string()
            ));
        }
        Ok(responses.remove(0))
    }
}

/// Represents an upstream provider with its configuration.
pub struct UpstreamProvider<C: HttpClient> {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub capabilities: ProviderCapabilities,
    pub use_incoming_auth: bool,
    pub client: C,
}

impl<C: HttpClient> UpstreamProvider<C> {
    /// Create a new upstream provider.
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        client: C,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: None,
            capabilities: crate::providers::ProviderKind::Custom.default_capabilities(),
            use_incoming_auth: false,
            client,
        }
    }

    /// Set the API key.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set whether to use incoming auth.
    pub fn with_incoming_auth(mut self, use_incoming: bool) -> Self {
        self.use_incoming_auth = use_incoming;
        self
    }

    /// Send a request to this provider.
    pub async fn send_request(
        &self,
        request: HttpRequest,
        incoming_auth: Option<&str>,
    ) -> Result<HttpResponse, AdapterError> {
        let mut req = request;

        // Inject auth header
        if let Some(auth) = incoming_auth.filter(|_| self.use_incoming_auth) {
            req.headers.insert("Authorization".to_string(), format!("Bearer {}", auth));
        } else if let Some(key) = &self.api_key {
            req.headers.insert("Authorization".to_string(), format!("Bearer {}", key));
        }

        self.client.send(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_client_records_requests() {
        let client = InMemoryClient::new();
        let response = HttpResponse::new(200).with_body(b"Hello".to_vec());
        client.enqueue_response(response.clone());

        let request = HttpRequest::new("GET", "https://example.com");
        let result = client.send(request.clone()).await.unwrap();

        assert_eq!(result, response);
        assert_eq!(client.request_count(), 1);
        assert_eq!(client.recorded_requests()[0], request);
    }

    #[tokio::test]
    async fn test_in_memory_client_returns_queued_responses_in_order() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(200));
        client.enqueue_response(HttpResponse::new(201));

        let r1 = client.send(HttpRequest::new("GET", "https://a.com")).await.unwrap();
        let r2 = client.send(HttpRequest::new("POST", "https://b.com")).await.unwrap();

        assert_eq!(r1.status, 200);
        assert_eq!(r2.status, 201);
    }

    #[tokio::test]
    async fn test_in_memory_client_returns_error_when_no_responses() {
        let client = InMemoryClient::new();

        let result = client.send(HttpRequest::new("GET", "https://example.com")).await;

        assert!(result.is_err());
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[tokio::test]
    async fn test_provider_injects_api_key_auth() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(200));

        let provider = UpstreamProvider::new("test", "https://api.example.com", client)
            .with_api_key("secret-key-123");

        let request = HttpRequest::new("POST", "https://api.example.com/v1/chat");
        let _ = provider.send_request(request, None).await;

        let recorded = provider.client.recorded_requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].headers.get("Authorization"),
            Some(&"Bearer secret-key-123".to_string())
        );
    }

    #[tokio::test]
    async fn test_provider_uses_incoming_auth_when_configured() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(200));

        let provider = UpstreamProvider::new("test", "https://api.example.com", client)
            .with_api_key("provider-key")
            .with_incoming_auth(true);

        let request = HttpRequest::new("POST", "https://api.example.com/v1/chat");
        let _ = provider.send_request(request, Some("incoming-token")).await;

        let recorded = provider.client.recorded_requests();
        assert_eq!(
            recorded[0].headers.get("Authorization"),
            Some(&"Bearer incoming-token".to_string())
        );
    }

    #[tokio::test]
    async fn test_provider_uses_provider_key_when_incoming_auth_disabled() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(200));

        let provider = UpstreamProvider::new("test", "https://api.example.com", client)
            .with_api_key("provider-key")
            .with_incoming_auth(false);

        let request = HttpRequest::new("POST", "https://api.example.com/v1/chat");
        let _ = provider.send_request(request, Some("incoming-token")).await;

        let recorded = provider.client.recorded_requests();
        assert_eq!(
            recorded[0].headers.get("Authorization"),
            Some(&"Bearer provider-key".to_string())
        );
    }
}

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 100,
            max_delay_ms: 5000,
        }
    }
}

impl RetryConfig {
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn with_no_delay(mut self) -> Self {
        self.base_delay_ms = 0;
        self.max_delay_ms = 0;
        self
    }
}

/// Send a request with retry logic for transient failures.
pub async fn send_request_with_retry<C: HttpClient>(
    client: &C,
    request: HttpRequest,
    config: &RetryConfig,
) -> Result<HttpResponse, AdapterError> {
    let mut last_error = None;
    
    for attempt in 0..=config.max_retries {
        match client.send(request.clone()).await {
            Ok(response) => {
                // Check if it's a retryable status code
                if should_retry(response.status) {
                    if attempt < config.max_retries {
                        last_error = Some(AdapterError::UpstreamError {
                            status: response.status,
                            body: String::from_utf8_lossy(&response.body).to_string(),
                        });
                        let delay = calculate_backoff(attempt, config);
                        if delay > 0 {
                            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        }
                        continue;
                    } else {
                        // Last attempt failed with retryable status
                        return Err(AdapterError::UpstreamError {
                            status: response.status,
                            body: String::from_utf8_lossy(&response.body).to_string(),
                        });
                    }
                }
                return Ok(response);
            }
            Err(e) => {
                if attempt < config.max_retries {
                    last_error = Some(e);
                    let delay = calculate_backoff(attempt, config);
                    if delay > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    }
                } else {
                    return Err(e);
                }
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| {
        AdapterError::TransportError("Max retries exceeded".to_string())
    }))
}

fn should_retry(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn calculate_backoff(attempt: u32, config: &RetryConfig) -> u64 {
    if config.base_delay_ms == 0 {
        return 0;
    }
    let delay = config.base_delay_ms * 2_u64.pow(attempt.min(6)); // Cap at 2^6 = 64x
    delay.min(config.max_delay_ms)
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn test_retry_on_server_error() {
        let client = InMemoryClient::new();
        // First two calls fail with 503, third succeeds
        client.enqueue_response(HttpResponse::new(503));
        client.enqueue_response(HttpResponse::new(503));
        client.enqueue_response(HttpResponse::new(200).with_body(b"success".to_vec()));

        let config = RetryConfig::default().with_no_delay();
        let request = HttpRequest::new("GET", "https://api.example.com");
        
        let result = send_request_with_retry(&client, request, &config).await.unwrap();
        
        assert_eq!(result.status, 200);
        assert_eq!(result.body, b"success");
        assert_eq!(client.request_count(), 3);
    }

    #[tokio::test]
    async fn test_no_retry_on_success() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(200));

        let config = RetryConfig::default();
        let request = HttpRequest::new("GET", "https://api.example.com");
        
        let result = send_request_with_retry(&client, request, &config).await.unwrap();
        
        assert_eq!(result.status, 200);
        assert_eq!(client.request_count(), 1);
    }

    #[tokio::test]
    async fn test_no_retry_on_client_error() {
        let client = InMemoryClient::new();
        client.enqueue_response(HttpResponse::new(400).with_body(b"bad request".to_vec()));

        let config = RetryConfig::default();
        let request = HttpRequest::new("POST", "https://api.example.com");
        
        let result = send_request_with_retry(&client, request, &config).await.unwrap();
        
        assert_eq!(result.status, 400);
        assert_eq!(client.request_count(), 1);
    }

    #[tokio::test]
    async fn test_returns_last_error_after_max_retries() {
        let client = InMemoryClient::new();
        // All calls fail
        client.enqueue_response(HttpResponse::new(503));
        client.enqueue_response(HttpResponse::new(503));
        client.enqueue_response(HttpResponse::new(503));
        client.enqueue_response(HttpResponse::new(503));

        let config = RetryConfig::default()
            .with_max_retries(2)
            .with_no_delay();
        let request = HttpRequest::new("GET", "https://api.example.com");
        
        let result = send_request_with_retry(&client, request, &config).await;
        
        assert!(result.is_err());
        assert_eq!(client.request_count(), 3); // initial + 2 retries
    }
}

// Production HTTP client using reqwest
#[cfg(not(test))]
use reqwest::Client as ReqwestClient;

/// Production HTTP client implementation using reqwest.
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    /// Create a new ReqwestHttpClient with default settings.
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self { client })
    }

    /// Create a new ReqwestHttpClient with custom timeout.
    pub fn with_timeout(seconds: u64) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(seconds))
            .build()?;
        Ok(Self { client })
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new().expect("Failed to create reqwest client")
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, AdapterError> {
        let method = match request.method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "PATCH" => reqwest::Method::PATCH,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            _ => reqwest::Method::POST,
        };

        let mut req_builder = self.client.request(method, &request.url);

        for (key, value) in &request.headers {
            req_builder = req_builder.header(key, value);
        }

        if !request.body.is_empty() {
            req_builder = req_builder.body(request.body.clone());
        }

        let response = req_builder
            .send()
            .await
            .map_err(|e| AdapterError::TransportError(e.to_string()))?;

        let status = response.status().as_u16();
        let mut headers = HashMap::new();
        for (key, value) in response.headers() {
            if let Ok(v) = value.to_str() {
                headers.insert(key.to_string(), v.to_string());
            }
        }

        let body = response
            .bytes()
            .await
            .map_err(|e| AdapterError::TransportError(e.to_string()))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
