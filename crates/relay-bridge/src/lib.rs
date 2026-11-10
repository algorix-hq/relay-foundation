//! Strict, bidirectional API-format conversion with format-scoped native fields and stream events.
//! Provider error detection belongs exclusively to adapters.

use std::{borrow::Cow, collections::BTreeSet, fmt};

use relay_protocol::{
    ConversionError, ConversionErrorKind, ConversionLoss, Request, Response, StreamEvent,
};
use serde_json::{Map, Value};

/// A wire-format identifier outside protocol data, for compatibility and native-field scoping.
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

/// Top-level wire fields accessed through `Input` and `Output`, outside protocol data.
pub type NativeFields = Map<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub request: Request,
    pub stream: bool,
    native: NativeFields,
    format: Option<ApiFormat>,
}

impl Input {
    pub fn new(request: Request) -> Self {
        Self {
            request,
            stream: false,
            native: NativeFields::new(),
            format: None,
        }
    }

    pub fn with_native(
        format: ApiFormat,
        request: Request,
        stream: bool,
        native: NativeFields,
    ) -> Self {
        Self {
            request,
            stream,
            native,
            format: Some(format),
        }
    }

    /// Only a same-format bridge receives native fields; other formats ignore them.
    /// The producing format is kept outside the protocol request.
    pub fn native_for(&self, format: &ApiFormat) -> Option<&NativeFields> {
        (self.format.as_ref() == Some(format)).then_some(&self.native)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub response: Response,
    native: NativeFields,
    format: Option<ApiFormat>,
}

impl Output {
    pub fn new(response: Response) -> Self {
        Self {
            response,
            native: NativeFields::new(),
            format: None,
        }
    }

    pub fn with_native(format: ApiFormat, response: Response, native: NativeFields) -> Self {
        Self {
            response,
            native,
            format: Some(format),
        }
    }

    /// Only a same-format bridge receives native fields; other formats ignore them.
    /// The producing format is kept outside the protocol response.
    pub fn native_for(&self, format: &ApiFormat) -> Option<&NativeFields> {
        (self.format.as_ref() == Some(format)).then_some(&self.native)
    }
}

/// Empty by default: conversion is fully strict. Bridges must reject omissions
/// outside the allowed scopes and report every drop as a `ConversionLoss`.
/// Format-specific compatibility settings belong to concrete bridge constructors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversionOptions {
    pub allow_loss: BTreeSet<LossScope>,
}

/// Parts of a request, response, or stream a consumer may allow a bridge to omit.
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
    fn decode_request(&self, body: &[u8]) -> Result<Input, ConversionError>;

    /// Uses `input.stream` and only `input.native_for(self.format())`. A native key
    /// colliding with a generated field is an `invalid` error, never an override.
    fn encode_request(
        &self,
        input: &Input,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError>;

    /// Decodes a successful response only; never inspects it for provider errors.
    fn decode_response(&self, body: &[u8]) -> Result<Output, ConversionError>;

    /// Unrepresentable output, including unknown content, requires explicit loss
    /// permission in `options`; every omission must be reported in `Encoded.losses`.
    /// Reads only `output.native_for(self.format())`; native collisions are invalid.
    fn encode_response(
        &self,
        output: &Output,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError>;

    fn stream_decoder(&self) -> Box<dyn StreamDecoder>;

    fn stream_encoder(&self, options: &ConversionOptions) -> Box<dyn StreamEncoder>;
}

/// A normal data frame, framed by the adapter with provider error frames removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub event: Option<String>,
    pub data: String,
}

/// Normal-event conversion only. Never synthesize `ItemEnd` or `Stop` on failure.
pub trait StreamDecoder: Send {
    /// Events appended before an error remain in `out`.
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), ConversionError>;

    /// Called at normal end of input. Ending before a stop is an `invalid`
    /// conversion error, not an implicit successful completion.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ConversionError>;
}

/// Converts normal events to frames; serialization of frames is the consumer's job.
/// Omissions require explicit loss permission and must all be reported by `finish`.
pub trait StreamEncoder: Send {
    fn push(&mut self, event: &StreamEvent, out: &mut Vec<Frame>) -> Result<(), ConversionError>;

    fn finish(&mut self, out: &mut Vec<Frame>) -> Result<Vec<ConversionLoss>, ConversionError>;
}
