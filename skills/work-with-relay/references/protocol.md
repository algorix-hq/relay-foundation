# Protocol types (`relay-protocol`)

All types are plain data with serde. They use `deny_unknown_fields`, so JSON with extra keys fails to deserialize. Most `Option` fields are skipped when `None`.

## Contents
- Request
- Messages and content
- Tools
- Response
- StreamEvent
- Errors (summary; details in errors.md)

## Request

```rust
pub struct Request {
    pub version: Version,                 // Version::V1, serialized as "1"
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Option<ToolConfig>,
    pub parameters: Option<Parameters>,
    pub response_format: Option<ResponseFormat>,
    pub reasoning: Option<Reasoning>,
    pub delivery: Option<Delivery>,       // how the response is delivered
}
pub struct Delivery { pub stream: bool, pub stream_usage: bool }
pub struct Parameters {
    pub max_output_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub service_tier: Option<ServiceTier>,
    pub stop: Vec<String>,
}
pub enum ServiceTier {
    Auto, Standard, Flex, Priority, Scale, Reserved, Batch, Custom(String),
}
pub enum ResponseFormat {
    Text, JsonObject,
    JsonSchema { name: Option<String>, description: Option<String>, schema: Value, strict: Option<bool> },
}
pub struct Reasoning {
    pub effort: Option<ReasoningEffort>,      // ReasoningEffort(String), passed through
    pub budget_tokens: Option<u64>,
    pub summary: Option<ReasoningSummary>,    // ReasoningSummary(String), passed through
    pub thinking: Option<ThinkingMode>,       // Enabled | Disabled | Adaptive
}
```

`Request` implements `PartialEq` but not `Eq` because of `f64` fields. A gateway usually gets a `Request` from `client.decode_request(body)` rather than building one. Set `delivery` to stream: `request.delivery = Some(Delivery { stream: true, stream_usage: true })`.

Example:

```rust
use relay_protocol::{Message, Parameters, Request, TextPart, UserContent, Version};

let request = Request {
    version: Version::V1,
    model: "target-model".into(),
    messages: vec![Message::User {
        content: vec![UserContent::Text(TextPart { text: "hello".into(), cache_control: None })],
    }],
    tools: None,
    parameters: Some(Parameters { max_output_tokens: Some(1024), temperature: None, top_p: None, service_tier: None, stop: vec![] }),
    response_format: None,
    reasoning: None,
    delivery: None,
};
```

## Messages and content

```rust
pub enum Message {                         // serde tag "role"
    System { content: Vec<TextPart> },
    Developer { content: Vec<TextPart> },
    User { content: Vec<UserContent> },
    Assistant { content: Vec<AssistantContent> },
}
pub struct TextPart { pub text: String, pub cache_control: Option<CacheControl> }
pub struct CacheControl { pub ttl_seconds: Option<u64> }   // ttl 0 is rejected
pub enum MediaSource { Base64 { media_type: String, data: String }, Url { url: String } }
pub struct ImagePart { pub source: MediaSource, pub detail: Option<ImageDetail>, pub cache_control: Option<CacheControl> }
pub struct DocumentPart { pub source: MediaSource, pub title: Option<String>, pub filename: Option<String>, pub cache_control: Option<CacheControl> }

pub enum UserContent {
    Text(TextPart), Image(ImagePart), Document(DocumentPart),
    ToolResult { tool_call_id: String, content: Vec<ToolResultContent>, is_error: Option<bool>, cache_control: Option<CacheControl> },
}
pub enum ToolResultContent { Text(TextPart), Image(ImagePart), Document(DocumentPart) }

pub enum AssistantContent {
    Text { id: String, provider_id: Option<String>, text: String, cache_control: Option<CacheControl> },
    ToolCall { id: String, provider_id: Option<String>, call_id: String, name: String,
               arguments: String /* exact JSON text, not parsed */, cache_control: Option<CacheControl> },
    Reasoning { id: String, provider_id: Option<String>, text: Option<String>, summary: Vec<String>, opaque: Option<String> },
    Unknown { id: String, provider_id: Option<String>, value: Value },
}
```

- `ToolCall.arguments` is the provider's exact JSON text. Keep it as a string; re-serializing changes key order and duplicates.
- `Reasoning.text: Some("")` differs from `None`. Preserve both.
- `Reasoning.opaque` must be sent back unchanged in later turns.
- `Unknown` keeps provider output Relay did not recognize. It cannot be encoded into a request; bridges reject it unless `LossScope::UnknownContent` is allowed.

## Tools

```rust
pub struct ToolConfig { pub definitions: Vec<Tool>, pub choice: Option<ToolChoice>, pub parallel: Option<bool> }
pub enum Tool { Function { name: String, description: Option<String>, parameters: Value, strict: Option<bool>, cache_control: Option<CacheControl> } }
pub enum ToolChoice { Auto, None, Required, Function { name: String } }
```

## Response

```rust
pub struct Response {
    pub version: Version,
    pub id: String,
    pub provider_id: Option<String>,
    pub created_at: Timestamp,          // { observed_at: u64, provider_created_at: Option<u64> }
    pub model: String,
    pub content: Vec<AssistantContent>,
    pub stop_reason: StopReason,
    pub usage: Option<Usage>,
}
pub enum StopReason { EndTurn, MaxTokens, ContextWindowExceeded, ToolUse, StopSequence, ContentFilter, Refusal, Other(String) }
pub struct Usage {
    pub input_tokens: u64,              // includes cache tokens
    pub output_tokens: u64,             // includes reasoning tokens
    pub cache_read_tokens: Option<u64>, pub cache_write_tokens: Option<u64>, pub reasoning_tokens: Option<u64>,
}
```

Cache and reasoning counts are subsets of the totals. Do not add them to the totals.

## StreamEvent

```rust
pub enum StreamEvent {
    Start { response_id: String, provider_id: Option<String>, created_at: Timestamp, model: Option<String> },
    Metadata(MetadataScope),
    ItemStart { index: u32, id: String, provider_id: Option<String>, item: StreamItem },
    Delta { index: u32, delta: StreamDelta },
    ItemEnd { index: u32 },
    Stop { stop_reason: StopReason, usage: Option<Usage> },
}
pub enum MetadataScope {
    Response { usage: Option<Usage> /* provisional */, model: Option<String> },
    Item { index: u32, provider_id: Option<String>, call_id: Option<String>, name: Option<String> },
}
pub enum StreamItem { Text, ToolCall { call_id: Option<String>, name: Option<String> }, Reasoning, Unknown }
pub enum StreamDelta {
    Text { text: String }, ToolCallArguments { text: String }, ReasoningText { text: String },
    ReasoningSummary { summary_index: u32, text: String }, ReasoningOpaque { value: String }, Unknown { value: Value },
}
```

Order: `Start`, then items. Each item has `ItemStart`, then its metadata and deltas, then `ItemEnd`. Different items may interleave. `Stop` ends a normal stream. `Metadata` updates are partial; absent fields keep earlier values. Usage in `Metadata` is provisional; only `Stop.usage` is final. Failures never appear as events. They arrive as `Err(relay_protocol::Error)` from `AdapterStream`.

## Errors

`relay_protocol::Error` variants: `Provider(ProviderError)`, `Conversion(ConversionError)`, `Decode(DecodeError)`, `IncompleteStream(IncompleteStream)`, `Configuration(ConfigurationError)`, `UnsupportedOperation(UnsupportedOperation)`. Each inner type converts into `relay_protocol::Error` with `From`. Details are in `errors.md`.
