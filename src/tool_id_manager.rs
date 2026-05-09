//! Tool Call ID Manager
//!
//! Handles normalization of tool call IDs from upstream providers.
//! Some providers (GLM, MiniMax) omit `tool_calls[].id` in SSE streams,
//! which causes Codex to forward empty IDs on the next turn and get rejected.
//! This module generates stable synthetic IDs (`call_{uuid}`) when needed.

use uuid::Uuid;

/// Normalize a tool call ID.
///
/// If `id` is empty, generates a synthetic ID with `call_` prefix.
/// Otherwise returns the original ID unchanged.
pub fn normalize_tool_id(id: &str) -> String {
    if id.is_empty() {
        format!("call_{}", Uuid::new_v4())
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_tool_id_empty_generates_synthetic() {
        let result = normalize_tool_id("");
        assert!(
            !result.is_empty(),
            "empty id should produce a non-empty synthetic id"
        );
        assert!(
            result.starts_with("call_"),
            "synthetic id should start with 'call_' prefix, got: {}",
            result
        );
    }

    #[test]
    fn test_normalize_tool_id_non_empty_returns_original() {
        let original = "call_abc123";
        let result = normalize_tool_id(original);
        assert_eq!(
            result, original,
            "non-empty id should be returned unchanged"
        );
    }
}
