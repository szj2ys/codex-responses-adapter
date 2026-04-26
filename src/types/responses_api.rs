//! Responses API types – modeled after codex-protocol's ResponseItem and
//! codex-api's ResponsesApiRequest, but defined independently to avoid pulling
//! in the full codex dependency tree.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request types (what Codex CLI sends us)
// ---------------------------------------------------------------------------

/// The top-level request body for `POST /v1/responses`.
///
/// Reference: codex-api/src/common.rs – `ResponsesApiRequest`
/// Reference: docs/specs/2026-03-11-search-routing.md
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesApiRequest {
    pub model: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub input: Vec<ResponseItem>,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default = "default_tool_choice")]
    pub tool_choice: String,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub store: bool,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    // Fields we accept but ignore for translation:
    #[serde(default)]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub text: Option<Value>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
    #[serde(default)]
    pub include: Vec<String>,
}

fn default_tool_choice() -> String {
    "auto".to_string()
}

// ---------------------------------------------------------------------------
// ResponseItem – the main conversational element
// ---------------------------------------------------------------------------

/// Reference: codex-protocol/src/models.rs – `ResponseItem`
/// Reference: docs/specs/2026-03-11-search-routing.md
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default)]
        id: Option<String>,
        role: String,
        #[serde(default)]
        content: Vec<ContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_turn: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    Reasoning {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        summary: Vec<Value>,
    },
    FunctionCall {
        #[serde(default)]
        id: Option<String>,
        name: String,
        arguments: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
    },
    WebSearchCall {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        query: Option<String>,
    },
    ImageGenerationCall {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    LocalShellCall {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        call_id: Option<String>,
    },
    /// Catch-all for unknown types.
    #[serde(other)]
    Other,
}

impl ResponseItem {
    /// Returns the type name for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Message { .. } => "message",
            Self::Reasoning { .. } => "reasoning",
            Self::FunctionCall { .. } => "function_call",
            Self::FunctionCallOutput { .. } => "function_call_output",
            Self::WebSearchCall { .. } => "web_search_call",
            Self::ImageGenerationCall { .. } => "image_generation_call",
            Self::LocalShellCall { .. } => "local_shell_call",
            Self::Other => "other",
        }
    }
}

/// Reference: codex-protocol/src/models.rs – `ContentItem`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
    },
    OutputText {
        text: String,
    },
    #[serde(other)]
    Other,
}

/// The output payload can be either a plain string or structured content items.
///
/// Reference: codex-protocol/src/models.rs – `FunctionCallOutputPayload`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FunctionCallOutputPayload {
    Text(String),
    Structured(Value),
}

// ---------------------------------------------------------------------------
// Response types (what we send back to Codex CLI)
// ---------------------------------------------------------------------------

/// A full Responses API response envelope.
/// Reference: docs/specs/2026-03-11-search-routing.md
#[derive(Debug, Serialize)]
pub struct ResponsesApiResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub status: String,
    pub output: Vec<ResponseItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Serialize)]
pub struct ResponseUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<Value>,
}
