//! Extension contracts for strict API-format conversion and normal stream normalization.
//! Provider error detection belongs exclusively to adapters.

use std::{borrow::Cow, collections::BTreeSet, fmt};

use relay_protocol::{
    ConversionError, ConversionErrorKind, ConversionLoss, Request, Response, StreamEvent,
};
use serde_json::{Map, Value};

/// A wire-format identifier used only for bridge/adapter compatibility at composition.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiFormat(Cow<'static, str>);

impl ApiFormat {
    pub const CHAT_COMPLETIONS: Self = Self(Cow::Borrowed("chat-completions"));
    pub const RESPONSES: Self = Self(Cow::Borrowed("responses"));
    pub const MESSAGES: Self = Self(Cow::Borrowed("messages"));

    pub fn new(value: impl Into<Cow<'static, str>>) -> Result<Self, ConversionError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ConversionError {
                kind: ConversionErrorKind::Invalid,
                path: String::new(),
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

/// Top-level fields of the producing bridge's own wire format, not represented by
/// `Request` or `Response`. Only that bridge reads them; they carry no format label.
/// Includes request options, state references, hosted tools, and response metadata.
/// Keys are not validated by this contract. Encoding a key that collides with a
/// field generated from `Request` is an `invalid` conversion error, never an override.
/// Each bridge defines per-format merge rules, such as hosted tools in a tools array.
pub type NativeFields = Map<String, Value>;

/// Empty by default: conversion is fully strict. Bridges must reject omissions
/// outside the allowed scopes and report every drop as a `ConversionLoss`.
/// Format-specific compatibility settings belong to concrete bridge constructors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversionOptions {
    pub allow_loss: BTreeSet<LossScope>,
}

/// Parts of a request a consumer may explicitly allow a bridge to omit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LossScope {
    CacheControl,
    ImageDetail,
    /// Reasoning content items, including their opaque values; not request settings.
    Reasoning,
    UnknownContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub body: Value,
    pub losses: Vec<ConversionLoss>,
}

pub trait Bridge: Send + Sync {
    fn format(&self) -> &ApiFormat;

    /// Strict ingress: unknown or unrepresentable input is an error. Fields
    /// intentionally preserved for same-format passthrough go into `NativeFields`.
    fn decode_request(&self, body: &[u8]) -> Result<(Request, NativeFields), ConversionError>;

    fn encode_request(
        &self,
        request: &Request,
        native: &NativeFields,
        stream: bool,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError>;

    /// Decodes a successful response only; never inspects it for provider errors.
    fn decode_response(&self, body: &[u8]) -> Result<(Response, NativeFields), ConversionError>;

    fn stream_decoder(&self) -> Box<dyn StreamDecoder>;
}

/// A normal data frame, framed by the adapter with provider error frames removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub data: String,
}

/// Normal-event conversion only. Never synthesize `ItemEnd` or `Stop` on failure.
pub trait StreamDecoder: Send {
    fn push(&mut self, frame: Frame) -> Result<Vec<StreamEvent>, ConversionError>;

    /// Called at normal end of input. Ending before a stop is an `invalid`
    /// conversion error, not an implicit successful completion.
    fn finish(&mut self) -> Result<Vec<StreamEvent>, ConversionError>;
}
