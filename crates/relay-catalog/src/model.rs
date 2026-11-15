use serde::{Deserialize, Serialize};

use crate::{Slug, Spec};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: Slug,
    pub lab: Slug,
    pub name: String,
    /// The lab's published spec, for display only; never fills in `ProviderModel.spec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Spec>,
}
