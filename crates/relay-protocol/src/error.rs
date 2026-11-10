use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversionError {
    pub kind: ConversionErrorKind,
    /// JSON-pointer-like location in the offending input; empty means the root.
    pub path: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionErrorKind {
    /// Valid input that the target format cannot express.
    Unrepresentable,
    /// Malformed input.
    Invalid,
    /// A feature not implemented by the conversion.
    Unsupported,
}

impl fmt::Display for ConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl Error for ConversionError {}

/// An omission explicitly allowed by the consumer, not an implicit best effort.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversionLoss {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ConversionLoss {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl Error for ConversionLoss {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderError {
    pub category: ProviderErrorCategory,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<RetryAfter>,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ProviderError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorCategory {
    Authentication,
    PermissionDenied,
    InsufficientQuota,
    RateLimited,
    Overloaded,
    InvalidRequest,
    UnsupportedFeature,
    ModelNotFound,
    ContextLengthExceeded,
    ServerError,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetryAfter {
    Seconds {
        seconds: u64,
    },
    /// The HTTP date is preserved without consulting a clock.
    HttpDate {
        date: String,
    },
}
