//! Strict API-format conversion and normal stream events.
//! Bridge failures distinguish conversion, decoding, and incomplete streams; only adapters classify provider errors.

pub mod chat_completions;
mod error;
pub mod messages;
pub mod responses;

pub use chat_completions::ChatCompletionsBridge;
pub use error::BridgeError;
pub use messages::MessagesBridge;

use std::{borrow::Cow, collections::BTreeSet, fmt};

use relay_protocol::{ConfigurationError, ConversionLoss, Request, Response, StreamEvent};
use serde_json::Value;

/// A wire-format identifier outside protocol data, for compatibility checks.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiFormat(Cow<'static, str>);

impl ApiFormat {
    pub const CHAT_COMPLETIONS: Self = Self(Cow::Borrowed("chat-completions"));
    pub const RESPONSES: Self = Self(Cow::Borrowed("responses"));
    pub const MESSAGES: Self = Self(Cow::Borrowed("messages"));

    pub fn new(value: impl Into<Cow<'static, str>>) -> Result<Self, ConfigurationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ConfigurationError {
                message: "API format must not be empty".into(),
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApiFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeContext {
    /// Adapter-generated fallback, used only when the provider sends no response ID.
    pub response_id: Option<String>,
    pub observed_at: u64,
}

/// Empty by default: conversion is fully strict. Bridges must reject omissions
/// outside the allowed scopes and report every drop as a `ConversionLoss`.
/// Format-specific compatibility settings belong to concrete bridge constructors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversionOptions {
    pub allow_loss: BTreeSet<LossScope>,
}

/// Conversion losses a consumer may explicitly allow in a request, response, or stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LossScope {
    CacheControl,
    /// Developer messages use the target's system role when it has no developer role.
    DeveloperRole,
    ImageDetail,
    /// Reasoning content items, including their opaque values; not request settings.
    Reasoning,
    /// Sampling settings (temperature, top_p) the target format cannot express are dropped.
    Sampling,
    UnknownContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub body: Value,
    pub losses: Vec<ConversionLoss>,
}

pub trait Bridge: Send + Sync {
    fn format(&self) -> &ApiFormat;

    /// Strict ingress: malformed or unrepresentable input yields `BridgeError::Conversion`.
    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError>;

    /// Malformed or unrepresentable input yields `BridgeError::Conversion`.
    fn encode_request(
        &self,
        request: &Request,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError>;

    /// Decodes a successful response only; never inspects it for provider errors.
    /// An uninterpretable successful response yields `BridgeError::Decode`.
    fn decode_response(
        &self,
        body: &[u8],
        context: &DecodeContext,
    ) -> Result<Response, BridgeError>;

    /// Unrepresentable output, including unknown content, requires explicit loss
    /// permission in `options`; every omission must be reported in `Encoded.losses`.
    /// Output the client format cannot express yields `BridgeError::Conversion`.
    /// `request` is the client request decoded at ingress by this bridge.
    fn encode_response(
        &self,
        request: &Request,
        response: &Response,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError>;

    fn stream_decoder(&self, context: &DecodeContext) -> Box<dyn StreamDecoder>;

    /// `request` is the client request decoded at ingress by this bridge.
    fn stream_encoder(
        &self,
        request: &Request,
        options: &ConversionOptions,
    ) -> Box<dyn StreamEncoder>;
}

/// A normal data frame, framed by the adapter with provider error frames removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub data: String,
}

/// Normal-event conversion only. Never synthesize `ItemEnd` or `Stop` on failure.
pub trait StreamDecoder: Send {
    /// Uninterpretable successful frames yield `BridgeError::Decode`.
    /// Events appended before an error remain in `out`.
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError>;

    /// Called at normal end of input; ending without a stop yields
    /// `BridgeError::IncompleteStream`, not implicit completion. Undecodable
    /// buffered data yields `BridgeError::Decode`; prior events remain in `out`.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError>;
}

/// Converts normal events to frames; serialization of frames is the consumer's job.
/// Omissions require explicit loss permission and are appended to `losses` as they occur.
pub trait StreamEncoder: Send {
    /// Output the client format cannot express yields `BridgeError::Conversion`.
    /// Frames and losses appended before an error remain in the supplied buffers.
    fn push(
        &mut self,
        event: &StreamEvent,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError>;

    /// Unrepresentable final output yields `BridgeError::Conversion`;
    /// frames and losses appended before an error remain in the supplied buffers.
    fn finish(
        &mut self,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError>;
}
