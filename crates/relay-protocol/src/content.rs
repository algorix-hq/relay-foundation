use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// Passed through unchanged; acceptance depends on the target model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ImageDetail(pub String);

impl<'de> Deserialize<'de> for ImageDetail {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            return Err(serde::de::Error::custom("image detail must not be empty"));
        }
        Ok(Self(value))
    }
}

/// Marks a cache breakpoint; an absent TTL keeps the target's default.
/// Bridges reject TTLs the target cannot express.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheControl {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_ttl_seconds"
    )]
    pub ttl_seconds: Option<u64>,
}

fn deserialize_ttl_seconds<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Option::<u64>::deserialize(deserializer)?;
    if value == Some(0) {
        return Err(serde::de::Error::custom(
            "ttl_seconds must be greater than zero",
        ));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename = "text")]
pub struct TextPart {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TextPart", deny_unknown_fields)]
struct TextPartFields {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

impl<'de> Deserialize<'de> for TextPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Text(#[serde(with = "TextPartFields")] TextPart),
        }

        let Wire::Text(part) = Wire::deserialize(deserializer)?;
        Ok(part)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePart {
    pub source: MediaSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentPart {
    pub source: MediaSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserContent {
    Text(#[serde(with = "TextPartFields")] TextPart),
    Image(ImagePart),
    Document(DocumentPart),
    ToolResult {
        tool_call_id: String,
        content: Vec<ToolResultContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolResultContent {
    Text(#[serde(with = "TextPartFields")] TextPart),
    Image(ImagePart),
    Document(DocumentPart),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssistantContent {
    Text {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolCall {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        call_id: String,
        name: String,
        /// Exact JSON text, preserving whitespace, key order, and duplicate keys.
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Reasoning {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        /// Presence, even when empty, differs from absence and must be preserved
        /// so bridges can restore the original block kind.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        summary: Vec<String>,
        /// Provider-issued value to send back unchanged in later requests;
        /// the protocol does not interpret it.
        #[serde(skip_serializing_if = "Option::is_none")]
        opaque: Option<String>,
    },
    /// Unrecognized provider output preserved so already-generated output is not lost.
    /// It has no neutral meaning and cannot be encoded into a request;
    /// bridges reject it as unrepresentable.
    Unknown {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        value: Value,
    },
}
