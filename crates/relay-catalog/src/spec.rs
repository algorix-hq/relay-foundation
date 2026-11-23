use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Missing optional fields mean "not verified" and must not alone block a request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tiers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
}

/// Serialized as `false` when unsupported and as an object when supported; `true` is rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reasoning {
    Unsupported,
    Supported(ReasoningSpec),
}

impl Serialize for Reasoning {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unsupported => serializer.serialize_bool(false),
            Self::Supported(spec) => spec.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Reasoning {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Flag(bool),
            Spec(ReasoningSpec),
        }

        match Wire::deserialize(deserializer)? {
            Wire::Flag(false) => Ok(Self::Unsupported),
            Wire::Flag(true) => Err(serde::de::Error::custom(
                "reasoning must be false or an object, not true",
            )),
            Wire::Spec(spec) => Ok(Self::Supported(spec)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<ReasoningBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Vec<ReasoningDisplay>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortSpec {
    pub levels: Vec<String>,
    /// The level the provider applies when a request names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_level: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningBudget {
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDisplay {
    Full,
    Summarized,
    Omitted,
}
