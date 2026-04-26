//! codex-responses-adapter: translates OpenAI Responses API → Chat Completions API
//!
//! This allows Codex CLI (which only speaks Responses API) to work with
//! third-party LLMs like GLM and MiniMax that expose Chat Completions
//! compatible endpoints.
//!
//! Reference: docs/specs/2026-03-11-search-routing.md

mod config;
mod error;
mod handler;
mod providers;
mod request_converter;
mod response_converter;
mod types;
mod web_search;

use clap::Parser;
use providers::ProviderKind;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Translate Responses API to Chat Completions API for third-party LLMs.
///
/// Two modes:
///   1. Config file: --config <path> or ~/.codex-responses-adapter.toml
///   2. CLI args:    --upstream-url ... --provider glm (single provider)
#[derive(Debug, Parser)]
#[command(
    name = "codex-responses-adapter",
    about = "Translate Responses API to Chat Completions API for third-party LLMs"
)]
struct Args {
    /// Optional path to the TOML config file.
    ///
    /// If omitted, the adapter will automatically load
    /// ~/.codex-responses-adapter.toml when that file exists. CLI args
    /// (--upstream-url etc.) are ignored whenever a config file is loaded.
    #[arg(long)]
    config: Option<String>,

    // ---- CLI-mode args (used when --config is NOT set) ----
    /// Port to listen on.
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Base URL of the upstream Chat Completions API.
    #[arg(long)]
    upstream_url: Option<String>,

    /// Target provider: glm, minimax, vllm, or custom.
    #[arg(long, default_value = "custom")]
    provider: String,

    /// Environment variable name containing the upstream API key.
    #[arg(long)]
    api_key_env: Option<String>,

    /// API key value directly (use --api-key-env for better security).
    #[arg(long)]
    api_key: Option<String>,

    /// Allow capability downgrade when the provider lacks a feature.
    #[arg(long)]
    allow_downgrade: bool,

    /// Override the model name sent to the upstream API (fallback for unmapped models).
    #[arg(long)]
    default_model: Option<String>,

    /// Model name mapping (comma-separated key=value pairs).
    /// Example: --model-map "o4-mini=glm-4-flash,o3=glm-4"
    #[arg(long)]
    model_map: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let config_path = args
        .config
        .clone()
        .or_else(default_config_path_if_exists);

    let server_config = if let Some(config_path) = config_path {
        // ---- Config file mode ----
        info!("loading config from: {config_path}");
        let adapter_config = config::AdapterConfig::load(&config_path)?;
        handler::ServerConfig::from_config(adapter_config)?
    } else {
        // ---- CLI mode ----
        let upstream_url = args
            .upstream_url
            .ok_or_else(|| anyhow::anyhow!("--upstream-url is required when not using --config"))?;

        // Resolve API key: prefer env var, fall back to direct value.
        let api_key = if let Some(env_var) = &args.api_key_env {
            let key = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
            if key.is_none() {
                tracing::warn!(
                    "API key env var '{env_var}' is not set or empty; requests will be sent without auth"
                );
            }
            key
        } else {
            args.api_key.clone()
        };

        let provider = ProviderKind::from_str(&args.provider);

        // Parse model map
        let model_map: std::collections::HashMap<String, String> = args
            .model_map
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?.trim().to_string();
                let val = parts.next()?.trim().to_string();
                if key.is_empty() || val.is_empty() {
                    None
                } else {
                    Some((key, val))
                }
            })
            .collect();

        handler::ServerConfig::from_cli(
            args.port,
            upstream_url,
            api_key,
            provider,
            args.allow_downgrade,
            model_map,
            args.default_model,
        )?
    };

    handler::run_server(server_config).await
}

fn default_config_path_if_exists() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(".codex-responses-adapter.toml");
    path.is_file().then(|| path.to_string_lossy().into_owned())
}
