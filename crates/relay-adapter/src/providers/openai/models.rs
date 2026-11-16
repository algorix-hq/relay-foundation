use relay_protocol::{ConfigurationError, DecodeError};

use super::super::{decode_models, join};
use super::OpenAiAdapter;
use crate::{Method, ModelDiscovery, ModelPage, ProviderRequest};

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
        decode_models(body, |_| Ok(None))
    }
}
