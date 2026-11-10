//! Provider request construction, wire framing, optional model discovery, and sole
//! ownership of provider error normalization, composed with bridges without transport execution.

mod http;
mod pipeline;

pub use http::*;
pub use pipeline::*;

use relay_bridge::{ApiFormat, Frame};
use relay_protocol::{ConversionError, ProviderError};
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
    fn supported_formats(&self) -> &[ApiFormat];

    /// Constructs endpoint, authentication, headers, and body. The pipeline
    /// guarantees `format` is supported. Invalid construction returns `invalid`.
    fn build_request(
        &self,
        format: &ApiFormat,
        model: &str,
        body: &Value,
        stream: bool,
    ) -> Result<HttpRequest, ConversionError>;

    /// Sole classifier of non-stream provider errors, including error payloads
    /// on 2xx responses. Preserve original code, HTTP status, request ID, and
    /// Retry-After when available; uncertain classifications use `Unknown`.
    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError>;

    fn frame_decoder(&self, format: &ApiFormat) -> Box<dyn FrameDecoder>;

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        None
    }
}

pub trait ModelDiscovery: Send + Sync {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<HttpRequest, ConversionError>;

    /// Classifies provider failures and converts successful discovery responses;
    /// it must preserve the same diagnostics as `Adapter::check_response`.
    fn parse_list_models(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<ModelPage, DiscoveryError>;
}

/// IDs and pagination only; an empty page is valid, not unsupported discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPage {
    pub model_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

pub type DiscoveryError = PipelineError;
