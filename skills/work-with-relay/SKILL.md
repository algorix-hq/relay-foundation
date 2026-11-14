---
name: work-with-relay
description: Use relay-foundation crates (`relay_protocol`, `relay_bridge`, `relay_adapter`) from a consumer codebase. Covers bridging Chat Completions, Responses, and Messages; OpenAI, Anthropic, and OpenAI-compatible adapters; streaming; errors; and custom adapters. Use it when code depends on a `relay_*` crate or the user wants to use Relay.
---

# Working with Relay

Relay is a set of three Rust crates that convert LLM API traffic. It never makes network calls: the consumer owns HTTP, the async runtime, retries, and cancellation. Relay turns bytes into typed values and back, and classifies provider failures.

| Crate | Owns |
|---|---|
| `relay-protocol` | Provider-neutral data: `Request`, `Response`, `StreamEvent`, `RelayError` |
| `relay-bridge` | One API format each: decode/encode requests, responses, and streams. Built-ins: `ChatCompletionsBridge`, `ResponsesBridge`, `MessagesBridge` |
| `relay-adapter` | One provider endpoint each: URL, auth headers, provider error classification, framing. Built-ins: `OpenAiAdapter` (Responses), `AnthropicAdapter` (Messages), `OpenAiCompatibleAdapter` (Chat Completions by default, or Responses) |

A gateway uses two bridges. The **client bridge** matches the API the gateway's caller speaks. The adapter carries its own **provider bridge** for the API the provider speaks. They are independent: a Chat Completions client can be served by an Anthropic adapter.

```
client bytes ─ client.decode_request ─▶ Request ─ adapter.build_request ─▶ ProviderRequest ─▶ consumer HTTP
client bytes ◀ client.encode_response ─ Response ◀ adapter.parse_response ◀ status, headers, body
client frames ◀ Transcoder (adapter.open_stream + client stream encoder) ◀ provider byte chunks
```

## Rules that keep consumer code correct

These come from the library's design. Code that breaks them usually compiles but behaves wrong.

1. **Pin a `v1` or later tag.** Depend through git with `tag = "v1.0.0-alpha.1"` or later. Never use a branch, `rev` on an untagged commit, crates.io, or a `v0.x` tag (those are the old, incompatible implementation). All three crates must use the same tag. See `references/dependency.md`.
2. **Only the adapter classifies provider errors.** Never inspect a provider body yourself to decide whether it is an error, and never map HTTP status codes to error categories in consumer code. Pass status, headers, and body to `parse_response`, `open_stream` / `RejectedResponse::classify`, or `parse_models`. Bridges have no provider error variant by design.
3. **Conversion is strict by default.** Anything the target format cannot express returns `ConversionError` (`Unrepresentable`, `Invalid`, or `Unsupported`). Do not pre-strip fields to make conversion succeed. If dropping is acceptable, allow it explicitly with `LossScope` and log or return every `ConversionLoss` you get back. See `references/errors.md`.
4. **Keep the decoded client `Request`.** `encode_response` and the stream encoder need the same `Request` the client bridge decoded at ingress.
5. **Relay never synthesizes completion.** A stream that fails ends with an error, not a fake `Stop`. On transport failure or client cancellation, drop the stream or transcoder without calling `finish`. Call `finish` only at a normal end of the provider body.
6. **Transport and cancellation are consumer errors.** `RelayError` has no transport variant. Wrap `RelayError` with your HTTP client's error in your own enum.
7. **Protocol stays provider-neutral.** Do not add provider-specific fields to `Request` or `Response`, and do not branch on provider names when building them. Provider settings go on the adapter or bridge constructor (for example `AnthropicAdapter::version`, `OpenAiAdapter::store`).
8. **Relay has no model catalog.** It does not know context windows, prices, or capabilities. Do not look for such APIs; keep them in consumer code.
9. **Respond to clients with the client format's error encoder.** Use `ChatCompletionsErrors`, `ResponsesErrors`, or `MessagesErrors` matching the client bridge, with `encode_error` before the response starts and `encode_stream_error` after.

## Pick the reference for the task

Read only what the task needs. The references give exact type names, method signatures, and per-format behavior, so they are usually enough to write compiling code without reading Relay's source.

| Task | Read |
|---|---|
| Add or upgrade the dependency | `references/dependency.md` |
| Build or read `Request`, `Response`, `StreamEvent` values | `references/protocol.md` |
| Wire a client bridge to an adapter, non-streaming or streaming, plus HTTP glue | `references/composition.md` |
| Select Chat Completions `reasoning_content` or `reasoning` for history, responses, and streams | `references/composition.md`, `references/errors.md` |
| Map errors to client responses, allow or report losses, know what a format cannot express | `references/errors.md` |
| Support a provider that is not built in | `references/custom-adapter.md` |

Open Relay's source or `EXAMPLES.md` only when one of these happens:

- `cargo check` reports a name or signature that differs from the references (the pinned tag may be newer);
- the task depends on a behavior the references do not describe.

When you do, go straight to the file that owns the item instead of browsing:

| Item | File |
|---|---|
| `Adapter`, `AdapterExt`, `FrameDecoder`, `ModelDiscovery`, `RejectedResponse` | `crates/relay-adapter/src/lib.rs` |
| Built-in adapters and their builder methods | `crates/relay-adapter/src/builtins.rs` |
| `AdapterStream`, `Transcoder`, `Outcome` | `crates/relay-adapter/src/stream.rs` |
| `ErrorEncoder`, `Stage`, `EncodedError` / built-in encoders | `crates/relay-adapter/src/error.rs` / `encoders.rs` |
| `Headers`, `Method`, `ProviderRequest` | `crates/relay-adapter/src/http.rs` |
| `SseDecoder` | `crates/relay-adapter/src/sse.rs` |
| `Bridge`, `StreamDecoder`, `StreamEncoder`, `ConversionOptions`, `LossScope`, `ApiFormat` | `crates/relay-bridge/src/lib.rs` |
| Per-format encode rules | `crates/relay-bridge/src/{chat_completions,responses,messages}/request.rs` |
| Protocol types | `crates/relay-protocol/src/{request,message,content,tool,response,stream,error}.rs` |

There are no other crates and no per-provider files; do not search for `anthropic.rs` or `providers/`. In a consumer project the source lives in Cargo's git checkout, `~/.cargo/git/checkouts/relay-foundation-*/<rev>/`.

## Before finishing

- Run `cargo check` and `cargo clippy -- -D warnings` in the consumer crate. Relay types are strict, and many wrong guesses show up as compile errors.
- Search the diff for places that inspect provider bodies or status codes outside the adapter, for dropped `losses` values, and for `finish()` calls on cancellation paths.
- Do not make live provider calls just to test wiring. Use recorded bodies or a local mock server.
