use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeStruct,
};
use serde_json::Value;

use crate::{Message, ToolConfig, Version};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: Version,
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Parameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
}

impl Serialize for ResponseFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text => serializer.serialize_str("text"),
            Self::JsonObject => serializer.serialize_str("json_object"),
            Self::JsonSchema {
                name,
                description,
                schema,
                strict,
            } => {
                let fields = 3 + usize::from(description.is_some()) + usize::from(strict.is_some());
                let mut object = serializer.serialize_struct("ResponseFormat", fields)?;
                object.serialize_field("type", "json_schema")?;
                object.serialize_field("name", name)?;
                if let Some(description) = description {
                    object.serialize_field("description", description)?;
                }
                object.serialize_field("schema", schema)?;
                if let Some(strict) = strict {
                    object.serialize_field("strict", strict)?;
                }
                object.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ResponseFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ResponseFormatVisitor;

        impl<'de> Visitor<'de> for ResponseFormatVisitor {
            type Value = ResponseFormat;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("text, json_object, or a json_schema response format object")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "text" => Ok(ResponseFormat::Text),
                    "json_object" => Ok(ResponseFormat::JsonObject),
                    _ => Err(E::unknown_variant(value, &["text", "json_object"])),
                }
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                #[derive(Deserialize)]
                #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
                enum Object {
                    JsonSchema {
                        name: String,
                        description: Option<String>,
                        schema: Value,
                        strict: Option<bool>,
                    },
                }

                let Object::JsonSchema {
                    name,
                    description,
                    schema,
                    strict,
                } = Object::deserialize(MapAccessDeserializer::new(map))?;
                Ok(ResponseFormat::JsonSchema {
                    name,
                    description,
                    schema,
                    strict,
                })
            }
        }

        deserializer.deserialize_any(ResponseFormatVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reasoning {
    /// Requested amount of reasoning; supported levels depend on the target model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Maximum tokens the model may spend on reasoning, counted within output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
    /// Requested level of detail for returned reasoning summaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
    /// Whether reasoning is enabled, disabled, or left to the model (`adaptive`),
    /// independent of effort and summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMode>,
}

/// Passed through unchanged; acceptance depends on the target model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReasoningEffort(pub String);

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            return Err(serde::de::Error::custom(
                "reasoning effort must not be empty",
            ));
        }
        Ok(Self(value))
    }
}

/// Passed through unchanged; acceptance depends on the target model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReasoningSummary(pub String);

impl<'de> Deserialize<'de> for ReasoningSummary {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            return Err(serde::de::Error::custom(
                "reasoning summary must not be empty",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    Enabled,
    Disabled,
    Adaptive,
}
