//! Error types for the adapter.

use std::fmt;

/// Errors that can occur during request/response translation or proxying.
#[derive(Debug)]
pub enum AdapterError {
    /// The request contains a feature not supported by the adapter or provider.
    UnsupportedFeature(String),
    /// The request contains an unsupported role.
    UnsupportedRole(String),
    /// The upstream provider returned an error.
    UpstreamError { status: u16, body: String },
    /// Failed to parse a request or response.
    ParseError(String),
    /// Network or I/O error.
    TransportError(String),
    /// The requested capability is not available and downgrade is disabled.
    CapabilityNotAvailable(String),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFeature(msg) => write!(f, "unsupported feature: {msg}"),
            Self::UnsupportedRole(role) => write!(f, "unsupported role: {role}"),
            Self::UpstreamError { status, body } => {
                write!(f, "upstream error (HTTP {status}): {body}")
            }
            Self::ParseError(msg) => write!(f, "parse error: {msg}"),
            Self::TransportError(msg) => write!(f, "transport error: {msg}"),
            Self::CapabilityNotAvailable(msg) => write!(f, "capability not available: {msg}"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl AdapterError {
    /// HTTP status code to return to the client.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::UnsupportedFeature(_)
            | Self::UnsupportedRole(_)
            | Self::CapabilityNotAvailable(_) => 400,
            Self::UpstreamError { status, .. } => *status,
            Self::ParseError(_) => 400,
            Self::TransportError(_) => 502,
        }
    }
}
