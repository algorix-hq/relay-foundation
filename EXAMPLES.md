# 0. How one request is converted

Actual output of passing the same `Request` through the three bridges and the built-in adapters. In the JSON below, only the key order was rearranged for readability.

## Request

```rust
use relay_protocol::{
    AssistantContent, Delivery, ImageDetail, ImagePart, MediaSource, Message, Parameters,
    Reasoning, ReasoningEffort, Request, TextPart, Tool, ToolChoice, ToolConfig,
    ToolResultContent, UserContent, Version,
};
use serde_json::json;

let request = Request {
    version: Version::V1,
    delivery: Some(Delivery { stream: true, stream_usage: true }),
    model: "target-model".into(),
    messages: vec![
        Message::User { content: vec![
            UserContent::Text(TextPart { text: "Describe the photo and the lookup result".into(), cache_control: None }),
            UserContent::Image(ImagePart {
                source: MediaSource::Url { url: "https://example.com/photo.png".into() },
                detail: Some(ImageDetail("high".into())), cache_control: None,
            }),
        ]},
        Message::Assistant { content: vec![AssistantContent::ToolCall {
            id: "resp_previous:0".into(), provider_id: None,
            call_id: "call_1".into(), name: "lookup".into(),
            arguments: r#"{ "key": "photo" }"#.into(), cache_control: None,
        }]},
        Message::User { content: vec![UserContent::ToolResult {
            tool_call_id: "call_1".into(),
            content: vec![ToolResultContent::Text(TextPart { text: "Seoul".into(), cache_control: None })],
            is_error: None, cache_control: None,
        }]},
    ],
    tools: Some(ToolConfig {
        definitions: vec![Tool::Function {
            name: "lookup".into(), description: Some("Look up by key".into()),
            parameters: json!({
                "type": "object", "properties": {"key": {"type": "string"}},
                "required": ["key"], "additionalProperties": false
            }),
            strict: Some(true), cache_control: None,
        }],
        choice: Some(ToolChoice::Auto), parallel: Some(false),
    }),
    parameters: Some(Parameters {
        max_output_tokens: Some(4096), temperature: Some(0.7), top_p: None, stop: vec![],
    }),
    response_format: None,
    reasoning: Some(Reasoning {
        effort: Some(ReasoningEffort("high".into())),
        budget_tokens: None, summary: None, thinking: None,
    }),
};
```
```json
{
  "version": "1",
  "model": "target-model",
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "Describe the photo and the lookup result"}, {"type": "image", "source": {"type": "url", "url": "https://example.com/photo.png"}, "detail": "high"}]},
    {"role": "assistant", "content": [{"type": "tool_call", "id": "resp_previous:0", "call_id": "call_1", "name": "lookup", "arguments": "{ \"key\": \"photo\" }"}]},
    {"role": "user", "content": [{"type": "tool_result", "tool_call_id": "call_1", "content": [{"type": "text", "text": "Seoul"}]}]}
  ],
  "tools": {
    "definitions": [
      {"type": "function", "name": "lookup", "description": "Look up by key", "parameters": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"], "additionalProperties": false}, "strict": true}
    ],
    "choice": "auto",
    "parallel": false
  },
  "parameters": {"max_output_tokens": 4096, "temperature": 0.7},
  "reasoning": {"effort": "high"},
  "delivery": {"stream": true, "stream_usage": true}
}
```

## Bridge `encode_request`

```rust
use relay_bridge::{
    Bridge, ChatCompletionsBridge, ConversionOptions, LossScope, MessagesBridge, ResponsesBridge,
};

let strict = ConversionOptions::default();
let chat = ChatCompletionsBridge::new().encode_request(&request, &strict)?;
let responses = ResponsesBridge::new().encode_request(&request, &strict)?;

let lossy = ConversionOptions {
    allow_loss: [LossScope::Sampling, LossScope::ImageDetail].into(),
};
let messages = MessagesBridge::new().encode_request(&request, &lossy)?;
```

| Protocol | Chat Completions | Responses | Messages |
|---|---|---|---|
| `messages` | `messages` | `input` | `messages` |
| `parameters.max_output_tokens` | `max_completion_tokens` | `max_output_tokens` | `max_tokens` |
| `parameters.temperature` | `temperature` | `temperature` | omitted, reported in `losses` |
| `reasoning.effort` | `reasoning_effort` | `reasoning.effort` | `output_config.effort` |
| `delivery` | `stream`, `stream_options.include_usage` | `stream` | `stream` |
| image `detail` | `image_url.detail` | `detail` | omitted, reported in `losses` |
| `tool_call` | assistant `tool_calls[]` | `function_call` item | `tool_use` block |
| `tool_call.arguments` | string as is | string as is | parsed `input` object |
| `tool_result` | `role: "tool"` message | `function_call_output` item | user `tool_result` block |
| `tools.parallel: false` | `parallel_tool_calls: false` | `parallel_tool_calls: false` | `tool_choice.disable_parallel_tool_use: true` |

Chat Completions:

```json
{
  "model": "target-model",
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "Describe the photo and the lookup result"}, {"type": "image_url", "image_url": {"url": "https://example.com/photo.png", "detail": "high"}}]},
    {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "lookup", "arguments": "{ \"key\": \"photo\" }"}}]},
    {"role": "tool", "tool_call_id": "call_1", "content": "Seoul"}
  ],
  "tools": [
    {"type": "function", "function": {"name": "lookup", "description": "Look up by key", "parameters": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"], "additionalProperties": false}, "strict": true}}
  ],
  "tool_choice": "auto",
  "parallel_tool_calls": false,
  "max_completion_tokens": 4096,
  "temperature": 0.7,
  "reasoning_effort": "high",
  "stream": true,
  "stream_options": {"include_usage": true}
}
```

Responses:

```json
{
  "model": "target-model",
  "input": [
    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Describe the photo and the lookup result"}, {"type": "input_image", "image_url": "https://example.com/photo.png", "detail": "high"}]},
    {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{ \"key\": \"photo\" }"},
    {"type": "function_call_output", "call_id": "call_1", "output": [{"type": "input_text", "text": "Seoul"}]}
  ],
  "tools": [
    {"type": "function", "name": "lookup", "description": "Look up by key", "parameters": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"], "additionalProperties": false}, "strict": true}
  ],
  "tool_choice": "auto",
  "parallel_tool_calls": false,
  "max_output_tokens": 4096,
  "temperature": 0.7,
  "reasoning": {"effort": "high"},
  "stream": true
}
```

Messages:

```json
{
  "model": "target-model",
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "Describe the photo and the lookup result"}, {"type": "image", "source": {"type": "url", "url": "https://example.com/photo.png"}}]},
    {"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"key": "photo"}}]},
    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": [{"type": "text", "text": "Seoul"}]}]}
  ],
  "tools": [
    {"name": "lookup", "description": "Look up by key", "input_schema": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"], "additionalProperties": false}, "strict": true}
  ],
  "tool_choice": {"type": "auto", "disable_parallel_tool_use": true},
  "max_tokens": 4096,
  "output_config": {"effort": "high"},
  "stream": true
}
```
```text
/parameters/temperature: Messages cannot express this sampling parameter
/messages/0/content/1/detail: Messages cannot express image detail
```

Without allowed loss, encoding returns `BridgeError::Conversion` at the first unrepresentable item. The table shows the result of adding each value to the request above, one at a time.

| Added value | Chat Completions | Responses | Messages |
|---|---|---|---|
| `reasoning.budget_tokens` | `/reasoning/budget_tokens` | `/reasoning/budget_tokens` | `/reasoning/budget_tokens` (`Invalid`), passes together with `thinking: Enabled` |
| `tool_result.is_error: Some(false)` | passes | `/messages/2/content/0/is_error` | passes |
| `response_format: JsonObject` | passes | passes | `/response_format` |
| `reasoning.summary` | `/reasoning/summary` | passes | `/reasoning/summary` |

## Adapter `build_request`

```rust
use std::sync::Arc;
use relay_adapter::{AdapterExt, AnthropicAdapter, OpenAiAdapter, OpenAiCompatibleAdapter};
use relay_bridge::{ChatCompletionsBridge, LossScope};

let openai = OpenAiAdapter::new("KEY");
let openai_chat = OpenAiCompatibleAdapter::new("https://api.openai.com/v1/chat/completions")
    .api_key("KEY")
    .with_bridge(Arc::new(ChatCompletionsBridge::new()))?;
let anthropic = AnthropicAdapter::new("KEY")
    .allow_loss([LossScope::Sampling, LossScope::ImageDetail]);
let ollama = OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions").api_key("KEY");

let (provider_request, losses) = anthropic.build_request(&request)?;
```

| Adapter | Bridge | `method` `url` | `headers` | `body` |
|---|---|---|---|---|
| `openai` | Responses | `POST https://api.openai.com/v1/responses` | `content-type: application/json`, `authorization: Bearer KEY` | Same as the Responses body |
| `openai_chat` | Chat Completions | `POST https://api.openai.com/v1/chat/completions` | `content-type`, `authorization` | Same as the Chat Completions body |
| `anthropic` | Messages | `POST https://api.anthropic.com/v1/messages` | `content-type`, `authorization: Bearer KEY`, `anthropic-version: 2026-10-21` | Same as the Messages body, 2 `losses` |
| `ollama` | Chat Completions | `POST https://ollama.com/v1/chat/completions` | `content-type`, `authorization` | `max_tokens: 4096` instead of `max_completion_tokens` |

# 1. Protocol (`relay-protocol`)

## Building a Request

```rust
use relay_protocol::{
    Delivery, Message, Parameters, Reasoning, ReasoningEffort, ReasoningSummary,
    Request, ResponseFormat, TextPart, ThinkingMode, UserContent, Version,
};

let mut request = Request {
    version: Version::V1,
    delivery: Some(Delivery { stream: true, stream_usage: true }),
    model: "target-model".into(),
    messages: vec![Message::User {
        content: vec![UserContent::Text(TextPart {
            text: "Describe the photo and the lookup result".into(), cache_control: None,
        })],
    }],
    tools: None,
    parameters: Some(Parameters {
        max_output_tokens: Some(4096), temperature: Some(0.7),
        top_p: Some(0.9), stop: vec![],
    }),
    response_format: Some(ResponseFormat::JsonObject),
    reasoning: Some(Reasoning {
        effort: Some(ReasoningEffort("high".into())), budget_tokens: Some(1024),
        summary: Some(ReasoningSummary("auto".into())),
        thinking: Some(ThinkingMode::Adaptive),
    }),
};
```
```rust
use relay_protocol::{AssistantContent, ImageDetail, ImagePart, MediaSource, ToolResultContent};

if let Message::User { content } = &mut request.messages[0] {
    content.push(UserContent::Image(ImagePart {
        source: MediaSource::Url { url: "https://example.com/photo.png".into() },
        detail: Some(ImageDetail("high".into())), cache_control: None,
    }));
}
request.messages.push(Message::Assistant {
    content: vec![AssistantContent::ToolCall {
        id: "resp_previous:0".into(), provider_id: None,
        call_id: "call_1".into(), name: "lookup".into(),
        arguments: r#"{ "key": "photo" }"#.into(), cache_control: None,
    }],
});
request.messages.push(Message::User {
    content: vec![UserContent::ToolResult {
        tool_call_id: "call_1".into(),
        content: vec![ToolResultContent::Text(TextPart {
            text: "Seoul".into(), cache_control: None,
        })],
        is_error: Some(false), cache_control: None,
    }],
});
```
```rust
use relay_protocol::{Tool, ToolChoice, ToolConfig};
use serde_json::json;

request.tools = Some(ToolConfig {
    definitions: vec![Tool::Function {
        name: "lookup".into(), description: Some("Look up by key".into()),
        parameters: json!({
            "type": "object", "properties": {"key": {"type": "string"}},
            "required": ["key"], "additionalProperties": false
        }),
        strict: Some(true), cache_control: None,
    }],
    choice: Some(ToolChoice::Auto), parallel: Some(false),
});
let wire = serde_json::to_string_pretty(&request)?;
```
```json
{
  "version": "1",
  "delivery": {"stream": true, "stream_usage": true},
  "model": "target-model",
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Describe the photo and the lookup result"
        },
        {
          "type": "image",
          "source": {
            "type": "url",
            "url": "https://example.com/photo.png"
          },
          "detail": "high"
        }
      ]
    },
    {
      "role": "assistant",
      "content": [
        {
          "type": "tool_call",
          "id": "resp_previous:0",
          "call_id": "call_1",
          "name": "lookup",
          "arguments": "{ \"key\": \"photo\" }"
        }
      ]
    },
    {
      "role": "user",
      "content": [
        {
          "type": "tool_result",
          "tool_call_id": "call_1",
          "content": [
            {
              "type": "text",
              "text": "Seoul"
            }
          ],
          "is_error": false
        }
      ]
    }
  ],
  "tools": {
    "definitions": [
      {
        "type": "function",
        "name": "lookup",
        "description": "Look up by key",
        "parameters": {
          "type": "object",
          "properties": {
            "key": {
              "type": "string"
            }
          },
          "required": [
            "key"
          ],
          "additionalProperties": false
        },
        "strict": true
      }
    ],
    "choice": "auto",
    "parallel": false
  },
  "parameters": {
    "max_output_tokens": 4096,
    "temperature": 0.7,
    "top_p": 0.9
  },
  "response_format": "json_object",
  "reasoning": {
    "effort": "high",
    "budget_tokens": 1024,
    "summary": "auto",
    "thinking": "adaptive"
  }
}
```
```json
{
  "tools": {
    "definitions": [],
    "choice": {
      "type": "function",
      "name": "lookup"
    }
  },
  "response_format": {
    "type": "json_schema",
    "name": "result",
    "description": "Result",
    "schema": {
      "type": "object"
    },
    "strict": true
  }
}
```

## Reading a Response

```json
{
  "version": "1",
  "id": "resp_1",
  "provider_id": "resp_1",
  "created_at": {"observed_at": 1235, "provider_created_at": 1234},
  "model": "target-model",
  "content": [
    {
      "type": "text",
      "id": "resp_1:0",
      "text": "Looked it up"
    },
    {
      "type": "tool_call",
      "id": "resp_1:1",
      "call_id": "call_2",
      "name": "lookup",
      "arguments": "{ \"key\":1, \"key\": 2 }"
    },
    {
      "type": "reasoning",
      "id": "reasoning_1",
      "provider_id": "reasoning_1",
      "summary": [
        "Lookup needed"
      ],
      "opaque": "opaque-value"
    },
    {
      "type": "reasoning",
      "id": "resp_1:3",
      "text": ""
    },
    {
      "type": "unknown",
      "id": "resp_1:4",
      "value": {
        "type": "future_output",
        "payload": 7
      }
    }
  ],
  "stop_reason": "pause_turn",
  "usage": {
    "input_tokens": 900,
    "output_tokens": 200,
    "cache_read_tokens": 500,
    "cache_write_tokens": 100,
    "reasoning_tokens": 120
  }
}
```
```rust
use relay_protocol::{AssistantContent, Response, StopReason};

let response: Response = serde_json::from_slice(&response_bytes)?;
for part in &response.content {
    match part {
        AssistantContent::Text { text, .. } => println!("{text}"),
        AssistantContent::ToolCall { call_id, name, arguments, .. } => {
            println!("{call_id}: {name}({arguments})");
        }
        AssistantContent::Reasoning { id, provider_id, text, summary, opaque } => {
            match text.as_deref() {
                None => println!("No reasoning text"),
                Some("") => println!("Empty reasoning text"),
                Some(text) => println!("{text}"),
            }
            println!("ID: {id}, provider ID: {provider_id:?}, summary: {summary:?}, preserved value: {opaque:?}");
        }
        AssistantContent::Unknown { value, .. } => println!("Uninterpreted output: {value}"),
    }
}
if let StopReason::Other(value) = &response.stop_reason {
    println!("Unknown normal stop reason: {value}");
}
```

## StreamEvent order

```json
{"type": "start", "response_id": "resp_1", "provider_id": "resp_1", "created_at": {"observed_at": 1235, "provider_created_at": 1234}, "model": "target-model"}
{"type": "metadata", "scope": "response", "usage": {"input_tokens": 20, "output_tokens": 3}}
{"type": "item_start", "index": 0, "id": "reasoning_1", "provider_id": "reasoning_1", "item": {"type": "reasoning"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_text", "text": "Checking"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_summary", "summary_index": 0, "text": "Lookup"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_opaque", "value": "opaque-value"}}
{"type": "item_end", "index": 0}
{"type": "item_start", "index": 1, "id": "resp_1:1", "item": {"type": "text"}}
{"type": "delta", "index": 1, "delta": {"type": "text", "text": "Answer"}}
{"type": "item_end", "index": 1}
{"type": "item_start", "index": 2, "id": "resp_1:2", "item": {"type": "tool_call"}}
{"type": "metadata", "scope": "item", "index": 2, "call_id": "call_1", "name": "lookup"}
{"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "{\"key\":"}}
{"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "1}"}}
{"type": "item_end", "index": 2}
{"type": "stop", "stop_reason": "tool_use", "usage": {"input_tokens": 20, "output_tokens": 9}}
```

## Branching on RelayError

```rust
use relay_protocol::{ProviderErrorCategory, RelayError};

fn describe(error: &RelayError) -> String {
    match error {
        RelayError::Provider(error) => {
            let category = match error.category {
                ProviderErrorCategory::Authentication => "authentication",
                ProviderErrorCategory::PermissionDenied => "permission",
                ProviderErrorCategory::InsufficientQuota => "quota",
                ProviderErrorCategory::RateLimited => "rate limit",
                ProviderErrorCategory::Overloaded => "overloaded",
                ProviderErrorCategory::InvalidRequest => "request",
                ProviderErrorCategory::UnsupportedFeature => "unsupported feature",
                ProviderErrorCategory::ModelNotFound => "model not found",
                ProviderErrorCategory::ContextLengthExceeded => "context length",
                ProviderErrorCategory::ServerError => "server",
                ProviderErrorCategory::Unknown => "unclassified",
            };
            format!("{category}: {}, retry after: {:?}", error.message, error.retry_after)
        }
        RelayError::Conversion(error) => format!("conversion {}: {}", error.path, error),
        RelayError::Decode(error) => format!("decode {}: {}", error.path, error),
        RelayError::IncompleteStream(error) => format!("incomplete: {error}"),
        RelayError::Configuration(error) => format!("configuration: {error}"),
        RelayError::UnsupportedOperation(error) => format!("unsupported: {error}"),
    }
}
```

# 2. Bridge (`relay-bridge`)

## Gateway ingress

```rust
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, DecodeContext};
use relay_protocol::Request;
use relay_bridge::chat_completions::MaxTokensField;

let client = ChatCompletionsBridge::new();
let input: Request = client.decode_request(&client_body)?;
println!("model: {}, delivery: {:?}", input.model, input.delivery);

let options = ConversionOptions::default();
let context = DecodeContext { response_id: Some("fallback-id".into()), observed_at: 1235 };
let output = client.decode_response(&provider_body, &context)?;
let encoded = client.encode_response(&input, &output, &options)?;
let client_json = encoded.body;
let response_losses = encoded.losses;

let legacy_server = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens);
```

## Chat Completions reasoning text

```rust
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, ReasoningField};
use relay_bridge::chat_completions::MaxTokensField;
use serde_json::json;

let bridge = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens)
    .with_reasoning_field(ReasoningField::ReasoningContent);
let body = serde_json::to_vec(&json!({
    "model": "target-model",
    "messages": [
        {"role": "assistant", "content": "42", "reasoning_content": "6 times 7 is 42."},
        {"role": "user", "content": "What is half of that?"}
    ],
    "reasoning_effort": "high"
}))?;
let request = bridge.decode_request(&body)?;
let encoded = bridge.encode_request(&request, &ConversionOptions::default())?;
assert_eq!(encoded.body["messages"][0]["reasoning_content"], "6 times 7 is 42.");
```

`ReasoningField::Disabled` is the default. Select `ReasoningContent` for `reasoning_content` or `Reasoning` for `reasoning`. The instance builder also works on `ChatCompletionsBridge::new()`. The selection applies in both directions to request/history, responses, and stream deltas, mapping the selected string field to protocol reasoning text.

Only reasoning text is representable: summaries, opaque values, provider identity, and merging multiple reasoning item boundaries into one string are not supported. Encoding unrepresentable reasoning fails unless the consumer explicitly allows `LossScope::Reasoning` and handles the reported losses.

**Breaking behavior:** known reasoning fields carrying data are now rejected when disabled or unselected, rather than silently discarded during decoding. Select the field each endpoint actually uses; there is no autodetection, model lookup, or provider configuration. `reasoning_effort` is independent of this option. There is no `<think>` extraction or `reasoning_details` support.

The consumer decides whether to include reasoning in replay/history; enabling the field does not add history automatically. Configure the client bridge and the adapter's provider bridge separately when their wire fields differ.

## Messages

```rust
use relay_bridge::{Bridge, ConversionOptions, LossScope, MessagesBridge};

let strict = MessagesBridge::new();
let with_fallback = MessagesBridge::with_default_max_tokens(4096);

let options = ConversionOptions {
    allow_loss: [LossScope::Sampling, LossScope::DeveloperRole].into(),
};
let encoded = with_fallback.encode_request(&request, &options)?;
```

## Responses

```rust
use relay_bridge::{Bridge, ConversionOptions, ResponsesBridge};
use relay_bridge::responses::ResponsesBridgeConfig;

let default_store = ResponsesBridge::new();
let stateless = ResponsesBridge::with_config(ResponsesBridgeConfig { store: Some(false) });

let encoded = stateless.encode_request(&request, &ConversionOptions::default())?;
```

## ConversionOptions and loss reporting

```rust
use relay_bridge::{ConversionOptions, LossScope};
use relay_protocol::ConversionLoss;

let options = ConversionOptions {
    allow_loss: [LossScope::UnknownContent].into(),
};
let encoded = client.encode_response(&input, &output, &options)?;
for ConversionLoss { path, message } in encoded.losses {
    eprintln!("omitted: {path}: {message}");
}
let client_json = encoded.body;
```

## Implementing a custom Bridge

```rust
use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, DecodeContext, Encoded,
    StreamDecoder, StreamEncoder,
};
use relay_protocol::{Request, Response};

struct GatewayJsonBridge { format: ApiFormat }

impl Bridge for GatewayJsonBridge {
    fn format(&self) -> &ApiFormat { &self.format }

    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError> {
        todo!("malformed or unrepresentable requests return BridgeError::Conversion")
    }
    fn encode_request(
        &self, request: &Request, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("unrepresentable input returns Conversion; allowed omissions go to losses")
    }
    fn decode_response(&self, body: &[u8], context: &DecodeContext) -> Result<Response, BridgeError> {
        todo!("decode success responses only; decode failures return BridgeError::Decode")
    }
    fn encode_response(
        &self, request: &Request, response: &Response, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("unrepresentable input returns Conversion; allowed omissions go to losses")
    }
    fn stream_decoder(&self, context: &DecodeContext) -> Box<dyn StreamDecoder> {
        Box::new(GatewayJsonDecoder { stopped: false })
    }
    fn stream_encoder(&self, request: &Request, options: &ConversionOptions) -> Box<dyn StreamEncoder> {
        Box::new(GatewayJsonEncoder { options: options.clone() })
    }
}
```
```rust
use relay_protocol::{ConversionError, ConversionErrorKind, DecodeError};

let invalid_request = BridgeError::Conversion(ConversionError {
    kind: ConversionErrorKind::Invalid,
    path: "/messages/0".into(), message: "content is required".into(),
});
let unrepresentable = BridgeError::Conversion(ConversionError {
    kind: ConversionErrorKind::Unrepresentable,
    path: "/content/0".into(), message: "no counterpart in the target format".into(),
});
let malformed_success = BridgeError::Decode(DecodeError {
    path: "/content".into(), message: "expected an array".into(),
});
```
```rust
use relay_bridge::{Frame, StreamDecoder};
use relay_protocol::{DecodeError, IncompleteStream, StreamEvent};

struct GatewayJsonDecoder { stopped: bool }

impl StreamDecoder for GatewayJsonDecoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        let event: StreamEvent = serde_json::from_str(&frame.data).map_err(|error| {
            BridgeError::Decode(DecodeError { path: "".into(), message: error.to_string() })
        })?;
        self.stopped |= matches!(&event, StreamEvent::Stop { .. });
        out.push(event);
        Ok(())
    }
    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(BridgeError::IncompleteStream(IncompleteStream {
                message: "input ended without stop".into(),
            }));
        }
        Ok(())
    }
}
```
```rust
use relay_bridge::{Frame, StreamEncoder};
use relay_protocol::{ConversionLoss, StreamEvent};

struct GatewayJsonEncoder { options: ConversionOptions }

impl StreamEncoder for GatewayJsonEncoder {
    fn push(
        &mut self, event: &StreamEvent, out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        todo!("push allowed omissions to losses as they occur; return BridgeError::Conversion when unrepresentable")
    }
    fn finish(
        &mut self, _out: &mut Vec<Frame>, _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        Ok(())
    }
}
```

# 3. Adapter (`relay-adapter`)

## Built-in Adapters

```rust
use std::sync::Arc;
use relay_adapter::{AnthropicAdapter, OpenAiAdapter, OpenAiCompatibleAdapter};
use relay_bridge::{ChatCompletionsBridge, LossScope, ResponsesBridge};

let anthropic = AnthropicAdapter::new("anthropic-key")
    .base_url("https://api.anthropic.com")
    .version("2026-10-21")
    .default_max_tokens(4096)
    .allow_loss([LossScope::Sampling, LossScope::DeveloperRole])
    .with_id_generator(|| "relay-response".into())
    .with_clock(|| 1235);

let responses = OpenAiAdapter::new("openai-key")
    .base_url("https://api.openai.com/v1")
    .organization("org-example")
    .project("proj-example")
    .store(false);
let chat = OpenAiCompatibleAdapter::new("https://api.openai.com/v1/chat/completions")
    .api_key("openai-key")
    .with_bridge(Arc::new(ChatCompletionsBridge::new()))?;

let ollama = OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions")
    .api_key("ollama-key")
    .models_url("https://ollama.com/v1/models")
    .without_tool_choice();
let local_responses = OpenAiCompatibleAdapter::new("http://localhost:11434/v1/responses")
    .with_bridge(Arc::new(ResponsesBridge::new()))?;
```

## Consumer HTTP transport

```rust
use relay_adapter::{Headers, Method, ProviderRequest};
use relay_protocol::RelayError;

#[derive(Debug)]
enum GatewayError {
    Relay(RelayError),
    Transport(Box<dyn std::error::Error + Send + Sync>),
    Cancelled,
}
impl From<RelayError> for GatewayError {
    fn from(error: RelayError) -> Self { Self::Relay(error) }
}
impl From<relay_bridge::BridgeError> for GatewayError {
    fn from(error: relay_bridge::BridgeError) -> Self { Self::Relay(error.into()) }
}
impl From<relay_protocol::ConfigurationError> for GatewayError {
    fn from(error: relay_protocol::ConfigurationError) -> Self { Self::Relay(error.into()) }
}
impl From<reqwest::Error> for GatewayError {
    fn from(error: reqwest::Error) -> Self { Self::Transport(Box::new(error)) }
}
```
```rust
async fn send(
    http: &reqwest::Client, request: ProviderRequest,
) -> Result<reqwest::Response, GatewayError> {
    let method = match request.method {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
    };
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in request.headers.0 {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| GatewayError::Transport(Box::new(error)))?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|error| GatewayError::Transport(Box::new(error)))?;
        headers.append(name, value);
    }
    Ok(http.request(method, request.url).headers(headers).body(request.body).send().await?)
}

fn headers_of(response: &reqwest::Response) -> Result<Headers, GatewayError> {
    let mut headers = Vec::new();
    for (name, value) in response.headers() {
        let value = value.to_str()
            .map_err(|error| GatewayError::Transport(Box::new(error)))?;
        headers.push((name.to_string(), value.to_owned()));
    }
    Ok(Headers(headers))
}
```

## Non-streaming request

```rust
use relay_adapter::{AdapterExt, AnthropicAdapter};
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, Encoded};

async fn complete(
    http: &reqwest::Client, client_body: &[u8],
) -> Result<Encoded, GatewayError> {
    let client = ChatCompletionsBridge::new();
    let adapter = AnthropicAdapter::new("sk-test");
    let input = client.decode_request(client_body)?;
    let (request, request_losses) = adapter.build_request(&input)?;
    for loss in request_losses { eprintln!("request omitted: {loss}"); }
    let response = send(http, request).await?;
    let status = response.status().as_u16();
    let headers = headers_of(&response)?;
    let body = response.bytes().await?;
    let output = adapter.parse_response(status, &headers, &body)?;
    let options = ConversionOptions::default();
    let encoded = client.encode_response(&input, &output, &options)?;
    Ok(encoded)
}
```

## Streaming A: Transcoder

```rust
use std::sync::Arc;
use relay_adapter::{AdapterExt, ChatCompletionsErrors, ErrorEncoder};

let mut input = client.decode_request(&client_body)?;
input.delivery = Some(relay_protocol::Delivery { stream: true, stream_usage: true });
let (request, request_losses) = adapter.build_request(&input)?;
for loss in request_losses { eprintln!("request omitted: {loss}"); }
let mut response = send(&http, request).await?;
let status = response.status().as_u16();
let headers = headers_of(&response)?;
let stream = match adapter.open_stream(status, &headers) {
    Ok(stream) => stream,
    Err(rejected) => {
        let body = response.bytes().await?;
        return Err(GatewayError::Relay(rejected.classify(&body)));
    }
};
let errors: Arc<dyn ErrorEncoder> = Arc::new(ChatCompletionsErrors::new());
let options = ConversionOptions::default();
let mut transcoder = stream.transcode(&client, &input, &options, errors);
```
```rust
use relay_adapter::Outcome;
use relay_bridge::Frame;

loop {
    if cancelled() { return Err(GatewayError::Cancelled); }
    let Some(bytes) = response.chunk().await? else { break };
    for frame in transcoder.push(&bytes) { emit(frame)?; }
    if transcoder.report().outcome.is_some() { break; }
}
if transcoder.report().outcome.is_none() {
    for frame in transcoder.finish() { emit(frame)?; }
}
let report = transcoder.report();
for loss in &report.losses { eprintln!("response omitted: {loss}"); }
match &report.outcome {
    Some(Outcome::Completed) => println!("completed"),
    Some(Outcome::Failed(error)) => eprintln!("ended after sending an error frame: {error}"),
    None => println!("still running or aborted"),
}
```

## Streaming B: AdapterStream directly

```rust
use relay_adapter::AdapterStream;
use relay_protocol::{RelayError, StreamEvent};

async fn collect_before_reply(
    mut response: reqwest::Response,
    mut stream: AdapterStream,
    cancelled: impl Fn() -> bool,
) -> Result<(Vec<StreamEvent>, Option<RelayError>), GatewayError> {
    let mut events = Vec::new();
    loop {
        if cancelled() { return Err(GatewayError::Cancelled); }
        let Some(bytes) = response.chunk().await? else { break };
        for item in stream.push(&bytes) {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return Ok((events, Some(error))),
            }
        }
    }
    for item in stream.finish() {
        match item {
            Ok(event) => events.push(event),
            Err(error) => return Ok((events, Some(error))),
        }
    }
    Ok((events, None))
}
```

## Error responses

```rust
use relay_adapter::{ChatCompletionsErrors, ErrorEncoder, MessagesErrors, ResponsesErrors, Stage};

let chat_errors = ChatCompletionsErrors::new();
let responses_errors = ResponsesErrors::new();
let messages_errors = MessagesErrors::new();
let http_error = messages_errors.encode_error(&error, Stage::Response);
let stream_error = responses_errors.encode_stream_error(&error);
```

```rust
use relay_adapter::{EncodedError, ErrorEncoder, Stage};
use relay_bridge::{Bridge, ConversionOptions};
use relay_protocol::RelayError;

fn ingress_error(client: &dyn Bridge, errors: &dyn ErrorEncoder, body: &[u8])
    -> Result<relay_protocol::Request, EncodedError>
{
    client.decode_request(body).map_err(|error| {
        errors.encode_error(&RelayError::from(error), Stage::Request)
    })
}

let output = adapter.parse_response(status, &headers, &body)
    .map_err(|error| errors.encode_error(&error, Stage::Response))?;
let encoded = client.encode_response(&input, &output, &ConversionOptions::default())
    .map_err(|error| errors.encode_error(&error.into(), Stage::Response))?;
```
```rust
use relay_adapter::ErrorEncoder;
use relay_protocol::RelayError;

fn frames_for_failure(
    errors: &dyn ErrorEncoder, error: &RelayError,
) -> Vec<relay_bridge::Frame> {
    errors.encode_stream_error(error)
}
```

## Model list

```rust
use relay_adapter::{Adapter, AdapterExt, ModelPage};

async fn models(
    http: &reqwest::Client, adapter: &dyn Adapter, cursor: Option<&str>,
) -> Result<ModelPage, GatewayError> {
    let request = adapter.list_models(cursor)?;
    let response = send(http, request).await?;
    let status = response.status().as_u16();
    let headers = headers_of(&response)?;
    let body = response.bytes().await?;
    Ok(adapter.parse_models(status, &headers, &body)?)
}

let page = models(&http, &adapter, None).await?;
for model_id in &page.model_ids { println!("{model_id}"); }
let next_cursor = page.next_cursor.as_deref();
```

## Implementing a custom Adapter

```rust
use std::sync::Arc;
use relay_adapter::{Adapter, Conversion, ModelDiscovery};
use relay_bridge::{ApiFormat, Bridge, ConversionOptions};
use relay_protocol::ConfigurationError;

struct GatewayAdapter {
    conversion: Conversion,
    token: String,
    discovery: Option<Box<dyn ModelDiscovery>>,
}
impl GatewayAdapter {
    fn new(
        bridge: Arc<dyn Bridge>, token: String,
        discovery: Option<Box<dyn ModelDiscovery>>,
    ) -> Result<Self, ConfigurationError> {
        let format = ApiFormat::new("gateway-json")?;
        let conversion = Conversion::new(bridge, &[format], ConversionOptions::default())?;
        Ok(Self { conversion, token, discovery })
    }
}
```
```rust
use relay_adapter::{FrameDecoder, Headers, Method, ProviderRequest};
use relay_protocol::ProviderError;
use serde_json::Value;

impl Adapter for GatewayAdapter {
    fn conversion(&self) -> &Conversion { &self.conversion }
    fn assemble_request(
        &self, model: &str, body: Value, stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        if self.token.is_empty() {
            return Err(ConfigurationError { message: "auth token is required".into() });
        }
        Ok(ProviderRequest {
            method: Method::Post, url: "https://provider.example/v1/generate".into(),
            headers: Headers(vec![
                ("authorization".into(), format!("Bearer {}", self.token)),
                ("content-type".into(), "application/json".into()),
                ("x-model".into(), model.into()),
                ("x-stream".into(), stream.to_string()),
            ]),
            body: serde_json::to_vec(&body)
                .map_err(|error| ConfigurationError { message: error.to_string() })?,
        })
    }
    fn check_response(&self, status: u16, headers: &Headers, body: &[u8])
        -> Result<(), ProviderError>
    {
        let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) || payload["type"] == "error" {
            return Err(gateway_failure(&payload, Some(status), headers));
        }
        Ok(())
    }
    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder> {
        Box::new(GatewayFrames { pending: Vec::new(), headers: headers.clone() })
    }
    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> { self.discovery.as_deref() }
}
```
```rust
use relay_protocol::{ProviderError, ProviderErrorCategory, RetryAfter};

fn gateway_failure(payload: &Value, status: Option<u16>, headers: &Headers) -> ProviderError {
    let code = payload["code"].as_str().map(str::to_owned);
    let category = match code.as_deref() {
        Some("busy") => ProviderErrorCategory::Overloaded,
        Some("rate_limit") => ProviderErrorCategory::RateLimited,
        Some("bad_token") => ProviderErrorCategory::Authentication,
        _ => ProviderErrorCategory::Unknown,
    };
    let retry_after = headers.get("retry-after").map(|value| {
        match value.parse::<u64>() {
            Ok(seconds) => RetryAfter::Seconds { seconds },
            Err(_) => RetryAfter::HttpDate { date: value.to_owned() },
        }
    });
    ProviderError {
        category,
        message: payload["message"].as_str().unwrap_or("provider request failed").into(),
        code, http_status: status,
        request_id: headers.get("request-id").map(str::to_owned), retry_after,
    }
}
```
```rust
use relay_adapter::{FrameDecoder, FrameItem};
use relay_bridge::Frame;
use relay_protocol::DecodeError;

struct GatewayFrames { pending: Vec<u8>, headers: Headers }

impl GatewayFrames {
    fn line(&self, bytes: &[u8]) -> Result<FrameItem, DecodeError> {
        let data = std::str::from_utf8(bytes).map_err(|error| DecodeError {
            path: "".into(), message: error.to_string(),
        })?;
        let payload: Value = serde_json::from_str(data).map_err(|error| DecodeError {
            path: "".into(), message: error.to_string(),
        })?;
        if payload["type"] == "error" {
            return Ok(FrameItem::Error(gateway_failure(&payload, None, &self.headers)));
        }
        Ok(FrameItem::Data(Frame { event: None, data: data.into() }))
    }
}
impl FrameDecoder for GatewayFrames {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            out.push(self.line(&line[..end])?);
        }
        Ok(())
    }
    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            out.push(self.line(&line)?);
        }
        Ok(())
    }
}
```
```rust
use relay_adapter::{ModelDiscovery, ModelPage};

struct GatewayDiscovery;
impl ModelDiscovery for GatewayDiscovery {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, ConfigurationError> {
        Ok(ProviderRequest {
            method: Method::Get, url: "https://provider.example/v1/models".into(),
            headers: Headers(cursor.into_iter().map(|value| {
                ("x-cursor".into(), value.to_owned())
            }).collect()),
            body: Vec::new(),
        })
    }
    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        let (model_ids, next_cursor): (Vec<String>, Option<String>) =
            serde_json::from_slice(body).map_err(|error| DecodeError {
                path: "".into(), message: error.to_string(),
            })?;
        Ok(ModelPage { model_ids, next_cursor })
    }
}
```
```rust
use relay_adapter::AdapterExt;

let bridge = Arc::new(GatewayJsonBridge { format: ApiFormat::new("gateway-json")? });
let adapter = GatewayAdapter::new(bridge, "test-token".into(), Some(Box::new(GatewayDiscovery)))?;
let (request, losses) = adapter.build_request(&input)?;
let response = send(&http, request).await?;
let status = response.status().as_u16();
let headers = headers_of(&response)?;
let body = response.bytes().await?;
let output = adapter.parse_response(status, &headers, &body)?;
```

## Injecting an ID generator and clock

```rust
let conversion = Conversion::new(bridge, &[ApiFormat::CHAT_COMPLETIONS], ConversionOptions::default())?
    .with_id_generator(|| "response-fallback".into())
    .with_clock(|| 1235);
let context = conversion.decode_context();
assert_eq!(context.response_id.as_deref(), Some("response-fallback"));
assert_eq!(context.observed_at, 1235);
```

# 4. Error outcomes

```json
[
  {"situation":"Encode Unknown request content without allowed loss","result":"Conversion(Unrepresentable)","client":"400 at request stage"},
  {"situation":"API format mismatch between bridge and adapter","result":"Configuration","client":"500"},
  {"situation":"Non-JSON error body with 503","result":"Provider(ServerError)","http_status":503,"client":"500"},
  {"situation":"Explicit overload error body with 200","result":"Provider(Overloaded)","http_status":200,"client":"Chat Completions/Responses 503; Messages 529"},
  {"situation":"Adapter wrongly treats non-success HTTP as success","result":"Provider(Unknown)","client":"502 at result stage; original status preserved"},
  {"situation":"Malformed success body with 200","result":"Decode","client":"502 at result stage"},
  {"situation":"Stream HTTP 429, classify on empty body","result":"Provider(RateLimited)","http_status":429,"client":"429 before response starts"},
  {"situation":"Provider error frame after normal data","result":"Provider(Overloaded) after earlier events","http_status":null,"client":"Format-specific error frame after earlier frames; no synthesized stop"},
  {"situation":"Event decode failure inside the same normal frame","result":"Decode after earlier events","client":"Format-specific error frame after earlier frames"},
  {"situation":"Framing failure after normal frames","result":"Decode after earlier events","client":"Format-specific error frame after earlier frames"},
  {"situation":"Normal EOF without stop","result":"IncompleteStream after buffered events","client":"Format-specific error frame after earlier frames; no synthesized completion"},
  {"situation":"Client encoder push fails after partial output","result":"Conversion","client":"Format-specific error frame after partial output"},
  {"situation":"Client encoder finish fails after tail output","result":"Conversion","client":"Format-specific error frame after tail output"},
  {"situation":"Normal stop and finish, with allowed omissions","result":"Outcome::Completed with loss list","client":"Normal frames and encoder tail output"},
  {"situation":"Transport drop or cancellation, dropped without finish","outcome":null,"client":"No automatic frames; consumer handles Transport/Cancelled"},
  {"situation":"Model discovery unsupported","result":"UnsupportedOperation(list_models)","client":"501"},
  {"situation":"Empty model page with next cursor","result":"ModelPage","client":"Normal list response"}
]
```
