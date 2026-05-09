//! codex-responses-adapter: translates OpenAI Responses API → Chat Completions API

mod config;
mod error;
mod handler;
mod providers;
mod request_converter;
mod response_converter;
mod tool_id_manager;
mod types;
mod web_search;

use clap::{Parser, Subcommand};
use providers::ProviderKind;
use std::collections::HashMap;
use std::io::{self, Write};
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_FILENAME: &str = ".codex-responses-adapter.toml";

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    name = "codex-responses-adapter",
    about = "Translate Responses API to Chat Completions API for third-party LLMs",
    subcommand_required = false,
)]
struct Cli {
    #[command(flatten)]
    args: ServerArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Interactive setup: create ~/.codex-responses-adapter.toml.
    Setup,
}

#[derive(Debug, Parser)]
struct ServerArgs {
    /// Host to bind to.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on.
    #[arg(long, default_value = "6789")]
    port: u16,

    /// Optional path to the TOML config file.
    #[arg(long)]
    config: Option<String>,

    /// Base URL of the upstream Chat Completions API.
    #[arg(long)]
    base_url: Option<String>,

    /// Target provider: glm, minimax, vllm, or custom.
    #[arg(long, default_value = "custom")]
    provider: String,

    /// Environment variable name containing the upstream API key.
    #[arg(long)]
    api_key_env: Option<String>,

    /// API key value directly.
    #[arg(long)]
    api_key: Option<String>,

    /// Allow capability downgrade when the provider lacks a feature.
    #[arg(long)]
    allow_downgrade: bool,

    /// Override the model name sent to the upstream API.
    #[arg(long)]
    default_model: Option<String>,

    /// Model name mapping (comma-separated key=value pairs).
    #[arg(long)]
    model_map: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Setup) => run_setup(),
        None => run_server(cli.args).await,
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

async fn run_server(args: ServerArgs) -> anyhow::Result<()> {
    let config_path = args.config.clone().or_else(default_config_path);

    let server_config = if let Some(path) = config_path {
        info!("loading config from: {path}");
        let adapter_config = config::AdapterConfig::load(&path)?;
        handler::ServerConfig::from_config(adapter_config)?
    } else {
        let base_url = args
            .base_url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--base-url is required when not using --config"))?;

        let api_key = resolve_api_key(&args);
        handler::ServerConfig::from_cli(
            args.host,
            args.port,
            base_url,
            api_key,
            ProviderKind::from_str(&args.provider),
            args.allow_downgrade,
            parse_model_map(&args.model_map),
            args.default_model,
        )?
    };

    handler::run_server(server_config).await
}

fn resolve_api_key(args: &ServerArgs) -> Option<String> {
    if let Some(env_var) = &args.api_key_env {
        let key = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
        if key.is_none() {
            tracing::warn!(
                "API key env var '{env_var}' is not set or empty; requests will be sent without auth"
            );
        }
        key
    } else {
        args.api_key.clone()
    }
}

fn parse_model_map(raw: &Option<String>) -> HashMap<String, String> {
    match raw.as_deref() {
        None | Some("") => HashMap::new(),
        Some(s) => s
            .split(',')
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?.trim();
                let val = parts.next()?.trim();
                if key.is_empty() || val.is_empty() {
                    None
                } else {
                    Some((key.to_string(), val.to_string()))
                }
            })
            .collect(),
    }
}

fn default_config_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(DEFAULT_CONFIG_FILENAME);
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn run_setup() -> anyhow::Result<()> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME env var not set"))?;
    let config_path = std::path::Path::new(&home).join(DEFAULT_CONFIG_FILENAME);

    println!("Config path: {}", config_path.display());

    if config_path.exists() {
        print!("Config already exists. Overwrite? [y/N]: ");
        io::stdout().flush()?;
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        if response.trim().to_lowercase() != "y" && response.trim().to_lowercase() != "yes" {
            println!("Skipped. Existing config: {}", config_path.display());
            return Ok(());
        }
    }

    let base_url = prompt_required("Base URL (e.g. https://open.bigmodel.cn/api/paas/v4): ");
    let api_key = prompt_hidden("API Key: ");

    let config = format!(
        r#"[server]
allow_downgrade = true
port = 6789

[providers.default]
base_url = "{base_url}"
api_key = "{api_key}"
provider_type = "custom"
"#,
    );

    std::fs::write(&config_path, &config)?;
    println!("\nConfig written to {}", config_path.display());
    println!("Run: codex-responses-adapter");

    Ok(())
}

fn prompt_required(label: &str) -> String {
    print!("{label}");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let value = input.trim().to_string();
    if value.is_empty() {
        eprintln!("Error: {label} is required");
        std::process::exit(1);
    }
    value
}

fn prompt_hidden(label: &str) -> String {
    print!("{label}");
    io::stdout().flush().unwrap();
    let input = read_hidden_input().unwrap_or_else(|_| {
        let mut fallback = String::new();
        io::stdin().read_line(&mut fallback).unwrap();
        fallback
    });
    let value = input.trim().to_string();
    if value.is_empty() {
        eprintln!("Error: {label} is required");
        std::process::exit(1);
    }
    value
}

// ---------------------------------------------------------------------------
// Hidden input
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn read_hidden_input() -> io::Result<String> {
    use std::os::unix::io::AsRawFd;
    match std::fs::File::open("/dev/tty") {
        Ok(tty) => {
            let fd = tty.as_raw_fd();
            let mut termios = unsafe {
                let mut t = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut t) != 0 {
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
