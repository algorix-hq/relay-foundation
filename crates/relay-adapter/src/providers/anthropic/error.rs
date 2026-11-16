use relay_protocol::{ProviderError, ProviderErrorCategory as Category};
use serde_json::Value;

use super::super::provider_error;
use crate::Headers;

pub(super) fn classify(
    payload: &Value,
    envelope: &Value,
    http_status: Option<u16>,
    headers: &Headers,
) -> ProviderError {
    let kind = payload["type"].as_str();
    let code = payload["details"]["error_code"].as_str().or(kind);
    let category = if code == Some("enforced_spend_limit_reached") {
        Some(Category::InsufficientQuota)
    } else {
        kind.and_then(category)
    };
    let request_id = headers
        .get("request-id")
        .or_else(|| envelope["request_id"].as_str());
    provider_error(payload, code, category, http_status, request_id, headers)
}

fn category(kind: &str) -> Option<Category> {
    Some(match kind {
        "invalid_request_error" | "request_too_large" => Category::InvalidRequest,
        "authentication_error" => Category::Authentication,
        "permission_error" => Category::PermissionDenied,
        "rate_limit_error" => Category::RateLimited,
        "api_error" => Category::ServerError,
        "overloaded_error" => Category::Overloaded,
        _ => return None,
    })
}
