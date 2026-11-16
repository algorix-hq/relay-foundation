pub mod anthropic;
pub mod openai;
pub mod openai_compatible;

use relay_protocol::{
    ConfigurationError, DecodeError, ProviderError, ProviderErrorCategory as Category, RetryAfter,
};
use serde_json::Value;

use crate::base::required_url;
use crate::{Headers, ModelPage};

fn decode_models(
    body: &[u8],
    next_cursor: impl FnOnce(&Value) -> Result<Option<String>, DecodeError>,
) -> Result<ModelPage, DecodeError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| decode_error("", error.to_string()))?;
    let data = value["data"]
        .as_array()
        .ok_or_else(|| decode_error("/data", "expected model array"))?;
    let model_ids = data
        .iter()
        .enumerate()
        .map(|(index, model)| {
            model["id"].as_str().map(str::to_owned).ok_or_else(|| {
                decode_error(&format!("/data/{index}/id"), "expected model ID string")
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(ModelPage {
        model_ids,
        next_cursor: next_cursor(&value)?,
    })
}

fn decode_error(path: &str, message: impl Into<String>) -> DecodeError {
    DecodeError {
        path: path.into(),
        message: message.into(),
    }
}

fn join(base_url: &str, path: &str) -> Result<String, ConfigurationError> {
    let base_url = required_url(base_url, "base_url")?;
    Ok(format!("{}{path}", base_url.trim_end_matches('/')))
}

fn provider_error(
    payload: &Value,
    code: Option<&str>,
    category: Option<Category>,
    http_status: Option<u16>,
    request_id: Option<&str>,
    headers: &Headers,
) -> ProviderError {
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
