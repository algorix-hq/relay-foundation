use std::fmt;

use relay_bridge::Frame;
use relay_protocol::{
    ConfigurationError, ConversionLoss, DecodeError, ProviderError, ProviderErrorCategory,
    RelayError, Request, Response, UnsupportedOperation,
};
use serde_json::Value;

use crate::{AdapterStream, Conversion, Headers, ProviderRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameItem {
    Data(Frame),
    Error(ProviderError),
}

/// Classifies every provider-declared stream failure, including typed events that
/// resemble normal data. Classify the error payload, never the HTTP 200 status;
/// mid-stream errors have `http_status: None`. Malformed framing yields `DecodeError`.
/// Items appended before an error remain in `out`, in input order.
pub trait FrameDecoder: Send {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError>;
    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError>;
}

pub trait Adapter: Send + Sync {
    fn conversion(&self) -> &Conversion;

    /// Checks consumer-declared server restrictions before bridge conversion.
    fn validate_request(&self, _request: &Request) -> Result<(), ConfigurationError> {
        Ok(())
    }

    fn assemble_request(
        &self,
        model: &str,
        body: Value,
        stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError>;

    /// Sole HTTP response classifier, including error payloads in 2xx responses.
    /// Must not depend on Content-Type: JSON error text may arrive as text/plain.
    /// Preserve available diagnostics; absent Retry-After is normal.
    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError>;

    /// Receives successful streaming response headers so the decoder can attach
    /// the response's request ID to mid-stream provider failures.
    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder>;

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        None
    }
}

pub trait ModelDiscovery: Send + Sync {
    fn build_list_models(
        &self,
        cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError>;

    /// Decodes successes only; the adapter classifies failures first.
    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError>;
}

/// IDs and pagination only; an empty page is valid, not unsupported discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPage {
    pub model_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

/// Consumer operations supplied once by the blanket implementation, not overridable by adapters.
pub trait AdapterExt: Adapter {
    fn build_request(
        &self,
        request: &Request,
    ) -> Result<(ProviderRequest, Vec<ConversionLoss>), RelayError>;

    fn parse_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<Response, RelayError>;

    /// A rejected response must be classified after the consumer reads its body.
    fn open_stream(
        &self,
        status: u16,
        headers: &Headers,
    ) -> Result<AdapterStream, RejectedResponse<'_>>;

    fn list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, RelayError>;

    fn parse_models(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<ModelPage, RelayError>;
}

impl<A: Adapter + ?Sized> AdapterExt for A {
    fn build_request(
        &self,
        request: &Request,
    ) -> Result<(ProviderRequest, Vec<ConversionLoss>), RelayError> {
        self.validate_request(request)?;
        let conversion = self.conversion();
        let encoded = conversion
            .bridge()
            .encode_request(request, conversion.options())?;
        let request = self.assemble_request(
            &request.model,
            encoded.body,
            request.delivery.is_some_and(|delivery| delivery.stream),
        )?;
        Ok((request, encoded.losses))
    }

    fn parse_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<Response, RelayError> {
        let context = self.conversion().decode_context();
        check_response(self, status, headers, body)?;
        Ok(self.conversion().bridge().decode_response(body, &context)?)
    }

    fn open_stream(
        &self,
        status: u16,
        headers: &Headers,
    ) -> Result<AdapterStream, RejectedResponse<'_>> {
        if !(200..300).contains(&status) {
            let headers = headers.clone();
            return Err(RejectedResponse {
                status,
                classify: Box::new(move |body| {
                    self.check_response(status, &headers, body)
                        .err()
                        .unwrap_or_else(|| unknown_http_error(status))
                }),
            });
        }
        Ok(AdapterStream {
            frames: self.frame_decoder(headers),
            decoder: self
                .conversion()
                .bridge()
                .stream_decoder(&self.conversion().decode_context()),
            terminal: false,
        })
    }

    fn list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, RelayError> {
        let discovery = self.model_discovery().ok_or_else(unsupported_discovery)?;
        Ok(discovery.build_list_models(cursor)?)
    }

    fn parse_models(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<ModelPage, RelayError> {
        let discovery = self.model_discovery().ok_or_else(unsupported_discovery)?;
        check_response(self, status, headers, body)?;
        Ok(discovery.decode_models(body)?)
    }
}

/// A non-success streaming response awaiting its error body.
pub struct RejectedResponse<'a> {
    status: u16,
    classify: Box<RejectionClassifier<'a>>,
}

type RejectionClassifier<'a> = dyn FnOnce(&[u8]) -> ProviderError + Send + 'a;

impl RejectedResponse<'_> {
    /// Always returns `RelayError::Provider`, even for an empty or undecodable body.
    pub fn classify(self, body: &[u8]) -> RelayError {
        (self.classify)(body).into()
    }
}

impl fmt::Debug for RejectedResponse<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RejectedResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

fn check_response<A: Adapter + ?Sized>(
    adapter: &A,
    status: u16,
    headers: &Headers,
    body: &[u8],
) -> Result<(), ProviderError> {
    adapter.check_response(status, headers, body)?;
    if !(200..300).contains(&status) {
        return Err(unknown_http_error(status));
    }
    Ok(())
}

fn unknown_http_error(status: u16) -> ProviderError {
    ProviderError {
        category: ProviderErrorCategory::Unknown,
        message: "provider returned an unsuccessful HTTP response".into(),
        code: None,
        http_status: Some(status),
        request_id: None,
        retry_after: None,
    }
}

fn unsupported_discovery() -> UnsupportedOperation {
    UnsupportedOperation {
        operation: "list_models".into(),
        message: "model discovery is not provided by this adapter".into(),
    }
}
