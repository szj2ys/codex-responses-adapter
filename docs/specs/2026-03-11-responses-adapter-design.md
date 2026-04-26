# Responses -> Chat API Adapter Design Document

## 1. Overview

### 1.1 Background

Codex CLI currently supports only `wire_api = "responses"` and has removed native support for the Chat API (`wire_api = "chat"`). However, the mainstream integration path for providers such as GLM and MiniMax is still an OpenAI Chat Completions compatible interface.

This document describes the detailed design of an adapter layer between Codex and third-party Chat APIs, allowing Codex to call backend models that do not support the Responses protocol through the Responses API.

### 1.2 Design Goals

- **MVP phase**: validate protocol conversion feasibility and establish clear boundaries
- **v1.x phase**: support tool calls and multi-turn conversations
- **v2.x phase**: fully emulate the Responses tool ecosystem

### 1.4 Current Implementation Notes

The current implementation has already been split out of the main `codex` repository and is now maintained as a standalone project at:

```text
codex-responses-adapter
```

The current executable name is `codex-responses-adapter`.

### 1.3 Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Codex CLI                              │
│                    (wire_api = "responses")                    │
└─────────────────────┬───────────────────────────────────────────┘
                      │ POST /v1/responses
                      ▼
┌─────────────────────────────────────────────────────────────────┐
│              Responses -> Chat Adapter (Rust)                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐ │
│  │  Handler    │  │  Request    │  │  Response               │ │
│  │  (axum)     │──│  Converter  │──│  Converter              │ │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘ │
│          │               │                                      │
│          ▼               ▼                                      │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │            Provider Capability Registry                    │ │
│  │         (GLM / MiniMax / vLLM / Custom)                    │ │
│  └─────────────────────────────────────────────────────────────┘ │
└─────────────────────┬───────────────────────────────────────────┘
                      │ POST /v1/chat/completions
                      ▼
┌─────────────────────────────────────────────────────────────────┐
│         Third-Party Models (GLM / MiniMax / Others)            │
│             (OpenAI Chat API Compatible)                       │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. Core Type Definitions

### 2.1 Upstream Types (Codex -> Adapter)

```rust
// Responses API request from Codex
struct ResponsesRequest {
    model: String,
    instructions: String,           // system instructions
    input: Vec<ResponseItem>,       // input messages / reasoning items
    tools: Vec<Value>,              // tool definitions
    tool_choice: String,            // tool selection strategy
    parallel_tool_calls: bool,      // whether to allow parallel calls
    reasoning: Option<Value>,       // reasoning configuration
    stream: bool,                   // whether to stream
    previous_response_id: Option<String>, // previous response ID
}

enum ResponseItem {
    Message { role, content },      // message item
    Reasoning { id, summary },      // reasoning item
    FunctionCall { name, arguments },  // function call request
    FunctionCallOutput { call_id, output }, // function call result
    WebSearchCall,                  // web search (unsupported)
    FileSearchCall,                 // file search (unsupported)
    ComputerCall,                   // computer control (unsupported)
}
```

### 2.2 Downstream Types (Adapter -> Chat API)

```rust
// Chat API request sent to GLM / MiniMax
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<Value>>,      // modern tools semantics
    tool_choice: Option<Value>,     // {"type": "auto"|"none"|{"type":"function",...}}
    stream: bool,
    // extra_body: HashMap,         // provider-specific parameters
}

struct ChatMessage {
    role: String,                   // system / user / assistant / tool
    content: String,
    name: Option<String>,           // used for tool messages
}
```

### 2.3 Return Types (Adapter -> Codex)

```rust
struct ResponsesResponse {
    id: String,
    object: String,                 // "response"
    created: u64,
    model: String,
    output: Vec<ResponseItemOutput>,
    usage: Option<Value>,
}

enum ResponseItemOutput {
    Message { id, role, content },
    // ...
}

enum ContentItemOutput {
    OutputText { text },
}
```

---

## 3. Conversion Rules

### 3.1 Request Conversion

| Responses field | Chat field | Conversion rule |
|----------------|------------|-----------------|
| `instructions` | `messages[0]` (system) | Convert system instructions into the first system message |
| `input[].Message` | `messages[]` | Convert content and normalize roles |
| `input[].Reasoning` | ❌ dropped | Chat API has no matching semantics; log a warning and ignore |
| `input[].FunctionCall` | `assistant.tool_calls` | Support round-trip by converting into Chat tools semantics |
| `input[].FunctionCallOutput` | `tool` message | Support round-trip by feeding back tool responses |
| `input[].WebSearchCall` | ❌ error | Hosted tools are unsupported |
| `tools` (function) | `tools` | Convert into Chat tools format |
| `tool_choice` | `tool_choice` | May require downgrade based on provider capability |
| `parallel_tool_calls` | `parallel_tool_calls` | Pass through only if supported by the provider |
| `previous_response_id` | ❌ error | Stateful sessions are unsupported in MVP |

### 3.2 Role Normalization

Base rules:

- `developer -> system`
- `system -> system`
- `user -> user`
- `assistant -> assistant`
- `tool -> tool`

### 3.2.1 MiniMax Special Rules

Based on observed behavior of the MiniMax domestic OpenAI-compatible API:

- It supports a single leading `system` message
- It does not support `developer`
- It does not accept additional `system` messages later in the conversation

Therefore, under the `minimax` provider, the adapter merges:

- `instructions`
- all subsequent `developer/system` text messages

into a **single leading `system` message**, so a second `system` message is never sent upstream.

### 3.3 Tools Conversion

**Input (Responses)**:
```json
{
  "type": "function",
  "name": "get_weather",
  "description": "Get weather information",
  "parameters": { "type": "object", "properties": {...} },
  "strict": true
}
```

**Output (Chat)**:
```json
{
  "type": "function",
  "function": {
    "name": "get_weather",
    "description": "Get weather information",
    "parameters": { "type": "object", "properties": {...} },
    "strict": true
  }
}
```

### 3.4 Response Conversion

| Chat field | Responses field | Notes |
|-----------|------------------|-------|
| `choices[].message.content` | `output[].message.content[].text` | Extract text content |
| `id` | `id` | Prefix with `resp_` for distinction |
| `usage` | `usage` | Pass through |

### 3.4.1 `<think>` Content Filtering

Some third-party models output reasoning content directly in the visible text, for example:

```text
<think>
internal reasoning...
</think>

final answer
```

The current implementation filters this content during response conversion:

- Non-streaming: fully clean `choices[].message.content`
- Streaming: use an incremental cross-chunk filter to remove `<think>...</think>` without leaking partial tags or reasoning text to the Codex frontend

---

## 4. Provider Capability Management

### 4.1 Capability Definition

```rust
struct ProviderCapabilities {
    supports_tools: bool,              // whether tools semantics are supported
    supports_tool_choice_auto: bool,   // whether "auto" is supported
    supports_parallel_tool_calls: bool, // whether parallel tool calls are supported
    supports_streaming: bool,          // whether streaming is supported
    supports_system_role: bool,        // whether the system role is supported
    requires_single_leading_system_message: bool, // whether a single leading system is required
    max_context_tokens: Option<u32>,   // max context size
}
```

### 4.2 Provider Registry

| Provider | tools | tool_choice=auto | parallel | streaming | system role | single leading system | max_tokens |
|----------|-------|------------------|----------|-----------|-------------|------------------------|------------|
| GLM-4    | ✅    | ✅               | ✅       | ✅        | ✅          | ❌                     | 128K       |
| MiniMax  | ✅    | ✅               | ✅       | ✅        | ✅          | ✅                     | 256K       |
| vLLM     | ✅    | ❌ (some versions) | ❌     | ✅        | ✅          | ❌                     | deployment-dependent |

### 4.3 Capability Downgrade Strategy

```
Request: tool_choice = "auto"
         ↓
Provider: supports_tool_choice_auto = false
         ↓
allow_downgrade = true  -> tool_choice = "none"
allow_downgrade = false -> return an error
```

---

## 5. Implementation Phases

### 5.1 MVP (Phase 1)

**Goal**: validate the feasibility of Codex <-> Adapter <-> Chat API protocol conversion

**Completed items (historical design)**:

| Module | Function | Status |
|------|------|------|
| `error.rs` | Define the `AdapterError` enum | ⬜ |
| `types.rs` | Define request / response structures | ⬜ |
| `request_converter.rs` | Request conversion (text-only) | ⬜ |
| `response_converter.rs` | Response conversion | ⬜ |
| `providers/mod.rs` | Capability registry | ⬜ |
| `handler.rs` | HTTP endpoint handling | ⬜ |
| `main.rs` | Service entrypoint | ⬜ |

**MVP feature scope (historical design)**:

- ✅ non-streaming text request / response
- ✅ role normalization (`developer/system -> system`)
- ✅ tools conversion (function type only)
- ✅ capability downgrade
- ❌ function round-trip (not included in the original MVP plan)
- ❌ reasoning continuity
- ❌ `previous_response_id`
- ❌ hosted tools
- ❌ streaming (not included in the original MVP plan)

> Note: the current implementation has already gone beyond this historical MVP scope and now supports streaming and function round-trip.

**Example configuration**:

```toml
# ~/.codex/config.toml
model_provider = "glm-adapter"

[model_providers.glm-adapter]
name = "GLM Adapter"
base_url = "http://localhost:3000/v1"
env_key = "GLM_API_KEY"
wire_api = "responses"

[model_providers.glm-adapter.target]
provider = "glm"
model = "glm-4"
base_url = "https://open.bigmodel.cn/api/paas/v4"
```

---

### 5.2 v1.0 (Phase 2)

**Goal**: support basic tool calling

**New features**:

| Feature | Description |
|------|------|
| Full function call round-trip | Receive `function_call`, invoke tools, and return results |
| SSE streaming text | Support text deltas when `stream: true` |
| Improved error handling | Distinguish between recoverable and unrecoverable errors |

**Architecture changes**:

```rust
// New: tool executor
trait ToolExecutor {
    async fn execute(&self, call: FunctionCall) -> Result<Value, AdapterError>;
}

struct DefaultToolExecutor {
    http_client: reqwest::Client,
}

impl ToolExecutor for DefaultToolExecutor {
    async fn execute(&self, call: FunctionCall) -> Result<Value, AdapterError> {
        // Parse arguments, call external tools, and return results
    }
}
```

---

### 5.3 v1.1 (Phase 3)

**Goal**: support multi-turn conversations

**New features**:

| Feature | Description |
|------|------|
| Session Store | In-memory cache with `previous_response_id` support |
| Reasoning backfill | Persist reasoning items for multi-turn reasoning recovery |
| Incremental requests | Support Codex requests that include partial history |

**Data model**:

```rust
struct Session {
    id: String,
    messages: Vec<ChatMessage>,
    reasoning_history: Vec<ReasoningItem>,
    tool_calls: Vec<ToolCallState>,
}
```

---

### 5.4 v2.0 (Phase 4)

**Goal**: fully emulate the Responses tool ecosystem

**New features**:

| Feature | Description |
|------|------|
| Web Search emulation | Adapt to third-party search APIs |
| File Search emulation | Adapt to vector databases |
| Computer mode | Remote code execution (if needed) |
| Tool recovery | Resume tool call state after interruption |

---

## 6. Boundaries and Limitations

### 6.1 Explicitly Unsupported Features in MVP

| Scenario | Codex request | Adapter behavior |
|------|-----------|--------------|
| Reasoning items | `input: [Reasoning {...}]` | Return HTTP 400 |
| Tool calls | `input: [FunctionCall {...}]` | Return HTTP 400 |
| Stateful session | `previous_response_id: "..."` | Return HTTP 400 |
| Streaming | `stream: true` | Return HTTP 400 |
| Hosted Tools | `input: [WebSearchCall ...]` | Return HTTP 400 |
| Unsupported role | `role: "critic"` | Return HTTP 400 |

### 6.2 Handling Capability Mismatches

```
Request tool_choice = "auto"
         ↓
Provider does not support auto
         ↓
allow_downgrade = true  -> downgrade to "none" and log it
allow_downgrade = false -> return 400 with an insufficient capability error
```

---

## 7. File Structure

```text
codex-responses-adapter/
├── Cargo.toml
├── Cargo.lock
├── codex-responses-adapter.example.toml
├── docs/
│   └── specs/
└── src/
    ├── main.rs
    ├── config.rs
    ├── error.rs
    ├── handler.rs
    ├── request_converter.rs
    ├── response_converter.rs
    ├── providers/
    │   └── mod.rs
    └── types/
        ├── mod.rs
        ├── chat_api.rs
        └── responses_api.rs
```

---

## 8. Configuration and Deployment

### 8.1 Authentication and Environment Variables

The recommended approach is now to use `~/.codex-responses-adapter.toml` for multi-provider routing instead of relying only on CLI environment variables.

For the OpenAI provider, the authentication mode is:

- `use_incoming_auth = true`
- Do not read an OpenAI API key
- Forward the incoming Codex user-account Bearer token directly

This appears in startup logs as:

```text
provider 'openai': https://api.openai.com/v1 (auth=user_account)
```

| Variable | Required | Default | Description |
|------|------|--------|------|
| `UPSTREAM_URL` | Yes | - | Base URL of the third-party API |
| `UPSTREAM_API_KEY` | Yes | - | Third-party API key |
| `PROVIDER` | Yes | "glm" | Target provider (`glm` / `minimax` / `custom`) |
| `ALLOW_DOWNGRADE` | No | false | Whether capability downgrade is allowed |
| `LISTEN` | No | "127.0.0.1:3000" | Listen address |

### 8.2 Startup Examples

```bash
# GLM
export UPSTREAM_API_KEY="your-glm-key"
cargo run -- \
    --upstream-url "https://open.bigmodel.cn/api/paas/v4" \
    --provider glm

# MiniMax
export UPSTREAM_API_KEY="your-minimax-key"
cargo run -- \
    --upstream-url "https://api.minimaxi.com/v1" \
    --provider minimax
```

---

## 9. Test Plan

### 9.1 MVP Test Cases

| # | Scenario | Input | Expected output |
|---|------|------|----------|
| 1 | Basic text request | `instructions` + user message | Correct conversion and text response |
| 2 | Role normalization | `role: "developer"` | Converted to `"system"` |
| 3 | Unsupported role | `role: "critic"` | HTTP 400 error |
| 4 | Empty tools array | `tools: []` | Omit the `tools` field |
| 5 | Tools conversion | valid function tools | Correctly converted into Chat format |
| 6 | Reject reasoning | input contains `Reasoning` | HTTP 400 error |
| 7 | Streaming text | `stream: true` | Correct Responses SSE output |
| 8 | Capability downgrade | `tool_choice=auto`, unsupported | Downgrade or error according to config |
| 9 | MiniMax multi-system instructions | `instructions` + `developer` | Merge into a single leading system |
| 10 | `<think>` output | content includes a think block | Filtered to final answer only |

---

## 10. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|------|----------|
| Provider does not support tools | Feature gaps | Capability registry + downgrade |
| Streaming conversion is complex | MVP becomes too heavy | Delay until v1.0 |
| Multi-turn state management | Higher design complexity | Introduce in v1.1; keep MVP fully stateless |
| Tool execution safety | Potential security risk | Sandbox tool execution + allowlist |

---

## 11. Implementation Status

> The original MVP defined in this document has been fully implemented, and the implementation has already been extended beyond that baseline.

### 11.1 Implemented Features

| Feature | Design phase | Status |
|------|---------|------|
| Responses <-> Chat API conversion | MVP | ✅ |
| `developer -> system` role mapping | MVP | ✅ |
| Hosted tools filtering (`web_search`, etc.) | MVP | ✅ |
| Streaming + non-streaming | MVP | ✅ |
| Function call round-trip conversion | v1.0 | ✅ |
| Provider capability registry | MVP | ✅ |
| **TOML configuration file** (`--config`) | Extension | ✅ |
| **Multi-provider routing + ordered fallback** | Extension | ✅ |
| **Path routing** (`/v1/{provider}/responses`) | Extension | ✅ |
| **OAuth token passthrough** (`use_incoming_auth`) | Extension | ✅ |
| **Model mapping** (Codex model -> upstream model) | Extension | ✅ |
| **MiniMax single leading system merge** | Extension | ✅ |
| **`<think>` filtering (streaming / non-streaming)** | Extension | ✅ |
| **User-account passthrough log marker** | Extension | ✅ |
| 21 unit tests | — | ✅ |

### 11.2 Usage

**Start the adapter:**

```bash
export GLM_API_KEY="your-key"
export MINIMAX_API_KEY="your-key"
codex-responses-adapter
# or explicitly: codex-responses-adapter --config ~/.codex-responses-adapter.toml
```

**Switch Codex profiles:**

```bash
codex --profile glm            # hybrid routing (OpenAI -> GLM -> MiniMax fallback)
codex --profile glm-only       # force GLM
codex --profile minimax-only   # force MiniMax
```

**Switch models in interactive mode:** enter `/model gpt-5.3-codex` to switch to GLM 4.7

**Testing:**

```bash
cargo test                                        # 21 tests
codex exec --profile glm "Say hello"              # e2e
curl http://127.0.0.1:3000/health                 # health check
curl -X POST http://127.0.0.1:3000/v1/glm/responses \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-5.4","input":"hi","stream":false}'
```

### 11.3 Future Improvements

1. Multi-turn session state management
2. Token usage accounting and rate limiting
3. Hot-reload for configuration
4. Closed-loop Web Search support

---

## 12. Web Search Adaptation

### 12.1 Background

In the OpenAI Responses API, `web_search` is a **hosted tool** executed on the OpenAI server side, and the client (Codex) does not participate. When the adapter proxies requests to GLM / MiniMax, those Chat API providers do not support hosted tools.

The adapter currently drops `web_search` tool definitions directly (`request_converter.rs`, lines 281-287), which means Codex users cannot use search.

### 12.2 Design

The adapter itself takes the role of the "hosted tool executor": **intercept search requests and execute them directly**.

```
                                  Inside Adapter
                              ┌──────────────────────────────────────┐
Codex ──(tools: web_search)─► │  1. web_search -> function tool def │
                              │  2. Send to LLM (GLM / MiniMax)      │
                              │        ↓                              │
                              │  3. LLM returns a function_call      │
                              │     ("__adapter_web_search")         │
                              │        ↓                              │
                              │  4. Adapter calls search API         │
                              │     (Tavily / Brave)                 │
                              │        ↓                              │
                              │  5. Inject search results into msgs  │
                              │  6. Request LLM again -> final answer│
                              └─────────────────────┬────────────────┘
                                                    │
Codex ◄──────── final answer (text) ────────────────┘
```

### 12.3 Configuration

```toml
# ~/.codex-responses-adapter.toml
[web_search]
provider = "tavily"            # "tavily" | "brave"
api_key_env = "TAVILY_API_KEY" # search API key env var
# api_key = "inline key (not recommended)"
max_results = 5                # number of returned results (default 5)
```

```rust
#[derive(Debug, Deserialize)]
pub struct WebSearchConfig {
    pub provider: WebSearchProvider,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_max_results")]
    pub max_results: usize,  // default 5
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Tavily,
    Brave,
}
```

### 12.4 Search API Interfaces

**Tavily**:
```
POST https://api.tavily.com/search
Headers: Content-Type: application/json
Body: { "api_key": "...", "query": "...", "max_results": 5 }
Response: { "results": [{ "title", "url", "content" }] }
```

**Brave**:
```
GET https://api.search.brave.com/res/v1/web/search?q=...&count=5
Headers: X-Subscription-Token: <api_key>
Response: { "web": { "results": [{ "title", "url", "description" }] } }
```

### 12.5 Tool Conversion

In `request_converter::convert_tools()`, when a `web_search` tool is detected and `WebSearchConfig` is present, convert it to:

```json
{
  "type": "function",
  "function": {
    "name": "__adapter_web_search",
    "description": "Search the web for current information. Use this when you need to find up-to-date information about any topic.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string", "description": "The search query" }
      },
      "required": ["query"]
    }
  }
}
```

### 12.6 Multi-Turn Loop in the Handler

`handle_responses` in `handler.rs` is currently a single LLM call. With search enabled, it becomes a multi-round loop:

```rust
const MAX_SEARCH_ROUNDS: usize = 3;

for round in 0..MAX_SEARCH_ROUNDS {
    let response = call_llm(&chat_req).await?;

    if let Some(search_call) = extract_search_call(&response) {
        // Execute the search
        let results = web_search_service.search(&search_call.query).await?;

        // Append assistant tool_call + tool response to messages
        chat_req.messages.push(/* assistant with tool_calls */);
        chat_req.messages.push(/* tool response with results */);
        continue;
    }

    // Non-search response -> return to Codex normally
    return build_response(response);
}
```

**Key details**:
- Limit to 3 rounds to prevent infinite search loops
- Always use non-streaming for the intermediate search steps; the final LLM answer follows the original request's streaming mode
- Search timeout: 10 seconds
- Format search results as plain text and inject them into the tool response

### 12.7 File Changes

| File | Change |
|------|------|
| `config.rs` | Add `WebSearchConfig` and `WebSearchProvider` |
| `web_search.rs` | **New** - search service (Tavily + Brave) |
| `request_converter.rs` | `convert_tools()` handles `web_search -> function` |
| `handler.rs` | Multi-turn loop + search interception |
| `main.rs` | Pass `WebSearchConfig` into `ServerConfig` |
| `codex-responses-adapter.example.toml` | Add `[web_search]` example |

### 12.8 Tests

| # | Scenario | Expected |
|---|------|------|
| 1 | Search configured + tools include `web_search` | Convert `web_search` into the `__adapter_web_search` function |
| 2 | Search not configured + tools include `web_search` | Drop `web_search` (current behavior) |
| 3 | Tavily response parsing | Correctly extract `title + url + content` |
| 4 | Brave response parsing | Correctly extract `title + url + description` |
| 5 | Multi-turn loop max 3 rounds | Return the last result after 3 rounds |

---

*Document version: 3.0*
*Last updated: 2026-03-11*
