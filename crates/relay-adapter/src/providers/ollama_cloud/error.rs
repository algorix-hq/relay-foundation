use relay_protocol::{ProviderError, ProviderErrorCategory as Category};
use serde_json::Value;

use super::super::openai;
use crate::Headers;

pub(super) fn classify(
    payload: &Value,
    envelope: &Value,
    http_status: Option<u16>,
    headers: &Headers,
) -> ProviderError {
    let mut error = openai::error::classify(payload, envelope, http_status, headers);
    if http_status == Some(404) && payload["type"] == "not_found_error" {
        error.category = Category::ModelNotFound;
    }
    error
}
