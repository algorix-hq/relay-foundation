use relay_bridge::Frame;
use relay_protocol::Error;

use crate::Headers;

/// Client-format wire mapping for already-normalized errors; never classifies failures.
/// Built-ins return upstream IDs only as `x-relay-upstream-request-id`, never as
/// the client format's own request-ID header. Retry-After dates remain verbatim.
pub trait ErrorEncoder: Send + Sync {
    /// Encodes an error before the client response has started.
    fn encode_error(&self, error: &Error, stage: Stage) -> EncodedError;

    /// Encodes an error after the client stream has started.
    fn encode_stream_error(&self, error: &Error) -> Vec<Frame>;
}

/// A conversion failure during client request decoding (`Request`) versus result
/// encoding (`Response`) can map to different client status codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Request,
    Response,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedError {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}
