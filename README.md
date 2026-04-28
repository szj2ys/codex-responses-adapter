# codex-responses-adapter

A local Rust proxy that lets [Codex CLI](https://github.com/openai/codex) talk to providers that expose an OpenAI-compatible Chat Completions API, even when Codex itself only speaks the Responses API.

It accepts `POST /v1/responses` from Codex, translates requests into `/v1/chat/completions` for downstream providers, and converts the result back into Responses-shaped responses. Model names are passed through unchanged — whatever Codex sends, the upstream receives.

## Install

```bash
brew tap szj2ys/codex
brew install codex-responses-adapter
```

Pre-built binaries are available for macOS (Intel + Apple Silicon) and Linux (x86_64 + ARM64). Homebrew installs the binary directly — no Rust toolchain needed.

To install from source:

```bash
cargo install --git https://github.com/szj2ys/codex-responses-adapter
```

## Setup

```bash
codex-responses-adapter setup
```

This walks you through creating `~/.codex-responses-adapter.toml` — just two questions:

```
Config path: ~/.codex-responses-adapter.toml

Base URL (e.g. https://open.bigmodel.cn/api/paas/v4):
> https://open.bigmodel.cn/api/paas/v4

API Key:
> ********

Config written. Run: codex-responses-adapter
```

That's it. The generated config is minimal:

```toml
[server]
allow_downgrade = true
port = 3000

[providers.default]
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "your-key"
provider_type = "custom"
```

## Run

```bash
codex-responses-adapter
```

The server listens on `127.0.0.1:3000`. Verify it's running:

```bash
curl http://127.0.0.1:3000/health
# {"status":"ok"}
```

## Codex Integration

Add the adapter as a provider in Codex config:

```toml
# ~/.codex/config.toml
model_provider = "responses-adapter"

[model_providers.responses-adapter]
name = "Responses Adapter"
base_url = "http://127.0.0.1:3000/v1"
wire_api = "responses"
env_key = "ADAPTER_KEY"
```

Then start Codex normally. The adapter must be running before Codex launches.

## CLI Mode

Without a config file, run in single-provider mode:

```bash
codex-responses-adapter \
  --base-url https://open.bigmodel.cn/api/paas/v4 \
  --api-key your-key
```

## Advanced Configuration

The minimal config from `setup` works for most users. For multi-provider routing, web search, or model mapping, edit `~/.codex-responses-adapter.toml` directly. See [codex-responses-adapter.example.toml](./codex-responses-adapter.example.toml) for a full example.

### Model Passthrough

By default, the adapter passes whatever model name Codex sends directly to the upstream provider — no conversion. If you want to remap model names (e.g. `o4-mini` → `glm-4-flash`), add a `[[models]]` section:

```toml
[[models]]
name = "o4-mini"
routes = [{ provider = "default", model = "glm-4-flash" }]
```

When `[[models]]` is absent, all model names pass through unchanged.

### Multi-Provider Routing

Define multiple providers and route requests with ordered fallback:

```toml
[providers.glm]
provider_type = "glm"
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "GLM_API_KEY"

[providers.minimax]
provider_type = "minimax"
base_url = "https://api.minimaxi.com/v1"
api_key_env = "MINIMAX_API_KEY"

[[models]]
name = "o4-mini"
routes = [
  { provider = "glm", model = "glm-4-flash" },
  { provider = "minimax", model = "MiniMax-Text-01" }
]
```

### Forcing a Specific Provider

Use `POST /v1/{provider}/responses` to bypass routing and pin requests to one provider. Example Codex config:

```toml
[model_providers.adapter-glm]
name = "Adapter GLM"
base_url = "http://127.0.0.1:3000/v1/glm"
wire_api = "responses"
env_key = "GLM_API_KEY"
```

### Web Search

Adapter-managed search via Tavily, Brave, or a custom backend:

```toml
[web_search]
enabled = true
strategy = "prefer_passthrough"
backend = "tavily"
max_results = 5
timeout_seconds = 10
allow_backend_fallback = true

[web_search.tavily]
api_key_env = "TAVILY_API_KEY"
```

For providers that support the Responses API natively (e.g. OpenAI), search requests are passed through directly.

## HTTP Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/responses` | Default adapter entrypoint |
| `POST` | `/v1/{provider}/responses` | Force a specific provider |
| `GET` | `/health` | Health check |

## Provider Types

| Type | Notes |
| --- | --- |
| `glm` | Standard Chat Completions translation |
| `minimax` | Merges multiple system messages into one for compatibility |
| `openai` | Supports native Responses API passthrough |
| `vllm` | Generic OpenAI-compatible; some features may require downgrade |
| `custom` | Most flexible preset, used by `setup` by default |

## Limitations

- `previous_response_id` is not supported
- Server-side conversation state is not implemented
- Hosted tools beyond web search are not emulated
- Some Responses features may be downgraded depending on provider capabilities
- Binds to `127.0.0.1` by default

## Development

```bash
cargo test
```

## License

[MIT](./LICENSE)
