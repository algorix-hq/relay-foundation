//! Responses API conversion, checked against openai 7.55.0.
//!
//! Request encoding emits system instructions as input items, never `instructions`.
//! `store` is omitted unless configured or supplied natively; `include` is never added.
//! Reasoning replay requires its original ID; opaque content is passed unchanged.
//!
//! Response decoding ignores new fields within known items, including annotations,
//! logprobs, phase, function-call item IDs/status/async/caller/namespace, and reasoning status.
//! Message IDs/status are retained in native `output` metadata aligned with protocol
//! content; adjacent text parts sharing that identity are regrouped on encoding.
//! Native `usage` retains token details not represented by the protocol.
//! The SDK's cache-write count is mapped too; its TTL breakdown remains native.
//! Multiple response reasoning-text segments are concatenated in wire order;
//! ingress rejects multiple segments because their boundaries cannot be retained.
//! Reasoning input status and other unrepresented item metadata are rejected on ingress.
//!
//! Gateway encoding requires ID, creation time, and all required response configuration
//! fields (`access_programs`, `instructions`, `metadata`, `parallel_tool_calls`,
//! `temperature`, `tool_choice`, `tools`, `top_p`) from native context. None is guessed.
//! ID may instead come from the protocol, time from `ConversionOptions::created_at`.
//! `object` is `response`, `error` is null for normal output, status/incomplete details
//! follow the stop reason, and `output_text` is derived from text. SDK incomplete
//! reasons `max_messages` and `steered` round-trip as exact `Other` values.
//! Message IDs/status must be supplied as native output metadata. Usage totals are summed; optional
//! protocol counters required by the wire must be supplied by native usage if absent.
//! Streaming has no native envelope in the shared contract; `stream_native` supplies
//! these same facts, including per-item message IDs/status and function-call item
//! IDs (distinct from call IDs), explicitly. Its `output` is an array of metadata
//! objects keyed by protocol item index, not complete output items.
//! Example: `[{}, {"id":"msg_1","status":"completed"}, {"id":"fc_1"}]`
//! supplies the message and function-call identities following a reasoning item.
//!
//! Streaming ignores lifecycle-only in_progress/queued/compaction progress, content
//! part and reasoning-summary part lifecycle and hosted-tool progress events.
//! Text/arguments/reasoning `.done` echoes and final item snapshots are reconciled
//! against streamed prefixes: only missing suffixes are emitted, never duplicates.
//! Output-bearing audio, refusal, annotation, image,
//! code, shell, MCP, custom-tool, and future events are preserved as unknown deltas.
//! Unknown item bodies are preserved once, on output_item.done, not duplicated.
//! Events without an active output item are preserved as standalone unknown items.

mod request;
mod response;
mod stream;
mod wire;

use crate::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, Encoded, Input, NativeFields, Output,
    StreamDecoder, StreamEncoder,
};

#[derive(Clone, Debug, Default)]
pub struct ResponsesBridgeConfig {
    /// None leaves the provider default unchanged; native `store` then wins.
    pub store: Option<bool>,
    /// Required gateway response facts and item metadata, used only by stream encoding.
    pub stream_native: NativeFields,
}

#[derive(Clone, Debug, Default)]
pub struct ResponsesBridge {
    config: ResponsesBridgeConfig,
}

impl ResponsesBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: ResponsesBridgeConfig) -> Self {
        Self { config }
    }
}

impl Bridge for ResponsesBridge {
    fn format(&self) -> &ApiFormat {
        &ApiFormat::RESPONSES
    }

    fn decode_request(&self, body: &[u8]) -> Result<Input, BridgeError> {
        request::decode(body)
    }

    fn encode_request(
        &self,
        input: &Input,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        request::encode(input, options, self.config.store)
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

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(stream::Decoder::default())
    }

    fn stream_encoder(&self, options: &ConversionOptions) -> Box<dyn StreamEncoder> {
        Box::new(stream::Encoder::new(
            options.clone(),
            self.config.stream_native.clone(),
        ))
    }
}
