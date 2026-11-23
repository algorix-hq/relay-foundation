---
name: work-with-relay
description: Use relay-foundation crates (`relay_protocol`, `relay_bridge`, `relay_adapter`, `relay_client`, `relay_catalog`) from a consumer codebase. Covers bridging Chat Completions, Responses, and Messages; OpenAI, Anthropic, Ollama Cloud, and OpenAI-compatible adapters; client transports; streaming; errors; and custom adapters. Use it when code depends on a `relay_*` crate or the user wants to use Relay.
---

# Working with Relay

Relay converts LLM API traffic, classifies provider failures, and offers a client over an injected transport plus a separate catalog types crate. Default builds have no HTTP client or async runtime; `relay-client` offers an opt-in `reqwest` transport. The consumer owns runtime choice, retries, timeouts, fallback, and cancellation policy.

| Crate | Owns |
|---|---|
| `relay-protocol` | Provider-neutral data: `Request`, `Response`, `StreamEvent`, `relay_protocol::Error` |
| `relay-bridge` | One API format each: decode/encode requests, responses, and streams. Built-ins: `ChatCompletionsBridge`, `ResponsesBridge`, `MessagesBridge` |
| `relay-adapter` | One provider endpoint each: URL, auth headers, provider error classification, framing. Built-ins under `providers`: `openai::OpenAiAdapter` (Responses), `anthropic::AnthropicAdapter` (Messages), `openai_compatible::OpenAiCompatibleAdapter` (Chat Completions with `max_tokens` and a selectable `reasoning_field`), `ollama_cloud::OllamaCloudAdapter` (Chat Completions, wraps `OpenAiCompatibleAdapter`), all built on `BaseAdapter`. Built-in adapters fix their bridge; pass a different bridge to `BaseAdapter::new` in a custom adapter |
| `relay-client` | `Client::generate`, `stream`, and `list_models` over a consumer-injected `Transport`; optional `ReqwestTransport` with feature `reqwest` |
| `relay-catalog` | Catalog data types only: `Lab`, `Provider`, `Auth`, `Model`, `ProviderModel`, `ProviderModelAlias`, `Spec`, `Pricing`, and `validate`. No data, no fetching |

A gateway uses two bridges. The **client bridge** matches the API the gateway's caller speaks. The adapter carries its own **provider bridge** for the API the provider speaks. They are independent: a Chat Completions client can be served by an Anthropic adapter.

Prefer `relay-client` when you want normalized responses or events without driving the adapter/HTTP loop yourself. Drive the adapter directly when you need raw provider bytes, such as gateway stream re-encoding with `Transcoder`. See `references/client.md`.

```
client bytes ─ client.decode_request ─▶ Request ─ adapter.build_request ─▶ ProviderRequest ─▶ consumer HTTP
client bytes ◀ client.encode_response ─ Response ◀ adapter.parse_response ◀ status, headers, body
client frames ◀ Transcoder (adapter.open_stream + client stream encoder) ◀ provider byte chunks
```

## Rules that keep consumer code correct

These come from the library's design. Code that breaks them usually compiles but behaves wrong.

1. **Pin a commit with `rev`.** Depend through git with `rev = "<commit>"`. Never use a branch, a tag, or crates.io. All `relay-*` crates must use the same `rev`. See `references/dependency.md`.
2. **Only the adapter classifies provider errors.** Never inspect a provider body yourself to decide whether it is an error, and never map HTTP status codes to error categories in consumer code. Pass status, headers, and body to `parse_response`, `open_stream` / `RejectedResponse::classify`, or `parse_models`. Bridges have no provider error variant by design.
3. **Conversion is strict by default.** Anything the target format cannot express returns `ConversionError` (`Unrepresentable`, `Invalid`, or `Unsupported`). Do not pre-strip fields to make conversion succeed. If dropping is acceptable, allow it explicitly with `LossScope` and log or return every `ConversionLoss` you get back. See `references/errors.md`.
4. **Keep the decoded client `Request`.** `encode_response` and the stream encoder need the same `Request` the client bridge decoded at ingress.
5. **Relay never synthesizes completion.** A stream that fails ends with an error, not a fake `Stop`. On transport failure or client cancellation, drop the stream or transcoder without calling `finish`. Call `finish` only at a normal end of the provider body.
6. **Transport failures stay separate.** `relay_protocol::Error` has no transport variant. `relay-client::ClientError` distinguishes `Protocol` and `Transport`; when driving an adapter directly, use your own wrapper. Dropping a client future or event stream cancels it without a synthesized error.
7. **Protocol stays provider-neutral.** Do not add provider-specific fields to `Request` or `Response`, and do not branch on provider names when building them. Provider settings go on the adapter or bridge constructor (for example `AnthropicAdapter::version`, `OpenAiAdapter::store`).
8. **Relay ships catalog types but no catalog data.** `relay-catalog` describes models, providers, specs, and prices; the consumer owns the data, its source, refresh, and caching. Bridges and adapters never read the catalog, so pre-checking a request against catalog data is consumer code. Use required `Provider.auth` (`Byok` or `Subscription`) for credential selection and match required `api_formats` strings against the bridge's `ApiFormat` in consumer code. Optional `Provider.base_url` and `Provider.reasoning_field` carry the API root and the Chat Completions reasoning field name as strings; pass them to the adapter (`OpenAiCompatibleAdapter::reasoning_field`) yourself. `ProviderModelAlias` has no canonical model link; send its `upstream_slug`, and treat optional `models` as possible routing targets, not the model that served a request. A missing catalog field means "not verified" and must not by itself block a request. `Model.spec` is display-only; use `ProviderModel.spec` for request decisions. `spec.reasoning` is `false` (verified unsupported) or an object with optional `effort: {levels, default_level}`, `budget: {min, max}`, and `display`; `true` is rejected.
   `spec.service_tiers` optionally lists supported protocol tier strings (for example `["standard", "priority"]`). When supplied, the list must be nonempty and its entries nonempty and unique. `spec.default_service_tier`, if present, must name a listed tier.
9. **Respond to clients with the client format's error encoder.** Use `ChatCompletionsErrors`, `ResponsesErrors`, or `MessagesErrors` matching the client bridge, with `encode_error` before the response starts and `encode_stream_error` after.

## Pick the reference for the task

Read only what the task needs. The references give exact type names, method signatures, and per-format behavior, so they are usually enough to write compiling code without reading Relay's source.

| Task | Read |
|---|---|
| Add or upgrade the dependency | `references/dependency.md` |
| Build or read `Request`, `Response`, `StreamEvent` values | `references/protocol.md` |
| Generate, stream, or list models with an injected transport or `ReqwestTransport` | `references/client.md` |
| Wire a client bridge to an adapter, non-streaming or streaming, plus HTTP glue | `references/composition.md` |
| Select Chat Completions `reasoning_content` or `reasoning` for history, responses, and streams | `references/composition.md`, `references/errors.md` |
| Map errors to client responses, allow or report losses, know what a format cannot express | `references/errors.md` |
| Support a provider that is not built in | `references/custom-adapter.md` |

Open Relay's source only when one of these happens:

- `cargo check` reports a name or signature that differs from the references (the pinned rev may be newer);
- the task depends on a behavior the references do not describe.

When you do, go straight to the file that owns the item instead of browsing:

| Item | File |
|---|---|
| `Adapter`, `AdapterExt`, `FrameDecoder`, `ModelDiscovery`, `RejectedResponse` | `crates/relay-adapter/src/lib.rs` |
| Provider adapters and their builder methods | `crates/relay-adapter/src/providers/{openai,anthropic,openai_compatible,ollama_cloud}/mod.rs` |
| `BaseAdapter`, `Classify` | `crates/relay-adapter/src/base.rs` |
| `AdapterStream`, `Transcoder`, `Outcome` | `crates/relay-adapter/src/stream.rs` |
| `ErrorEncoder`, `Stage`, `EncodedError` / built-in encoders | `crates/relay-adapter/src/error.rs` / `encoders.rs` |
| `Headers`, `Method`, `ProviderRequest` | `crates/relay-adapter/src/http.rs` |
| `Client`, `Transport`, `TransportResponse`, `Generated`, `EventStream`, `ClientError` | `crates/relay-client/src/lib.rs` |
| `ReqwestTransport` | `crates/relay-client/src/reqwest.rs` |
| `SseDecoder` | `crates/relay-adapter/src/sse.rs` |
| `Bridge`, `StreamDecoder`, `StreamEncoder`, `ConversionOptions`, `LossScope`, `ApiFormat` | `crates/relay-bridge/src/lib.rs` |
| Per-format encode rules | `crates/relay-bridge/src/{chat_completions,responses,messages}/request.rs` |
| Protocol types | `crates/relay-protocol/src/{request,message,content,tool,response,stream,error}.rs` |
| Catalog types | `crates/relay-catalog/src/lib.rs` |

In a consumer project the source lives in Cargo's git checkout, `~/.cargo/git/checkouts/relay-foundation-*/<rev>/`.

## Before finishing

- Run `cargo check` and `cargo clippy -- -D warnings` in the consumer crate. Relay types are strict, and many wrong guesses show up as compile errors.
- Search the diff for places that inspect provider bodies or status codes outside the adapter, for dropped `losses` values, and for `finish()` calls on cancellation paths.
- Do not make live provider calls just to test wiring. Use recorded bodies or a local mock server.
