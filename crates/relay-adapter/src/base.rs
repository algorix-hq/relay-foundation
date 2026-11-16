use std::sync::Arc;

use relay_bridge::{Bridge, ConversionOptions, Frame, LossScope};
use relay_protocol::{ConfigurationError, DecodeError, ProviderError};
use serde_json::Value;

use crate::{
    Adapter, Conversion, FrameDecoder, FrameItem, Headers, Method, ProviderRequest, SseDecoder,
};

pub type Classify = fn(
    payload: &Value,
    envelope: &Value,
    http_status: Option<u16>,
    headers: &Headers,
) -> ProviderError;

/// JSON POST requests and SSE framing with caller-supplied error classification.
/// Recognizes `type: "response.failed"` at `response.error`, `type: "error"`
/// at `error` or the envelope itself, non-null `error`, and `status: "failed"`.
/// SSE events named `error` or `response.failed` are also failures.
/// Implement `Adapter` directly when framing or the error envelope differs.
pub struct BaseAdapter {
    pub(crate) conversion: Conversion,
    classify: Classify,
    headers: Headers,
    url: String,
}

impl BaseAdapter {
    pub fn new(bridge: Arc<dyn Bridge>, url: impl Into<String>, classify: Classify) -> Self {
        Self {
            conversion: Conversion::from_bridge(bridge, ConversionOptions::default()),
            classify,
            headers: Headers(vec![("content-type".into(), "application/json".into())]),
            url: url.into(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_header(&name.into(), value.into());
        self
    }

    pub fn allow_loss(mut self, scopes: impl IntoIterator<Item = LossScope>) -> Self {
        self.conversion.allow_loss(scopes);
        self
    }

    pub fn with_id_generator(
        mut self,
        generator: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        self.conversion = self.conversion.with_id_generator(generator);
        self
    }

    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.conversion = self.conversion.with_clock(clock);
        self
    }

    pub(crate) fn set_header(&mut self, name: &str, value: String) {
        if let Some((_, current)) = self.headers.0.iter_mut().find(|(key, _)| key == name) {
            *current = value;
        } else {
            self.headers.0.push((name.into(), value));
        }
    }

    pub(crate) fn request(&self, method: Method, url: String, body: Vec<u8>) -> ProviderRequest {
        ProviderRequest {
            method,
            url,
            headers: self.headers.clone(),
            body,
        }
    }

    pub(crate) fn post(
        &self,
        url: String,
        body: Value,
    ) -> Result<ProviderRequest, ConfigurationError> {
        let body = serde_json::to_vec(&body).map_err(|error| ConfigurationError {
            message: error.to_string(),
        })?;
        Ok(self.request(Method::Post, url, body))
    }
}

impl Adapter for BaseAdapter {
    fn conversion(&self) -> &Conversion {
        &self.conversion
    }

    fn assemble_request(
        &self,
        _model: &str,
        body: Value,
        _stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        self.post(required_url(&self.url, "url")?, body)
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

pub(crate) fn required_url(url: &str, name: &str) -> Result<String, ConfigurationError> {
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
