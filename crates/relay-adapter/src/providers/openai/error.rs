use relay_protocol::{ProviderError, ProviderErrorCategory as Category};
use serde_json::Value;

use super::super::provider_error;
use crate::Headers;

pub(in crate::providers) fn classify(
    payload: &Value,
    _envelope: &Value,
    http_status: Option<u16>,
    headers: &Headers,
) -> ProviderError {
    let code = payload["code"].as_str();
    let category = code
        .and_then(code_category)
        .or_else(|| payload["type"].as_str().and_then(type_category));
    let request_id = headers.get("x-request-id");
    provider_error(payload, code, category, http_status, request_id, headers)
}

fn code_category(code: &str) -> Option<Category> {
    Some(match code {
        "invalid_api_key" => Category::Authentication,
        "model_not_found" => Category::ModelNotFound,
        "insufficient_quota"
        | "credit_balance_exhausted"
        | "organization_spend_limit_exceeded"
        | "project_spend_limit_exceeded"
        | "organization_usage_limit_exceeded" => Category::InsufficientQuota,
        "rate_limit_exceeded" => Category::RateLimited,
        "context_length_exceeded" => Category::ContextLengthExceeded,
        "server_is_overloaded" => Category::Overloaded,
        "server_error" => Category::ServerError,
        _ => return None,
    })
}

fn type_category(kind: &str) -> Option<Category> {
    Some(match kind {
        "insufficient_quota" => Category::InsufficientQuota,
        "rate_limit_error" => Category::RateLimited,
        "service_unavailable_error" => Category::Overloaded,
        "server_error" => Category::ServerError,
        "invalid_request_error" => Category::InvalidRequest,
        _ => return None,
    })
}
