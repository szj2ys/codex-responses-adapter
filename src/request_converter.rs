//! Request conversion: Responses API → Chat Completions API.
//!
//! Reference: docs/specs/2026-03-11-search-routing.md

use serde_json::json;
use serde_json::Value;

use crate::error::AdapterError;
use crate::providers::ProviderCapabilities;
use crate::translation::normalize_tool_id;
use crate::types::chat_api::ChatCompletionsRequest;
use crate::types::chat_api::ChatMessage;
use crate::types::chat_api::FunctionCall as ChatFunctionCall;
use crate::types::chat_api::ToolCall;
use crate::types::responses_api::ContentItem;
use crate::types::responses_api::FunctionCallOutputPayload;
use crate::types::responses_api::ResponseItem;
use crate::types::responses_api::ResponsesApiRequest;
use crate::web_search::ADAPTER_WEB_SEARCH_TOOL_NAME;

/// Translate a Responses API request into a Chat Completions request.
///
/// Validates the request against provider capabilities and returns errors
/// for unsupported features (reasoning items, hosted tools, etc.).
pub fn convert_request(
    req: &ResponsesApiRequest,
    capabilities: &ProviderCapabilities,
    allow_downgrade: bool,
    enable_internal_web_search: bool,
) -> Result<ChatCompletionsRequest, AdapterError> {
    tracing::debug!(
        "converting Responses API request: model={}, input_items={}, tools_count={}",
        req.model,
        req.input.len(),
        req.tools.len()
    );
    // Validate: previous_response_id not supported
    if req.previous_response_id.is_some() {
        return Err(AdapterError::UnsupportedFeature(
            "previous_response_id is not supported; stateful sessions require server-side state"
                .into(),
        ));
    }

    // Validate streaming against provider capability
    // When allow_downgrade is true, silently downgrade streaming to non-streaming
    // instead of rejecting the request. This prevents LiteLLM fallback chains
    // that route to geo-blocked providers (e.g., Vertex AI in China).
    let effective_stream = if req.stream && !capabilities.supports_streaming {
        if allow_downgrade {
            tracing::warn!(
                "provider does not support streaming; downgrading to non-streaming"
            );
            false
        } else {
            return Err(AdapterError::CapabilityNotAvailable(
                "streaming is not supported by this provider".into(),
            ));
        }
    } else {
        req.stream
    };

    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut leading_system_segments: Vec<String> = Vec::new();

    if !req.instructions.is_empty() {
        leading_system_segments.push(req.instructions.clone());
    }

    // 2. Walk input items and convert to messages.
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();

    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                flush_tool_calls(&mut messages, &mut pending_tool_calls);

                let chat_role = normalize_role(role, capabilities)?;

                if capabilities.requires_single_leading_system_message && chat_role == "system" {
                    if let Some(text) = content_items_to_text(content) {
                        if !text.is_empty() {
                            leading_system_segments.push(text);
                        }
                    }
                    continue;
                }

                let chat_content = content_items_to_chat_content(content);

                messages.push(ChatMessage {
                    role: chat_role.to_string(),
                    content: Some(chat_content),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                pending_tool_calls.push(ToolCall {
                    id: normalize_tool_id(call_id),
                    call_type: "function".to_string(),
                    function: ChatFunctionCall {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                });
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                flush_tool_calls(&mut messages, &mut pending_tool_calls);

                let output_text = match output {
                    FunctionCallOutputPayload::Text(s) => s.clone(),
                    FunctionCallOutputPayload::Structured(v) => {
                        serde_json::to_string(v).unwrap_or_default()
                    }
                };

                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(Value::String(output_text)),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                });
            }
            ResponseItem::Reasoning { .. } => {
                // Design §3.1: Reasoning items are dropped (MVP logs a warning).
                tracing::warn!("dropping Reasoning item from input (not supported by Chat API)");
            }
            ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::LocalShellCall { .. } => {
                return Err(AdapterError::UnsupportedFeature(format!(
                    "hosted tool items ({}) are not supported through the adapter",
                    item.type_name()
                )));
            }
            ResponseItem::Other => {
                tracing::debug!("skipping unknown/other ResponseItem variant");
            }
        }
    }

    flush_tool_calls(&mut messages, &mut pending_tool_calls);

    if !leading_system_segments.is_empty() {
        let role = if capabilities.supports_system_role {
            "system"
        } else {
            "user"
        };
        messages.insert(
            0,
            ChatMessage {
                role: role.to_string(),
                content: Some(Value::String(leading_system_segments.join("\n\n"))),
                tool_calls: None,
                tool_call_id: None,
            },
        );
    }

    // 3. Build tools — translate from Responses flat format to Chat nested format.
    //    Design §3.3: Responses uses {type, name, description, parameters, strict}
    //    Chat uses {type: "function", function: {name, description, parameters, strict}}
    let tools = if req.tools.is_empty() {
        None
    } else if !capabilities.supports_tools {
        if allow_downgrade {
            tracing::warn!("provider does not support tools; dropping tools from request");
            None
        } else {
            return Err(AdapterError::CapabilityNotAvailable(
                "provider does not support tools".into(),
            ));
        }
    } else {
        let converted = convert_tools(&req.tools, enable_internal_web_search, capabilities.supports_strict_tool_schema)?;
        if converted.is_empty() {
            None
        } else {
            Some(converted)
        }
    };

    // 4. tool_choice — with capability downgrade.
    let tool_choice = if tools.is_none() {
        None
    } else {
        Some(convert_tool_choice(
            &req.tool_choice,
            capabilities,
            allow_downgrade,
        )?)
    };

    let parallel_tool_calls = if tools.is_none() {
        None
    } else if capabilities.supports_parallel_tool_calls {
        Some(req.parallel_tool_calls)
    } else {
        None
    };

    Ok(ChatCompletionsRequest {
        model: req.model.clone(),
        messages,
        tools,
        tool_choice,
        stream: effective_stream,
        parallel_tool_calls,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Design §3.2: Role normalization.
fn normalize_role(
    role: &str,
    capabilities: &ProviderCapabilities,
) -> Result<&'static str, AdapterError> {
    match role {
        "developer" | "system" => Ok(if capabilities.supports_system_role {
            "system"
        } else {
            "user"
        }),
        "user" => Ok("user"),
        "assistant" => Ok("assistant"),
        "tool" => Ok("tool"),
        other => Err(AdapterError::UnsupportedRole(other.to_string())),
    }
}

/// Flush accumulated FunctionCall items into a single assistant message.
fn flush_tool_calls(messages: &mut Vec<ChatMessage>, pending: &mut Vec<ToolCall>) {
    if pending.is_empty() {
        return;
    }
    messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(std::mem::take(pending)),
        tool_call_id: None,
    });
}

/// Convert ContentItem list to Chat API content value.
fn content_items_to_chat_content(items: &[ContentItem]) -> Value {
    if items.len() == 1 {
        match &items[0] {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                return Value::String(text.clone());
            }
            _ => {}
        }
    }

    let parts: Vec<Value> = items
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(json!({ "type": "text", "text": text }))
            }
            ContentItem::InputImage { image_url } => Some(json!({
                "type": "image_url",
                "image_url": { "url": image_url }
            })),
            ContentItem::Other => None,
        })
        .collect();

    Value::Array(parts)
}

fn content_items_to_text(items: &[ContentItem]) -> Option<String> {
    let parts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } | ContentItem::Other => None,
        })
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Design §3.3: Convert Responses-format tools to Chat-format tools.
///
/// Responses API flat format:
/// ```json
/// { "type": "function", "name": "get_weather", "description": "...", "parameters": {...}, "strict": true }
/// ```
///
/// Chat API nested format:
/// ```json
/// { "type": "function", "function": { "name": "get_weather", "description": "...", "parameters": {...}, "strict": true } }
/// ```
///
/// When `supports_strict_tool_schema` is false, the `strict` field is stripped
/// because many providers (GLM, MiniMax, mimo, etc.) reject it with "Param Incorrect".
fn build_function_tool(tool: &Value, supports_strict: bool) -> Option<Value> {
    if let Some(function_obj) = tool.get("function") {
        // Already in nested format — strip strict if unsupported.
        if !supports_strict {
            if let Some(obj) = function_obj.as_object() {
                let mut cleaned = obj.clone();
                cleaned.remove("strict");
                return Some(json!({
                    "type": "function",
                    "function": Value::Object(cleaned)
                }));
            }
        }
        return Some(json!({
            "type": "function",
            "function": function_obj.clone()
        }));
    }

    let mut function_obj = serde_json::Map::new();
    for field in ["name", "description", "parameters"] {
        if let Some(v) = tool.get(field) {
            function_obj.insert(field.to_string(), v.clone());
        }
    }
    // Only include strict if the provider supports it.
    if supports_strict {
        if let Some(v) = tool.get("strict") {
            function_obj.insert("strict".to_string(), v.clone());
        }
    }

    Some(json!({
        "type": "function",
        "function": Value::Object(function_obj)
    }))
}

fn convert_tools(
    tools: &[Value],
    enable_internal_web_search: bool,
    supports_strict_tool_schema: bool,
) -> Result<Vec<Value>, AdapterError> {
    let converted: Vec<Value> = tools
        .iter()
        .filter_map(|tool| {
            let tool_type = tool
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("function");

            match tool_type {
                "function" => build_function_tool(tool, supports_strict_tool_schema),
                "custom" | "namespace" => {
                    if tool.get("function").is_some()
                        || (tool.get("name").is_some() && tool.get("parameters").is_some())
                    {
                        build_function_tool(tool, supports_strict_tool_schema)
                    } else {
                        let tool_json = serde_json::to_string(tool).unwrap_or_else(|_| "<failed to serialize>".to_string());
                        tracing::warn!(
                            "dropping unconvertible tool type '{}': missing function or name+parameters. Full tool: {}",
                            tool_type, tool_json
                        );
                        None
                    }
                }
                "web_search" | "web_search_preview" if enable_internal_web_search => Some(json!({
                    "type": "function",
                    "function": {
                        "name": ADAPTER_WEB_SEARCH_TOOL_NAME,
                        "description": "Search the web for current information.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" }
                            },
                            "required": ["query"]
                        }
                    }
                })),
                // Hosted tools (web_search*, file_search*, computer*) → silently drop.
                // Codex interactive mode automatically includes these, but third-party
                // Chat API providers (GLM, MiniMax) don't support them.
                other => {
                    let tool_json = serde_json::to_string(tool).unwrap_or_else(|_| "<failed to serialize>".to_string());
                    tracing::warn!(
                        "dropping unsupported tool type '{}': not supported by Chat API providers. Full tool: {}",
                        other, tool_json
                    );
                    None
                }
            }
        })
        .collect();

    Ok(converted)
}

/// Convert tool_choice with capability downgrade.
fn convert_tool_choice(
    tool_choice: &str,
    capabilities: &ProviderCapabilities,
    allow_downgrade: bool,
) -> Result<Value, AdapterError> {
    match tool_choice {
        "auto" => {
            if capabilities.supports_tool_choice_auto {
                Ok(Value::String("auto".to_string()))
            } else if allow_downgrade {
                tracing::warn!("provider does not support tool_choice=auto; downgrading to 'none'");
                Ok(Value::String("none".to_string()))
            } else {
                Err(AdapterError::CapabilityNotAvailable(
                    "tool_choice=auto is not supported by this provider".into(),
                ))
            }
        }
        "none" | "required" => Ok(Value::String(tool_choice.to_string())),
        // Could be a JSON object like {"type":"function","function":{"name":"..."}}
        other => match serde_json::from_str::<Value>(other) {
            Ok(v) => Ok(v),
            Err(_) => Ok(Value::String(other.to_string())),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderKind;
    use serde_json::json;
    use tracing_test::traced_test;

    fn glm_caps() -> ProviderCapabilities {
        ProviderKind::Glm.default_capabilities()
    }

    fn minimax_caps() -> ProviderCapabilities {
        ProviderKind::Minimax.default_capabilities()
    }

    fn make_request(input: Vec<ResponseItem>, tools: Vec<Value>) -> ResponsesApiRequest {
        ResponsesApiRequest {
            model: "glm-4".to_string(),
            instructions: "You are helpful.".to_string(),
            input,
            tools,
            tool_choice: "auto".to_string(),
            parallel_tool_calls: false,
            stream: false,
            store: false,
            reasoning: None,
            text: None,
            service_tier: None,
            prompt_cache_key: None,
            include: vec![],
            previous_response_id: None,
        }
    }

    #[test]
    fn test_simple_message() {
        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            vec![],
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        assert_eq!(result.messages.len(), 2); // system + user
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(result.messages[1].role, "user");
    }

    #[test]
    fn test_developer_role_normalized() {
        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Be concise.".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            vec![],
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        // instructions (system) + developer→system
        assert_eq!(result.messages.len(), 2);
        assert!(result.messages.iter().all(|m| m.role == "system"));
    }

    #[test]
    fn test_minimax_merges_system_messages_into_one_leading_message() {
        let req = make_request(
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "Hello".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                },
                ResponseItem::Message {
                    id: None,
                    role: "developer".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "Be concise.".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                },
            ],
            vec![],
        );

        let result = convert_request(&req, &minimax_caps(), false, false).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(
            result.messages[0].content,
            Some(Value::String("You are helpful.\n\nBe concise.".to_string()))
        );
        assert_eq!(result.messages[1].role, "user");
        assert_eq!(
            result.messages[1].content,
            Some(Value::String("Hello".to_string()))
        );
    }

    #[test]
    fn test_unsupported_role_returns_error() {
        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "critic".to_string(),
                content: vec![ContentItem::InputText {
                    text: "test".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            vec![],
        );

        let err = convert_request(&req, &glm_caps(), false, false).unwrap_err();
        assert!(matches!(err, AdapterError::UnsupportedRole(_)));
    }

    #[test]
    fn test_tools_format_conversion() {
        // Responses API flat format
        let tools = vec![json!({
            "type": "function",
            "name": "get_weather",
            "description": "Get weather information",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                }
            },
            "strict": true
        })];

        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Weather?".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            tools,
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        let chat_tools = result.tools.unwrap();

        // Should be in Chat nested format
        assert_eq!(chat_tools.len(), 1);
        let tool = &chat_tools[0];
        assert_eq!(tool["type"], "function");
        assert!(tool.get("function").is_some());
        assert_eq!(tool["function"]["name"], "get_weather");
        // strict is stripped because glm_caps has supports_strict_tool_schema=false
        assert!(tool["function"].get("strict").is_none());
    }

    #[test]
    fn test_tools_already_nested_format() {
        // Already in Chat nested format (passthrough)
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "shell",
                "parameters": {}
            }
        })];

        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "test".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            tools.clone(),
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        let chat_tools = result.tools.unwrap();
        assert_eq!(chat_tools[0], tools[0]); // unchanged
    }

    #[test]
    fn test_web_search_tool_converted_when_internal_search_enabled() {
        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "What happened today?".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            vec![json!({ "type": "web_search" })],
        );

        let result = convert_request(&req, &glm_caps(), false, true).unwrap();
        let tools = result.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], ADAPTER_WEB_SEARCH_TOOL_NAME);
        assert_eq!(tools[0]["function"]["parameters"]["required"][0], "query");
    }

    #[test]
    fn test_function_call_round_trip() {
        let req = make_request(
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "Run ls".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                },
                ResponseItem::FunctionCall {
                    id: None,
                    name: "shell".to_string(),
                    arguments: r#"{"cmd":"ls"}"#.to_string(),
                    call_id: "call_123".to_string(),
                },
                ResponseItem::FunctionCallOutput {
                    call_id: "call_123".to_string(),
                    output: FunctionCallOutputPayload::Text("file1.txt".to_string()),
                },
            ],
            vec![json!({"type": "function", "name": "shell", "parameters": {}})],
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        // system + user + assistant(tool_calls) + tool
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.messages[2].role, "assistant");
        assert!(result.messages[2].tool_calls.is_some());
        assert_eq!(result.messages[3].role, "tool");
        assert_eq!(
            result.messages[3].tool_call_id,
            Some("call_123".to_string())
        );
    }

    #[test]
    fn test_function_call_empty_call_id_gets_synthetic_id() {
        let req = make_request(
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "Run ls".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                },
                ResponseItem::FunctionCall {
                    id: None,
                    name: "shell".to_string(),
                    arguments: r#"{"cmd":"ls"}"#.to_string(),
                    call_id: "".to_string(), // Empty call_id
                },
            ],
            vec![json!({"type": "function", "name": "shell", "parameters": {}})],
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        // system + user + assistant(tool_calls)
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[2].role, "assistant");
        assert!(result.messages[2].tool_calls.is_some());

        let tool_calls = result.messages[2].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        // Empty call_id should be replaced with a synthetic non-empty ID
        assert!(!tool_calls[0].id.is_empty(), "tool call id should not be empty");
        assert!(
            tool_calls[0].id.starts_with("call_"),
            "synthetic id should start with 'call_', got: {}",
            tool_calls[0].id
        );
    }

    #[test]
    fn test_function_call_valid_call_id_preserved() {
        let req = make_request(
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "Run ls".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                },
                ResponseItem::FunctionCall {
                    id: None,
                    name: "shell".to_string(),
                    arguments: r#"{"cmd":"ls"}"#.to_string(),
                    call_id: "my_custom_id_123".to_string(), // Valid custom call_id
                },
            ],
            vec![json!({"type": "function", "name": "shell", "parameters": {}})],
        );

        let result = convert_request(&req, &glm_caps(), false, false).unwrap();
        // system + user + assistant(tool_calls)
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[2].role, "assistant");
        assert!(result.messages[2].tool_calls.is_some());

        let tool_calls = result.messages[2].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        // Valid call_id should be preserved unchanged
        assert_eq!(tool_calls[0].id, "my_custom_id_123");
    }

    #[test]
    fn test_capability_downgrade_tool_choice() {
        let mut caps = ProviderKind::Vllm.default_capabilities();
        caps.supports_tool_choice_auto = false;

        let req = make_request(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "test".to_string(),
                }],
                end_turn: None,
                phase: None,
            }],
            vec![json!({"type": "function", "name": "test", "parameters": {}})],
        );

        // With downgrade enabled → should succeed with downgraded value
        let result = convert_request(&req, &caps, true, false).unwrap();
        assert_eq!(result.tool_choice, Some(Value::String("none".to_string())));

        // Without downgrade → should error
        let err = convert_request(&req, &caps, false, false).unwrap_err();
        assert!(matches!(err, AdapterError::CapabilityNotAvailable(_)));
    }

    #[test]
    fn test_custom_tool_with_name_and_parameters_converts() {
        let tools = vec![json!({
            "type": "custom",
            "name": "mcp_fetch",
            "description": "Fetch a URL",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                }
            },
            "strict": true
        })];

        let result = convert_tools(&tools, false, false).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["function"]["name"], "mcp_fetch");
        assert_eq!(result[0]["function"]["description"], "Fetch a URL");
        // strict is stripped because supports_strict_tool_schema=false
        assert!(result[0]["function"].get("strict").is_none());
        assert!(result[0]["function"]["parameters"].is_object());
    }

    #[test]
    fn test_namespace_tool_with_function_sub_object_converts() {
        let tools = vec![json!({
            "type": "namespace",
            "function": {
                "name": "get_weather",
                "description": "Get the weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    }
                }
            }
        })];

        let result = convert_tools(&tools, false, false).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["function"]["name"], "get_weather");
        assert_eq!(result[0]["function"]["description"], "Get the weather");
    }

    #[test]
    #[traced_test]
    fn test_unconvertible_custom_namespace_tools_dropped_with_warn() {
        let tools = vec![
            json!({ "type": "custom", "description": "missing name and params" }),
            json!({ "type": "namespace", "description": "missing name and params" }),
        ];

        let result = convert_tools(&tools, false, false).unwrap();
        assert!(result.is_empty());

        assert!(logs_contain("dropping unconvertible tool type 'custom'"));
        assert!(logs_contain("dropping unconvertible tool type 'namespace'"));
    }

    #[test]
    #[traced_test]
    fn test_hosted_tools_dropped_with_warn() {
        let tools = vec![
            json!({ "type": "file_search" }),
            json!({ "type": "computer_use_preview" }),
        ];

        let result = convert_tools(&tools, false, false).unwrap();
        assert!(result.is_empty());

        assert!(logs_contain("dropping unsupported tool type 'file_search'"));
        assert!(logs_contain("dropping unsupported tool type 'computer_use_preview'"));
    }
}
