//! Messages API format (non-beta SDK 1.7.0).
//!
//! Leading system messages become top-level text blocks. Inside `messages`, a
//! content-carrying system group cannot be first, must immediately follow a user
//! turn or an assistant turn ending in a server tool result, and must precede an
//! assistant turn or end the array. Consecutive system messages are judged as a
//! group; no messages are reordered. See
//! <https://platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages#limitations>.
//! Server tool results have no request representation, so this bridge can encode
//! only the user-turn case. Model-specific feature support is never checked.
//! Request `service_tier` accepts `auto` and `standard_only` (the standard tier).
//!
//! Response decoding ignores new fields in known blocks, including SDK text
//! `citations` and tool-use `caller` and `toolset_name`. Unknown block types are
//! retained whole. Tool inputs arrive as parsed JSON and are re-serialized into
//! compact JSON, not represented as original argument text. Usage input totals
//! include ordinary input plus cache reads and cache writes. Stream usage updates
//! replace cumulative counters; absent/null counters retain their previous value.
//! Usage detail fields with no neutral counterpart (`cache_creation`,
//! `inference_geo`, `server_tool_use`, `service_tier`, `speed`) are ignored.
//! `output_tokens_details.thinking_tokens` maps to reasoning tokens.
//! Top-level response fields without counterparts (`container`, `stop_sequence`,
//! `stop_details`, `diagnostics`, and future fields) are ignored.
//! Response IDs use the provider ID or the decode context fallback; timestamps
//! carry only the context's observed time. Block IDs use `{response_id}:{index}`
//! with no provider item ID. Request history uses the message JSON path as its
//! ID namespace. Replay sends tool correlation IDs and opaque values, never item IDs.
//! Request reasoning with a provider item ID requires explicit Reasoning loss
//! permission: Messages has no such replay identity and cannot interpret its opaque value.
//! Messages always streams usage, so `delivery.stream_usage` is ignored on encoding.
//! Encoding releases `message_start` with the first response metadata usage before
//! any item starts. If an item starts first, the whole stream waits for `Stop`
//! and uses its final usage. Counters are never invented; a stop without usage
//! is unrepresentable even when provisional usage was received.
//! Reasoning streams as soon as its kind is known: text starts thinking and an
//! opaque-only item starts redacted thinking. When reasoning loss is allowed,
//! unsigned items are buffered until `ItemEnd` and then released or dropped
//! atomically. Without that permission, an unsigned item errors at `ItemEnd`;
//! frames already emitted remain emitted.
//!
//! Plain text documents use base64 `text/plain` in the neutral media source;
//! Messages' text source is restored on encoding. JSON schemas have no name
//! on ingress because Messages has no schema-name field. Names, descriptions,
//! and explicit strictness cannot be encoded without data loss.
//! Content filtering is not equated to refusal and is rejected on encoding.

mod content;
mod request;
mod response;
mod stream;

use relay_protocol::{
    ConversionError, ConversionErrorKind, ConversionLoss, DecodeError, Request, Response,
};
use serde_json::{Map, Value};

use crate::{ApiFormat, Bridge, BridgeError, ConversionOptions, DecodeContext, Encoded, LossScope};

#[derive(Clone, Debug, Default)]
pub struct MessagesBridge {
    default_max_tokens: Option<u64>,
}

impl MessagesBridge {
    /// Requires each request to supply `parameters.max_output_tokens`.
    pub const fn new() -> Self {
        Self {
            default_max_tokens: None,
        }
    }

    /// Uses the consumer's fallback only when the request omits its output limit.
    pub const fn with_default_max_tokens(max_tokens: u64) -> Self {
        Self {
            default_max_tokens: Some(max_tokens),
        }
    }
}

impl Bridge for MessagesBridge {
    fn format(&self) -> &ApiFormat {
        &ApiFormat::MESSAGES
    }

    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError> {
        request::decode(body)
    }

    fn encode_request(
        &self,
        request: &Request,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        request::encode(request, options, self.default_max_tokens)
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

    fn stream_decoder(&self, context: &DecodeContext) -> Box<dyn crate::StreamDecoder> {
        Box::new(stream::Decoder::new(context.clone()))
    }

    fn stream_encoder(
        &self,
        _request: &Request,
        options: &ConversionOptions,
    ) -> Box<dyn crate::StreamEncoder> {
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

fn decode_error(error: BridgeError) -> BridgeError {
    match error {
        BridgeError::Conversion(error) => DecodeError {
            path: error.path,
            message: error.message,
        }
        .into(),
        error => error,
    }
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, BridgeError> {
    value
        .as_object()
        .ok_or_else(|| invalid(path, "expected an object"))
}

fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, BridgeError> {
    value
        .as_array()
        .ok_or_else(|| invalid(path, "expected an array"))
}

fn string(value: &Value, path: &str) -> Result<String, BridgeError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(path, "expected a string"))
}

fn unsigned(value: &Value, path: &str) -> Result<u64, BridgeError> {
    value
        .as_u64()
        .ok_or_else(|| invalid(path, "expected an unsigned integer"))
}

fn boolean(value: &Value, path: &str) -> Result<bool, BridgeError> {
    value
        .as_bool()
        .ok_or_else(|| invalid(path, "expected a boolean"))
}

fn fields(value: &Value, allowed: &[&str], path: &str) -> Result<(), BridgeError> {
    for key in object(value, path)?.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unrepresentable(
                &pointer(path, key),
                "field has no neutral representation",
            ));
        }
    }
    Ok(())
}

fn pointer(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn optional_string(value: &Value, key: &str, path: &str) -> Result<Option<String>, BridgeError> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| string(v, &pointer(path, key)))
        .transpose()
}

fn put_optional(body: &mut Value, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        body[key] = value;
    }
}

fn allow_loss(
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
    scope: LossScope,
    path: &str,
    message: &str,
) -> Result<(), BridgeError> {
    if !options.allow_loss.contains(&scope) {
        return Err(unrepresentable(path, message));
    }
    losses.push(ConversionLoss {
        path: path.into(),
        message: message.into(),
    });
    Ok(())
}
