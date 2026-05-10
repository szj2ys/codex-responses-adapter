//! Web search orchestration - coordinates search execution strategy.

use crate::config::{WebSearchConfig, WebSearchStrategy, WebSearchBackend};
use crate::providers::ProviderCapabilities;

/// The execution strategy for handling a web search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchExecution {
    /// Pass through to upstream provider's /responses endpoint.
    Passthrough,
    /// Handle search via adapter-managed backend (Tavily, Brave, Custom).
    AdapterManaged,
    /// Search cannot be executed; contains reason.
    Unsupported(String),
}

/// Context for search strategy decisions.
#[derive(Debug, Clone)]
pub struct SearchContext {
    pub capabilities: ProviderCapabilities,
}

impl SearchContext {
    pub fn new(capabilities: ProviderCapabilities) -> Self {
        Self { capabilities }
    }
}

const ERR_DISABLED: &str = "web_search was requested but [web_search].enabled is false";
const ERR_NO_PASSTHROUGH_OR_BACKEND: &str = "web_search was requested, but the selected provider does not support /responses passthrough and no adapter-managed backend is implemented yet";
const ERR_FORCE_BACKEND_NO_BACKEND: &str = "web_search strategy 'force_backend' is configured, but no adapter-managed web_search backend is configured";

/// Determine the search execution strategy based on configuration and provider.
pub fn determine_strategy(
    config: &WebSearchConfig,
    context: &SearchContext,
) -> SearchExecution {
    if !config.enabled {
        return SearchExecution::Unsupported(ERR_DISABLED.to_string());
    }

    match config.strategy {
        WebSearchStrategy::PreferPassthrough => {
            if context.capabilities.supports_responses_api {
                return SearchExecution::Passthrough;
            }
            if config.backend.is_some() {
                return SearchExecution::AdapterManaged;
            }
            SearchExecution::Unsupported(ERR_NO_PASSTHROUGH_OR_BACKEND.to_string())
        }
        WebSearchStrategy::ForceBackend => {
            if config.backend.is_some() {
                return SearchExecution::AdapterManaged;
            }
            SearchExecution::Unsupported(ERR_FORCE_BACKEND_NO_BACKEND.to_string())
        }
    }
}

/// Returns true if fallback to backend is allowed.
pub fn should_fallback_to_backend(config: &WebSearchConfig) -> bool {
    config.allow_backend_fallback && config.backend.is_some()
}

/// Get the search strategy name for logging.
pub fn strategy_name(strategy: WebSearchStrategy) -> &'static str {
    match strategy {
        WebSearchStrategy::PreferPassthrough => "prefer_passthrough",
        WebSearchStrategy::ForceBackend => "force_backend",
    }
}

/// Get the configured backend name for logging.
pub fn backend_name(backend: Option<WebSearchBackend>) -> &'static str {
    match backend {
        Some(WebSearchBackend::Tavily) => "tavily",
        Some(WebSearchBackend::Brave) => "brave",
        Some(WebSearchBackend::Custom) => "custom",
        None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            supports_responses_api: false,
            supports_tools: true,
            supports_tool_choice_auto: true,
            supports_parallel_tool_calls: true,
            supports_streaming: true,
            supports_system_role: true,
            requires_single_leading_system_message: false,
            max_context_tokens: None,
        }
    }

    fn base_config() -> WebSearchConfig {
        WebSearchConfig {
            enabled: true,
            strategy: WebSearchStrategy::PreferPassthrough,
            backend: Some(WebSearchBackend::Tavily),
            max_results: 5,
            timeout_seconds: 30,
            allow_backend_fallback: false,
            tavily: Default::default(),
            brave: Default::default(),
            custom: Default::default(),
        }
    }

    #[test]
    fn disabled_search_returns_unsupported() {
        let mut config = base_config();
        config.enabled = false;
        let context = SearchContext::new(base_capabilities());

        let result = determine_strategy(&config, &context);

        assert!(
            matches!(result, SearchExecution::Unsupported(ref msg) if msg.contains("enabled is false")),
            "Expected Unsupported when web_search is disabled, got {:?}",
            result
        );
    }

    #[test]
    fn prefer_passthrough_with_capable_provider_uses_passthrough() {
        let config = base_config();
        let mut capabilities = base_capabilities();
        capabilities.supports_responses_api = true;
        let context = SearchContext::new(capabilities);

        let result = determine_strategy(&config, &context);

        assert_eq!(
            result,
            SearchExecution::Passthrough,
            "Expected Passthrough when provider supports responses API"
        );
    }

    #[test]
    fn prefer_passthrough_without_api_support_uses_adapter_managed() {
        let config = base_config();
        let capabilities = base_capabilities();
        let context = SearchContext::new(capabilities);

        let result = determine_strategy(&config, &context);

        assert_eq!(
            result,
            SearchExecution::AdapterManaged,
            "Expected AdapterManaged when provider doesn't support responses API but backend is configured"
        );
    }

    #[test]
    fn prefer_passthrough_without_backend_returns_unsupported() {
        let mut config = base_config();
        config.backend = None;
        let capabilities = base_capabilities();
        let context = SearchContext::new(capabilities);

        let result = determine_strategy(&config, &context);

        assert!(
            matches!(result, SearchExecution::Unsupported(ref msg) if msg.contains("does not support /responses passthrough")),
            "Expected Unsupported when neither passthrough nor backend is available"
        );
    }

    #[test]
    fn force_backend_with_backend_uses_adapter_managed() {
        let mut config = base_config();
        config.strategy = WebSearchStrategy::ForceBackend;
        let mut capabilities = base_capabilities();
        capabilities.supports_responses_api = true;
        let context = SearchContext::new(capabilities);

        let result = determine_strategy(&config, &context);

        assert_eq!(
            result,
            SearchExecution::AdapterManaged,
            "Expected AdapterManaged when ForceBackend strategy is used with a backend"
        );
    }

    #[test]
    fn force_backend_without_backend_returns_unsupported() {
        let mut config = base_config();
        config.strategy = WebSearchStrategy::ForceBackend;
        config.backend = None;
        let context = SearchContext::new(base_capabilities());

        let result = determine_strategy(&config, &context);

        assert!(
            matches!(result, SearchExecution::Unsupported(ref msg) if msg.contains("force_backend")),
            "Expected Unsupported when ForceBackend is set but no backend is configured"
        );
    }

    #[test]
    fn should_fallback_when_allowed_and_backend_exists() {
        let mut config = base_config();
        config.allow_backend_fallback = true;
        
        assert!(should_fallback_to_backend(&config));

        config.allow_backend_fallback = false;
        assert!(!should_fallback_to_backend(&config));

        config.allow_backend_fallback = true;
        config.backend = None;
        assert!(!should_fallback_to_backend(&config));
    }

    #[test]
    fn strategy_name_returns_correct_value() {
        assert_eq!(strategy_name(WebSearchStrategy::PreferPassthrough), "prefer_passthrough");
        assert_eq!(strategy_name(WebSearchStrategy::ForceBackend), "force_backend");
    }

    #[test]
    fn backend_name_returns_correct_value() {
        assert_eq!(backend_name(Some(WebSearchBackend::Tavily)), "tavily");
        assert_eq!(backend_name(Some(WebSearchBackend::Brave)), "brave");
        assert_eq!(backend_name(Some(WebSearchBackend::Custom)), "custom");
        assert_eq!(backend_name(None), "none");
    }
}
