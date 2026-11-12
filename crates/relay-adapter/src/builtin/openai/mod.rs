pub(super) mod error;
mod models;

use std::sync::Arc;

use relay_bridge::{LossScope, ResponsesBridge, responses::ResponsesBridgeConfig};
use relay_protocol::{ConfigurationError, ProviderError};
use serde_json::Value;

use super::{Builtin, join};
use crate::{Adapter, Conversion, FrameDecoder, Headers, ModelDiscovery, ProviderRequest};

/// Uses Responses. Chat Completions on OpenAI goes through `OpenAiCompatibleAdapter`.
/// Retry-After takes precedence over retry-after-ms, which is rounded up to
/// whole seconds when no Retry-After is present.
pub struct OpenAiAdapter {
    inner: Builtin,
    base_url: String,
}

impl OpenAiAdapter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: Builtin::new(
                error::classify,
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

impl Adapter for OpenAiAdapter {
    fn conversion(&self) -> &Conversion {
        &self.inner.conversion
    }

    fn assemble_request(
        &self,
        _model: &str,
        body: Value,
        _stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        self.inner.post(join(&self.base_url, "/responses")?, body)
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
