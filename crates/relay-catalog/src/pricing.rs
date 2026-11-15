use serde::{Deserialize, Serialize};

use crate::string::deserialize_non_empty;

/// Missing optional fields mean "not verified", not free.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<Price>,
}

/// `amount / 10^scale` in `currency` per 1M tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub amount: u64,
    pub scale: u32,
    #[serde(deserialize_with = "deserialize_non_empty")]
    pub currency: String,
}
