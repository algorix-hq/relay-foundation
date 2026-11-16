# Client

Prefer `relay-client` for normalized generation, events, or model discovery. It drives an adapter over a consumer-supplied transport. Use the adapter directly when you need raw provider bytes or `Transcoder` for gateway output.

## Transport choice

Enable `features = ["reqwest"]` on the git dependency to use the ready-made transport:

```rust
use std::sync::Arc;
use relay_adapter::providers::anthropic::AnthropicAdapter;
use relay_client::{Client, ReqwestTransport};

let client = Client::new(
    Arc::new(AnthropicAdapter::new("key").default_max_tokens(4096)),
    Arc::new(ReqwestTransport::new()),
);
```

`ReqwestTransport::with_client(reqwest::Client)` reuses an application-configured client. `Default` is equivalent to `new`. Invalid request headers and non-UTF-8 response header values become transport errors. Duplicate header values retain their order; reqwest's `HeaderMap` does not guarantee global order across header names.

Without this feature, the crate has no HTTP client or runtime dependency. Implement:

```rust
use futures_core::{future::BoxFuture, stream::BoxStream};
use relay_adapter::{Headers, ProviderRequest};
use relay_client::{BoxError, Transport, TransportResponse};

struct ApplicationTransport;

impl Transport for ApplicationTransport {
    fn send(&self, request: ProviderRequest) -> BoxFuture<'_, Result<TransportResponse, BoxError>> {
        Box::pin(async move {
            todo!("send request once; return status, headers, and an owned body stream")
        })
    }
}
```

`BoxError = Box<dyn std::error::Error + Send + Sync>`. `Transport: Send + Sync` receives an owned `ProviderRequest`. `TransportResponse` has public `status: u16`, `headers: Headers`, and `body: BoxStream<'static, Result<Vec<u8>, BoxError>>`. The future and body stream must be `Send` and cancel their transport work on drop.

`Client::new` takes `Arc<dyn relay_adapter::Adapter>` and `Arc<dyn Transport>`. `Client` is cloneable; all three operation futures are `Send`.

## Generate

Given a `relay_protocol::Request`:

```rust
let generated = client.generate(&request).await?;
for loss in &generated.losses { eprintln!("request omitted: {loss}"); }
let response = generated.response;
```

`generate(&Request) -> Result<Generated, ClientError>` is async. It clones the input and sets `delivery = None`, regardless of the caller's streaming setting. `Generated` exposes `response: relay_protocol::Response` and `losses: Vec<relay_protocol::ConversionLoss>`, and derives `Clone`, `Debug`, and `PartialEq`.

## Stream

```rust
use futures_util::StreamExt;

let mut events = client.stream(&request).await?;
for loss in events.losses() { eprintln!("request omitted: {loss}"); }
while let Some(item) = events.next().await {
    let event = item?;
    println!("{event:?}");
}
```

`stream(&Request) -> Result<EventStream, ClientError>` is async. It clones the request, forces `delivery.stream = true`, and preserves `stream_usage` if delivery was present, otherwise defaulting it to `true`. On successful headers it returns `EventStream: Stream<Item = Result<relay_protocol::StreamEvent, ClientError>> + Send + Unpin`. `losses()` borrows the request conversion losses.

A rejected HTTP response is read and classified by the adapter before `stream` returns: the error is `ClientError::Protocol(Error::Provider(..))`. A body read failure is `Transport`, even on this rejection path.

Earlier events are preserved before a streaming failure. After the first error, the stream returns `None` and never polls the body again. Only normal body EOF calls `AdapterStream::finish`; required termination missing at EOF yields `Error::IncompleteStream`. No completion events are synthesized.

## List models

```rust
let page = client.list_models(None).await?;
for id in &page.model_ids { println!("{id}"); }
if let Some(cursor) = page.next_cursor.as_deref() {
    let next_page = client.list_models(Some(cursor)).await?;
}
```

`list_models(Option<&str>) -> Result<relay_adapter::ModelPage, ClientError>` is async. It returns IDs and pagination only, not capabilities. Unsupported discovery returns `Protocol(Error::UnsupportedOperation(..))`; an empty page is a valid success.

## ClientError

```rust
use relay_client::ClientError;
use relay_protocol::Error;

match client.generate(&request).await {
    Ok(generated) => println!("{:?}", generated.response.stop_reason),
    Err(ClientError::Protocol(Error::Provider(error))) => {
        eprintln!("{:?}, status {:?}, retry after {:?}",
            error.category, error.http_status, error.retry_after);
    }
    Err(ClientError::Protocol(error)) => eprintln!("relay: {error}"),
    Err(ClientError::Transport(error)) => eprintln!("transport: {error}"),
}
```

`ClientError::Protocol(relay_protocol::Error)` includes conversion, configuration, decoding, incomplete-stream, unsupported-operation, and adapter-classified provider failures. `ClientError::Transport(BoxError)` preserves send and body-read failures without classification. `ClientError` implements `Debug`, `Display`, `std::error::Error` with the inner error as `source`, and `From<relay_protocol::Error>`.

## Cancellation and retry ownership

The client sends once. It has no retry, timeout, or fallback. The consumer owns these policies and transport-level settings. Drop a pending operation future or `EventStream` to cancel; do not call `finish` on cancellation or transport failure. Cancellation does not synthesize an error or completion. Retrying after events have been delivered requires an explicit consumer policy for that partial output.
