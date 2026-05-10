//! Translation module - unified request/response conversion.
//!
//! Merges functionality from request_converter.rs and response_converter.rs

use crate::error::AdapterError;
use uuid::Uuid;

/// Direction of translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationDirection {
    /// Responses API → Chat Completions API
    Request,
    /// Chat Completions API → Responses API
    Response,
}

/// Normalize a tool call ID.
/// If `id` is empty, generates a synthetic ID with `call_` prefix.
pub fn normalize_tool_id(id: &str) -> String {
    if id.is_empty() {
        format!("call_{}", Uuid::new_v4())
    } else {
        id.to_string()
    }
}

/// Translation service for converting between API formats.
pub struct TranslationService;

impl TranslationService {
    pub fn new() -> Self {
        Self
    }

    /// Normalize tool ID for request conversion.
    pub fn normalize_request_tool_id(&self, id: &str) -> String {
        normalize_tool_id(id)
    }

    /// Normalize tool ID for response conversion.
    pub fn normalize_response_tool_id(&self, id: &str) -> String {
        normalize_tool_id(id)
    }
}

impl Default for TranslationService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_tool_id_empty_generates_synthetic() {
        let result = normalize_tool_id("");
        assert!(!result.is_empty(), "empty id should produce non-empty id");
        assert!(
            result.starts_with("call_"),
            "synthetic id should start with 'call_'"
        );
    }

    #[test]
    fn test_normalize_tool_id_non_empty_returns_original() {
        let original = "call_abc123";
        let result = normalize_tool_id(original);
        assert_eq!(result, original);
    }

    #[test]
    fn test_translation_service_normalize() {
        let service = TranslationService::new();
        let result = service.normalize_request_tool_id("");
        assert!(result.starts_with("call_"));
        
        let result2 = service.normalize_response_tool_id("test_id");
        assert_eq!(result2, "test_id");
    }
}
