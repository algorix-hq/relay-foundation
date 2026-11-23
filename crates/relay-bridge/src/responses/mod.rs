//! Responses API conversion, checked against openai 7.55.0.
//!
//! Request encoding emits system instructions as input items, never `instructions`.
//! `store` is omitted unless configured; `include` is never added. Request ingress
//! rejects every field without a protocol representation, including status and phase.
//! Request `service_tier: "default"` maps to the standard tier.
//! Reasoning replay requires `provider_id`; a Relay ID is never used for replay.
//! Missing replay identity requires explicit Reasoning loss permission.
//!
//! Response decoding ignores new fields within known items, including annotations,
//! logprobs, phase, status, async, caller, and namespace. Unknown item kinds are Unknown.
//! Top-level fields without protocol counterparts are ignored, including metadata,
//! service_tier, previous_response_id, conversation, and request configuration echoes.
//! Usage total_tokens and token details other than cached_tokens, cache_write_tokens,
//! and reasoning_tokens are ignored, including cache-write TTL breakdowns.
//! Provider response and item IDs become both Relay IDs and provider_id; missing IDs
//! use DecodeContext.response_id and `{response_id}:{index}`, respectively.
//! Timestamps retain observed_at and optional provider_created_at independently.
//! OpenAPI permits numeric created_at; live gpt-6-luna returned integer 1794402281; fractional values fail Decode without truncation.
//! Multiple response reasoning-text segments are concatenated in wire order;
//! ingress rejects multiple segments because their boundaries cannot be retained.
//! Reasoning input status and other unrepresented item metadata are rejected on ingress.
//!
//! Client response and stream envelopes share encode_settings with request encoding:
//! tools comes from Request.tools.definitions (default []), tool_choice from its
//! choice (default auto), parallel_tool_calls from its parallel (default true).
//! temperature, top_p, and max_output_tokens come from Request.parameters; absent
//! sampling values are null and absent max_output_tokens is omitted. reasoning comes
//! from Request.reasoning and text.format from Request.response_format, both omitted
//! when absent. instructions and access_programs are null; metadata is {}. Other
//! unsupported echoes, including previous_response_id, are absent. Instructions are
//! system input items, and ingress rejects unsupported configuration rather than
//! retaining it. Client encoding uses Relay IDs and provider_created_at with observed_at
//! as fallback. Message status follows completion; adjacent text with one ID regroups.
//! `object` is `response`, `error` is null for normal output, status/incomplete details
//! follow the stop reason, and `output_text` is derived from text. SDK incomplete
//! reasons `max_messages` and `steered` round-trip as exact `Other` values.
//! Usage totals are summed; missing required cache-read/cache-write/reasoning counters are errors.
//! The SDK permits absent usage, including on terminal responses; provisional usage
//! is never promoted to final usage. Function call_id stays distinct from item id.
//!
//! Streaming ignores lifecycle-only queued/compaction progress, content
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
    ApiFormat, Bridge, BridgeError, ConversionOptions, DecodeContext, Encoded, StreamDecoder,
    StreamEncoder,
};
use relay_protocol::{Request, Response};

#[derive(Clone, Debug, Default)]
pub struct ResponsesBridgeConfig {
    /// None leaves the provider default unchanged.
    pub store: Option<bool>,
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

    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError> {
        request::decode(body)
    }

    fn encode_request(
        &self,
        input: &Request,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        request::encode(input, options, self.config.store)
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
        request: &Request,
        output: &Response,
        options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        response::encode(request, output, options)
    }

    fn stream_decoder(&self, context: &DecodeContext) -> Box<dyn StreamDecoder> {
        Box::new(stream::Decoder::new(context.clone()))
    }

    fn stream_encoder(
        &self,
        request: &Request,
        options: &ConversionOptions,
    ) -> Box<dyn StreamEncoder> {
        Box::new(stream::Encoder::new(options.clone(), request.clone()))
    }
}
