# Search Routing Design

## Summary

This document proposes a search-capable extension for `codex-responses-adapter`.

The adapter currently has a hard boundary:

- It accepts OpenAI `Responses API` requests from Codex.
- It converts them into downstream `Chat Completions API` requests.
- It drops hosted tools such as `web_search`.

That boundary is acceptable for plain chat and function tools, but it breaks current-information workflows. The design below adds a routing layer so the adapter can choose one of several search execution strategies:

1. Direct passthrough to downstream OpenAI `Responses API`
2. Adapter-managed built-in search via Tavily
3. Adapter-managed built-in search via Brave
4. Adapter-managed custom search API defined by the user

This design intentionally separates "search routing" from "provider routing". A downstream model provider decides where inference runs. A search backend decides how hosted search semantics are fulfilled.

## Goals

- Preserve native OpenAI `web_search` behavior when an OpenAI Responses-capable downstream is configured
- Support non-OpenAI downstream inference providers by letting the adapter execute search itself
- Support first-party built-ins for Tavily and Brave
- Support arbitrary user-defined search APIs without code changes for each provider
- Keep the existing non-search request path stable
- Avoid coupling the adapter core to Codex skills or browser automation internals

## Non-Goals

- Full browser automation in the first implementation
- Generic hosted-tool execution beyond search
- Multi-step search planning or iterative browsing
- Cross-request server-side conversation state

## Current State

Today the adapter has two behaviors that prevent search:

- `request_converter::convert_tools()` drops `web_search*` tool definitions
- `convert_request()` rejects `web_search_call` input items as unsupported hosted tools

As a result, a downstream OpenAI provider configured through this adapter still does not receive the original `Responses` search semantics, because the request is converted too early into `chat/completions`.

## Proposed Architecture

### 1. Two outbound protocols

The adapter should support two outbound modes per route:

- `responses_passthrough`
- `chat_translation`

The selected mode is not hardcoded by provider name. It is capability-driven.

Suggested capability flag:

```rust
pub struct ProviderCapabilities {
    pub supports_responses_api: bool,
    pub supports_tools: bool,
    pub supports_tool_choice_auto: bool,
    pub supports_parallel_tool_calls: bool,
    pub supports_streaming: bool,
    pub supports_system_role: bool,
    pub requires_single_leading_system_message: bool,
    pub max_context_tokens: Option<u32>,
}
```

### 2. Search strategy selection

For each incoming request, the adapter determines whether search is requested. If not, existing behavior remains unchanged.

If search is requested, the adapter selects one strategy:

1. `openai_passthrough`
2. `adapter_builtin_search`
3. `adapter_custom_search`

The strategy is chosen by config, not by heuristics.

### 3. Search backend abstraction

Adapter-managed search backends should implement a single internal trait:

```rust
pub struct SearchRequest {
    pub query: String,
    pub max_results: usize,
}

pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct SearchResponse {
    pub query: String,
    pub provider: String,
    pub results: Vec<SearchResultItem>,
}

#[async_trait::async_trait]
pub trait SearchBackend {
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse, AdapterError>;
}
```

This keeps handler logic independent from any specific backend.

## Search Strategy Design

### Option A: OpenAI passthrough

Recommended as the first search path.

If the chosen downstream route supports `Responses API`, and config allows passthrough for search-capable requests, the adapter must not translate the request into chat format. It should forward the original request body directly to:

- `POST {upstream}/responses`

This preserves:

- native `web_search`
- future hosted tools
- exact OpenAI tool semantics
- upstream streaming behavior

This is the cleanest option when the user has an OpenAI account login and wants native OpenAI search.

### Option B: Built-in Tavily / Brave

If passthrough is unavailable or disabled, the adapter converts `web_search` into an internal function tool, for example `__adapter_web_search`.

Flow:

1. Convert `web_search` tool definition to `__adapter_web_search`
2. Send translated tool to downstream chat provider
3. If model returns a function call to `__adapter_web_search`, execute the configured search backend
4. Inject search results as a tool response message
5. Call the model again to produce the final answer

This is the right path for GLM, MiniMax, vLLM, or any provider without native Responses hosted tools.

### Option C: Custom search API

This uses the same adapter-managed function-tool loop as Tavily / Brave, but the backend implementation is configured by the user.

The adapter performs one outbound HTTP request using configured method, URL, headers, and payload templates, then extracts a normalized list of results from the returned JSON.

This provides extensibility without requiring new code for each search vendor.

## Configuration Design

### Provider config

Add a new outbound protocol capability to providers:

```toml
[providers.openai]
name = "OpenAI Official"
upstream_url = "https://api.openai.com/v1"
provider_type = "openai"
use_incoming_auth = true
supports_responses_api = true
```

`supports_responses_api` should default to:

- `true` for `openai`
- `false` for `glm`, `minimax`, `vllm`, `custom`

This can still be overridden explicitly in config.

### Search config

Use one top-level search section with an explicit strategy:

```toml
[web_search]
enabled = true
strategy = "prefer_passthrough" # prefer_passthrough | force_backend
backend = "tavily"              # tavily | brave | custom
max_results = 5
timeout_seconds = 10
allow_backend_fallback = true
```

Built-in backends:

```toml
[web_search.tavily]
api_key_env = "TAVILY_API_KEY"

[web_search.brave]
api_key_env = "BRAVE_SEARCH_API_KEY"
```

Custom backend:

```toml
[web_search.custom]
url = "https://search.example.com/query"
method = "POST"
headers = { Authorization = "Bearer ${SEARCH_API_KEY}" }
body_template = "{\"query\":\"{{query}}\",\"limit\":{{max_results}}}"
results_path = "data.items"
title_path = "title"
url_path = "url"
snippet_path = "snippet"
```

## Routing Rules

### Detecting search

The request is considered search-capable if either condition is true:

- `tools` contains `web_search` or `web_search_preview`
- `input` contains a hosted search item such as `web_search_call`

### Decision order

Recommended order:

1. Request contains search
2. `web_search.enabled == true`
3. Current route supports `Responses API`
4. `web_search.strategy == "prefer_passthrough"`

If all are true:

- passthrough original request to downstream `/responses`

Else:

- use adapter-managed search backend

If search is requested but neither passthrough nor backend execution is available:

- return a clear 400 error explaining that search is requested but no configured search path exists

### Fallback behavior

Search requests need stricter fallback than plain chat requests.

Recommended behavior:

- If passthrough route fails and `allow_backend_fallback == true`, adapter may retry using adapter-managed backend on a chat provider
- If passthrough route fails and no backend is configured, do not silently drop search; fail explicitly
- Never downgrade a search request into a plain non-search request without a clear config opt-in

## Request Transformation Rules

### Passthrough path

No semantic transformation:

- preserve request body
- preserve tools
- preserve `tool_choice`
- preserve `parallel_tool_calls`
- preserve streaming

Only route-level mutations are allowed:

- upstream URL selection
- auth header selection

### Adapter-managed path

In `request_converter`:

- If `web_search` backend is enabled, convert `web_search*` tools into one internal function tool
- If backend is disabled, keep current drop behavior only when downgrade is allowed

Internal function definition:

```json
{
  "type": "function",
  "function": {
    "name": "__adapter_web_search",
    "description": "Search the web for current information.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string" }
      },
      "required": ["query"]
    }
  }
}
```

### Hosted call items

If the input already contains `web_search_call`, the adapter should not reject it unconditionally anymore.

Rules:

- passthrough mode: forward unchanged
- adapter-managed mode: treat it as unsupported for now unless the implementation also supports replaying prior hosted search turns

That limitation should be documented clearly.

## Custom Search Backend Model

The custom backend should be deliberately constrained to JSON APIs.

### Request templating

Support a minimal template system:

- `{{query}}`
- `{{max_results}}`

Headers and body can contain these placeholders.

### Response extraction

Support simple dotted field paths only:

- `results_path = "data.items"`
- `title_path = "title"`
- `url_path = "link"`
- `snippet_path = "summary"`

Do not introduce a full JSONPath engine in v1. The dotted-path approach is enough for most internal APIs and keeps parsing logic simple.

## Browser / Skill Integration

The adapter should not directly depend on Codex skills.

Reason:

- skills are session-oriented orchestration primitives
- the adapter is a long-running server process
- direct skill coupling makes testing, portability, and failure handling worse

If browser-backed search is needed later, add a separate executor backend with a stable process or HTTP boundary.

Possible future extension:

```toml
[web_search.executor]
kind = "command"
command = "agent-browser"
args = ["web-search", "--query", "{{query}}", "--limit", "{{max_results}}"]
format = "json"
```

This leaves room for browser search without hardwiring skill semantics into the adapter core.

## Handler Changes

### Existing path

Current handler flow is single-shot:

1. parse request
2. convert to chat
3. call upstream once
4. translate response

### New flow

Recommended handler flow:

1. parse `ResponsesApiRequest`
2. resolve candidate routes
3. detect whether request uses search
4. if route supports passthrough and strategy prefers passthrough:
   - forward raw request to `/responses`
5. otherwise:
   - convert search tool to internal function tool
   - execute adapter-managed search loop
   - return final response

Adapter-managed loop should remain bounded:

- max search rounds: 3
- backend timeout: configurable, default 10 seconds

## Error Handling

Clear user-visible errors matter here because silent degradation is the worst possible outcome for search.

Add explicit error cases:

- search requested but no passthrough or backend configured
- custom backend config invalid
- custom backend returned non-JSON response
- custom backend response extraction failed
- search backend timeout
- search backend auth missing

Recommended principle:

- fail closed for search semantics
- do not silently strip search unless downgrade is explicitly enabled

## Security Considerations

- Treat custom search URLs as trusted admin config, not user input
- Never let model output choose arbitrary backend URLs
- Enforce outbound timeout and result size caps
- Truncate snippets to a reasonable length before reinjecting into prompts
- Log backend type and timing, but never log secrets
- For custom headers, expand environment variables server-side only

## Testing Plan

### Unit tests

- detect search tools in request
- provider capability selects passthrough
- provider without `supports_responses_api` uses adapter-managed backend
- tool conversion for Tavily/Brave/custom-enabled mode
- custom backend dotted-path extraction
- error when search requested but no valid path exists

### Integration tests

- OpenAI passthrough request keeps `web_search` unchanged
- Tavily backend converts to `__adapter_web_search` and completes a second model round
- Brave backend parsing normalizes results correctly
- custom backend request templating and extraction work against a mock server
- fallback from passthrough to backend works only when enabled

## Implementation Phases

### Phase 1

- Add provider capability: `supports_responses_api`
- Add search config model
- Add passthrough route for search requests to OpenAI `/responses`

This phase restores native OpenAI search quickly.

### Phase 2

- Add adapter-managed Tavily backend
- Add adapter-managed Brave backend
- Add internal `__adapter_web_search` tool translation and handler loop

### Phase 3

- Add custom JSON search backend
- Add stronger validation and observability

### Phase 4

- Optional command/executor backend for browser-based search

## Key Decisions

### Decision 1: Prefer OpenAI passthrough when available

Reason:

- least semantic loss
- least code
- preserves native hosted tool behavior

Trade-off:

- search requests may route differently from non-search requests

### Decision 2: Use backend abstraction for all adapter-managed search

Reason:

- avoids conditionals spread across handler and converter
- makes Tavily, Brave, and custom API support symmetrical

Trade-off:

- requires a bit more up-front structure

### Decision 3: Keep custom backend limited to JSON plus dotted paths

Reason:

- enough flexibility for most APIs
- avoids DSL and parser complexity

Trade-off:

- not every arbitrary API shape will fit without preprocessing

## Recommendation

Implement this in the following order:

1. OpenAI passthrough for search requests
2. Tavily and Brave adapter-managed backend
3. Custom JSON backend
4. Optional executor backend for browser search

This sequence restores the most important user-facing capability first while keeping the longer-term architecture extensible.
