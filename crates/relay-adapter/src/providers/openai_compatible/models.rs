use relay_protocol::{ConfigurationError, DecodeError};

use super::super::{decode_models, required_url};
use super::OpenAiCompatibleAdapter;
use crate::{Method, ModelDiscovery, ModelPage, ProviderRequest};

impl ModelDiscovery for OpenAiCompatibleAdapter {
    fn build_list_models(
        &self,
        _cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError> {
        let url = required_url(self.models_url.as_deref().unwrap_or_default(), "models_url")?;
        Ok(self.inner.request(Method::Get, url, Vec::new()))
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        decode_models(body, |_| Ok(None))
    }
}
