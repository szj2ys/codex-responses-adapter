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

use clap::{Parser, Subcommand};
use providers::ProviderKind;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "codex-responses-adapter",
    about = "Translate Responses API to Chat Completions API for third-party LLMs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the adapter server.
    #[command(name = "run")]
    Run(RunArgs),
    /// Interactive setup: create ~/.codex-responses-adapter.toml.
    #[command(name = "setup")]
    Setup,
}

#[derive(Debug, Parser)]
struct RunArgs {
    /// Optional path to the TOML config file.
    ///
    /// If omitted, the adapter will automatically load
    /// ~/.codex-responses-adapter.toml when that file exists. CLI args
    /// (--base-url etc.) are ignored whenever a config file is loaded.
    #[arg(long)]
    config: Option<String>,

    // ---- CLI-mode args (used when --config is NOT set) ----
    /// Port to listen on.
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Base URL of the upstream Chat Completions API.
    #[arg(long)]
    base_url: Option<String>,

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

    let cli = Cli::parse();

    match cli.command {
        Command::Setup => run_setup().await,
        Command::Run(args) => run_server_with_args(args).await,
    }
}

async fn run_server_with_args(args: RunArgs) -> anyhow::Result<()> {
    let config_path = args
        .config
        .clone()
        .or_else(default_config_path_if_exists);

    let server_config = if let Some(config_path) = config_path {
        info!("loading config from: {config_path}");
        let adapter_config = config::AdapterConfig::load(&config_path)?;
        handler::ServerConfig::from_config(adapter_config)?
    } else {
        let base_url = args
            .base_url
            .ok_or_else(|| anyhow::anyhow!("--base-url is required when not using --config"))?;

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
            base_url,
            api_key,
            provider,
            args.allow_downgrade,
            model_map,
            args.default_model,
        )?
    };

    handler::run_server(server_config).await
}

// ---------------------------------------------------------------------------
// Setup subcommand
// ---------------------------------------------------------------------------

use std::io::{self, Write};

const DEFAULT_CONFIG_FILENAME: &str = ".codex-responses-adapter.toml";

async fn run_setup() -> anyhow::Result<()> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME env var not set"))?;
    let config_path = std::path::Path::new(&home).join(DEFAULT_CONFIG_FILENAME);

    println!("Config path: {}", config_path.display());

    if config_path.exists() {
        print!("Config already exists. Overwrite? [y/N]: ");
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        let response = response.trim().to_lowercase();
        if response != "y" && response != "yes" {
            println!("Skipped. Existing config: {}", config_path.display());
            return Ok(());
        }
    }

    print!("Base URL (e.g. https://open.bigmodel.cn/api/paas/v4): ");
    io::stdout().flush()?;
    let mut base_url = String::new();
    io::stdin().read_line(&mut base_url)?;
    let base_url = base_url.trim();
    if base_url.is_empty() {
        anyhow::bail!("Base URL is required");
    }

    print!("API Key: ");
    io::stdout().flush()?;
    let api_key = read_hidden_input()?;
    if api_key.trim().is_empty() {
        anyhow::bail!("API Key is required");
    }

    let config_content = format!(
        r#"[server]
allow_downgrade = true
port = 3000

[providers.default]
base_url = "{}"
api_key = "{}"
provider_type = "custom"
"#,
        base_url,
        api_key.trim()
    );

    std::fs::write(&config_path, config_content)?;
    println!("\nConfig written to {}", config_path.display());
    println!("Run: codex-responses-adapter");

    Ok(())
}

#[cfg(unix)]
fn read_hidden_input() -> io::Result<String> {
    use std::os::unix::io::AsRawFd;
    // Try to use /dev/tty for hidden input; fall back to plain stdin if not a TTY.
    match std::fs::File::open("/dev/tty") {
        Ok(tty) => {
            let fd = tty.as_raw_fd();
            let mut termios = unsafe {
                let mut t = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut t) != 0 {
                    // Not a TTY, fall through to plain read
                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    return Ok(input);
                }
                t
            };
            let original = termios;
            termios.c_lflag &= !libc::ECHO;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios); }

            let mut input = String::new();
            let result = io::stdin().read_line(&mut input);

            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original); }
            println!();
            result?;
            Ok(input)
        }
        Err(_) => {
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            Ok(input)
        }
    }
}
#[cfg(windows)]
fn read_hidden_input() -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    let handle = io::stdin().as_raw_handle();
    let mut mode: u32 = 0;
    unsafe {
        winapi::um::consoleapi::GetConsoleMode(handle, &mut mode);
        winapi::um::consoleapi::SetConsoleMode(handle, mode & !0x0004);
    }
    let mut input = String::new();
    let result = io::stdin().read_line(&mut input);
    unsafe {
        winapi::um::consoleapi::SetConsoleMode(handle, mode);
    }
    println!();
    result?;
    Ok(input)
}

#[cfg(not(any(unix, windows)))]
fn read_hidden_input() -> io::Result<String> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input)
}

fn default_config_path_if_exists() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(DEFAULT_CONFIG_FILENAME);
    path.is_file().then(|| path.to_string_lossy().into_owned())
}
