//! Configuration file support for the adapter proxy.
//!
//! When run with `--config <path>` or the default home config file, the adapter
//! reads a TOML file that defines upstream providers, model routing with
//! fallback, and server settings.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Top-level adapter configuration.
#[derive(Debug, Deserialize)]
pub struct AdapterConfig {
    #[serde(default)]
    pub server: ServerSection,

    /// Named upstream providers (e.g. "glm", "minimax").
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Model routing table.  Each entry maps a Codex model name to one or more
    /// upstream provider+model pairs (tried in order = fallback).
    #[serde(default)]
    pub models: Vec<ModelEntry>,

    /// Fallback route for model names not listed in `models`.
    pub default_route: Option<RouteTarget>,

    /// Hosted web search routing behavior.
    #[serde(default)]
    pub web_search: WebSearchConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub allow_downgrade: bool,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            port: default_port(),
            allow_downgrade: false,
        }
    }
}

fn default_port() -> u16 {
    3000
}

/// Configuration for a single upstream provider.
#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub upstream_url: String,
    /// Environment variable name containing the API key.
    pub api_key_env: Option<String>,
    /// Direct API key value (not recommended).
    pub api_key: Option<String>,
    /// Provider type for capability presets: "glm", "minimax", "vllm", "custom".
    #[serde(default = "default_provider_type")]
    pub provider_type: String,
    /// When true, forward the bearer token from the incoming Codex request
    /// instead of using api_key/api_key_env.  This lets Codex's OAuth login
    /// token pass through to the upstream provider (e.g. OpenAI).
    #[serde(default)]
    pub use_incoming_auth: bool,
    /// Override provider capability when the upstream supports `/responses`.
    #[serde(default)]
    pub supports_responses_api: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WebSearchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub strategy: WebSearchStrategy,
    #[serde(default)]
    pub backend: Option<WebSearchBackend>,
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    #[serde(default = "default_web_search_timeout_seconds")]
    pub timeout_seconds: u64,
    #[allow(dead_code)]
    #[serde(default)]
    pub allow_backend_fallback: bool,
    #[serde(default)]
    pub tavily: SearchBackendAuthConfig,
    #[serde(default)]
    pub brave: SearchBackendAuthConfig,
    #[serde(default)]
    pub custom: CustomSearchConfig,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strategy: WebSearchStrategy::PreferPassthrough,
            backend: None,
            max_results: default_web_search_max_results(),
            timeout_seconds: default_web_search_timeout_seconds(),
            allow_backend_fallback: false,
            tavily: SearchBackendAuthConfig::default(),
            brave: SearchBackendAuthConfig::default(),
            custom: CustomSearchConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchStrategy {
    #[default]
    PreferPassthrough,
    ForceBackend,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchBackend {
    Tavily,
    Brave,
    Custom,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SearchBackendAuthConfig {
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
}

impl SearchBackendAuthConfig {
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(env_var) = &self.api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                if !key.trim().is_empty() {
                    return Some(key);
                }
            }
        }
        self.api_key.clone()
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct CustomSearchConfig {
    pub url: Option<String>,
    #[serde(default = "default_custom_search_method")]
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub body_template: Option<String>,
    pub results_path: Option<String>,
    pub title_path: Option<String>,
    pub url_path: Option<String>,
    pub snippet_path: Option<String>,
}

fn default_custom_search_method() -> String {
    "POST".to_string()
}

fn default_web_search_max_results() -> usize {
    5
}

fn default_web_search_timeout_seconds() -> u64 {
    10
}

fn default_provider_type() -> String {
    "custom".to_string()
}

impl ProviderConfig {
    /// Resolve the API key: env var first, then direct value.
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(env_var) = &self.api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                if !key.trim().is_empty() {
                    return Some(key);
                }
            }
        }
        self.api_key.clone()
    }
}

/// A model routing entry: maps a Codex model name to upstream routes.
#[derive(Debug, Deserialize, Clone)]
pub struct ModelEntry {
    /// The model name that Codex sends (e.g. "o4-mini").
    pub name: String,
    /// Ordered list of route targets.  The adapter tries them in order;
    /// on upstream failure it falls through to the next.
    pub routes: Vec<RouteTarget>,
}

/// A single route target: which provider and model to use.
#[derive(Debug, Deserialize, Clone)]
pub struct RouteTarget {
    pub provider: String,
    pub model: String,
}

impl AdapterConfig {
    /// Load config from a TOML file.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: AdapterConfig = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
impl AdapterConfig {
    /// Look up routes for a given model name.
    /// Returns the model entry's routes if found, otherwise wraps default_route
    /// in a Vec, or returns an empty Vec.
    pub fn routes_for_model(&self, model_name: &str) -> Vec<RouteTarget> {
        for entry in &self.models {
            if entry.name == model_name {
                return entry.routes.clone();
            }
        }
        if let Some(default) = &self.default_route {
            return vec![default.clone()];
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[server]
port = 4000
allow_downgrade = true

[providers.glm]
name = "GLM"
upstream_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "test-key"
provider_type = "glm"

[providers.openai]
name = "OpenAI"
upstream_url = "https://api.openai.com/v1"
provider_type = "custom"
use_incoming_auth = true

[[models]]
name = "gpt-5.4"
routes = [
  { provider = "openai", model = "gpt-5.4" },
  { provider = "glm", model = "glm-4-plus" },
]

[default_route]
provider = "glm"
model = "glm-4-flash"
"#;
        let config: AdapterConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.port, 4000);
        assert!(config.server.allow_downgrade);
        assert!(!config.web_search.enabled);
        assert_eq!(config.web_search.max_results, 5);
        assert_eq!(config.providers.len(), 2);
        assert!(config.providers["openai"].use_incoming_auth);
        assert!(!config.providers["glm"].use_incoming_auth);
        assert_eq!(
            config.providers["glm"].api_key,
            Some("test-key".to_string())
        );
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.models[0].name, "gpt-5.4");
        assert_eq!(config.models[0].routes.len(), 2);
        assert_eq!(config.models[0].routes[0].provider, "openai");
        assert_eq!(config.models[0].routes[1].provider, "glm");
        assert!(config.default_route.is_some());
        assert_eq!(config.default_route.as_ref().unwrap().model, "glm-4-flash");
    }

    #[test]
    fn test_routes_for_model_explicit() {
        let config = AdapterConfig {
            server: ServerSection::default(),
            providers: HashMap::new(),
            models: vec![ModelEntry {
                name: "o4-mini".to_string(),
                routes: vec![
                    RouteTarget {
                        provider: "glm".to_string(),
                        model: "glm-4-flash".to_string(),
                    },
                    RouteTarget {
                        provider: "minimax".to_string(),
                        model: "MiniMax-Text-01".to_string(),
                    },
                ],
            }],
            default_route: Some(RouteTarget {
                provider: "glm".to_string(),
                model: "default-model".to_string(),
            }),
            web_search: WebSearchConfig::default(),
        };

        let routes = config.routes_for_model("o4-mini");
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].provider, "glm");
        assert_eq!(routes[0].model, "glm-4-flash");
        assert_eq!(routes[1].provider, "minimax");
    }

    #[test]
    fn test_routes_for_model_uses_default() {
        let config = AdapterConfig {
            server: ServerSection::default(),
            providers: HashMap::new(),
            models: vec![],
            default_route: Some(RouteTarget {
                provider: "glm".to_string(),
                model: "glm-4-flash".to_string(),
            }),
            web_search: WebSearchConfig::default(),
        };

        let routes = config.routes_for_model("unknown-model");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].provider, "glm");
        assert_eq!(routes[0].model, "glm-4-flash");
    }

    #[test]
    fn test_routes_for_model_empty_when_no_default() {
        let config = AdapterConfig {
            server: ServerSection::default(),
            providers: HashMap::new(),
            models: vec![],
            default_route: None,
            web_search: WebSearchConfig::default(),
        };

        let routes = config.routes_for_model("unknown-model");
        assert!(routes.is_empty());
    }

    #[test]
    fn test_server_defaults() {
        let toml_str = "[server]\n";
        let config: AdapterConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.port, 3000);
        assert!(!config.server.allow_downgrade);
        assert_eq!(
            config.web_search.strategy,
            WebSearchStrategy::PreferPassthrough
        );
    }

    #[test]
    fn test_provider_resolve_api_key_direct() {
        let pc = ProviderConfig {
            name: None,
            upstream_url: "http://example.com".to_string(),
            api_key_env: None,
            api_key: Some("direct-key".to_string()),
            provider_type: "custom".to_string(),
            use_incoming_auth: false,
            supports_responses_api: None,
        };
        assert_eq!(pc.resolve_api_key(), Some("direct-key".to_string()));
    }

    #[test]
    fn test_provider_resolve_api_key_none() {
        let pc = ProviderConfig {
            name: None,
            upstream_url: "http://example.com".to_string(),
            api_key_env: Some("NONEXISTENT_ENV_VAR_FOR_TEST".to_string()),
            api_key: None,
            provider_type: "custom".to_string(),
            use_incoming_auth: false,
            supports_responses_api: None,
        };
        assert_eq!(pc.resolve_api_key(), None);
    }

    #[test]
    fn test_use_incoming_auth_defaults_false() {
        let toml_str = r#"
[providers.test]
upstream_url = "http://example.com"
"#;
        let config: AdapterConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.providers["test"].use_incoming_auth);
        assert_eq!(config.providers["test"].supports_responses_api, None);
    }

    #[test]
    fn test_parse_web_search_config() {
        let toml_str = r#"
[web_search]
enabled = true
strategy = "force_backend"
backend = "custom"
max_results = 8
timeout_seconds = 15
allow_backend_fallback = true

[web_search.tavily]
api_key = "test-search-key"

[web_search.custom]
url = "https://search.example.com/query"
method = "POST"
results_path = "data.items"
title_path = "title"
url_path = "url"
snippet_path = "snippet"
"#;
        let config: AdapterConfig = toml::from_str(toml_str).unwrap();
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.strategy, WebSearchStrategy::ForceBackend);
        assert_eq!(config.web_search.backend, Some(WebSearchBackend::Custom));
        assert_eq!(config.web_search.max_results, 8);
        assert_eq!(config.web_search.timeout_seconds, 15);
        assert!(config.web_search.allow_backend_fallback);
        assert_eq!(
            config.web_search.tavily.resolve_api_key(),
            Some("test-search-key".to_string())
        );
        assert_eq!(
            config.web_search.custom.results_path.as_deref(),
            Some("data.items")
        );
    }
}
