//! Adapter-owned bridge conversion, request construction, streaming, and optional model discovery.
//! Adapters alone classify provider errors; consumers execute transport and own connection failures.

mod conversion;
mod error;
mod http;
mod stream;

pub use conversion::*;
pub use error::*;
pub use http::*;
pub use stream::*;

use relay_bridge::{Frame, Input, Output};
use relay_protocol::{ConversionError, ConversionLoss, ProviderError};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameItem {
    Data(Frame),
    Error(ProviderError),
}

/// Owns wire framing and detection of mid-stream provider errors. Malformed
/// framing is a conversion error; only normal data frames may reach the bridge.
/// Return data and provider errors in input order, preserving data before an error.
pub trait FrameDecoder: Send {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<FrameItem>, ConversionError>;
    fn finish(&mut self) -> Result<Vec<FrameItem>, ConversionError>;
}

pub trait Adapter: Send + Sync {
    /// Format support is validated once by `Conversion::new`, not declared twice.
    fn conversion(&self) -> &Conversion;

    /// Constructs endpoint, authentication, and headers, and may add required body
    /// fields. The bridge format is available through `conversion()`.
    fn assemble_request(
        &self,
        model: &str,
        body: Value,
        stream: bool,
    ) -> Result<ProviderRequest, ConversionError>;

    /// Sole classifier of non-stream provider errors, including error payloads
    /// on 2xx responses. Preserve original code, HTTP status, request ID, and
    /// Retry-After when available; uncertain classifications use `Unknown`.
    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError>;

    fn frame_decoder(&self) -> Box<dyn FrameDecoder>;

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        None
    }
}

pub trait ModelDiscovery: Send + Sync {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, ConversionError>;

    /// Decodes successes only; the adapter classifies errors with `check_response`.
    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, ConversionError>;
}

/// IDs and pagination only; an empty page is valid, not unsupported discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPage {
    pub model_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

/// Consumer operations supplied by Relay's blanket implementation, not adapters.
pub trait AdapterExt: Adapter {
    fn build_request(
        &self,
        input: &Input,
    ) -> Result<(ProviderRequest, Vec<ConversionLoss>), ConversionError> {
        let conversion = self.conversion();
        let encoded = conversion
            .bridge()
            .encode_request(input, conversion.options())?;
        let request = self.assemble_request(&input.request.model, encoded.body, input.stream)?;
        Ok((request, encoded.losses))
    }

    /// Also handles rejected streaming requests after the consumer reads their body.
    fn parse_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<Output, AdapterError> {
        self.check_response(status, headers, body)?;
        Ok(self.conversion().bridge().decode_response(body)?)
    }

    fn stream_decoder(&self) -> AdapterStream {
        AdapterStream {
            frames: self.frame_decoder(),
            decoder: self.conversion().bridge().stream_decoder(),
            terminal: false,
        }
    }

    /// `None` means unsupported discovery, not an empty result page.
    fn list_models(
        &self,
        cursor: Option<&str>,
    ) -> Option<Result<ProviderRequest, ConversionError>> {
        Some(self.model_discovery()?.build_list_models(cursor))
    }

    /// Provider errors are classified before successful discovery data is decoded.
    fn parse_models(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Option<Result<ModelPage, AdapterError>> {
        let discovery = self.model_discovery()?;
        Some(
            self.check_response(status, headers, body)
                .map_err(AdapterError::from)
                .and_then(|()| discovery.decode_models(body).map_err(AdapterError::from)),
        )
    }
}

impl<A: Adapter + ?Sized> AdapterExt for A {}
