use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

/// Relay failures, preserving the layer-owned cause. Transport failures and
/// cancellation are intentionally absent; consumers compose those themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayError {
    /// A declared remote failure normalized by the adapter.
    Provider(ProviderError),
    /// Meaning could not be preserved when converting between representations.
    Conversion(ConversionError),
    /// A successful response or stream frame could not be interpreted.
    Decode(DecodeError),
    /// Input ended without the stream's normal terminal signal.
    IncompleteStream(IncompleteStream),
    /// Required setup was missing or invalid.
    Configuration(ConfigurationError),
    /// An optional operation was not provided.
    UnsupportedOperation(UnsupportedOperation),
}

impl fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "provider error: {error}"),
            Self::Conversion(error) => write!(formatter, "conversion error: {error}"),
            Self::Decode(error) => write!(formatter, "decode error: {error}"),
            Self::IncompleteStream(error) => write!(formatter, "incomplete stream: {error}"),
            Self::Configuration(error) => write!(formatter, "configuration error: {error}"),
            Self::UnsupportedOperation(error) => {
                write!(formatter, "unsupported operation: {error}")
            }
        }
    }
}

impl Error for RelayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(error) => Some(error),
            Self::Conversion(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::IncompleteStream(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::UnsupportedOperation(error) => Some(error),
        }
    }
}

impl From<ProviderError> for RelayError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl From<ConversionError> for RelayError {
    fn from(error: ConversionError) -> Self {
        Self::Conversion(error)
    }
}

impl From<DecodeError> for RelayError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<IncompleteStream> for RelayError {
    fn from(error: IncompleteStream) -> Self {
        Self::IncompleteStream(error)
    }
}

impl From<ConfigurationError> for RelayError {
    fn from(error: ConfigurationError) -> Self {
        Self::Configuration(error)
    }
}

impl From<UnsupportedOperation> for RelayError {
    fn from(error: UnsupportedOperation) -> Self {
        Self::UnsupportedOperation(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecodeError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl Error for DecodeError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncompleteStream {
    pub message: String,
}

impl fmt::Display for IncompleteStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for IncompleteStream {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationError {
    pub message: String,
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ConfigurationError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedOperation {
    pub operation: String,
    pub message: String,
}

impl fmt::Display for UnsupportedOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl Error for UnsupportedOperation {}

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
