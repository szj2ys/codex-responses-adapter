//! Provider capability registry.
//!
//! Each target provider (GLM, MiniMax, vLLM, etc.) has a set of capabilities
//! that determine which Responses API features can be translated and which
//! must be rejected or downgraded.
//!
//! Reference: docs/specs/2026-03-11-search-routing.md

use serde::{Deserialize, Serialize};

/// Capabilities of a downstream Chat API provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Whether the provider supports the OpenAI Responses API directly.
    pub supports_responses_api: bool,
    /// Whether the provider supports the `tools` parameter.
    pub supports_tools: bool,
    ///  Whether the provider supports `tool_choice = "auto"`.
    pub supports_tool_choice_auto: bool,
    /// Whether the provider supports parallel tool calls.
    pub supports_parallel_tool_calls: bool,
    /// Whether the provider supports SSE streaming.
    pub supports_streaming: bool,
    /// Whether the provider supports the `system` message role.
    /// When false, system messages are converted to user messages.
    pub supports_system_role: bool,
    /// Whether all system/developer instructions must be merged into a single
    /// leading system message for compatibility with the downstream provider.
    pub requires_single_leading_system_message: bool,
    /// Maximum context window (tokens), if known.
    pub max_context_tokens: Option<u32>,
    /// Whether the provider supports the `strict` field in tool/function definitions.
    /// When false, `strict` is stripped from tool definitions before sending.
    /// OpenAI supports this; most other providers do not.
    pub supports_strict_tool_schema: bool,
}

/// Known provider presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Openai,
    Glm,
    Minimax,
    Vllm,
    /// Mimo (Vertex AI) - does not support parallel_tool_calls
    Mimo,
    Custom,
}

impl ProviderKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "openai" => Self::Openai,
            "glm" => Self::Glm,
            "minimax" => Self::Minimax,
            "vllm" => Self::Vllm,
            "mimo" => Self::Mimo,
            _ => Self::Custom,
        }
    }

    /// Default capabilities for this provider.
    pub fn default_capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Openai => ProviderCapabilities {
                supports_responses_api: true,
                supports_tools: true,
                supports_tool_choice_auto: true,
                supports_parallel_tool_calls: true,
                supports_streaming: true,
                supports_system_role: true,
                requires_single_leading_system_message: false,
                max_context_tokens: Some(400_000),
                supports_strict_tool_schema: true,
            },
            Self::Glm => ProviderCapabilities {
                supports_responses_api: false,
                supports_tools: true,
                supports_tool_choice_auto: true,
                supports_parallel_tool_calls: true,
                supports_streaming: true,
                supports_system_role: true,
                requires_single_leading_system_message: false,
                max_context_tokens: Some(128_000),
                supports_strict_tool_schema: false,
            },
            Self::Minimax => ProviderCapabilities {
                supports_responses_api: false,
                supports_tools: true,
                supports_tool_choice_auto: true,
                supports_parallel_tool_calls: true,
                supports_streaming: true,
                supports_system_role: true,
                requires_single_leading_system_message: true,
                max_context_tokens: Some(256_000),
                supports_strict_tool_schema: false,
            },
            Self::Vllm => ProviderCapabilities {
                supports_responses_api: false,
                supports_tools: true,
                supports_tool_choice_auto: false,
                supports_parallel_tool_calls: false,
                supports_streaming: true,
                supports_system_role: true,
                requires_single_leading_system_message: false,
                max_context_tokens: None,
                supports_strict_tool_schema: false,
            },
            Self::Mimo => ProviderCapabilities {
                supports_responses_api: false,
                supports_tools: true,
                supports_tool_choice_auto: true,
                supports_parallel_tool_calls: false,
                supports_streaming: false,
                supports_system_role: true,
                requires_single_leading_system_message: false,
                max_context_tokens: None,
                supports_strict_tool_schema: false,
            },
            Self::Custom => ProviderCapabilities {
                supports_responses_api: false,
                supports_tools: true,
                supports_tool_choice_auto: true,
                supports_parallel_tool_calls: true,
                supports_streaming: true,
                supports_system_role: true,
                requires_single_leading_system_message: false,
                max_context_tokens: None,
                supports_strict_tool_schema: false,
            },
        }
    }
}
