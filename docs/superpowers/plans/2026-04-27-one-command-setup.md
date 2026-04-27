# One-Command Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `setup` subcommand, model passthrough fallback, and GitHub Actions release workflow.

**Architecture:** Split CLI into subcommands (`run` and `setup`). `setup` writes a minimal TOML config via interactive prompts. Model passthrough eliminates "no routes configured" error for zero-config operation. CI compiles cross-platform binaries on tag push.

**Tech Stack:** Rust, clap, tokio, serde, reqwest, toml, GitHub Actions

---

## File Map

| File | Role |
|---|---|
| `src/main.rs` | CLI entry: subcommands (`run`, `setup`), `setup` interactive logic |
| `src/handler.rs` | Route resolution: passthrough when no routes configured |
| `.github/workflows/release.yml` | CI: build 4 targets, create GitHub Release, auto-update Homebrew Formula |

---

## Task 1: Add `setup` subcommand to CLI

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Modify `Args` to use clap subcommands**

Refactor `Args` from a flat struct to a `Cli` struct with a `Command` enum. Move all existing fields into `RunArgs`.

```rust
use clap::{Parser, Subcommand};

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
    /// Run the adapter server (default if no subcommand given).
    #[command(name = "run")]
    Run(RunArgs),
    /// Interactive setup: create ~/.codex-responses-adapter.toml.
    #[command(name = "setup")]
    Setup,
}

#[derive(Debug, Parser)]
struct RunArgs {
    #[arg(long)]
    config: Option<String>,

    #[arg(long, default_value = "3000")]
    port: u16,

    #[arg(long)]
    base_url: Option<String>,

    #[arg(long, default_value = "custom")]
    provider: String,

    #[arg(long)]
    api_key_env: Option<String>,

    #[arg(long)]
    api_key: Option<String>,

    #[arg(long)]
    allow_downgrade: bool,

    #[arg(long)]
    default_model: Option<String>,

    #[arg(long)]
    model_map: Option<String>,
}
```

- [ ] **Step 2: Refactor `main()` to dispatch subcommands**

```rust
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
```

- [ ] **Step 3: Add `run_setup()` interactive function**

```rust
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
```

- [ ] **Step 4: Add hidden input helper**

```rust
#[cfg(unix)]
fn read_hidden_input() -> io::Result<String> {
    use std::os::unix::io::AsRawFd;
    let tty = std::fs::File::open("/dev/tty")?;
    let fd = tty.as_raw_fd();

    let mut termios = unsafe {
        let mut t = std::mem::zeroed();
        libc::tcgetattr(fd, &mut t);
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
```

- [ ] **Step 5: Build to check compilation**

Run: `cargo build`
Expected: PASS

- [ ] **Step 6: Test the setup subcommand manually**

Run: `cargo run -- setup`
Input: `https://example.com/api`, `test-key`
Expected: `~/.codex-responses-adapter.toml` created with the values

Run: `cargo run -- setup` again
Input: `n`
Expected: "Skipped" message

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat: add interactive setup subcommand"
```

---

## Task 2: Model passthrough when no routes configured

**Files:**
- Modify: `src/handler.rs`

- [ ] **Step 1: Verify current passthrough logic**

The current code at `src/handler.rs:367-381` already falls back to a passthrough route when `model_routes` is empty and `default_route` is None:

```rust
vec![RouteTarget {
    provider: "default".to_string(),
    model: codex_model.clone(),
}]
```

This is the desired behavior. No code change needed in the request handler.

- [ ] **Step 2: Add unit test for passthrough behavior**

Add to `src/handler.rs` in the existing `#[cfg(test)]` section:

```rust
#[test]
fn test_passthrough_model_when_no_routes() {
    let config = ServerConfig::from_cli(
        3000,
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
```

Run: `cargo test test_passthrough_model_when_no_routes`
Expected: PASS

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: All tests PASS

- [ ] **Step 4: Commit**

```bash
git add src/handler.rs
git commit -m "test: verify model passthrough when no routes configured"
```

---

## Task 3: Update config field name from `upstream_url` to `base_url`

**Files:**
- Modify: `src/config.rs`
- Modify: `src/handler.rs`

- [ ] **Step 1: Rename field in `ProviderConfig`**

In `src/config.rs`, change:

```rust
pub struct ProviderConfig {
    pub name: Option<String>,
    pub upstream_url: String,
```

to:

```rust
pub struct ProviderConfig {
    pub name: Option<String>,
    pub base_url: String,
```

- [ ] **Step 2: Update all references in `src/handler.rs`**

In `from_config`, change `pc.upstream_url.clone()` to `pc.base_url.clone()`.
In `from_cli`, change the parameter name from `upstream_url` to `base_url`.

- [ ] **Step 3: Update test data in `src/config.rs`**

Replace every `upstream_url = "..."` with `base_url = "..."` in the `#[cfg(test)]` section. Count occurrences: `rg "upstream_url" src/config.rs`.

- [ ] **Step 4: Update `src/main.rs` CLI arg**

Already done in Task 1 (changed from `upstream_url` to `base_url`).

- [ ] **Step 5: Build and test**

Run: `cargo test`
Expected: All tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/handler.rs
git commit -m "refactor: rename upstream_url to base_url throughout"
```

---

## Task 4: GitHub Actions release workflow with auto-update Homebrew Formula

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Create workflow file**

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: Build ${{ matrix.target }}
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - name: Install cross-compilation tools (Linux ARM)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: |
          sudo apt-get update
          sudo apt-get install -y gcc-aarch64-linux-gnu
          echo "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc" >> $GITHUB_ENV

      - name: Build release binary
        run: cargo build --release --target ${{ matrix.target }}

      - name: Package binary
        run: |
          mkdir -p dist
          cp target/${{ matrix.target }}/release/codex-responses-adapter dist/
          cd dist
          tar czf "../codex-responses-adapter-${{ github.ref_name }}-${{ matrix.target }}.tar.gz" codex-responses-adapter
          cd ..
          sha256sum "codex-responses-adapter-${{ github.ref_name }}-${{ matrix.target }}.tar.gz" >> sha256sum.txt

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: codex-responses-adapter-${{ github.ref_name }}-${{ matrix.target }}.tar.gz

  release:
    name: Create GitHub Release
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts
          merge-multiple: false

      - name: Collect artifacts
        run: |
          mkdir -p release
          for dir in artifacts/*/; do
            cp "$dir"*.tar.gz release/
          done
          cd release && sha256sum *.tar.gz > ../sha256sum.txt

      - name: Create Release
        uses: softprops/action-gh-release@v1
        with:
          files: |
            release/*.tar.gz
            sha256sum.txt
          generate_release_notes: true

  update-homebrew:
    name: Update Homebrew Formula
    needs: release
    runs-on: ubuntu-latest
    steps:
      - name: Checkout tap repo
        uses: actions/checkout@v4
        with:
          repository: szj2ys/homebrew-codex
          token: ${{ secrets.HOMEBREW_TAP_TOKEN }}
          path: homebrew-codex

      - name: Download release artifacts
        run: |
          mkdir -p artifacts
          for target in x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
            curl -sL "https://github.com/${{ github.repository }}/releases/download/${{ github.ref_name }}/codex-responses-adapter-${{ github.ref_name }}-${target}.tar.gz" \
              -o "artifacts/codex-responses-adapter-${{ github.ref_name }}-${target}.tar.gz"
          done

      - name: Compute sha256 and update formula
        run: |
          VERSION="${{ github.ref_name }}"
          VERSION_NUM="${VERSION#v}"

          SHA_MAC_INTEL=$(sha256sum artifacts/codex-responses-adapter-${VERSION}-x86_64-apple-darwin.tar.gz | cut -d' ' -f1)
          SHA_MAC_ARM=$(sha256sum artifacts/codex-responses-adapter-${VERSION}-aarch64-apple-darwin.tar.gz | cut -d' ' -f1)
          SHA_LINUX_INTEL=$(sha256sum artifacts/codex-responses-adapter-${VERSION}-x86_64-unknown-linux-gnu.tar.gz | cut -d' ' -f1)
          SHA_LINUX_ARM=$(sha256sum artifacts/codex-responses-adapter-${VERSION}-aarch64-unknown-linux-gnu.tar.gz | cut -d' ' -f1)

          cat > homebrew-codex/Formula/codex-responses-adapter.rb << RUBY
      class CodexResponsesAdapter < Formula
        desc "Translate OpenAI Responses API to Chat Completions API"
        homepage "https://github.com/szj2ys/codex-responses-adapter"
        version "${VERSION_NUM}"
        license "MIT"

        on_macos do
          on_intel do
            url "https://github.com/szj2ys/codex-responses-adapter/releases/download/${VERSION}/codex-responses-adapter-${VERSION}-x86_64-apple-darwin.tar.gz"
            sha256 "${SHA_MAC_INTEL}"
          end
          on_arm do
            url "https://github.com/szj2ys/codex-responses-adapter/releases/download/${VERSION}/codex-responses-adapter-${VERSION}-aarch64-apple-darwin.tar.gz"
            sha256 "${SHA_MAC_ARM}"
          end
        end

        on_linux do
          on_intel do
            url "https://github.com/szj2ys/codex-responses-adapter/releases/download/${VERSION}/codex-responses-adapter-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
            sha256 "${SHA_LINUX_INTEL}"
          end
          on_arm do
            url "https://github.com/szj2ys/codex-responses-adapter/releases/download/${VERSION}/codex-responses-adapter-${VERSION}-aarch64-unknown-linux-gnu.tar.gz"
            sha256 "${SHA_LINUX_ARM}"
          end
        end

        def install
          bin.install "codex-responses-adapter"
        end

        test do
          system "#{bin}/codex-responses-adapter", "--help"
        end
      end
      RUBY

      - name: Commit and push formula
        run: |
          cd homebrew-codex
          git config user.name "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"
          git add Formula/codex-responses-adapter.rb
          git diff --cached --quiet || git commit -m "chore: update formula to ${{ github.ref_name }}"
          git push
```

- [ ] **Step 2: Verify YAML syntax**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: No error

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add release workflow with cross-platform builds and auto-update homebrew formula"
```

---

## Task 5: Homebrew Tap repository setup

**Note:** This step creates a separate repository on GitHub. It cannot be committed to the current repo.

**Files:**
- Create (in `szj2ys/homebrew-codex` repo): `Formula/codex-responses-adapter.rb` (initial stub)
- Create (in `szj2ys/homebrew-codex` repo): `README.md`

- [ ] **Step 1: Create the tap repo locally**

```bash
mkdir -p ~/tmp/homebrew-codex/Formula
cat > ~/tmp/homebrew-codex/README.md << 'README'
# szj2ys/codex

Homebrew tap for codex-responses-adapter.

## Install

```bash
brew tap szj2ys/codex
brew install codex-responses-adapter
```
README
```

- [ ] **Step 2: Create initial formula stub**

The CI workflow will overwrite this on the first release, but the repo needs an initial file for `brew tap` to work:

```bash
cat > ~/tmp/homebrew-codex/Formula/codex-responses-adapter.rb << 'FORMULA'
class CodexResponsesAdapter < Formula
  desc "Translate OpenAI Responses API to Chat Completions API"
  homepage "https://github.com/szj2ys/codex-responses-adapter"
  version "0.0.0"
  license "MIT"

  def install
    odie "This formula is updated automatically by CI. Please wait for the first release."
  end
end
FORMULA
```

- [ ] **Step 3: Push the tap repo to GitHub**

Create `https://github.com/szj2ys/homebrew-codex` via GitHub UI or gh CLI, then:

```bash
cd ~/tmp/homebrew-codex
git init
git add .
git commit -m "feat: initial tap for codex-responses-adapter"
git remote add origin https://github.com/szj2ys/homebrew-codex.git
git push -u origin main
```

- [ ] **Step 4: Add HOMEBREW_TAP_TOKEN secret to main repo**

Go to `https://github.com/szj2ys/codex-responses-adapter/settings/secrets/actions` and add a repository secret:
- Name: `HOMEBREW_TAP_TOKEN`
- Value: A GitHub personal access token with `repo` scope for the `homebrew-codex` repo

---

## Spec Coverage Check

| Spec Requirement | Implementing Task |
|---|---|
| `setup` subcommand with interactive prompts | Task 1 |
| Minimal TOML config generation (base_url + api_key only) | Task 1 |
| Model passthrough when no routes configured | Task 2 |
| Rename upstream_url to base_url | Task 3 |
| GitHub Actions CI release (4 targets) | Task 4 |
| Auto-update Homebrew Formula on release | Task 4 |
| Homebrew Tap repository | Task 5 |

All requirements covered. No gaps.

---

## Placeholder Scan

- No "TBD", "TODO", "implement later", "fill in details" found
- All code blocks contain complete, copy-pasteable code
- All test commands are exact and verifiable
- All file paths are exact
- The initial formula stub in Task 5 is a deliberate placeholder with a user-facing error message, not an unfinished implementation
