use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeStruct,
};
use serde_json::Value;

use crate::CacheControl;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    pub definitions: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Tool {
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        parameters: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Function { name: String },
}

impl Serialize for ToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::None => serializer.serialize_str("none"),
            Self::Required => serializer.serialize_str("required"),
            Self::Function { name } => {
                let mut object = serializer.serialize_struct("ToolChoice", 2)?;
                object.serialize_field("type", "function")?;
                object.serialize_field("name", name)?;
                object.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ToolChoiceVisitor;

        impl<'de> Visitor<'de> for ToolChoiceVisitor {
            type Value = ToolChoice;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("auto, none, required, or a function tool choice object")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "auto" => Ok(ToolChoice::Auto),
                    "none" => Ok(ToolChoice::None),
                    "required" => Ok(ToolChoice::Required),
                    _ => Err(E::unknown_variant(value, &["auto", "none", "required"])),
                }
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                #[derive(Deserialize)]
                #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
                enum Object {
                    Function { name: String },
                }

                let Object::Function { name } =
                    Object::deserialize(MapAccessDeserializer::new(map))?;
                Ok(ToolChoice::Function { name })
            }
        }

        deserializer.deserialize_any(ToolChoiceVisitor)
    }
}
