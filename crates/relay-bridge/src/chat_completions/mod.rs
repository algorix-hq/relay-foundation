//! Chat Completions API format (OpenAI SDK 7.55.0).
//!
//! `ChatCompletionsBridge::new()` emits `max_completion_tokens`; compatible servers
//! can use `ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens)`.
//! Response decoding ignores annotations, audio, choice logprobs, and usage details
//! for total, audio, image, text, and accepted/rejected prediction tokens. Refusals are retained
//! as unknown content, not converted into text. Stream-only moderation, obfuscation,
//! service tier and fingerprint metadata have no stream-event counterpart.
//! Top-level response fields without protocol counterparts, including service_tier,
//! system_fingerprint, and future fields, are ignored rather than retained.
//! New fields inside recognized response items are also ignored.
//! Item IDs are not supplied by this format; tool-call IDs are correlation IDs.
//! Historical assistant items use the message's JSON path as a deterministic ID
//! namespace because request history has no response envelope.
//! Provisional usage cannot be encoded: this format's usage chunks represent final
//! totals, so response usage metadata is rejected rather than promoted to final.
//! Only `[DONE]` establishes a normal stream finish, even after a finish reason.
//! The prescribed stop-reason mappings coarsen termination categories rather than
//! omit content; they do not require a loss scope or produce omission reports.
//! Legacy calls without a tool-call ID are preserved as unknown output; ingress
//! rejects them rather than inventing IDs. Inline documents require a media-typed
//! base64 data URL because the neutral source must retain its media type.

mod request;
mod response;
mod stream;

use relay_protocol::{
    ConversionError, ConversionErrorKind, ConversionLoss, DecodeError, Request, Response,
};
use serde_json::{Map, Value};

use crate::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, DecodeContext, Encoded, LossScope,
    StreamDecoder, StreamEncoder,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaxTokensField {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Clone, Debug, Default)]
pub struct ChatCompletionsBridge {
    max_tokens_field: MaxTokensField,
}

impl ChatCompletionsBridge {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_max_tokens_field(max_tokens_field: MaxTokensField) -> Self {
        Self { max_tokens_field }
    }
}

impl Bridge for ChatCompletionsBridge {
    fn format(&self) -> &ApiFormat {
        &ApiFormat::CHAT_COMPLETIONS
    }
    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError> {
        request::decode(body)
    }
    fn encode_request(
        &self,
        request: &Request,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        request::encode(request, options, self.max_tokens_field)
    }
    fn decode_response(
        &self,
        body: &[u8],
        context: &DecodeContext,
    ) -> Result<Response, BridgeError> {
        response::decode(body, context)
    }
    fn encode_response(
        &self,
        _request: &Request,
        response: &Response,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        response::encode(response, options)
    }
    fn stream_decoder(&self, context: &DecodeContext) -> Box<dyn StreamDecoder> {
        Box::new(stream::Decoder::new(context.clone()))
    }
    fn stream_encoder(
        &self,
        _request: &Request,
        options: &ConversionOptions,
    ) -> Box<dyn StreamEncoder> {
        Box::new(stream::Encoder::new(options.clone()))
    }
}

fn invalid(path: &str, message: impl Into<String>) -> BridgeError {
    ConversionError {
        kind: ConversionErrorKind::Invalid,
        path: path.into(),
        message: message.into(),
    }
    .into()
}

fn unrepresentable(path: &str, message: impl Into<String>) -> BridgeError {
    ConversionError {
        kind: ConversionErrorKind::Unrepresentable,
        path: path.into(),
        message: message.into(),
    }
    .into()
}

fn decode_error(path: &str, message: impl Into<String>) -> BridgeError {
    DecodeError {
        path: path.into(),
        message: message.into(),
    }
    .into()
}

trait FromValue: Sized {
    fn from_value(value: Value) -> Result<Self, String>;
}

macro_rules! scalar_values {
    ($($ty:ty),+ $(,)?) => { $(impl FromValue for $ty {
        fn from_value(value: Value) -> Result<Self, String> {
            serde_json::from_value(value).map_err(|error| error.to_string())
        }
    })+ };
}

scalar_values!(
    String,
    bool,
    u64,
    u32,
    f64,
    Value,
    relay_protocol::ReasoningEffort,
    relay_protocol::ImageDetail
);

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: Value) -> Result<Self, String> {
        let Value::Array(values) = value else {
            return Err("expected array".into());
        };
        values.into_iter().map(T::from_value).collect()
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: Value) -> Result<Self, String> {
        if value.is_null() {
            Ok(None)
        } else {
            T::from_value(value).map(Some)
        }
    }
}

fn parse<T: FromValue>(value: Value, path: &str) -> Result<T, BridgeError> {
    T::from_value(value).map_err(|error| invalid(path, error))
}

fn object(value: Value, path: &str) -> Result<Map<String, Value>, BridgeError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(invalid(path, "expected object")),
    }
}

fn required<T: FromValue>(
    object: &mut Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<T, BridgeError> {
    let path = format!("{path}/{key}");
    parse(
        object
            .remove(key)
            .ok_or_else(|| invalid(&path, "required field missing"))?,
        &path,
    )
}

fn optional<T: FromValue>(
    object: &mut Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<T>, BridgeError> {
    object
        .remove(key)
        .filter(|v| !v.is_null())
        .map(|value| parse(value, &format!("{path}/{key}")))
        .transpose()
}

fn no_remaining(object: Map<String, Value>, path: &str) -> Result<(), BridgeError> {
    if let Some((key, _)) = object.into_iter().next() {
        return Err(unrepresentable(
            &format!("{path}/{key}"),
            "field has no protocol representation",
        ));
    }
    Ok(())
}

fn omit(
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
    scope: LossScope,
    path: &str,
) -> Result<(), BridgeError> {
    if !options.allow_loss.contains(&scope) {
        return Err(unrepresentable(path, "no Chat Completions representation"));
    }
    losses.push(ConversionLoss {
        path: path.into(),
        message: "omitted: no Chat Completions representation".into(),
    });
    Ok(())
}
