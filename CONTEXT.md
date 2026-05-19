# Domain Glossary: codex-responses-adapter

## Core Concepts

### Adapter
The codex-responses-adapter translates OpenAI Responses API requests to Chat Completions API for third-party LLM providers (GLM, MiniMax, vLLM, etc.).

### Responses API
OpenAI's newer API format used by Codex CLI. Stateful, with features like `previous_response_id`, hosted tools, and structured output.

### Chat Completions API
The older, widely-supported OpenAI API format. Stateless, simpler request/response model.

### Provider
An upstream LLM service that exposes a Chat Completions API endpoint. Examples: GLM, MiniMax, vLLM, OpenAI.

### Provider Capabilities
Features a provider supports: streaming, tools, tool_choice, system role, etc. Used for translation and downgrade decisions.

### Route / Routing
The mapping from a Codex model name to one or more upstream provider+model pairs. Supports fallback chains.

### Web Search
A hosted tool that allows the model to search the web. The adapter supports:
- **Passthrough**: Forward to provider's native /responses endpoint
- **Adapter-managed**: Use a configured backend (Tavily, Brave, Custom)

## Key Terms

- **Passthrough**: Forwarding a request directly to the provider's native Responses API endpoint without translation.
- **Capability Downgrade**: When a provider doesn't support a feature, silently drop it (if `allow_downgrade` is enabled) or error.
- **Streaming (SSE)**: Server-Sent Events for real-time response chunks.
- **Tool Call**: A function invocation requested by the model.
