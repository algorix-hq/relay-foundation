mod anthropic;
mod openai;
mod openai_compatible;

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAiAdapter;
pub use openai_compatible::OpenAiCompatibleAdapter;

use std::sync::Arc;

use relay_bridge::{Bridge, ConversionOptions, Frame};
use relay_protocol::{
    ConfigurationError, DecodeError, ProviderError, ProviderErrorCategory as Category, RetryAfter,
};
use serde_json::Value;

use crate::{
    Conversion, FrameDecoder, FrameItem, Headers, Method, ModelPage, ProviderRequest, SseDecoder,
};

type Classify = fn(&Value, &Value, Option<u16>, &Headers) -> ProviderError;

struct Builtin {
    conversion: Conversion,
    classify: Classify,
    headers: Headers,
}

impl Builtin {
    fn new(classify: Classify, bridge: Arc<dyn Bridge>, api_key: Option<String>) -> Self {
        let mut inner = Self {
            conversion: Conversion::from_bridge(bridge, ConversionOptions::default()),
            classify,
            headers: Headers(vec![("content-type".into(), "application/json".into())]),
        };
        if let Some(api_key) = api_key {
            inner.set_header("authorization", format!("Bearer {api_key}"));
        }
        inner
    }

    fn set_header(&mut self, name: &str, value: String) {
        if let Some((_, current)) = self.headers.0.iter_mut().find(|(key, _)| key == name) {
            *current = value;
        } else {
            self.headers.0.push((name.into(), value));
        }
    }

    fn request(&self, method: Method, url: String, body: Vec<u8>) -> ProviderRequest {
        ProviderRequest {
            method,
            url,
            headers: self.headers.clone(),
            body,
        }
    }

    fn post(&self, url: String, body: Value) -> Result<ProviderRequest, ConfigurationError> {
        let body = serde_json::to_vec(&body).map_err(|error| ConfigurationError {
            message: error.to_string(),
        })?;
        Ok(self.request(Method::Post, url, body))
    }

    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError> {
        let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let payload = error_payload(&value);
        if !(200..300).contains(&status) || payload.is_some() {
            return Err((self.classify)(
                payload.unwrap_or(&Value::Null),
                &value,
                Some(status),
                headers,
            ));
        }
        Ok(())
    }

    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder> {
        Box::new(ProviderFrames {
            sse: SseDecoder::new(),
            classify: self.classify,
            headers: headers.clone(),
        })
    }
}

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

fn required_url(url: &str, name: &str) -> Result<String, ConfigurationError> {
    if url.trim().is_empty() {
        return Err(ConfigurationError {
            message: format!("{name} is required"),
        });
    }
    Ok(url.to_owned())
}

fn error_payload(value: &Value) -> Option<&Value> {
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

struct ProviderFrames {
    sse: SseDecoder,
    classify: Classify,
    headers: Headers,
}

impl ProviderFrames {
    fn classify(&self, frames: Vec<Frame>, out: &mut Vec<FrameItem>) {
        for frame in frames {
            let value: Value = serde_json::from_str(&frame.data).unwrap_or(Value::Null);
            let payload = error_payload(&value);
            if frame.event.as_deref() == Some("error")
                || frame.event.as_deref() == Some("response.failed")
                || payload.is_some()
            {
                out.push(FrameItem::Error((self.classify)(
                    payload.unwrap_or(&value),
                    &value,
                    None,
                    &self.headers,
                )));
                break;
            }
            out.push(FrameItem::Data(frame));
        }
    }
}

impl FrameDecoder for ProviderFrames {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        let mut frames = Vec::new();
        let result = self.sse.push(bytes, &mut frames);
        self.classify(frames, out);
        result
    }

    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        let mut frames = Vec::new();
        let result = self.sse.finish(&mut frames);
        self.classify(frames, out);
        result
    }
}
