use serde::{Deserialize, Serialize};

use crate::{Slug, string::deserialize_optional_non_empty};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lab {
    pub id: Slug,
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_non_empty"
    )]
    pub symbol: Option<String>,
}
