use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    Pricing, Slug, Spec,
    string::{
        deserialize_non_empty, deserialize_non_empty_strings, deserialize_optional_non_empty,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Auth {
    Byok,
    Subscription,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: Slug,
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_non_empty"
    )]
    pub symbol: Option<String>,
    pub auth: Auth,
    /// Wire-format identifiers matching `relay_bridge::ApiFormat` strings, such as `chat-completions`, `responses`, and `messages`.
    #[serde(deserialize_with = "deserialize_non_empty_strings")]
    pub api_formats: Vec<String>,
}

/// Keyed by `(provider, upstream_slug)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub provider: Slug,
    /// The exact string sent as the request model.
    #[serde(deserialize_with = "deserialize_non_empty")]
    pub upstream_slug: String,
    pub model: Slug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Spec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
}

/// A provider-defined model name that routes to models it may not disclose; keyed by `(provider, upstream_slug)` together with `ProviderModel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelAlias {
    pub provider: Slug,
    /// The exact string sent as the request model.
    #[serde(deserialize_with = "deserialize_non_empty")]
    pub upstream_slug: String,
    pub name: String,
    /// Models the alias may route to; `None` means undisclosed. Never names the model that served a given request.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_alias_models"
    )]
    pub models: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Spec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
}

pub(crate) fn deserialize_alias_models<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Slug>>, D::Error> {
    let value = Option::<Vec<Slug>>::deserialize(deserializer)?;
    if value.as_ref().is_some_and(Vec::is_empty) {
        return Err(serde::de::Error::custom(
            "models must not be empty; omit it when undisclosed",
        ));
    }
    Ok(value)
}
