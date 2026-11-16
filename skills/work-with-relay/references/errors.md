# Errors and losses

## Who produces which error

| Variant | Produced by | Meaning |
|---|---|---|
| `relay_protocol::Error::Provider(ProviderError)` | adapter only | The provider declared a failure, in an HTTP response (any status, including 200 with an error body) or a mid-stream frame |
| `relay_protocol::Error::Conversion(ConversionError)` | bridge | Input is malformed (`Invalid`), valid but not expressible in the target format (`Unrepresentable`), or not implemented (`Unsupported`) |
| `relay_protocol::Error::Decode(DecodeError)` | bridge or adapter framing | A success response or stream frame could not be interpreted |
| `relay_protocol::Error::IncompleteStream` | bridge stream decoder | Provider body ended normally without a stop signal |
| `relay_protocol::Error::Configuration` | adapter or `Conversion` | Setup is invalid, for example an empty URL or a bridge the adapter does not support |
| `relay_protocol::Error::UnsupportedOperation` | adapter | Optional operation missing, for example `list_models` without discovery |

`BridgeError` has only `Conversion`, `Decode`, and `IncompleteStream`, and converts into `relay_protocol::Error` with `.into()`.

```rust
pub struct ProviderError {
    pub category: ProviderErrorCategory,
    pub message: String,
    pub code: Option<String>,
    pub http_status: Option<u16>,        // None for mid-stream errors
    pub request_id: Option<String>,
    pub retry_after: Option<RetryAfter>, // Seconds { seconds } | HttpDate { date }
}
pub enum ProviderErrorCategory {
    Authentication, PermissionDenied, InsufficientQuota, RateLimited, Overloaded, InvalidRequest,
    UnsupportedFeature, ModelNotFound, ContextLengthExceeded, ServerError, Unknown,
}
pub struct ConversionError { pub kind: ConversionErrorKind, pub path: String, pub message: String }
```

Consumers decide retry policy from `category` and `retry_after`. Do not re-classify by parsing `message` or the raw body.

## Answering the client

Use the error encoder for the **client** format. It maps an already-classified `relay_protocol::Error` to status, headers, and body; it never classifies.

```rust
use relay_adapter::{EncodedError, ErrorEncoder, MessagesErrors, Stage};

let errors = MessagesErrors::new();
// before the response started
let EncodedError { status, headers, body } = errors.encode_error(&error, Stage::Request);
// after the stream started
let frames = errors.encode_stream_error(&error);
```

Use `Stage::Request` for failures while decoding the client request, `Stage::Response` for failures while producing the result. The same conversion error can map to different statuses (for example 400 at request, 502 at response).

Built-in mappings, from the library's own table:

| Situation | Result | Client sees |
|---|---|---|
| Request has `Unknown` content and no loss allowed | `Conversion(Unrepresentable)` | 400 at request stage |
| Bridge and adapter API format mismatch | `Configuration` | 500 |
| 503 with non-JSON error body | `Provider(ServerError)` | 500 |
| 200 with explicit overload error body | `Provider(Overloaded)` | 503 (Chat Completions, Responses), 529 (Messages) |
| 200 with malformed success body | `Decode` | 502 at response stage |
| Streaming 429 classified via `RejectedResponse::classify` | `Provider(RateLimited)` | 429 before the stream starts |
| Provider error frame mid-stream | `Provider(...)` after earlier events | error frames after earlier frames; no synthetic stop |
| Normal EOF without stop | `IncompleteStream` | error frames; no synthetic completion |
| Transport cut or cancellation, transcoder dropped | `outcome: None` | nothing automatic; consumer handles `Transport`/`Cancelled` |
| Model discovery not provided | `UnsupportedOperation(list_models)` | 501 |

Transport errors and cancellation are not `relay_protocol::Error`. Map them to your own client response (often 502 or a closed connection).

Built-in encoders return the upstream request ID as `x-relay-upstream-request-id`, not as the client format's own request-ID header. They copy `retry_after` to a `retry-after` header and always set `content-type: application/json`.

Status codes chosen by the built-in encoders (all three formats unless noted):

| Error | Status |
|---|---|
| `Provider` `Authentication` / `PermissionDenied` | 401 / 403 |
| `Provider` `RateLimited` | 429 |
| `Provider` `InsufficientQuota` | 429; Messages 402 |
| `Provider` `Overloaded` | 503; Messages 529 |
| `Provider` `InvalidRequest`, `UnsupportedFeature`, `ContextLengthExceeded` | 400 |
| `Provider` `ModelNotFound` / `ServerError` | 404 / 500 |
| `Provider` `Unknown` | the provider's `http_status`, or 502 |
| `Conversion` at `Stage::Request` | 400 |
| `Conversion` at `Stage::Response`, `Decode`, `IncompleteStream` | 502 |
| `Configuration` / `UnsupportedOperation` | 500 / 501 |

Bodies follow each client format's own error envelope. Stream error frames: Chat Completions sends one `data:` frame without an event name; Responses and Messages send `event: error`. You do not need to construct any of these by hand.

## Losses

Conversion is strict. `ConversionOptions::default()` allows nothing. To accept a specific kind of drop, allow its `LossScope`:

| `LossScope` | Allows dropping or downgrading |
|---|---|
| `CacheControl` | cache breakpoints the target cannot express |
| `DeveloperRole` | developer messages sent as system when the target has no developer role |
| `ImageDetail` | image `detail` hints |
| `Reasoning` | reasoning content items, including opaque values (not reasoning request settings) |
| `Sampling` | `temperature`/`top_p` the target cannot express |
| `UnknownContent` | unrecognized provider output (`Unknown` content or deltas) |

`LossScope` is `#[non_exhaustive]`; match it with a wildcard arm.

Where to set it:

- Request to provider: `adapter.allow_loss([LossScope::Sampling])` on built-ins, or the `ConversionOptions` given to `Conversion::new` in a custom adapter.
- Response or stream to client: the `ConversionOptions` passed to `client.encode_response` or `stream.transcode`.

```rust
use relay_bridge::{ConversionOptions, LossScope};

let options = ConversionOptions { allow_loss: [LossScope::UnknownContent].into() };
let encoded = client.encode_response(&input, &output, &options)?;
for loss in &encoded.losses {           // ConversionLoss { path, message }
    tracing::warn!(path = %loss.path, message = %loss.message, "dropped");
}
```

Every allowed drop comes back as a `ConversionLoss`: from `build_request` as the second tuple element, from `encode_request`/`encode_response` in `Encoded.losses`, and from streams in `TranscodeReport.losses`. Log, meter, or return them; do not discard them with `_`.

Only allow a scope when the user or product has accepted that drop. When conversion fails, report the `ConversionError` (`kind`, `path`, `message`) to the client instead of editing the request to make it pass.

## What each target format cannot express when encoding a request

These come from the built-in bridges' `encode_request`. "Loss" means the field is dropped and reported when the scope is allowed, and fails otherwise. "Error" means it always fails; no `LossScope` helps.

**Messages** (`MessagesBridge`, used by `AnthropicAdapter`)

| Field | Result |
|---|---|
| `parameters.temperature`, `parameters.top_p` | Loss `Sampling`. Messages never receives either one, even if only one is set. |
| `parameters.max_output_tokens` absent | Error, unless `default_max_tokens` is configured |
| `Message::Developer` | Loss `DeveloperRole` (sent as system) |
| `cache_control` with a TTL other than 300 or 3600 seconds | Loss `CacheControl` |
| `ImagePart.detail` | Loss `ImageDetail` |
| `DocumentPart.filename` | Error |
| Assistant `Reasoning` items, `Unknown` items | Loss `Reasoning` / `UnknownContent` |
| `reasoning.summary` | Error |
| `reasoning.effort` other than `low`, `medium`, `high`, `xhigh`, `max` | Error |
| `reasoning.budget_tokens` without `thinking: Enabled`, or `Enabled` without a budget | Error (`Invalid`) |
| `response_format: JsonObject` | Error |
| `response_format: JsonSchema` with `name`, `description`, or `strict` | Error (only `schema` is carried) |

**Responses** (`ResponsesBridge`, used by `OpenAiAdapter`)

| Field | Result |
|---|---|
| `parameters.stop` | Error |
| `reasoning.budget_tokens`, `reasoning.thinking` | Error |
| `UserContent::ToolResult { is_error: Some(true) }` | Error |
| `DocumentPart.title` | Error |
| `JsonSchema` without `name` | Error |
| `cache_control` | Loss `CacheControl` |

**Chat Completions** (`ChatCompletionsBridge`, used by `OpenAiCompatibleAdapter` and `OllamaCloudAdapter`)

| Field | Result |
|---|---|
| `reasoning.budget_tokens`, `reasoning.summary`, `reasoning.thinking` | Error (only `effort` is carried) |
| Assistant reasoning content | One text-only item is carried when `ReasoningField::ReasoningContent` or `ReasoningField::Reasoning` is selected; otherwise Loss `Reasoning` |
| Reasoning summaries, opaque values, provider identity, or multiple reasoning item boundaries | Not representable in the selected string field; Loss `Reasoning`, never silently merged |
| `cache_control` anywhere | Loss `CacheControl` |
| `DocumentPart.title`, URL documents | Error |
| `ToolResult { is_error: Some(true) }`, non-text tool results | Error |
| `JsonSchema` without `name` | Error |

**Breaking decoding behavior:** Chat Completions now rejects data in known reasoning fields when `ReasoningField::Disabled` (the default), or when the field is not the selected one, instead of silently discarding it. This applies to request/history, responses, and stream deltas. The selected field accepts string reasoning text only; `reasoning_details` is not supported. Decoding has no `ConversionOptions` escape hatch. Configure `.with_reasoning_field(...)` explicitly on the relevant bridge; `reasoning_effort` does not select a field. See `composition.md` for configuration.

For a field marked Error, the gateway's options are to reject the request with the `ConversionError` (the default and usually right), or to change product behavior deliberately, for example routing that request to a provider whose format can express it. Do not delete the field from the client JSON to get past the error; that hides a decision from the client.
