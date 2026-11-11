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
//! Encoding requires provider-supplied usage. The neutral stream exposes usage
//! only on `Stop`, so the encoder must buffer every frame until that event.
//! Both `message_start` and `message_delta` use those actual cumulative totals,
//! not invented initial counters. A stop without usage is unrepresentable.
//! Reasoning is assembled until its end to distinguish thinking from redacted
//! thinking, retain the final complete opaque value, and omit unsigned items
//! atomically when allowed. This adds no wait beyond the required usage gate:
//! all items must have ended before `Stop` can release any frames.
//!
//! Plain text documents use base64 `text/plain` in the neutral media source;
//! Messages' text source is restored on encoding. JSON schemas use an empty name
//! on ingress because Messages has no schema-name field. An empty name means
//! "no name given"; nonempty names, descriptions, and explicit strictness cannot
//! be encoded without data loss.
//! Content filtering is not equated to refusal and is rejected on encoding.

mod content;
mod request;
mod response;
mod stream;

use relay_protocol::{ConversionError, ConversionErrorKind, ConversionLoss, DecodeError};
use serde_json::{Map, Value};

use crate::{ApiFormat, Bridge, BridgeError, ConversionOptions, Encoded, Input, LossScope, Output};

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

    fn decode_request(&self, body: &[u8]) -> Result<Input, BridgeError> {
        request::decode(body)
    }

    fn encode_request(
        &self,
        input: &Input,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        request::encode(input, options, self.default_max_tokens)
    }

    fn decode_response(&self, body: &[u8]) -> Result<Output, BridgeError> {
        response::decode(body)
    }

    fn encode_response(
        &self,
        output: &Output,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        response::encode(output, options)
    }

    fn stream_decoder(&self) -> Box<dyn crate::StreamDecoder> {
        Box::new(stream::Decoder::default())
    }

    fn stream_encoder(&self, options: &ConversionOptions) -> Box<dyn crate::StreamEncoder> {
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

fn merge_native(body: &mut Value, native: &Map<String, Value>) -> Result<(), BridgeError> {
    let generated = body
        .as_object_mut()
        .ok_or_else(|| invalid("", "expected object"))?;
    for (key, value) in native {
        if generated.contains_key(key) {
            return Err(invalid(
                &pointer("", key),
                "native field collides with generated field",
            ));
        }
        generated.insert(key.clone(), value.clone());
    }
    Ok(())
}
