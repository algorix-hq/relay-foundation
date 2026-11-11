use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{AssistantContent, Version};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: Version,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub created_at: Timestamp,
    pub model: String,
    pub content: Vec<AssistantContent>,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timestamp {
    /// Unix seconds when the adapter started receiving the response.
    pub observed_at: u64,
    /// Unix seconds sent by the provider, never estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_created_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    /// Normal output termination because the model's context window was filled.
    ContextWindowExceeded,
    ToolUse,
    StopSequence,
    ContentFilter,
    Refusal,
    Other(String),
}

impl Serialize for StopReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::ContextWindowExceeded => "context_window_exceeded",
            Self::ToolUse => "tool_use",
            Self::StopSequence => "stop_sequence",
            Self::ContentFilter => "content_filter",
            Self::Refusal => "refusal",
            Self::Other(value) => value,
        })
    }
}

impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "context_window_exceeded" => Self::ContextWindowExceeded,
            "tool_use" => Self::ToolUse,
            "stop_sequence" => Self::StopSequence,
            "content_filter" => Self::ContentFilter,
            "refusal" => Self::Refusal,
            _ => Self::Other(value),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Total input tokens, including tokens read from and written to cache.
    /// Cache token counts are subsets of this total.
    pub input_tokens: u64,
    /// Total output tokens, including any reasoning tokens counted by the provider.
    pub output_tokens: u64,
    /// Subset of input tokens served from an existing cache, not an extra charge count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Subset of input tokens used to create cache entries, across all cache TTLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Subset of output tokens used for reasoning, when separately reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}
