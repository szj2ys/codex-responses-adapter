# codex-responses-adapter

`codex-responses-adapter` is a local Rust proxy that lets Codex talk to providers that expose an OpenAI-compatible Chat Completions API, even when Codex itself only speaks the OpenAI Responses API.

It accepts `POST /v1/responses` from Codex, translates requests into `/chat/completions` for downstream providers such as GLM, MiniMax, vLLM, or custom OpenAI-compatible endpoints, and converts the result back into a Responses-shaped response.

## Why This Exists

Codex CLI expects a Responses API endpoint. Many third-party model providers still expose Chat Completions instead. This adapter bridges that mismatch so you can keep `wire_api = "responses"` in Codex while routing inference to non-OpenAI backends.

## Features

- Responses API to Chat Completions translation
- Streaming and non-streaming response conversion
- Function tool round-trip support
- Model name mapping from Codex model IDs to upstream model IDs
- Multi-provider routing with ordered fallback
- Provider-specific path override via `POST /v1/{provider}/responses`
- Optional bearer-token passthrough with `use_incoming_auth = true`
- Provider capability handling, including downgrade behavior for unsupported features
- `<think>...</think>` filtering in both normal and streaming responses
- Adapter-managed `web_search` via Tavily, Brave, or a custom backend
- Native `/responses` passthrough for providers that already support the Responses API

## Project Status

This is a working adapter for the current MVP and early extension scope. The repository currently includes unit coverage for request conversion, response conversion, search routing, and config parsing.

As of March 11, 2026, `cargo test` passes with 37 tests.

## Quick Start

### 1. Build

```bash
cargo build --release
```

### 2. Create a config file

```bash
cp codex-responses-adapter.example.toml ~/.codex-responses-adapter.toml
```

Edit `~/.codex-responses-adapter.toml` to point at your upstream providers and model mappings.

### 3. Export provider credentials

Example:

```bash
export GLM_API_KEY="your-glm-key"
export MINIMAX_API_KEY="your-minimax-key"
export TAVILY_API_KEY="your-tavily-key"
```

### 4. Start the adapter

```bash
./target/release/codex-responses-adapter
```

The server listens on `127.0.0.1:3000` by default.

### 5. Verify health

```bash
curl http://127.0.0.1:3000/health
```

Expected response:

```json
{"status":"ok"}
```

## Codex Integration

Point Codex at the adapter instead of a provider directly:

```toml
# ~/.codex/config.toml
model_provider = "glm-adapter"

[model_providers.glm-adapter]
name = "GLM Adapter"
base_url = "http://127.0.0.1:3000/v1"
wire_api = "responses"
env_key = "GLM_API_KEY"
```

The adapter then maps Codex model names to your configured upstream provider/model pairs.

## Using With Codex

### Startup Order

1. Start `codex-responses-adapter`
2. Confirm `GET /health` returns `{"status":"ok"}`
3. Start Codex

If the adapter is not running first, Codex will fail on the initial request to `http://127.0.0.1:3000/v1`.

### Recommended Codex Profile Setup

If you keep multiple providers in Codex, create a dedicated profile for the adapter:

```toml
# ~/.codex/config.toml
model_provider = "responses-adapter"

[model_providers.responses-adapter]
name = "Responses Adapter"
base_url = "http://127.0.0.1:3000/v1"
wire_api = "responses"
env_key = "GLM_API_KEY"
```

`env_key` is still required by Codex config, but in practice upstream authentication is handled by the adapter config:

- `api_key_env` or `api_key` for provider-owned credentials
- `use_incoming_auth = true` when you want to pass through the bearer token from Codex

### How Model Names Map

Codex sends a model name such as `o4-mini`. The adapter uses `[[models]]` in `~/.codex-responses-adapter.toml` to choose the actual upstream route.

Example:

```toml
[[models]]
name = "o4-mini"
routes = [
  { provider = "glm", model = "glm-4-flash" },
  { provider = "minimax", model = "MiniMax-Text-01" }
]
```

That means:

- Codex asks for `o4-mini`
- the adapter tries `glm/glm-4-flash` first
- if that route fails, it falls back to `minimax/MiniMax-Text-01`

### Running Codex Commands

Once the adapter is running, normal Codex usage stays the same.

Interactive session:

```bash
codex
```

One-shot execution:

```bash
codex exec "Say hello in one sentence"
```

If your local Codex setup uses profiles, call the adapter profile explicitly:

```bash
codex --profile responses-adapter
codex exec --profile responses-adapter "Summarize the purpose of this repository"
```

### Forcing a Specific Provider

The default Codex path is `POST /v1/responses`, which follows the adapter's routing table.

If you want to pin requests to one provider for debugging, create a separate Codex provider entry that points directly at:

- `http://127.0.0.1:3000/v1/glm/responses`
- `http://127.0.0.1:3000/v1/minimax/responses`

Example:

```toml
[model_providers.responses-adapter-glm]
name = "Responses Adapter GLM"
base_url = "http://127.0.0.1:3000/v1/glm"
wire_api = "responses"
env_key = "GLM_API_KEY"
```

This bypasses model-level fallback and forces the named adapter provider.

### Search Behavior In Codex

When Codex triggers `web_search`, the adapter chooses one of two behaviors:

- passthrough to upstream `/responses` if the selected provider supports the Responses API
- adapter-managed search if `[web_search]` is enabled and a backend is configured

If neither path is available, the request fails clearly instead of silently dropping search.

## Minimal Config Example

```toml
[server]
port = 3000
allow_downgrade = true

[providers.glm]
provider_type = "glm"
upstream_url = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "GLM_API_KEY"

[providers.minimax]
provider_type = "minimax"
upstream_url = "https://api.minimaxi.com/v1"
api_key_env = "MINIMAX_API_KEY"

[[models]]
name = "o4-mini"
routes = [
  { provider = "glm", model = "glm-4-flash" },
  { provider = "minimax", model = "MiniMax-Text-01" }
]

[default_route]
provider = "glm"
model = "glm-4-flash"
```

For a fuller example, see [codex-responses-adapter.example.toml](./codex-responses-adapter.example.toml).

## CLI Mode

If you do not want a config file, the adapter can run in single-provider mode:

```bash
codex-responses-adapter \
  --upstream-url https://open.bigmodel.cn/api/paas/v4 \
  --provider glm \
  --api-key-env GLM_API_KEY \
  --default-model glm-4-flash \
  --model-map "o4-mini=glm-4-flash,o3=glm-4"
```

## HTTP Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/responses` | Default adapter entrypoint |
| `POST` | `/v1/{provider}/responses` | Force a specific configured provider |
| `GET` | `/health` | Health check |

## Search Routing

The adapter supports two search paths:

1. Native passthrough to upstream `POST /responses` when the selected provider supports the Responses API.
2. Adapter-managed search when `web_search` is enabled and a backend is configured.

Supported adapter-managed backends:

- Tavily
- Brave
- Custom JSON API

Example:

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

## Provider Notes

- `glm`: chat translation path by default
- `minimax`: merges developer/system text into one leading system message for compatibility
- `openai`: can be configured for direct Responses passthrough with `supports_responses_api = true`
- `vllm`: supported as a generic OpenAI-compatible downstream, but some capabilities may require downgrade
- `custom`: use when the upstream is OpenAI-compatible but not covered by a preset

## Current Limitations

- `previous_response_id` is not supported
- Server-side conversation state is not implemented
- Hosted tools are not generically emulated; only the current web-search path has adapter support
- Some Responses features may be dropped or downgraded depending on downstream provider capabilities
- The adapter binds to `127.0.0.1` by default rather than all interfaces

## Troubleshooting

### The adapter starts but Codex requests fail

Check:

- your upstream `upstream_url`
- your API key environment variables
- your model mapping names
- whether the selected provider supports the requested feature set

### Search requests fail

Check:

- `[web_search].enabled = true`
- a valid `backend` is configured
- the corresponding backend API key is present
- whether your route should use passthrough or adapter-managed search

### I want to use the upstream bearer token instead of a local API key

Set:

```toml
[providers.openai]
provider_type = "openai"
upstream_url = "https://api.openai.com/v1"
use_incoming_auth = true
supports_responses_api = true
```

## Development

```bash
cargo test
```

Key files:

- [src/main.rs](./src/main.rs)
- [src/handler.rs](./src/handler.rs)
- [src/request_converter.rs](./src/request_converter.rs)
- [src/response_converter.rs](./src/response_converter.rs)
- [src/web_search.rs](./src/web_search.rs)
- [docs/specs/2026-03-11-responses-adapter-design.md](./docs/specs/2026-03-11-responses-adapter-design.md)
- [docs/specs/2026-03-11-search-routing.md](./docs/specs/2026-03-11-search-routing.md)

## License

This repository is licensed under the [MIT License](./LICENSE).
