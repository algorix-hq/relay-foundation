# Custom adapters

Built-ins cover OpenAI, Anthropic, and OpenAI-compatible servers. Any other provider (for example Kiro or ChatGPT) is a consumer-owned adapter. Write it in the consumer crate, not in relay-foundation.

## Decide the bridge first

- If the provider speaks Chat Completions, Responses, or Messages on the wire, reuse that built-in bridge. Then check whether `OpenAiCompatibleAdapter` already fits; if so, no custom adapter is needed.
- If the provider has its own wire format, implement `relay_bridge::Bridge` for it as well, with an `ApiFormat::new("your-format")?` identifier. Implement `StreamDecoder` and `StreamEncoder` for streaming. The bridge must stay strict: return `BridgeError::Conversion` for unrepresentable input and push a `ConversionLoss` for every allowed drop. A bridge never detects provider errors.

## Trait to implement

```rust
pub trait Adapter: Send + Sync {
    fn conversion(&self) -> &Conversion;
    fn validate_request(&self, _request: &Request) -> Result<(), ConfigurationError> { Ok(()) } // optional
    fn assemble_request(&self, model: &str, body: Value, stream: bool) -> Result<ProviderRequest, ConfigurationError>;
    fn check_response(&self, status: u16, headers: &Headers, body: &[u8]) -> Result<(), ProviderError>;
    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder>;
    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> { None }  // optional
}
pub trait FrameDecoder: Send {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError>;
    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError>;
}
pub enum FrameItem { Data(Frame), Error(ProviderError) }
pub trait ModelDiscovery: Send + Sync {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, ConfigurationError>;
    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError>;
}
```

Do not implement `AdapterExt`. It has a blanket implementation that supplies `build_request`, `parse_response`, `open_stream`, `list_models`, and `parse_models` for every `Adapter`.

Responsibilities:

- `assemble_request`: URL, auth, and headers around the bridge's JSON body. No network call. Return `ConfigurationError` for missing setup such as an empty token. Keep secrets out of error messages.
- `check_response`: the only place provider HTTP failures are recognized, including error payloads inside 2xx responses. Do not depend on `Content-Type`. Keep `code`, `request_id`, and `retry_after` when present. If it returns `Ok` for a non-2xx status, the blanket implementation still reports `Provider(Unknown)`.
- `frame_decoder`: split the provider's stream bytes into `Frame`s and turn provider error frames into `FrameItem::Error`, with `http_status: None`. Classify the error payload, not the 200 status. Malformed framing returns `DecodeError`. For SSE providers, wrap `relay_adapter::SseDecoder` (`new`, `push(bytes, &mut Vec<Frame>)`, `finish`) and inspect each frame for error events.

## `SseDecoder` behavior

You do not need to read its source. It:

- accepts `\n`, `\r`, and `\r\n` line endings, and handles events and UTF-8 sequences split across `push` calls;
- ignores comment lines (`:`) and fields other than `event` and `data`; joins multiple `data:` lines with `\n`; strips one leading space after the colon;
- emits `Frame { event: Some(name) or None, data }` on each blank line;
- on `finish`, flushes an unterminated final event only if it has `data`;
- returns `DecodeError` for invalid UTF-8 and keeps frames already pushed to `out`.

It does not interpret `data`. `[DONE]` sentinels arrive as ordinary frames for the bridge's stream decoder; forward them as `FrameItem::Data`.

## How built-ins classify, for reference

Built-in adapters look for an error payload in any of these shapes, in a 2xx or non-2xx body, and in each stream frame: `{"type":"error","error":{...}}`, `{"error":{...}}` with a non-null `error`, `{"type":"response.failed","response":{"error":{...}}}`, and `{"status":"failed","error":{...}}`. They map the provider's documented `type` or `code` first and fall back to the HTTP status (401 → `Authentication`, 403 → `PermissionDenied`, 429 → `RateLimited`, 5xx → `ServerError`, anything else including mid-stream errors without a status → `Unknown`). `Retry-After` is read as seconds or an HTTP date; when only `retry-after-ms` is present it is rounded up to whole seconds. They never match message text. Copy this approach for a custom provider; the classification module itself is private.

## Skeleton

```rust
use std::sync::Arc;
use relay_adapter::{Adapter, Conversion, FrameDecoder, FrameItem, Headers, Method, ProviderRequest, SseDecoder};
use relay_bridge::{ApiFormat, Bridge, ConversionOptions, Frame};
use relay_protocol::{ConfigurationError, DecodeError, ProviderError, ProviderErrorCategory, RetryAfter};
use serde_json::Value;

pub struct KiroAdapter {
    conversion: Conversion,
    url: String,
    token: String,
}

impl KiroAdapter {
    pub fn new(bridge: Arc<dyn Bridge>, url: String, token: String, options: ConversionOptions)
        -> Result<Self, ConfigurationError>
    {
        // List every format this endpoint accepts; Conversion::new rejects other bridges.
        let conversion = Conversion::new(bridge, &[ApiFormat::CHAT_COMPLETIONS], options)?;
        Ok(Self { conversion, url, token })
    }
}

impl Adapter for KiroAdapter {
    fn conversion(&self) -> &Conversion { &self.conversion }

    fn assemble_request(&self, _model: &str, body: Value, _stream: bool) -> Result<ProviderRequest, ConfigurationError> {
        if self.token.is_empty() {
            return Err(ConfigurationError { message: "token is required".into() });
        }
        Ok(ProviderRequest {
            method: Method::Post,
            url: self.url.clone(),
            headers: Headers(vec![
                ("authorization".into(), format!("Bearer {}", self.token)),
                ("content-type".into(), "application/json".into()),
            ]),
            body: serde_json::to_vec(&body).map_err(|e| ConfigurationError { message: e.to_string() })?,
        })
    }

    fn check_response(&self, status: u16, headers: &Headers, body: &[u8]) -> Result<(), ProviderError> {
        let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) || payload.get("error").is_some() {
            return Err(kiro_error(&payload, Some(status), headers));
        }
        Ok(())
    }

    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder> {
        Box::new(KiroFrames { sse: SseDecoder::new(), headers: headers.clone() })
    }
}

fn kiro_error(payload: &Value, status: Option<u16>, headers: &Headers) -> ProviderError {
    let code = payload["error"]["code"].as_str().map(str::to_owned);
    // Map documented provider codes first; fall back to HTTP status; never match on message text.
    let category = match (code.as_deref(), status) {
        (Some("throttled"), _) | (_, Some(429)) => ProviderErrorCategory::RateLimited,
        (_, Some(401)) => ProviderErrorCategory::Authentication,
        (_, Some(403)) => ProviderErrorCategory::PermissionDenied,
        (_, Some(s)) if s >= 500 => ProviderErrorCategory::ServerError,
        _ => ProviderErrorCategory::Unknown,
    };
    let retry_after = headers.get("retry-after").map(|v| match v.parse::<u64>() {
        Ok(seconds) => RetryAfter::Seconds { seconds },
        Err(_) => RetryAfter::HttpDate { date: v.to_owned() },
    });
    ProviderError {
        category,
        message: payload["error"]["message"].as_str().unwrap_or("provider request failed").into(),
        code,
        http_status: status,
        request_id: headers.get("x-request-id").map(str::to_owned),
        retry_after,
    }
}

struct KiroFrames { sse: SseDecoder, headers: Headers }

impl KiroFrames {
    fn classify(&self, frames: Vec<Frame>, out: &mut Vec<FrameItem>) {
        for frame in frames {
            let is_error = frame.event.as_deref() == Some("error");
            if is_error {
                let payload: Value = serde_json::from_str(&frame.data).unwrap_or(Value::Null);
                out.push(FrameItem::Error(kiro_error(&payload, None, &self.headers)));
            } else {
                out.push(FrameItem::Data(frame));
            }
        }
    }
}

impl FrameDecoder for KiroFrames {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        let mut frames = Vec::new();
        let result = self.sse.push(bytes, &mut frames);
        self.classify(frames, out);   // keep frames decoded before a framing error
        result
    }
    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        let mut frames = Vec::new();
        let result = self.sse.finish(&mut frames);
        self.classify(frames, out);
        result
    }
}
```

The category table above is a placeholder. Replace it with the provider's documented error codes. Use `Unknown` for anything you cannot map with confidence.

Use it exactly like a built-in: `adapter.build_request(&input)`, `adapter.parse_response(...)`, `adapter.open_stream(...)` with `relay_adapter::AdapterExt` in scope.

## Testing a custom adapter

- Feed recorded provider bodies and stream chunks (split at awkward byte boundaries) into `parse_response` and `open_stream`. No live calls.
- Cover: 2xx success, non-2xx with JSON body, non-2xx with empty or non-JSON body, 2xx with an error payload, a mid-stream error frame, and EOF without a stop.
- Inject `with_id_generator` and `with_clock` on `Conversion` for stable output.
