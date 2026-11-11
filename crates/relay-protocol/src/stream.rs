use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{StopReason, Timestamp, Usage};

/// Normal response events only; failures are delivered separately, never as stops.
/// Indices follow ItemStart order. Item metadata, deltas, and end follow its start;
/// different items may interleave, and items need not have deltas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamEvent {
    Start {
        response_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        created_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Partial updates only: absent fields leave previous values unchanged; no deletion.
    Metadata(MetadataScope),
    /// Precedes the item's first delta, synthesized if the provider has no start.
    ItemStart {
        index: u32,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        item: StreamItem,
    },
    Delta {
        index: u32,
        delta: StreamDelta,
    },
    /// Emitted only on a provider end signal or an establishing normal finish,
    /// never synthesized on failure.
    ItemEnd {
        index: u32,
    },
    Stop {
        stop_reason: StopReason,
        /// Final cumulative total. Absence means no final usage, even if metadata
        /// previously reported a provisional total.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum MetadataScope {
    Response {
        /// Provisional cumulative total, never implicitly promoted to final usage.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Item {
        index: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamItem {
    #[serde(deserialize_with = "deserialize_empty_item")]
    Text,
    ToolCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(deserialize_with = "deserialize_empty_item")]
    Reasoning,
    /// Unrecognized provider output preserved so already-generated output is not lost.
    /// It has no neutral meaning and cannot be encoded into a request;
    /// bridges reject it as unrepresentable.
    #[serde(deserialize_with = "deserialize_empty_item")]
    Unknown,
}

fn deserialize_empty_item<'de, D: Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    // Serde ignores extra fields on internally tagged unit variants.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EmptyItem {}

    EmptyItem::deserialize(deserializer)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamDelta {
    Text {
        text: String,
    },
    ToolCallArguments {
        text: String,
    },
    ReasoningText {
        text: String,
    },
    ReasoningSummary {
        summary_index: u32,
        text: String,
    },
    /// Carries the complete opaque value for the item, not a fragment.
    ReasoningOpaque {
        value: String,
    },
    /// Unrecognized provider output preserved so already-generated output is not lost.
    /// It has no neutral meaning and cannot be encoded into a request;
    /// bridges reject it as unrepresentable.
    Unknown {
        value: Value,
    },
}
