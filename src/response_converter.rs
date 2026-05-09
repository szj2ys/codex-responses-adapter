//! Response conversion: Chat Completions API → Responses API.
//!
//! Reference: docs/specs/2026-03-11-search-routing.md

use serde_json::json;
use uuid::Uuid;

use crate::tool_id_manager::normalize_tool_id;
use crate::types::chat_api::ChatCompletionsResponse;
use crate::types::chat_api::ChatStreamChunk;
use crate::types::responses_api::ContentItem;
use crate::types::responses_api::ResponseItem;
use crate::types::responses_api::ResponseUsage;
use crate::types::responses_api::ResponsesApiResponse;

// ---------------------------------------------------------------------------
// Non-streaming response conversion
// ---------------------------------------------------------------------------

/// Translate a non-streaming Chat Completions response into a Responses API
/// response envelope.
///
/// Design §3.4: id is prefixed with "resp_" to distinguish from Chat ids.
pub fn convert_response(chat_resp: &ChatCompletionsResponse) -> ResponsesApiResponse {
    let mut output: Vec<ResponseItem> = Vec::new();

    if let Some(choice) = chat_resp.choices.first() {
        // Text content → Message item
        if let Some(content) = &choice.message.content {
            let cleaned_content = strip_think_blocks(content);
            if !cleaned_content.is_empty() {
                output.push(ResponseItem::Message {
                    id: Some(format!("msg_{}", Uuid::new_v4())),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: cleaned_content,
                    }],
                    end_turn: Some(true),
                    phase: None,
                });
            }
        }

        // Tool calls → FunctionCall items
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                output.push(ResponseItem::FunctionCall {
                    id: Some(format!("fc_{}", Uuid::new_v4())),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                    call_id: normalize_tool_id(&tc.id),
                });
            }
        }
    }

    let usage = chat_resp.usage.as_ref().map(|u| ResponseUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
        input_tokens_details: None,
        output_tokens_details: None,
    });

    ResponsesApiResponse {
        id: format!("resp_{}", chat_resp.id),
        response_type: "response".to_string(),
        status: "completed".to_string(),
        output,
        usage,
    }
}

// ---------------------------------------------------------------------------
// SSE event building
// ---------------------------------------------------------------------------

/// Build the complete SSE event stream for a non-streaming response.
/// Returns a string with all SSE events that Codex's SSE parser expects.
pub fn build_sse_events(resp: &ResponsesApiResponse) -> String {
    let mut sse = String::new();

    // 1. response.created
    let created = json!({
        "type": "response.created",
        "response": {}
    });
    sse.push_str(&format!("event: response.created\ndata: {created}\n\n"));

    // 2. response.output_item.done for each output item
    for item in &resp.output {
        let item_json = serde_json::to_value(item).unwrap_or_default();
        let event = json!({
            "type": "response.output_item.done",
            "item": item_json
        });
        sse.push_str(&format!(
            "event: response.output_item.done\ndata: {event}\n\n"
        ));
    }

    // 3. response.completed
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": resp.id,
            "usage": resp.usage,
        }
    });
    sse.push_str(&format!("event: response.completed\ndata: {completed}\n\n"));

    sse
}

// ---------------------------------------------------------------------------
// Streaming translation
// ---------------------------------------------------------------------------

/// State machine for translating a Chat Completions SSE stream into
/// Responses API SSE events.
///
/// Codex expects the following event sequence for text output:
///   1. `response.output_item.added` (with a partial Message item)
///   2. `response.output_text.delta` (one per text chunk)
///   3. `response.output_item.done`  (with the complete Message item)
///   4. `response.completed`
pub struct StreamTranslator {
    response_id: Option<String>,
    /// Accumulated tool call data, keyed by index.
    tool_calls: std::collections::HashMap<i64, AccumulatingToolCall>,
    /// Whether we've emitted the `response.created` event.
    created_emitted: bool,
    /// Whether we've emitted the `response.output_item.added` for the text message.
    text_item_added: bool,
    /// The stable message ID for the text output item.
    text_item_id: String,
    /// Accumulated full text content (for output_item.done at finish).
    text_content: String,
    think_filter: ThinkBlockFilter,
}

struct AccumulatingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamTranslator {
    pub fn new() -> Self {
        Self {
            response_id: None,
            tool_calls: std::collections::HashMap::new(),
            created_emitted: false,
            text_item_added: false,
            text_item_id: format!("msg_{}", Uuid::new_v4()),
            text_content: String::new(),
            think_filter: ThinkBlockFilter::new(),
        }
    }

    /// Process a single Chat stream chunk and return zero or more SSE event
    /// strings to send to the client.
    pub fn process_chunk(&mut self, chunk: &ChatStreamChunk) -> Vec<String> {
        let mut events: Vec<String> = Vec::new();

        if self.response_id.is_none() {
            self.response_id = Some(format!("resp_{}", chunk.id));
        }

        if !self.created_emitted {
            self.created_emitted = true;
            let created = json!({
                "type": "response.created",
                "response": {}
            });
            events.push(format!("event: response.created\ndata: {created}\n\n"));
        }

        for choice in &chunk.choices {
            // Text delta → first emit output_item.added, then output_text.delta
            if let Some(content) = &choice.delta.content {
                let visible_content = self.think_filter.process(content);

                if !visible_content.is_empty() {
                    // Emit output_item.added on the very first text delta.
                    // Codex requires this before it will accept output_text.delta events.
                    if !self.text_item_added {
                        self.text_item_added = true;
                        let item = json!({
                            "type": "message",
                            "id": self.text_item_id,
                            "role": "assistant",
                            "content": [],
                        });
                        let added = json!({
                            "type": "response.output_item.added",
                            "item": item,
                        });
                        events.push(format!(
                            "event: response.output_item.added\ndata: {added}\n\n"
                        ));
                    }

                    // Accumulate text for the final output_item.done
                    self.text_content.push_str(&visible_content);

                    let delta_event = json!({
                        "type": "response.output_text.delta",
                        "delta": visible_content,
                    });
                    events.push(format!(
                        "event: response.output_text.delta\ndata: {delta_event}\n\n"
                    ));
                }
            }

            // Tool call deltas → accumulate
            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tc in tool_calls {
                    let entry =
                        self.tool_calls
                            .entry(tc.index)
                            .or_insert_with(|| AccumulatingToolCall {
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });

                    if let Some(id) = &tc.id {
                        entry.id.clone_from(id);
                    }
                    if let Some(func) = &tc.function {
                        if let Some(name) = &func.name {
                            entry.name.push_str(name);
                        }
                        if let Some(args) = &func.arguments {
                            entry.arguments.push_str(args);
                        }
                    }
                }
            }

            // finish_reason → emit final items + completed
            if choice.finish_reason.is_some() {
                events.extend(self.finish_events(chunk));
            }
        }

        events
    }

    fn finish_events(&mut self, chunk: &ChatStreamChunk) -> Vec<String> {
        let mut events: Vec<String> = Vec::new();

        let remaining_visible = self.think_filter.finish();
        if !remaining_visible.is_empty() {
            if !self.text_item_added {
                self.text_item_added = true;
                let item = json!({
                    "type": "message",
                    "id": self.text_item_id,
                    "role": "assistant",
                    "content": [],
                });
                let added = json!({
                    "type": "response.output_item.added",
                    "item": item,
                });
                events.push(format!(
                    "event: response.output_item.added\ndata: {added}\n\n"
                ));
            }

            self.text_content.push_str(&remaining_visible);
            let delta_event = json!({
                "type": "response.output_text.delta",
                "delta": remaining_visible,
            });
            events.push(format!(
                "event: response.output_text.delta\ndata: {delta_event}\n\n"
            ));
        }

        // Emit the completed text message as output_item.done
        if self.text_item_added && !self.text_content.is_empty() {
            let item = json!({
                "type": "message",
                "id": self.text_item_id,
                "role": "assistant",
                "content": [{"type": "output_text", "text": self.text_content}],
                "end_turn": true,
            });
            let event = json!({
                "type": "response.output_item.done",
                "item": item,
            });
            events.push(format!(
                "event: response.output_item.done\ndata: {event}\n\n"
            ));
        }

        // Emit accumulated tool calls
        let mut tool_calls: Vec<_> = self.tool_calls.drain().collect();
        tool_calls.sort_by_key(|(idx, _)| *idx);

        for (_, tc) in tool_calls {
            let item = json!({
                "type": "function_call",
                "id": format!("fc_{}", Uuid::new_v4()),
                "name": tc.name,
                "arguments": tc.arguments,
                "call_id": normalize_tool_id(&tc.id),
            });
            let event = json!({
                "type": "response.output_item.done",
                "item": item,
            });
            events.push(format!(
                "event: response.output_item.done\ndata: {event}\n\n"
            ));
        }

        // response.completed
        let response_id = self
            .response_id
            .clone()
            .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4()));

        let usage_json = chunk.usage.as_ref().map(|u| {
            json!({
                "input_tokens": u.prompt_tokens,
                "output_tokens": u.completion_tokens,
                "total_tokens": u.total_tokens,
            })
        });

        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "usage": usage_json,
            }
        });
        events.push(format!("event: response.completed\ndata: {completed}\n\n"));

        events
    }
}

#[derive(Default)]
struct ThinkBlockFilter {
    inside_think: bool,
    carry: String,
    suppress_leading_whitespace: bool,
}

impl ThinkBlockFilter {
    fn new() -> Self {
        Self::default()
    }

    fn process(&mut self, input: &str) -> String {
        let mut data = std::mem::take(&mut self.carry);
        data.push_str(input);
        let mut output = String::new();

        while !data.is_empty() {
            if self.inside_think {
                if let Some(end_idx) = data.find("</think>") {
                    data = data[end_idx + "</think>".len()..].to_string();
                    self.inside_think = false;
                    self.suppress_leading_whitespace = true;
                } else {
                    let carry_len = partial_tag_suffix_len(&data, "</think>");
                    self.carry = data[data.len().saturating_sub(carry_len)..].to_string();
                    return output;
                }
            } else if let Some(start_idx) = data.find("<think>") {
                let visible = &data[..start_idx];
                push_visible_text(&mut output, visible, &mut self.suppress_leading_whitespace);
                data = data[start_idx + "<think>".len()..].to_string();
                self.inside_think = true;
            } else {
                let carry_len = partial_tag_suffix_len(&data, "<think>");
                let visible_end = data.len().saturating_sub(carry_len);
                let visible = &data[..visible_end];
                push_visible_text(&mut output, visible, &mut self.suppress_leading_whitespace);
                self.carry = data[visible_end..].to_string();
                return output;
            }
        }

        output
    }

    fn finish(&mut self) -> String {
        if self.inside_think {
            self.carry.clear();
            return String::new();
        }

        let mut output = String::new();
        let carry = std::mem::take(&mut self.carry);
        push_visible_text(&mut output, &carry, &mut self.suppress_leading_whitespace);
        output
    }
}

fn strip_think_blocks(content: &str) -> String {
    let mut filter = ThinkBlockFilter::new();
    let mut output = filter.process(content);
    output.push_str(&filter.finish());
    output
}

fn partial_tag_suffix_len(text: &str, tag: &str) -> usize {
    let max_len = text.len().min(tag.len().saturating_sub(1));
    for len in (1..=max_len).rev() {
        if text.ends_with(&tag[..len]) {
            return len;
        }
    }
    0
}

fn push_visible_text(output: &mut String, text: &str, suppress_leading_whitespace: &mut bool) {
    if text.is_empty() {
        return;
    }

    if *suppress_leading_whitespace {
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            return;
        }
        output.push_str(trimmed);
        *suppress_leading_whitespace = false;
        return;
    }

    output.push_str(text);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::chat_api::Choice;
    use crate::types::chat_api::ChoiceMessage;
    use crate::types::chat_api::StreamChoice;
    use crate::types::chat_api::StreamDelta;
    use crate::types::chat_api::StreamFunctionCall;
    use crate::types::chat_api::StreamToolCall;
    use crate::types::chat_api::Usage;

    #[test]
    fn test_streaming_tool_call_empty_id_gets_synthetic() {
        let mut translator = StreamTranslator::new();

        // First chunk: tool call delta with NO id (simulates GLM/MiniMax behavior)
        let first = ChatStreamChunk {
            id: "chatcmpl-tool".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                    tool_calls: Some(vec![StreamToolCall {
                        index: 0,
                        id: None, // Empty/missing ID
                        call_type: Some("function".to_string()),
                        function: Some(StreamFunctionCall {
                            name: Some("shell".to_string()),
                            arguments: Some(r#"{"cmd":"ls"}"#.to_string()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
            model: None,
        };

        // Second chunk: finish the tool call
        let second = ChatStreamChunk {
            id: "chatcmpl-tool".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            model: None,
        };

        let _first_events = translator.process_chunk(&first);
        let second_events = translator.process_chunk(&second);

        // Find the function_call output_item.done event
        let function_call_event = second_events
            .iter()
            .find(|e| e.contains("function_call") && e.contains("output_item.done"))
            .expect("should have function_call output_item.done event");

        // Extract the call_id from the event
        let call_id: serde_json::Value = serde_json::from_str(
            &function_call_event
                .lines()
                .find(|l| l.starts_with("data:"))
                .unwrap()
                .trim_start_matches("data: "),
        )
        .unwrap();

        let call_id_str = call_id["item"]["call_id"].as_str().unwrap();
        assert!(
            !call_id_str.is_empty(),
            "call_id should not be empty, got: {:?}",
            call_id_str
        );
        assert!(
            call_id_str.starts_with("call_"),
            "synthetic call_id should start with 'call_', got: {}",
            call_id_str
        );
    }

    #[test]
    fn test_convert_text_response() {
        let chat_resp = ChatCompletionsResponse {
            id: "chatcmpl-123".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_string(),
                    content: Some("Hello!".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            model: None,
        };

        let resp = convert_response(&chat_resp);
        assert_eq!(resp.id, "resp_chatcmpl-123");
        assert_eq!(resp.output.len(), 1);
        assert!(resp.usage.is_some());
    }

    #[test]
    fn test_convert_text_response_strips_think_blocks() {
        let chat_resp = ChatCompletionsResponse {
            id: "chatcmpl-think".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_string(),
                    content: Some(
                        "<think>\ninternal reasoning\n</think>\n\nHi! What can I help you with?"
                            .to_string(),
                    ),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
            model: None,
        };

        let resp = convert_response(&chat_resp);
        assert_eq!(resp.output.len(), 1);
        match &resp.output[0] {
            ResponseItem::Message { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::OutputText { text } => {
                        assert_eq!(text, "Hi! What can I help you with?");
                    }
                    other => panic!("expected OutputText, got {other:?}"),
                }
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn test_convert_tool_calls_response() {
        let chat_resp = ChatCompletionsResponse {
            id: "chatcmpl-456".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![crate::types::chat_api::ToolCall {
                        id: "call_abc".to_string(),
                        call_type: "function".to_string(),
                        function: crate::types::chat_api::FunctionCall {
                            name: "shell".to_string(),
                            arguments: r#"{"cmd":"pwd"}"#.to_string(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
            model: None,
        };

        let resp = convert_response(&chat_resp);
        assert_eq!(resp.output.len(), 1);
        match &resp.output[0] {
            ResponseItem::FunctionCall { name, call_id, .. } => {
                assert_eq!(name, "shell");
                assert_eq!(call_id, "call_abc");
            }
            other => panic!("expected FunctionCall, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_tool_call_empty_id_gets_synthetic() {
        let chat_resp = ChatCompletionsResponse {
            id: "chatcmpl-empty-id".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![crate::types::chat_api::ToolCall {
                        id: "".to_string(), // Empty ID - simulates GLM/MiniMax behavior
                        call_type: "function".to_string(),
                        function: crate::types::chat_api::FunctionCall {
                            name: "shell".to_string(),
                            arguments: r#"{"cmd":"ls"}"#.to_string(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
            model: None,
        };

        let resp = convert_response(&chat_resp);
        assert_eq!(resp.output.len(), 1);
        match &resp.output[0] {
            ResponseItem::FunctionCall { name, call_id, .. } => {
                assert_eq!(name, "shell");
                assert!(
                    !call_id.is_empty(),
                    "call_id should not be empty, got: {:?}",
                    call_id
                );
                assert!(
                    call_id.starts_with("call_"),
                    "synthetic call_id should start with 'call_', got: {}",
                    call_id
                );
            }
            other => panic!("expected FunctionCall, got {:?}", other),
        }
    }

    #[test]
    fn test_sse_events_contain_expected_types() {
        let resp = ResponsesApiResponse {
            id: "resp-test".to_string(),
            response_type: "response".to_string(),
            status: "completed".to_string(),
            output: vec![ResponseItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Hi".to_string(),
                }],
                end_turn: Some(true),
                phase: None,
            }],
            usage: None,
        };

        let sse = build_sse_events(&resp);
        assert!(sse.contains("event: response.created"));
        assert!(sse.contains("event: response.output_item.done"));
        assert!(sse.contains("event: response.completed"));
    }

    #[test]
    fn test_streaming_strips_think_blocks_across_chunks() {
        let mut translator = StreamTranslator::new();

        let first = ChatStreamChunk {
            id: "chatcmpl-stream".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".to_string()),
                    content: Some("<think>\ninternal".to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
            model: None,
        };
        let second = ChatStreamChunk {
            id: "chatcmpl-stream".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: Some(" reasoning\n</think>\n\nHi".to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
            model: None,
        };
        let third = ChatStreamChunk {
            id: "chatcmpl-stream".to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: Some(" there!".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
            }),
            model: None,
        };

        let first_events = translator.process_chunk(&first);
        let second_events = translator.process_chunk(&second);
        let third_events = translator.process_chunk(&third);

        assert_eq!(first_events.len(), 1);
        assert!(first_events[0].contains("response.created"));

        assert_eq!(second_events.len(), 2);
        assert!(second_events[0].contains("response.output_item.added"));
        assert!(second_events[1].contains("\"delta\":\"Hi\""));

        assert!(third_events
            .iter()
            .any(|event| event.contains("\"delta\":\" there!\"")));
        assert!(third_events.iter().any(|event| {
            event.contains("\"text\":\"Hi there!\"") && event.contains("response.output_item.done")
        }));
        assert!(third_events
            .iter()
            .any(|event| event.contains("response.completed")));
        assert!(third_events.iter().all(|event| !event.contains("<think>")));
    }
}
