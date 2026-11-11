use relay_protocol::{ProviderError, ProviderErrorCategory as Category, RetryAfter};
use serde_json::Value;

use crate::Headers;

#[derive(Clone, Copy)]
pub(crate) enum Provider {
    Anthropic,
    OpenAi,
    Compatible,
}

impl Provider {
    pub(crate) fn error_payload<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        if value["type"] == "response.failed" {
            return Some(&value["response"]["error"]);
        }
        if value["type"] == "error" {
            return Some(value.get("error").unwrap_or(value));
        }
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            return Some(error);
        }
        if value["status"] == "failed" {
            return Some(&value["error"]);
        }
        None
    }

    pub(crate) fn classify(
        self,
        payload: &Value,
        envelope: &Value,
        http_status: Option<u16>,
        headers: &Headers,
    ) -> ProviderError {
        let kind = payload["type"].as_str();
        let (code, category, request_id) = match self {
            Self::Anthropic => {
                let code = payload["details"]["error_code"].as_str().or(kind);
                let category = if code == Some("enforced_spend_limit_reached") {
                    Some(Category::InsufficientQuota)
                } else {
                    kind.and_then(anthropic_category)
                };
                let request_id = headers
                    .get("request-id")
                    .or_else(|| envelope["request_id"].as_str());
                (code, category, request_id)
            }
            Self::OpenAi | Self::Compatible => {
                let code = payload["code"].as_str();
                let category = code
                    .and_then(openai_code)
                    .or_else(|| kind.and_then(openai_type));
                (code, category, headers.get("x-request-id"))
            }
        };
        ProviderError {
            category: category.unwrap_or_else(|| status_category(http_status)),
            message: payload["message"]
                .as_str()
                .unwrap_or("provider returned an error")
                .into(),
            code: code.map(str::to_owned),
            http_status,
            request_id: request_id.map(str::to_owned),
            retry_after: retry_after(headers),
        }
    }
}

fn anthropic_category(kind: &str) -> Option<Category> {
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

fn openai_code(code: &str) -> Option<Category> {
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

fn openai_type(kind: &str) -> Option<Category> {
    Some(match kind {
        "insufficient_quota" => Category::InsufficientQuota,
        "rate_limit_error" => Category::RateLimited,
        "service_unavailable_error" => Category::Overloaded,
        "server_error" => Category::ServerError,
        "invalid_request_error" => Category::InvalidRequest,
        _ => return None,
    })
}

fn status_category(status: Option<u16>) -> Category {
    match status {
        Some(401) => Category::Authentication,
        Some(403) => Category::PermissionDenied,
        Some(429) => Category::RateLimited,
        Some(500..=599) => Category::ServerError,
        _ => Category::Unknown,
    }
}

fn retry_after(headers: &Headers) -> Option<RetryAfter> {
    if let Some(value) = headers.get("retry-after") {
        return Some(match value.parse() {
            Ok(seconds) => RetryAfter::Seconds { seconds },
            Err(_) => RetryAfter::HttpDate { date: value.into() },
        });
    }
    headers
        .get("retry-after-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|milliseconds| RetryAfter::Seconds {
            seconds: milliseconds.div_ceil(1000),
        })
}
