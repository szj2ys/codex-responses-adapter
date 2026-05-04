//! HTTP handler for the adapter proxy.
//!
//! Supports two modes:
//! 1. **CLI mode** (no config file): single upstream provider via CLI args.
//! 2. **Config mode** (`--config`): multi-provider routing with fallback.
//!
//! Reference: docs/specs/2026-03-11-search-routing.md

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use http_body_util::Full;
use http_body_util::StreamBody;
use hyper::body::Frame;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Method;
use hyper::Request;
use hyper::Response;
use hyper::StatusCode;
use reqwest::header::CONTENT_TYPE as REQWEST_CONTENT_TYPE;
use reqwest::Client;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::config::AdapterConfig;
use crate::config::RouteTarget;
use crate::config::WebSearchConfig;
use crate::config::WebSearchStrategy;
use crate::error::AdapterError;
use crate::providers::ProviderCapabilities;
use crate::providers::ProviderKind;
use crate::request_converter;
use crate::response_converter;
use crate::types::chat_api::ChatCompletionsRequest;
use crate::types::chat_api::ChatCompletionsResponse;
use crate::types::chat_api::ChatMessage;
use crate::types::chat_api::ChatStreamChunk;
use crate::types::chat_api::FunctionCall;
use crate::types::chat_api::ToolCall;
use crate::types::responses_api::FunctionCallOutputPayload;
use crate::types::responses_api::ResponseItem;
use crate::types::responses_api::ResponsesApiRequest;
use crate::web_search;
use crate::web_search::ADAPTER_WEB_SEARCH_TOOL_NAME;

const LOG_UPSTREAM_REQUEST_ENV: &str = "CHAT_ADAPTER_PROXY_LOG_UPSTREAM_REQUEST";

// ---------------------------------------------------------------------------
// Server configuration (from CLI or config file)
// ---------------------------------------------------------------------------

/// Configuration for the proxy server – built from CLI args or config file.
pub struct ServerConfig {
    pub port: u16,
    pub allow_downgrade: bool,
    pub web_search: WebSearchConfig,
    /// Named upstream providers.  Key = provider name (e.g. "glm").
    pub providers: HashMap<String, UpstreamProvider>,
    /// Model routing: Codex model name → ordered list of route targets.
    pub model_routes: HashMap<String, Vec<RouteTarget>>,
    /// Default route when no model mapping matches.
    pub default_route: Option<RouteTarget>,
}

/// A fully resolved upstream provider, ready to make HTTP requests.
pub struct UpstreamProvider {
    pub client: Client,
    pub base_url: String,
    pub api_key: Option<String>,
    pub capabilities: ProviderCapabilities,
    /// When true, forward the incoming Codex bearer token.
    pub use_incoming_auth: bool,
}

impl ServerConfig {
    /// Build from CLI args (single-provider mode).
    pub fn from_cli(
        port: u16,
        base_url: String,
        api_key: Option<String>,
        provider: ProviderKind,
        allow_downgrade: bool,
        model_map: HashMap<String, String>,
        default_model: Option<String>,
    ) -> anyhow::Result<Self> {
        let capabilities = provider.default_capabilities();
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;

        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            UpstreamProvider {
                client,
                base_url,
                api_key,
                capabilities,
                use_incoming_auth: false,
            },
        );

        // Convert simple model_map to route targets
        let mut model_routes: HashMap<String, Vec<RouteTarget>> = HashMap::new();
        for (codex_name, upstream_model) in &model_map {
            model_routes.insert(
                codex_name.clone(),
                vec![RouteTarget {
                    provider: "default".to_string(),
                    model: upstream_model.clone(),
                }],
            );
        }

        let default_route = default_model.map(|m| RouteTarget {
            provider: "default".to_string(),
            model: m,
        });

        Ok(Self {
            port,
            allow_downgrade,
            web_search: WebSearchConfig::default(),
            providers,
            model_routes,
            default_route,
        })
    }

    /// Build from config file (multi-provider mode).
    pub fn from_config(config: AdapterConfig) -> anyhow::Result<Self> {
        let mut providers = HashMap::new();

        for (name, pc) in &config.providers {
            let api_key = pc.resolve_api_key();
            let provider_kind = ProviderKind::from_str(&pc.provider_type);
            let mut capabilities = provider_kind.default_capabilities();
            if let Some(supports_responses_api) = pc.supports_responses_api {
                capabilities.supports_responses_api = supports_responses_api;
            }
            let provider_label = pc.name.as_deref().unwrap_or(name);

            let client = Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?;

            if !pc.use_incoming_auth && api_key.is_none() {
                warn!(
                    "provider '{}' ({}): no API key found (env={:?})",
                    name, provider_label, pc.api_key_env
                );
            } else if pc.use_incoming_auth {
                info!(
                    "provider '{}' ({}): using incoming bearer auth",
                    name, provider_label
                );
            }

            providers.insert(
                name.clone(),
                UpstreamProvider {
                    client,
                    base_url: pc.base_url.clone(),
                    api_key,
                    capabilities,
                    use_incoming_auth: pc.use_incoming_auth,
                },
            );
        }

        let mut model_routes: HashMap<String, Vec<RouteTarget>> = HashMap::new();
        for entry in &config.models {
            model_routes.insert(entry.name.clone(), entry.routes.clone());
        }

        Ok(Self {
            port: config.server.port,
            allow_downgrade: config.server.allow_downgrade,
            web_search: config.web_search,
            providers,
            model_routes,
            default_route: config.default_route,
        })
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = TcpListener::bind(addr).await?;
    info!("codex-responses-adapter listening on http://{addr}");

    for (name, p) in &config.providers {
        let auth_mode = if p.use_incoming_auth {
            "user_account"
        } else if p.api_key.is_some() {
            "api_key"
        } else {
            "none"
        };
        info!(
            "  provider '{}': {} (auth={})",
            name, p.base_url, auth_mode
        );
    }
    info!("  model routes: {} entries", config.model_routes.len());
    info!("  allow_downgrade: {}", config.allow_downgrade);

    let shared = Arc::new(config);

    loop {
        let (stream, _remote) = listener.accept().await?;
        let shared = shared.clone();

        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = service_fn(move |req| {
                let shared = shared.clone();
                async move { handle_request(req, shared).await }
            });

            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                let err_text = err.to_string();
                if err_text.contains("connection closed before message completed") {
                    debug!("connection closed before request completed: {err_text}");
                } else {
                    error!("connection error: {err}");
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

type BoxBody = http_body_util::Either<
    Full<Bytes>,
    StreamBody<ReceiverStream<Result<Frame<Bytes>, Infallible>>>,
>;

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ServerConfig>,
) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    match method {
        Method::POST => {
            // /v1/responses → default mixed routing
            // /v1/{provider}/responses → force specific provider
            if path == "/v1/responses" {
                Ok(handle_responses(req, state, None).await)
            } else if path.starts_with("/v1/") && path.ends_with("/responses") {
                let provider_name = path
                    .strip_prefix("/v1/")
                    .and_then(|s| s.strip_suffix("/responses"))
                    .unwrap_or("")
                    .to_string();
                if provider_name.is_empty() || !state.providers.contains_key(&provider_name) {
                    Ok(json_response(
                        StatusCode::NOT_FOUND,
                        &json!({"error": format!("provider '{}' not found. available: {:?}", provider_name, state.providers.keys().collect::<Vec<_>>())}),
                    ))
                } else {
                    info!("path override: forcing provider '{provider_name}'");
                    Ok(handle_responses(req, state, Some(provider_name)).await)
                }
            } else {
                Ok(json_response(
                    StatusCode::NOT_FOUND,
                    &json!({"error": "not found"}),
                ))
            }
        }
        Method::GET if path == "/health" => {
            Ok(json_response(StatusCode::OK, &json!({"status": "ok"})))
        }
        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            &json!({"error": "not found"}),
        )),
    }
}

async fn handle_responses(
    req: Request<Incoming>,
    state: Arc<ServerConfig>,
    force_provider: Option<String>,
) -> Response<BoxBody> {
    // Extract incoming bearer token for passthrough auth
    let incoming_auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    // Read body
    let body_bytes = match req.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => return adapter_error_response(AdapterError::ParseError(e.to_string())),
    };

    // Parse as ResponsesApiRequest
    let mut responses_req: ResponsesApiRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            return adapter_error_response(AdapterError::ParseError(format!(
                "invalid request JSON: {e}"
            )));
        }
    };

    let is_streaming = responses_req.stream;
    let codex_model = responses_req.model.clone();
    let mut is_search_request = request_uses_web_search(&responses_req);

    // Strip web_search tools when disabled to avoid unsupported_feature errors
    if is_search_request && !state.web_search.enabled {
        responses_req.tools.retain(|tool| {
            !matches!(
                tool.get("type").and_then(|v| v.as_str()),
                Some("web_search") | Some("web_search_preview")
            )
        });
        is_search_request = false;
    }

    // Resolve routes: if forced, use only that provider; otherwise use model map
    let routes = if let Some(ref forced) = force_provider {
        // For forced provider: find the mapped model for this provider, or use original model
        let model = state
            .model_routes
            .get(&codex_model)
            .and_then(|routes| routes.iter().find(|r| &r.provider == forced))
            .map(|r| r.model.clone())
            .unwrap_or_else(|| codex_model.clone());
        vec![RouteTarget {
            provider: forced.clone(),
            model,
        }]
    } else {
        state
            .model_routes
            .get(&codex_model)
            .cloned()
            .unwrap_or_else(|| {
                if let Some(default) = &state.default_route {
                    vec![default.clone()]
                } else {
                    vec![RouteTarget {
                        provider: "default".to_string(),
                        model: codex_model.clone(),
                    }]
                }
            })
    };

    // Try each route in order (primary + fallbacks)
    let mut last_error: Option<AdapterError> = None;

    for (i, route) in routes.iter().enumerate() {
        let provider = match state.providers.get(&route.provider) {
            Some(p) => p,
            None => {
                warn!("provider '{}' not found, skipping route", route.provider);
                last_error = Some(AdapterError::TransportError(format!(
                    "provider '{}' not configured",
                    route.provider
                )));
                continue;
            }
        };

        if i == 0 {
            info!(
                "routing '{}' → provider '{}' model '{}'",
                codex_model, route.provider, route.model
            );
        } else {
            warn!(
                "fallback #{}: trying provider '{}' model '{}'",
                i, route.provider, route.model
            );
        }

        if is_search_request {
            info!(
                "web_search detected: strategy='{}' backend='{}'",
                search_strategy_name(&state.web_search),
                configured_backend_name(&state.web_search)
            );
            match search_strategy_for_route(&state.web_search, provider) {
                SearchExecution::Passthrough => {
                    info!(
                        "search request: passthrough to provider '{}' /responses",
                        route.provider
                    );
                    match forward_responses_passthrough(
                        provider,
                        route,
                        &incoming_auth,
                        &body_bytes,
                    )
                    .await
                    {
                        Ok(resp) => return proxy_upstream_response(resp).await,
                        Err(err) => {
                            if should_fallback_to_backend(&state.web_search) {
                                warn!(
                                    "search passthrough via provider '{}' failed; falling back to adapter-managed backend: {}",
                                    route.provider, err
                                );
                            } else {
                                last_error = Some(err);
                                continue;
                            }
                        }
                    }
                }
                SearchExecution::AdapterManaged => {}
                SearchExecution::Unsupported(message) => {
                    warn!(
                        "web_search unsupported on provider '{}': {}",
                        route.provider, message
                    );
                    last_error = Some(AdapterError::UnsupportedFeature(message));
                    continue;
                }
            }
        }

        let normalized_req = if is_search_request {
            match search_strategy_for_route(&state.web_search, provider) {
                SearchExecution::AdapterManaged => {
                    info!(
                        "web_search using adapter-managed backend '{}' on provider '{}'",
                        configured_backend_name(&state.web_search),
                        route.provider
                    );
                    match replay_web_search_calls(&responses_req, provider, &state.web_search).await
                    {
                        Ok(req) => req,
                        Err(err) => {
                            last_error = Some(err);
                            continue;
                        }
                    }
                }
                _ => responses_req.clone(),
            }
        } else {
            responses_req.clone()
        };

        // Convert request with this provider's capabilities
        let mut chat_req = match request_converter::convert_request(
            &normalized_req,
            &provider.capabilities,
            state.allow_downgrade,
            is_search_request,
        ) {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };

        chat_req.model = route.model.clone();

        if is_search_request {
            match execute_adapter_managed_search(
                provider,
                route,
                &incoming_auth,
                &state.web_search,
                chat_req,
            )
            .await
            {
                Ok(chat_resp) => {
                    return chat_response_to_client(chat_resp, is_streaming);
                }
                Err(err) => {
                    last_error = Some(err);
                    continue;
                }
            }
        } else {
            let upstream_resp =
                match send_chat_request(provider, route, &incoming_auth, &chat_req).await {
                    Ok(resp) => resp,
                    Err(err) => {
                        last_error = Some(err);
                        continue;
                    }
                };

            if is_streaming {
                return handle_streaming(upstream_resp).await;
            } else {
                return handle_non_streaming(upstream_resp).await;
            }
        }
    }

    // All routes failed
    adapter_error_response(last_error.unwrap_or_else(|| {
        AdapterError::TransportError(format!("no routes configured for model '{codex_model}'"))
    }))
}

#[derive(Debug, PartialEq, Eq)]
enum SearchExecution {
    Passthrough,
    AdapterManaged,
    Unsupported(String),
}

fn request_uses_web_search(req: &ResponsesApiRequest) -> bool {
    req.tools.iter().any(|tool| {
        matches!(
            tool.get("type").and_then(|value| value.as_str()),
            Some("web_search") | Some("web_search_preview")
        )
    }) || req.input.iter().any(|item| {
        matches!(
            item,
            crate::types::responses_api::ResponseItem::WebSearchCall { .. }
        )
    })
}

fn search_strategy_for_route(
    web_search: &WebSearchConfig,
    provider: &UpstreamProvider,
) -> SearchExecution {
    if !web_search.enabled {
        return SearchExecution::Unsupported(
            "web_search was requested but [web_search].enabled is false".to_string(),
        );
    }

    match web_search.strategy {
        WebSearchStrategy::PreferPassthrough => {
            if provider.capabilities.supports_responses_api {
                SearchExecution::Passthrough
            } else if web_search.backend.is_some() {
                SearchExecution::AdapterManaged
            } else {
                SearchExecution::Unsupported(
                    "web_search was requested, but the selected provider does not support /responses passthrough and no adapter-managed backend is implemented yet".to_string(),
                )
            }
        }
        WebSearchStrategy::ForceBackend => {
            if web_search.backend.is_some() {
                SearchExecution::AdapterManaged
            } else {
                SearchExecution::Unsupported(
                    "web_search strategy 'force_backend' is configured, but no adapter-managed web_search backend is configured".to_string(),
                )
            }
        }
    }
}

fn should_fallback_to_backend(web_search: &WebSearchConfig) -> bool {
    web_search.allow_backend_fallback && web_search.backend.is_some()
}

fn search_strategy_name(web_search: &WebSearchConfig) -> &'static str {
    match web_search.strategy {
        WebSearchStrategy::PreferPassthrough => "prefer_passthrough",
        WebSearchStrategy::ForceBackend => "force_backend",
    }
}

fn configured_backend_name(web_search: &WebSearchConfig) -> &'static str {
    match web_search.backend {
        Some(crate::config::WebSearchBackend::Tavily) => "tavily",
        Some(crate::config::WebSearchBackend::Brave) => "brave",
        Some(crate::config::WebSearchBackend::Custom) => "custom",
        None => "none",
    }
}

async fn replay_web_search_calls(
    req: &ResponsesApiRequest,
    provider: &UpstreamProvider,
    web_search_config: &WebSearchConfig,
) -> Result<ResponsesApiRequest, AdapterError> {
    let mut normalized = req.clone();
    let mut rebuilt_input = Vec::with_capacity(normalized.input.len());

    for item in &normalized.input {
        match item {
            ResponseItem::WebSearchCall {
                id,
                status,
                call_id,
                query,
            } => {
                let replayable = status.as_deref().unwrap_or("completed") == "completed";
                if !replayable {
                    return Err(AdapterError::UnsupportedFeature(
                        "adapter-managed web_search replay only supports completed web_search_call items"
                            .to_string(),
                    ));
                }

                let query = query
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        AdapterError::UnsupportedFeature(
                            "adapter-managed web_search replay requires web_search_call.query"
                                .to_string(),
                        )
                    })?
                    .to_string();

                let tool_call_id = call_id
                    .clone()
                    .or_else(|| id.clone())
                    .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));

                info!(
                    "web_search replaying completed historical call: call_id='{}' query='{}' backend='{}'",
                    tool_call_id,
                    query,
                    configured_backend_name(web_search_config)
                );

                rebuilt_input.push(ResponseItem::FunctionCall {
                    id: id.clone(),
                    name: ADAPTER_WEB_SEARCH_TOOL_NAME.to_string(),
                    arguments: json!({ "query": query }).to_string(),
                    call_id: tool_call_id.clone(),
                });

                let search_resp = web_search::search(
                    &provider.client,
                    web_search_config,
                    web_search::SearchRequest {
                        query,
                        max_results: web_search_config.max_results,
                    },
                )
                .await?;

                rebuilt_input.push(ResponseItem::FunctionCallOutput {
                    call_id: tool_call_id,
                    output: FunctionCallOutputPayload::Text(web_search::format_search_results(
                        &search_resp,
                    )),
                });
            }
            other => rebuilt_input.push(other.clone()),
        }
    }

    normalized.input = rebuilt_input;
    Ok(normalized)
}

async fn forward_responses_passthrough(
    provider: &UpstreamProvider,
    route: &RouteTarget,
    incoming_auth: &Option<String>,
    body_bytes: &Bytes,
) -> Result<reqwest::Response, AdapterError> {
    let responses_url = format!("{}/responses", provider.base_url.trim_end_matches('/'));
    let mut upstream = provider.client.post(&responses_url);

    if provider.use_incoming_auth {
        if let Some(token) = incoming_auth {
            upstream = upstream.bearer_auth(token);
        } else {
            return Err(AdapterError::TransportError(
                "provider requires incoming bearer auth for /responses passthrough, but no bearer token was provided"
                    .to_string(),
            ));
        }
    } else if let Some(key) = &provider.api_key {
        upstream = upstream.bearer_auth(key);
    }

    let upstream_resp = match upstream
        .header("Content-Type", "application/json")
        .body(body_bytes.clone())
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            return Err(AdapterError::TransportError(format!(
                "provider '{}' /responses passthrough failed: {err}",
                route.provider
            )));
        }
    };

    if !upstream_resp.status().is_success() {
        let status = upstream_resp.status().as_u16();
        let body = upstream_resp.text().await.unwrap_or_default();
        return Err(AdapterError::UpstreamError { status, body });
    }

    Ok(upstream_resp)
}

async fn send_chat_request(
    provider: &UpstreamProvider,
    route: &RouteTarget,
    incoming_auth: &Option<String>,
    chat_req: &ChatCompletionsRequest,
) -> Result<reqwest::Response, AdapterError> {
    maybe_log_upstream_request(&route.provider, chat_req);

    let chat_completions_url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );

    let mut upstream = provider.client.post(&chat_completions_url);

    if provider.use_incoming_auth {
        if let Some(token) = incoming_auth {
            upstream = upstream.bearer_auth(token);
        } else {
            warn!(
                "provider '{}' requires incoming auth but no bearer token found",
                route.provider
            );
        }
    } else if let Some(key) = &provider.api_key {
        upstream = upstream.bearer_auth(key);
    }

    let upstream_resp = upstream
        .header("Content-Type", "application/json")
        .json(chat_req)
        .send()
        .await
        .map_err(|err| {
            error!("provider '{}' request failed: {err}", route.provider);
            AdapterError::TransportError(err.to_string())
        })?;

    if !upstream_resp.status().is_success() {
        let status = upstream_resp.status().as_u16();
        let body = upstream_resp.text().await.unwrap_or_default();
        error!(
            "provider '{}' returned HTTP {status}: {body}",
            route.provider
        );
        return Err(AdapterError::UpstreamError { status, body });
    }

    Ok(upstream_resp)
}

async fn execute_adapter_managed_search(
    provider: &UpstreamProvider,
    route: &RouteTarget,
    incoming_auth: &Option<String>,
    web_search_config: &WebSearchConfig,
    mut chat_req: ChatCompletionsRequest,
) -> Result<ChatCompletionsResponse, AdapterError> {
    const MAX_SEARCH_ROUNDS: usize = 3;
    chat_req.stream = false;

    for round in 0..MAX_SEARCH_ROUNDS {
        info!(
            "web_search adapter round {}/{} on provider '{}'",
            round + 1,
            MAX_SEARCH_ROUNDS,
            route.provider
        );
        let upstream_resp = send_chat_request(provider, route, incoming_auth, &chat_req).await?;
        let chat_resp = parse_chat_response(upstream_resp).await?;

        let Some(search_calls) = extract_adapter_search_calls(&chat_resp)? else {
            info!(
                "web_search adapter finished without further tool calls on round {}",
                round + 1
            );
            return Ok(chat_resp);
        };

        info!(
            "web_search adapter received {} tool call(s) on round {}",
            search_calls.len(),
            round + 1
        );

        append_search_tool_call_message(&mut chat_req, &search_calls);
        for search_call in search_calls {
            let started_at = Instant::now();
            let search_resp = web_search::search(
                &provider.client,
                web_search_config,
                web_search::SearchRequest {
                    query: search_call.query,
                    max_results: web_search_config.max_results,
                },
            )
            .await?;

            info!(
                "web_search query completed: backend='{}' query='{}' results={} duration_ms={}",
                search_resp.provider,
                search_resp.query,
                search_resp.results.len(),
                started_at.elapsed().as_millis()
            );

            append_search_tool_result_message(&mut chat_req, &search_call.call_id, &search_resp);
        }
    }

    Err(AdapterError::UnsupportedFeature(
        "adapter-managed web_search exceeded the maximum of 3 search rounds".to_string(),
    ))
}

fn chat_response_to_client(
    chat_resp: ChatCompletionsResponse,
    _is_streaming: bool,
) -> Response<BoxBody> {
    let responses_api_resp = response_converter::convert_response(&chat_resp);
    let sse_body = response_converter::build_sse_events(&responses_api_resp);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(http_body_util::Either::Left(Full::new(Bytes::from(
            sse_body,
        ))))
        .unwrap()
}

async fn parse_chat_response(
    upstream_resp: reqwest::Response,
) -> Result<ChatCompletionsResponse, AdapterError> {
    let body = upstream_resp
        .text()
        .await
        .map_err(|err| AdapterError::TransportError(format!("failed to read upstream: {err}")))?;

    serde_json::from_str(&body).map_err(|err| {
        error!("failed to parse upstream response: {err}, body: {body}");
        AdapterError::ParseError(format!("invalid upstream response: {err}"))
    })
}

#[derive(Debug, Clone)]
struct AdapterSearchCall {
    call_id: String,
    query: String,
    raw_arguments: String,
}

fn append_search_tool_call_message(
    chat_req: &mut ChatCompletionsRequest,
    search_calls: &[AdapterSearchCall],
) {
    let assistant_tool_calls = search_calls
        .iter()
        .map(|call| ToolCall {
            id: call.call_id.clone(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: ADAPTER_WEB_SEARCH_TOOL_NAME.to_string(),
                arguments: call.raw_arguments.clone(),
            },
        })
        .collect();

    chat_req.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(assistant_tool_calls),
        tool_call_id: None,
    });
}

fn append_search_tool_result_message(
    chat_req: &mut ChatCompletionsRequest,
    call_id: &str,
    search_resp: &web_search::SearchResponse,
) {
    chat_req.messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(serde_json::Value::String(
            web_search::format_search_results(search_resp),
        )),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
    });
}

fn extract_adapter_search_calls(
    chat_resp: &ChatCompletionsResponse,
) -> Result<Option<Vec<AdapterSearchCall>>, AdapterError> {
    let Some(choice) = chat_resp.choices.first() else {
        return Ok(None);
    };

    let Some(tool_calls) = &choice.message.tool_calls else {
        return Ok(None);
    };

    if tool_calls.is_empty() {
        return Ok(None);
    }

    if tool_calls
        .iter()
        .all(|call| call.function.name == ADAPTER_WEB_SEARCH_TOOL_NAME)
    {
        let mut calls = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            let args: serde_json::Value =
                serde_json::from_str(&call.function.arguments).map_err(|err| {
                    AdapterError::ParseError(format!(
                        "invalid {} arguments JSON: {err}",
                        ADAPTER_WEB_SEARCH_TOOL_NAME
                    ))
                })?;

            let query = args
                .get("query")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    AdapterError::ParseError(format!(
                        "{} arguments missing required string field 'query'",
                        ADAPTER_WEB_SEARCH_TOOL_NAME
                    ))
                })?
                .to_string();

            calls.push(AdapterSearchCall {
                call_id: call.id.clone(),
                query,
                raw_arguments: call.function.arguments.clone(),
            });
        }
        Ok(Some(calls))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Response handling (unchanged)
// ---------------------------------------------------------------------------

async fn handle_non_streaming(upstream_resp: reqwest::Response) -> Response<BoxBody> {
    let body = match upstream_resp.text().await {
        Ok(b) => b,
        Err(e) => {
            return adapter_error_response(AdapterError::TransportError(format!(
                "failed to read upstream: {e}"
            )));
        }
    };

    let chat_resp: ChatCompletionsResponse = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            error!("failed to parse upstream response: {e}, body: {body}");
            return adapter_error_response(AdapterError::ParseError(format!(
                "invalid upstream response: {e}"
            )));
        }
    };

    let responses_api_resp = response_converter::convert_response(&chat_resp);
    let sse_body = response_converter::build_sse_events(&responses_api_resp);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(http_body_util::Either::Left(Full::new(Bytes::from(
            sse_body,
        ))))
        .unwrap()
}

async fn handle_streaming(upstream_resp: reqwest::Response) -> Response<BoxBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(128);

    tokio::spawn(async move {
        let mut translator = response_converter::StreamTranslator::new();
        let mut stream = upstream_resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    error!("upstream stream error: {e}");
                    break;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find("\n\n") {
                let line_block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                let data = extract_sse_data(&line_block);

                if data == "[DONE]" || data.is_empty() {
                    continue;
                }

                let stream_chunk: ChatStreamChunk = match serde_json::from_str(&data) {
                    Ok(c) => c,
                    Err(e) => {
                        error!("failed to parse stream chunk: {e}, data: {data}");
                        continue;
                    }
                };

                let events = translator.process_chunk(&stream_chunk);
                for event in events {
                    if tx.send(Ok(Frame::data(Bytes::from(event)))).await.is_err() {
                        return;
                    }
                }
            }
        }

        // Handle remaining buffer
        if !buffer.trim().is_empty() {
            let data = extract_sse_data(&buffer);
            if !data.is_empty() && data != "[DONE]" {
                if let Ok(stream_chunk) = serde_json::from_str::<ChatStreamChunk>(&data) {
                    let events = translator.process_chunk(&stream_chunk);
                    for event in events {
                        let _ = tx.send(Ok(Frame::data(Bytes::from(event)))).await;
                    }
                }
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    let body = StreamBody::new(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(http_body_util::Either::Right(body))
        .unwrap()
}

async fn proxy_upstream_response(upstream_resp: reqwest::Response) -> Response<BoxBody> {
    let status = upstream_resp.status();
    let content_type = upstream_resp
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let body = match upstream_resp.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            return adapter_error_response(AdapterError::TransportError(format!(
                "failed to read upstream /responses body: {err}"
            )));
        }
    };

    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header("Content-Type", content_type);
    }

    builder
        .body(http_body_util::Either::Left(Full::new(body)))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_sse_data(block: &str) -> String {
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(data) = trimmed.strip_prefix("data:") {
            return data.trim().to_string();
        }
    }
    String::new()
}

fn maybe_log_upstream_request(
    provider_name: &str,
    chat_req: &crate::types::chat_api::ChatCompletionsRequest,
) {
    let should_log = std::env::var(LOG_UPSTREAM_REQUEST_ENV)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);

    if !should_log {
        return;
    }

    match serde_json::to_string_pretty(chat_req) {
        Ok(body) => info!("upstream request for provider '{provider_name}':\n{body}"),
        Err(err) => {
            warn!("failed to serialize upstream request for provider '{provider_name}': {err}")
        }
    }
}

fn adapter_error_response(err: AdapterError) -> Response<BoxBody> {
    let status_code = err.status_code();
    let body = json!({
        "type": "response.failed",
        "response": {
            "id": format!("resp_{}", uuid::Uuid::new_v4()),
            "error": {
                "message": err.to_string(),
                "code": match &err {
                    AdapterError::UnsupportedFeature(_) => "unsupported_feature",
                    AdapterError::UnsupportedRole(_) => "unsupported_role",
                    AdapterError::CapabilityNotAvailable(_) => "capability_not_available",
                    AdapterError::UpstreamError { .. } => "upstream_error",
                    AdapterError::ParseError(_) => "parse_error",
                    AdapterError::TransportError(_) => "transport_error",
                }
            }
        }
    });
    let sse_error = format!("event: response.failed\ndata: {body}\n\n");

    Response::builder()
        .status(StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", "text/event-stream")
        .body(http_body_util::Either::Left(Full::new(Bytes::from(
            sse_error,
        ))))
        .unwrap()
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(http_body_util::Either::Left(Full::new(Bytes::from(
            value.to_string(),
        ))))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebSearchBackend;
    use crate::config::WebSearchStrategy;
    use crate::providers::ProviderKind;
    use crate::types::chat_api::Choice;
    use crate::types::chat_api::ChoiceMessage;
    use crate::types::responses_api::ContentItem;
    use crate::types::responses_api::ResponseItem;
    use serde_json::json;

    fn search_request_with_tool(tool_type: &str) -> ResponsesApiRequest {
        ResponsesApiRequest {
            model: "gpt-5.4".to_string(),
            instructions: String::new(),
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "latest news".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            tools: vec![json!({ "type": tool_type })],
            tool_choice: "auto".to_string(),
            parallel_tool_calls: false,
            stream: false,
            store: false,
            previous_response_id: None,
            reasoning: None,
            text: None,
            service_tier: None,
            prompt_cache_key: None,
            include: vec![],
        }
    }

    #[test]
    fn detects_web_search_tools() {
        assert!(request_uses_web_search(&search_request_with_tool(
            "web_search"
        )));
        assert!(request_uses_web_search(&search_request_with_tool(
            "web_search_preview"
        )));
        assert!(!request_uses_web_search(&search_request_with_tool(
            "function"
        )));
    }

    #[test]
    fn detects_web_search_call_items() {
        let req = ResponsesApiRequest {
            model: "gpt-5.4".to_string(),
            instructions: String::new(),
            input: vec![ResponseItem::WebSearchCall {
                id: None,
                status: Some("completed".to_string()),
                call_id: None,
                query: None,
            }],
            tools: vec![],
            tool_choice: "auto".to_string(),
            parallel_tool_calls: false,
            stream: false,
            store: false,
            previous_response_id: None,
            reasoning: None,
            text: None,
            service_tier: None,
            prompt_cache_key: None,
            include: vec![],
        };
        assert!(request_uses_web_search(&req));
    }

    #[test]
    fn search_passthrough_requires_enabled_config_and_responses_support() {
        let mut provider = UpstreamProvider {
            client: Client::builder().build().unwrap(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            capabilities: ProviderKind::Openai.default_capabilities(),
            use_incoming_auth: true,
        };

        assert_eq!(
            search_strategy_for_route(
                &WebSearchConfig {
                    enabled: true,
                    strategy: WebSearchStrategy::PreferPassthrough,
                    ..WebSearchConfig::default()
                },
                &provider
            ),
            SearchExecution::Passthrough
        );

        provider.capabilities.supports_responses_api = false;
        assert!(matches!(
            search_strategy_for_route(
                &WebSearchConfig {
                    enabled: true,
                    strategy: WebSearchStrategy::PreferPassthrough,
                    backend: Some(WebSearchBackend::Tavily),
                    ..WebSearchConfig::default()
                },
                &provider
            ),
            SearchExecution::AdapterManaged
        ));
    }

    #[test]
    fn force_backend_requires_backend_config() {
        let provider = UpstreamProvider {
            client: Client::builder().build().unwrap(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            capabilities: ProviderKind::Openai.default_capabilities(),
            use_incoming_auth: true,
        };

        assert!(matches!(
            search_strategy_for_route(
                &WebSearchConfig {
                    enabled: true,
                    strategy: WebSearchStrategy::ForceBackend,
                    ..WebSearchConfig::default()
                },
                &provider
            ),
            SearchExecution::Unsupported(_)
        ));

        assert!(matches!(
            search_strategy_for_route(
                &WebSearchConfig {
                    enabled: true,
                    strategy: WebSearchStrategy::ForceBackend,
                    backend: Some(WebSearchBackend::Brave),
                    ..WebSearchConfig::default()
                },
                &provider
            ),
            SearchExecution::AdapterManaged
        ));
    }

    #[test]
    fn extracts_adapter_search_calls_from_chat_response() {
        let chat_resp = ChatCompletionsResponse {
            id: "chatcmpl_123".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: ADAPTER_WEB_SEARCH_TOOL_NAME.to_string(),
                            arguments: r#"{"query":"today news"}"#.to_string(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
            model: None,
        };

        let calls = extract_adapter_search_calls(&chat_resp).unwrap().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_123");
        assert_eq!(calls[0].query, "today news");
    }

    #[test]
    fn backend_fallback_requires_flag_and_backend() {
        assert!(!should_fallback_to_backend(&WebSearchConfig::default()));
        assert!(!should_fallback_to_backend(&WebSearchConfig {
            enabled: true,
            allow_backend_fallback: true,
            ..WebSearchConfig::default()
        }));
        assert!(should_fallback_to_backend(&WebSearchConfig {
            enabled: true,
            allow_backend_fallback: true,
            backend: Some(WebSearchBackend::Tavily),
            ..WebSearchConfig::default()
        }));
    }

    #[test]
    fn appends_search_results_as_follow_up_messages() {
        let mut chat_req = ChatCompletionsRequest {
            model: "glm-4-flash".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(serde_json::Value::String("Find Rust news".to_string())),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            tool_choice: None,
            stream: false,
            parallel_tool_calls: None,
        };

        let search_calls = vec![AdapterSearchCall {
            call_id: "call_1".to_string(),
            query: "latest rust news".to_string(),
            raw_arguments: r#"{"query":"latest rust news"}"#.to_string(),
        }];

        append_search_tool_call_message(&mut chat_req, &search_calls);
        append_search_tool_result_message(
            &mut chat_req,
            "call_1",
            &crate::web_search::SearchResponse {
                query: "latest rust news".to_string(),
                provider: "custom".to_string(),
                results: vec![crate::web_search::SearchResultItem {
                    title: "Rust blog".to_string(),
                    url: "https://blog.rust-lang.org".to_string(),
                    snippet: "Rust update".to_string(),
                }],
            },
        );

        assert_eq!(chat_req.messages.len(), 3);
        assert_eq!(chat_req.messages[1].role, "assistant");
        assert_eq!(chat_req.messages[2].role, "tool");
        assert_eq!(chat_req.messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(chat_req.messages[2]
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("Web search results for query: latest rust news"));
    }

    #[tokio::test]
    async fn replay_web_search_call_requires_query() {
        let provider = UpstreamProvider {
            client: Client::builder().build().unwrap(),
            base_url: "https://example.com".to_string(),
            api_key: None,
            capabilities: ProviderKind::Glm.default_capabilities(),
            use_incoming_auth: false,
        };

        let req = ResponsesApiRequest {
            model: "gpt-5.4".to_string(),
            instructions: String::new(),
            input: vec![ResponseItem::WebSearchCall {
                id: Some("ws_1".to_string()),
                status: Some("completed".to_string()),
                call_id: Some("call_ws_1".to_string()),
                query: None,
            }],
            tools: vec![],
            tool_choice: "auto".to_string(),
            parallel_tool_calls: false,
            stream: false,
            store: false,
            previous_response_id: None,
            reasoning: None,
            text: None,
            service_tier: None,
            prompt_cache_key: None,
            include: vec![],
        };

        let err = replay_web_search_calls(
            &req,
            &provider,
            &WebSearchConfig {
                enabled: true,
                strategy: WebSearchStrategy::ForceBackend,
                backend: Some(WebSearchBackend::Custom),
                ..WebSearchConfig::default()
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("requires web_search_call.query"));
    }

    #[test]
    fn test_passthrough_model_when_no_routes() {
        let config = ServerConfig::from_cli(
            6789,
            "https://example.com/v1".to_string(),
            Some("test-key".to_string()),
            ProviderKind::Custom,
            false,
            std::collections::HashMap::new(),
            None,
        )
        .unwrap();

        assert!(config.model_routes.is_empty());
        assert!(config.default_route.is_none());
    }
}
