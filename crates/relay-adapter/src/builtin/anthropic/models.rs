use relay_protocol::{ConfigurationError, DecodeError};

use super::super::{decode_error, decode_models, join};
use super::AnthropicAdapter;
use crate::{Method, ModelDiscovery, ModelPage, ProviderRequest};

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
        decode_models(body, |value| {
            let has_more = value["has_more"]
                .as_bool()
                .ok_or_else(|| decode_error("/has_more", "expected boolean"))?;
            if !has_more {
                return Ok(None);
            }
            let last_id = value["last_id"]
                .as_str()
                .ok_or_else(|| decode_error("/last_id", "expected pagination cursor"))?;
            Ok(Some(last_id.into()))
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
