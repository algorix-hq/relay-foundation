# Composing bridges and adapters

## Contents
- Choosing the client bridge and adapter
- Built-in constructors
- Consumer error type and HTTP glue
- Non-streaming request
- Streaming with Transcoder
- Streaming with AdapterStream (retry or fallback before replying)
- Model listing

## Choosing the client bridge and adapter

| Client sends | Client bridge | Client error encoder |
|---|---|---|
| `POST /v1/chat/completions` | `ChatCompletionsBridge::new()` | `ChatCompletionsErrors::new()` |
| `POST /v1/responses` | `ResponsesBridge::new()` | `ResponsesErrors::new()` |
| `POST /v1/messages` | `MessagesBridge::new()` | `MessagesErrors::new()` |

| Provider | Adapter | Provider bridge |
|---|---|---|
| OpenAI Responses | `OpenAiAdapter::new(key)` | Responses (fixed) |
| OpenAI Chat Completions | `OpenAiCompatibleAdapter::new("https://api.openai.com/v1/chat/completions").api_key(key).with_bridge(Arc::new(ChatCompletionsBridge::new()))?` | Chat Completions with `max_completion_tokens` |
| Anthropic | `AnthropicAdapter::new(key)` | Messages (fixed) |
| Ollama or other compatible server | `OpenAiCompatibleAdapter::new(full_url).api_key(key)` | Chat Completions with `max_tokens`; `with_bridge` accepts a Chat Completions or Responses bridge |

The client bridge and the adapter's bridge are separate objects. `Conversion::new` and `with_bridge` check that the adapter's bridge format is one the adapter supports, and return `ConfigurationError` otherwise.

## Built-in constructors

```rust
use std::sync::Arc;
use relay_adapter::{AnthropicAdapter, OpenAiAdapter, OpenAiCompatibleAdapter};
use relay_bridge::{ChatCompletionsBridge, LossScope, ResponsesBridge};

let anthropic = AnthropicAdapter::new(key)
    .base_url("https://api.anthropic.com")      // default; path /v1/messages is appended
    .version("2026-10-21")                      // anthropic-version header, default 2026-10-21
    .default_max_tokens(4096)                   // used only when the request omits max_output_tokens
    .allow_loss([LossScope::Sampling]);         // optional, see errors.md

let openai = OpenAiAdapter::new(key)
    .base_url("https://api.openai.com/v1")      // default; /responses is appended
    .organization("org-...")
    .project("proj-...")
    .store(false);

let ollama = OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions") // full URL
    .api_key(key)
    .models_url("https://ollama.com/v1/models") // enables list_models
    .without_tool_choice();                     // server rejects tool_choice; requests that set it fail early

let local_responses = OpenAiCompatibleAdapter::new("http://localhost:11434/v1/responses")
    .with_bridge(Arc::new(ResponsesBridge::new()))?;
```

All three built-ins also have `allow_loss`, `with_id_generator`, and `with_clock`. The Anthropic Messages API requires an output limit; without `default_max_tokens` a request lacking `max_output_tokens` fails conversion.

Bridge constructors with options: `ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens)` (`relay_bridge::chat_completions::MaxTokensField`), `MessagesBridge::with_default_max_tokens(n)`, `ResponsesBridge::with_config(ResponsesBridgeConfig { store: Some(false) })` (`relay_bridge::responses::ResponsesBridgeConfig`).

Bridge methods need the `relay_bridge::Bridge` trait in scope. Consumer adapter methods (`build_request`, `parse_response`, `open_stream`, `list_models`, `parse_models`) need `relay_adapter::AdapterExt` in scope.

## Consumer error type and HTTP glue

Relay has no HTTP types of its own beyond plain data: `ProviderRequest { method: Method, url: String, headers: Headers, body: Vec<u8> }`, `Method::{Get, Post}`, and `Headers(pub Vec<(String, String)>)` with case-insensitive `get`.

```rust
use relay_adapter::{Headers, Method, ProviderRequest};
use relay_protocol::RelayError;

#[derive(Debug)]
enum GatewayError {
    Relay(RelayError),
    Transport(Box<dyn std::error::Error + Send + Sync>),
    Cancelled,
}
impl From<RelayError> for GatewayError { fn from(e: RelayError) -> Self { Self::Relay(e) } }
impl From<relay_bridge::BridgeError> for GatewayError { fn from(e: relay_bridge::BridgeError) -> Self { Self::Relay(e.into()) } }
impl From<relay_protocol::ConfigurationError> for GatewayError { fn from(e: relay_protocol::ConfigurationError) -> Self { Self::Relay(e.into()) } }
impl From<reqwest::Error> for GatewayError { fn from(e: reqwest::Error) -> Self { Self::Transport(Box::new(e)) } }

async fn send(http: &reqwest::Client, request: ProviderRequest) -> Result<reqwest::Response, GatewayError> {
    let method = match request.method { Method::Get => reqwest::Method::GET, Method::Post => reqwest::Method::POST };
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in request.headers.0 {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| GatewayError::Transport(Box::new(e)))?;
        let value = reqwest::header::HeaderValue::from_str(&value).map_err(|e| GatewayError::Transport(Box::new(e)))?;
        headers.append(name, value);
    }
    Ok(http.request(method, request.url).headers(headers).body(request.body).send().await?)
}

fn headers_of(response: &reqwest::Response) -> Result<Headers, GatewayError> {
    let mut headers = Vec::new();
    for (name, value) in response.headers() {
        let value = value.to_str().map_err(|e| GatewayError::Transport(Box::new(e)))?;
        headers.push((name.to_string(), value.to_owned()));
    }
    Ok(Headers(headers))
}
```

Do not check `response.status().is_success()` and build an error yourself. Pass status, headers, and body to the adapter.

## Non-streaming request

```rust
use relay_adapter::{AdapterExt, AnthropicAdapter};
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, Encoded};

async fn complete(http: &reqwest::Client, adapter: &AnthropicAdapter, client_body: &[u8]) -> Result<Encoded, GatewayError> {
    let client = ChatCompletionsBridge::new();
    let input = client.decode_request(client_body)?;                // keep `input`
    let (request, request_losses) = adapter.build_request(&input)?;
    for loss in request_losses { tracing::warn!(%loss, "request field dropped"); }
    let response = send(http, request).await?;
    let status = response.status().as_u16();
    let headers = headers_of(&response)?;
    let body = response.bytes().await?;
    let output = adapter.parse_response(status, &headers, &body)?;  // classifies provider errors
    let encoded = client.encode_response(&input, &output, &ConversionOptions::default())?;
    for loss in &encoded.losses { tracing::warn!(%loss, "response field dropped"); }
    Ok(encoded)                                                     // encoded.body is serde_json::Value
}
```

`ConversionOptions` passed to `encode_response` controls client-side losses. Adapter-side losses are configured with the adapter's `allow_loss`.

## Streaming with Transcoder

Use this when provider frames go straight to the client.

```rust
use std::sync::Arc;
use relay_adapter::{AdapterExt, ChatCompletionsErrors, ErrorEncoder, Outcome};
use relay_bridge::{Bridge, ConversionOptions, Frame};
use relay_protocol::Delivery;

let mut input = client.decode_request(&client_body)?;
// decode_request already sets delivery from the client's `stream` field; set it only to force streaming.
input.delivery = Some(Delivery { stream: true, stream_usage: true });
let (request, losses) = adapter.build_request(&input)?;
let mut response = send(&http, request).await?;
let status = response.status().as_u16();
let headers = headers_of(&response)?;

let stream = match adapter.open_stream(status, &headers) {
    Ok(stream) => stream,
    Err(rejected) => {
        let body = response.bytes().await?;
        return Err(GatewayError::Relay(rejected.classify(&body)));  // always RelayError::Provider
    }
};
let errors: Arc<dyn ErrorEncoder> = Arc::new(ChatCompletionsErrors::new());
let mut transcoder = stream.transcode(&client, &input, &ConversionOptions::default(), errors);

loop {
    if cancelled() { return Err(GatewayError::Cancelled); }     // drop without finish
    let Some(bytes) = response.chunk().await? else { break };    // transport error: `?` drops without finish
    for frame in transcoder.push(&bytes) { emit(frame)?; }
    if transcoder.report().outcome.is_some() { break; }
}
if transcoder.report().outcome.is_none() {
    for frame in transcoder.finish() { emit(frame)?; }           // only at normal EOF
}
let report = transcoder.report();
for loss in &report.losses { tracing::warn!(%loss, "stream field dropped"); }
match &report.outcome {
    Some(Outcome::Completed) => {}
    Some(Outcome::Failed(error)) => tracing::warn!(%error, "error frame already sent to client"),
    None => {}
}
```

`Frame { event: Option<String>, data: String }` is not serialized. For SSE, write `event: {event}\n` when `event` is `Some`, then `data: {data}\n\n`. Send frames exactly as produced. The Chat Completions encoder emits the final `[DONE]` frame itself; do not add your own terminators or stop events.

When the transcoder hits a provider error, decode error, or incomplete stream, it emits the client format's error frames itself and sets `Outcome::Failed`. Do not send another error.

## Streaming with AdapterStream (retry or fallback before replying)

Use `AdapterStream` directly when you must see events before sending anything, for example to retry on a different adapter.

```rust
for item in stream.push(&bytes) {
    match item {
        Ok(event) => events.push(event),
        Err(error) => return Ok((events, Some(error))),   // stream is now terminal
    }
}
// at normal EOF:
for item in stream.finish() { /* same match */ }
```

After an error or `finish`, further `push` and `finish` calls yield nothing.

## Model listing

```rust
use relay_adapter::{Adapter, AdapterExt, ModelPage};

let request = adapter.list_models(cursor)?;                  // UnsupportedOperation if no discovery
let response = send(&http, request).await?;
let page: ModelPage = adapter.parse_models(status, &headers, &body)?;
// page.model_ids: Vec<String>, page.next_cursor: Option<String>; an empty page is valid
```

`OpenAiCompatibleAdapter` supports discovery only after `models_url(...)`.

## ID and clock injection

For deterministic tests, `with_id_generator(|| "id".into())` and `with_clock(|| 1235)` on a built-in adapter (or on `Conversion`) set the fallback response ID and `observed_at`.

## Test fixtures

To test gateway wiring offline, run a local mock server (for example an axum router on `127.0.0.1:0`) and point the adapter at it with `base_url` or the compatible adapter's URL. Write the provider bodies yourself from the provider's API docs; a minimal Messages response is enough:

```json
{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}
```

A Messages stream needs `message_start`, `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta` (with `stop_reason` and `usage`), and `message_stop`, each as `event: <name>` plus a JSON `data:` line. If you need more realistic recordings, Relay's own tests keep them under `crates/relay-bridge/tests/fixtures/{chat_completions,messages,responses}/` and `crates/relay-adapter/tests/fixtures/`.
