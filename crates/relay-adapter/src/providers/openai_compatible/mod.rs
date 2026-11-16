mod models;

use std::sync::Arc;

use relay_bridge::{
    ApiFormat, Bridge, ChatCompletionsBridge, LossScope, chat_completions::MaxTokensField,
};
use relay_protocol::{ConfigurationError, ProviderError, Request};
use serde_json::Value;

use super::openai;
use crate::{
    Adapter, BaseAdapter, Classify, Conversion, FrameDecoder, Headers, ModelDiscovery,
    ProviderRequest,
};

/// `new` takes the full request URL. Defaults to Chat Completions with
/// `max_tokens`, not `max_completion_tokens`, as documented by Ollama;
/// `with_bridge` accepts any Chat Completions or Responses bridge.
/// `api_error`, `not_found_error`, and all unrecognized types are ambiguous on
/// compatible servers and use HTTP-status fallback, never message matching.
/// Retry-After takes precedence over retry-after-ms; milliseconds alone are
/// rounded up to whole seconds.
pub struct OpenAiCompatibleAdapter {
    inner: BaseAdapter,
    models_url: Option<String>,
    without_tool_choice: bool,
}

impl OpenAiCompatibleAdapter {
    pub fn new(url: impl Into<String>) -> Self {
        Self::from_parts(
            url,
            Arc::new(ChatCompletionsBridge::with_max_tokens_field(
                MaxTokensField::MaxTokens,
            )),
            openai::error::classify,
        )
    }

    pub(in crate::providers) fn from_parts(
        url: impl Into<String>,
        bridge: Arc<ChatCompletionsBridge>,
        classify: Classify,
    ) -> Self {
        Self {
            inner: BaseAdapter::new(bridge, url, classify),
            models_url: None,
            without_tool_choice: false,
        }
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.inner
            .set_header("authorization", format!("Bearer {}", api_key.into()));
        self
    }

    pub fn with_bridge(self, bridge: Arc<dyn Bridge>) -> Result<Self, ConfigurationError> {
        self.with_bridge_formats(bridge, &[ApiFormat::CHAT_COMPLETIONS, ApiFormat::RESPONSES])
    }

    pub(in crate::providers) fn with_bridge_formats(
        mut self,
        bridge: Arc<dyn Bridge>,
        supported_formats: &[ApiFormat],
    ) -> Result<Self, ConfigurationError> {
        self.inner
            .conversion
            .replace_bridge(bridge, supported_formats)?;
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

impl Adapter for OpenAiCompatibleAdapter {
    fn conversion(&self) -> &Conversion {
        &self.inner.conversion
    }

    fn validate_request(&self, request: &Request) -> Result<(), ConfigurationError> {
        let has_tool_choice = request
            .tools
            .as_ref()
            .is_some_and(|tools| tools.choice.is_some());
        if self.without_tool_choice && has_tool_choice {
            return Err(ConfigurationError {
                message: "server does not support tool_choice (tools.choice)".into(),
            });
        }
        Ok(())
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
        self.models_url
            .as_ref()
            .map(|_| self as &dyn ModelDiscovery)
    }
}
