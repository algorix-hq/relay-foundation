mod error;
mod models;

use std::sync::Arc;

use relay_bridge::{LossScope, MessagesBridge};
use relay_protocol::{ConfigurationError, ProviderError};
use serde_json::Value;

use super::{Builtin, join};
use crate::{Adapter, Conversion, FrameDecoder, Headers, ModelDiscovery, ProviderRequest};

/// Uses Messages with Bearer authentication and version `2026-10-21` by default.
/// Retry-After seconds or dates are preserved; when only retry-after-ms is
/// present, milliseconds are rounded up to whole seconds.
pub struct AnthropicAdapter {
    inner: Builtin,
    base_url: String,
}

impl AnthropicAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        let mut inner = Builtin::new(
            error::classify,
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

impl Adapter for AnthropicAdapter {
    fn conversion(&self) -> &Conversion {
        &self.inner.conversion
    }

    fn assemble_request(
        &self,
        _model: &str,
        body: Value,
        _stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        self.inner.post(join(&self.base_url, "/v1/messages")?, body)
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
        Some(self)
    }
}
