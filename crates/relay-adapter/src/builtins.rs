use std::sync::Arc;

use relay_bridge::{
    ApiFormat, Bridge, ChatCompletionsBridge, ConversionOptions, LossScope, MessagesBridge,
    ResponsesBridge, chat_completions::MaxTokensField, responses::ResponsesBridgeConfig,
};
use relay_protocol::{ConfigurationError, DecodeError, ProviderError, Request};
use serde_json::Value;

use crate::{
    Adapter, Conversion, FrameDecoder, FrameItem, Headers, Method, ModelDiscovery, ModelPage,
    ProviderRequest, SseDecoder, classification::Provider,
};

/// Uses Messages with Bearer authentication and version `2026-10-21` by default.
/// Retry-After seconds or dates are preserved; when only retry-after-ms is
/// present, milliseconds are rounded up to whole seconds.
pub struct AnthropicAdapter {
    inner: Builtin,
    base_url: String,
}

/// Uses Responses. Chat Completions on OpenAI goes through `OpenAiCompatibleAdapter`.
/// Retry-After takes precedence over retry-after-ms, which is rounded up to
/// whole seconds when no Retry-After is present.
pub struct OpenAiAdapter {
    inner: Builtin,
    base_url: String,
}

/// `new` takes the full request URL. Defaults to Chat Completions with
/// `max_tokens`, not `max_completion_tokens`, as documented by Ollama;
/// `with_bridge` accepts any Chat Completions or Responses bridge.
/// `api_error`, `not_found_error`, and all unrecognized types are ambiguous on
/// compatible servers and use HTTP-status fallback, never message matching.
/// Retry-After takes precedence over retry-after-ms; milliseconds alone are
/// rounded up to whole seconds.
pub struct OpenAiCompatibleAdapter {
    inner: Builtin,
    url: String,
    models_url: Option<String>,
    without_tool_choice: bool,
}

struct Builtin {
    conversion: Conversion,
    provider: Provider,
    headers: Headers,
}

impl AnthropicAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        let mut inner = Builtin::new(
            Provider::Anthropic,
            Arc::new(MessagesBridge::new()),
            Some(api_key.into()),
        );
        inner.set_header("anthropic-version", "2026-10-21".into());
        Self {
            inner,
            base_url: "https://api.anthropic.com".into(),
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.inner.set_header("anthropic-version", version.into());
        self
    }

    /// Used only when a request omits `parameters.max_output_tokens`.
    pub fn default_max_tokens(mut self, max_tokens: u64) -> Self {
        self.inner
            .conversion
            .set_bridge(Arc::new(MessagesBridge::with_default_max_tokens(
                max_tokens,
            )));
        self
    }

    fn request_url(&self) -> Result<String, ConfigurationError> {
        join(&self.base_url, "/v1/messages")
    }

    fn discovery(&self) -> Option<&dyn ModelDiscovery> {
        Some(self)
    }
}

impl OpenAiAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: Builtin::new(
                Provider::OpenAi,
                Arc::new(ResponsesBridge::new()),
                Some(api_key.into()),
            ),
            base_url: "https://api.openai.com/v1".into(),
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn organization(mut self, organization: impl Into<String>) -> Self {
        self.inner
            .set_header("openai-organization", organization.into());
        self
    }

    pub fn project(mut self, project: impl Into<String>) -> Self {
        self.inner.set_header("openai-project", project.into());
        self
    }

    pub fn store(mut self, store: bool) -> Self {
        self.inner
            .conversion
            .set_bridge(Arc::new(ResponsesBridge::with_config(
                ResponsesBridgeConfig { store: Some(store) },
            )));
        self
    }

    fn request_url(&self) -> Result<String, ConfigurationError> {
        join(&self.base_url, "/responses")
    }

    fn discovery(&self) -> Option<&dyn ModelDiscovery> {
        Some(self)
    }
}

impl OpenAiCompatibleAdapter {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            inner: Builtin::new(
                Provider::Compatible,
                Arc::new(ChatCompletionsBridge::with_max_tokens_field(
                    MaxTokensField::MaxTokens,
                )),
                None,
            ),
            url: url.into(),
            models_url: None,
            without_tool_choice: false,
        }
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.inner
            .set_header("authorization", format!("Bearer {}", api_key.into()));
        self
    }

    pub fn with_bridge(mut self, bridge: Arc<dyn Bridge>) -> Result<Self, ConfigurationError> {
        self.inner
            .conversion
            .replace_bridge(bridge, &[ApiFormat::CHAT_COMPLETIONS, ApiFormat::RESPONSES])?;
        Ok(self)
    }

    /// Enables model discovery; without it `list_models` is unsupported.
    pub fn models_url(mut self, models_url: impl Into<String>) -> Self {
        self.models_url = Some(models_url.into());
        self
    }

    /// Declares a server restriction, not a model capability. A request with any
    /// explicit tool choice is rejected before conversion or request assembly.
    pub fn without_tool_choice(mut self) -> Self {
        self.without_tool_choice = true;
        self
    }

    fn request_url(&self) -> Result<String, ConfigurationError> {
        required_url(&self.url, "url")
    }

    fn discovery(&self) -> Option<&dyn ModelDiscovery> {
        self.models_url
            .as_ref()
            .map(|_| self as &dyn ModelDiscovery)
    }
}

macro_rules! adapter {
    ($name:ident, $validate:expr) => {
        impl $name {
            pub fn allow_loss(mut self, scopes: impl IntoIterator<Item = LossScope>) -> Self {
                self.inner.conversion.allow_loss(scopes);
                self
            }

            pub fn with_id_generator(
                mut self,
                generator: impl Fn() -> String + Send + Sync + 'static,
            ) -> Self {
                self.inner.conversion = self.inner.conversion.with_id_generator(generator);
                self
            }

            pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
                self.inner.conversion = self.inner.conversion.with_clock(clock);
                self
            }
        }

        impl Adapter for $name {
            fn conversion(&self) -> &Conversion {
                &self.inner.conversion
            }

            fn validate_request(&self, request: &Request) -> Result<(), ConfigurationError> {
                ($validate)(self, request)
            }

            fn assemble_request(
                &self,
                _model: &str,
                body: Value,
                _stream: bool,
            ) -> Result<ProviderRequest, ConfigurationError> {
                let body = serde_json::to_vec(&body).map_err(|error| ConfigurationError {
                    message: error.to_string(),
                })?;
                Ok(self.inner.request(Method::Post, self.request_url()?, body))
            }

            fn check_response(
                &self,
                status: u16,
                headers: &Headers,
                body: &[u8],
            ) -> Result<(), ProviderError> {
                let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                let payload = self.inner.provider.error_payload(&value);
                if !(200..300).contains(&status) || payload.is_some() {
                    return Err(self.inner.provider.classify(
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
                    provider: self.inner.provider,
                    headers: headers.clone(),
                })
            }

            fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
                self.discovery()
            }
        }
    };
}

adapter!(AnthropicAdapter, |_: &AnthropicAdapter, _: &Request| Ok(()));
adapter!(OpenAiAdapter, |_: &OpenAiAdapter, _: &Request| Ok(()));
adapter!(
    OpenAiCompatibleAdapter,
    |adapter: &OpenAiCompatibleAdapter, request: &Request| {
        if adapter.without_tool_choice
            && request
                .tools
                .as_ref()
                .is_some_and(|tools| tools.choice.is_some())
        {
            return Err(ConfigurationError {
                message: "server does not support tool_choice (tools.choice)".into(),
            });
        }
        Ok(())
    }
);

impl ModelDiscovery for AnthropicAdapter {
    fn build_list_models(
        &self,
        cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError> {
        let mut path = "/v1/models?limit=1000".to_owned();
        if let Some(cursor) = cursor {
            path.push_str("&after_id=");
            path.push_str(&query_value(cursor));
        }
        Ok(self
            .inner
            .request(Method::Get, join(&self.base_url, &path)?, Vec::new()))
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        self.inner.decode_models(body)
    }
}

impl ModelDiscovery for OpenAiAdapter {
    fn build_list_models(
        &self,
        _cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError> {
        Ok(self
            .inner
            .request(Method::Get, join(&self.base_url, "/models")?, Vec::new()))
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        self.inner.decode_models(body)
    }
}

impl ModelDiscovery for OpenAiCompatibleAdapter {
    fn build_list_models(
        &self,
        _cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError> {
        let url = required_url(self.models_url.as_deref().unwrap_or_default(), "models_url")?;
        Ok(self.inner.request(Method::Get, url, Vec::new()))
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        self.inner.decode_models(body)
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

impl Builtin {
    fn new(provider: Provider, bridge: Arc<dyn Bridge>, api_key: Option<String>) -> Self {
        let mut inner = Self {
            conversion: Conversion::from_bridge(bridge, ConversionOptions::default()),
            provider,
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

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
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
        let next_cursor = if matches!(self.provider, Provider::Anthropic) {
            let has_more = value["has_more"]
                .as_bool()
                .ok_or_else(|| decode_error("/has_more", "expected boolean"))?;
            if has_more {
                Some(
                    value["last_id"]
                        .as_str()
                        .ok_or_else(|| decode_error("/last_id", "expected pagination cursor"))?
                        .into(),
                )
            } else {
                None
            }
        } else {
            None
        };
        Ok(ModelPage {
            model_ids,
            next_cursor,
        })
    }
}

fn query_value(value: &str) -> String {
    let mut encoded = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    encoded
}

fn decode_error(path: &str, message: impl Into<String>) -> DecodeError {
    DecodeError {
        path: path.into(),
        message: message.into(),
    }
}

struct ProviderFrames {
    sse: SseDecoder,
    provider: Provider,
    headers: Headers,
}

impl ProviderFrames {
    fn classify(&self, frames: Vec<relay_bridge::Frame>, out: &mut Vec<FrameItem>) {
        for frame in frames {
            let value: Value = serde_json::from_str(&frame.data).unwrap_or(Value::Null);
            let payload = self.provider.error_payload(&value);
            if frame.event.as_deref() == Some("error")
                || frame.event.as_deref() == Some("response.failed")
                || payload.is_some()
            {
                out.push(FrameItem::Error(self.provider.classify(
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
