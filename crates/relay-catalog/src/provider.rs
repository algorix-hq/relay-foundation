use serde::{Deserialize, Serialize};

use crate::{
    Pricing, Slug, Spec,
    string::{deserialize_non_empty, deserialize_optional_non_empty},
};

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
