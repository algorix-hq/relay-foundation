mod error;

use std::sync::Arc;

use relay_bridge::{
    ChatCompletionsBridge, LossScope, ReasoningField, chat_completions::MaxTokensField,
};
use relay_protocol::{ConfigurationError, ProviderError, Request};
use serde_json::Value;

use super::openai_compatible::OpenAiCompatibleAdapter;
use crate::{Adapter, Conversion, FrameDecoder, Headers, ModelDiscovery, ProviderRequest};

/// Chat Completions at `https://ollama.com/v1` with `max_tokens` and the
/// `reasoning` text field. Requests with an explicit tool choice are rejected
/// because Ollama ignores `tool_choice`. A 404 `not_found_error` is
/// `ModelNotFound`; unknown paths return a different envelope and stay `Unknown`.
/// Use `OpenAiCompatibleAdapter` for other endpoints or the Responses API.
pub struct OllamaCloudAdapter {
    inner: OpenAiCompatibleAdapter,
}

impl OllamaCloudAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        let bridge = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens)
            .with_reasoning_field(ReasoningField::Reasoning);
        let inner = OpenAiCompatibleAdapter::from_parts(
            "https://ollama.com/v1/chat/completions",
            Arc::new(bridge),
            error::classify,
        )
        .api_key(api_key)
        .models_url("https://ollama.com/v1/models")
        .without_tool_choice();
        Self { inner }
    }

    pub fn allow_loss(self, scopes: impl IntoIterator<Item = LossScope>) -> Self {
        Self {
            inner: self.inner.allow_loss(scopes),
        }
    }

    pub fn with_id_generator(self, generator: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self {
            inner: self.inner.with_id_generator(generator),
        }
    }

    pub fn with_clock(self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            inner: self.inner.with_clock(clock),
        }
    }
}

impl Adapter for OllamaCloudAdapter {
    fn conversion(&self) -> &Conversion {
        self.inner.conversion()
    }

    fn validate_request(&self, request: &Request) -> Result<(), ConfigurationError> {
        self.inner.validate_request(request)
    }

    fn assemble_request(
        &self,
        model: &str,
        body: Value,
        stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        self.inner.assemble_request(model, body, stream)
    }

    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError> {
        self.inner.check_response(status, headers, body)
    }

    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder> {
        self.inner.frame_decoder(headers)
    }

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        self.inner.model_discovery()
    }
}
