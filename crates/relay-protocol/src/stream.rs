use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{StopReason, Usage};

/// Normal response events only; failures are delivered separately, never as stops.
/// Indices are assigned by the bridge in item order within one response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamEvent {
    ResponseStart {
        /// Present only if sent by the provider, never synthesized.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Precedes the item's first delta, synthesized if the provider has no start.
    ItemStart {
        index: u32,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamItem {
    #[serde(deserialize_with = "deserialize_empty_item")]
    Text,
    ToolCall {
        /// Present only if the provider sends an ID; never synthesized.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
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
