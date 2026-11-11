# 1. Protocol (`relay-protocol`)

## Request 만들기

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
            text: "사진과 조회 결과를 설명해 줘".into(), cache_control: None,
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
            text: "서울".into(), cache_control: None,
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
        name: "lookup".into(), description: Some("키로 조회".into()),
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
          "text": "사진과 조회 결과를 설명해 줘"
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
              "text": "서울"
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
        "description": "키로 조회",
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
    "description": "결과",
    "schema": {
      "type": "object"
    },
    "strict": true
  }
}
```

## Response 읽기

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
      "text": "조회했어"
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
        "조회 필요"
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
                None => println!("추론 텍스트 없음"),
                Some("") => println!("빈 추론 텍스트 있음"),
                Some(text) => println!("{text}"),
            }
            println!("ID: {id}, 제공자 ID: {provider_id:?}, 요약: {summary:?}, 보존할 값: {opaque:?}");
        }
        AssistantContent::Unknown { value, .. } => println!("미해석 출력: {value}"),
    }
}
if let StopReason::Other(value) = &response.stop_reason {
    println!("알 수 없는 정상 종료 사유: {value}");
}
```

## StreamEvent 순서

```json
{"type": "start", "response_id": "resp_1", "provider_id": "resp_1", "created_at": {"observed_at": 1235, "provider_created_at": 1234}, "model": "target-model"}
{"type": "metadata", "scope": "response", "usage": {"input_tokens": 20, "output_tokens": 3}}
{"type": "item_start", "index": 0, "id": "reasoning_1", "provider_id": "reasoning_1", "item": {"type": "reasoning"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_text", "text": "확인 중"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_summary", "summary_index": 0, "text": "조회"}}
{"type": "delta", "index": 0, "delta": {"type": "reasoning_opaque", "value": "opaque-value"}}
{"type": "item_end", "index": 0}
{"type": "item_start", "index": 1, "id": "resp_1:1", "item": {"type": "text"}}
{"type": "delta", "index": 1, "delta": {"type": "text", "text": "답변"}}
{"type": "item_end", "index": 1}
{"type": "item_start", "index": 2, "id": "resp_1:2", "item": {"type": "tool_call"}}
{"type": "metadata", "scope": "item", "index": 2, "call_id": "call_1", "name": "lookup"}
{"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "{\"key\":"}}
{"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "1}"}}
{"type": "item_end", "index": 2}
{"type": "stop", "stop_reason": "tool_use", "usage": {"input_tokens": 20, "output_tokens": 9}}
```

## RelayError 분기

```rust
use relay_protocol::{ProviderErrorCategory, RelayError};

fn describe(error: &RelayError) -> String {
    match error {
        RelayError::Provider(error) => {
            let category = match error.category {
                ProviderErrorCategory::Authentication => "인증",
                ProviderErrorCategory::PermissionDenied => "권한",
                ProviderErrorCategory::InsufficientQuota => "할당량",
                ProviderErrorCategory::RateLimited => "속도 제한",
                ProviderErrorCategory::Overloaded => "과부하",
                ProviderErrorCategory::InvalidRequest => "요청",
                ProviderErrorCategory::UnsupportedFeature => "미지원 기능",
                ProviderErrorCategory::ModelNotFound => "모델 없음",
                ProviderErrorCategory::ContextLengthExceeded => "입력 길이",
                ProviderErrorCategory::ServerError => "서버",
                ProviderErrorCategory::Unknown => "미분류",
            };
            format!("{category}: {}, 대기 정보: {:?}", error.message, error.retry_after)
        }
        RelayError::Conversion(error) => format!("변환 {}: {}", error.path, error),
        RelayError::Decode(error) => format!("해석 {}: {}", error.path, error),
        RelayError::IncompleteStream(error) => format!("미완료: {error}"),
        RelayError::Configuration(error) => format!("설정: {error}"),
        RelayError::UnsupportedOperation(error) => format!("미지원: {error}"),
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
println!("모델: {}, 전달: {:?}", input.model, input.delivery);

let options = ConversionOptions::default();
let context = DecodeContext { response_id: Some("fallback-id".into()), observed_at: 1235 };
let output = client.decode_response(&provider_body, &context)?;
let encoded = client.encode_response(&input, &output, &options)?;
let client_json = encoded.body;
let response_losses = encoded.losses;

let legacy_server = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens);
```

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

## ConversionOptions와 손실 보고

```rust
use relay_bridge::{ConversionOptions, LossScope};
use relay_protocol::ConversionLoss;

let options = ConversionOptions {
    allow_loss: [LossScope::UnknownContent].into(),
};
let encoded = client.encode_response(&input, &output, &options)?;
for ConversionLoss { path, message } in encoded.losses {
    eprintln!("생략: {path}: {message}");
}
let client_json = encoded.body;
```

## Custom Bridge 구현

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
        todo!("요청 문법·표현 불가 오류는 BridgeError::Conversion")
    }
    fn encode_request(
        &self, request: &Request, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("표현 불가는 Conversion, 허용한 생략은 losses")
    }
    fn decode_response(&self, body: &[u8], context: &DecodeContext) -> Result<Response, BridgeError> {
        todo!("성공 응답만 해석하고 해석 실패는 BridgeError::Decode")
    }
    fn encode_response(
        &self, request: &Request, response: &Response, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("표현 불가는 Conversion, 허용한 생략은 losses")
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
    path: "/messages/0".into(), message: "content가 필요함".into(),
});
let unrepresentable = BridgeError::Conversion(ConversionError {
    kind: ConversionErrorKind::Unrepresentable,
    path: "/content/0".into(), message: "대상 형식에 대응 항목이 없음".into(),
});
let malformed_success = BridgeError::Decode(DecodeError {
    path: "/content".into(), message: "배열이 필요함".into(),
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
                message: "stop 없이 입력이 끝남".into(),
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
        todo!("허용한 생략은 발생 즉시 losses에 추가하고, 표현 불가 시 BridgeError::Conversion")
    }
    fn finish(
        &mut self, _out: &mut Vec<Frame>, _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        Ok(())
    }
}
```

# 3. Adapter (`relay-adapter`)

## 소비자 HTTP 전송

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

## 일반 요청

```rust
use relay_adapter::{AdapterExt, AnthropicAdapter};
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, Encoded};

async fn complete(
    http: &reqwest::Client, client_body: &[u8],
) -> Result<Encoded, GatewayError> {
    let client = ChatCompletionsBridge::new();
    let adapter = AnthropicAdapter::new("sk-test")?;
    let input = client.decode_request(client_body)?;
    let (request, request_losses) = adapter.build_request(&input)?;
    for loss in request_losses { eprintln!("요청 생략: {loss}"); }
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

## 스트리밍 A: Transcoder

```rust
use std::sync::Arc;
use relay_adapter::{AdapterExt, ChatCompletionsErrors, ErrorEncoder};

let mut input = client.decode_request(&client_body)?;
input.delivery = Some(relay_protocol::Delivery { stream: true, stream_usage: true });
let (request, request_losses) = adapter.build_request(&input)?;
for loss in request_losses { eprintln!("요청 생략: {loss}"); }
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
for loss in &report.losses { eprintln!("응답 생략: {loss}"); }
match &report.outcome {
    Some(Outcome::Completed) => println!("정상 완료"),
    Some(Outcome::Failed(error)) => eprintln!("오류 프레임 전송 후 종료: {error}"),
    None => println!("아직 실행 중이거나 중단됨"),
}
```

## 스트리밍 B: AdapterStream 직접

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

## 오류 응답

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

## Model 목록

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

## Custom Adapter 구현

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
            return Err(ConfigurationError { message: "인증 토큰이 필요함".into() });
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
        message: payload["message"].as_str().unwrap_or("제공자 요청 실패").into(),
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

## ID 생성기와 시계 주입

```rust
let conversion = Conversion::new(bridge, &[ApiFormat::CHAT_COMPLETIONS], ConversionOptions::default())?
    .with_id_generator(|| "response-fallback".into())
    .with_clock(|| 1235);
let context = conversion.decode_context();
assert_eq!(context.response_id.as_deref(), Some("response-fallback"));
assert_eq!(context.observed_at, 1235);
```

# 4. 오류 결과

```json
[
  {"상황":"요청의 Unknown 콘텐츠를 손실 허용 없이 인코딩","결과":"Conversion(Unrepresentable)","클라이언트":"요청 단계 400"},
  {"상황":"bridge와 어댑터의 API 형식 불일치","결과":"Configuration","클라이언트":"소비자 설정 오류 정책"},
  {"상황":"503의 비JSON 오류 본문, 테스트 어댑터가 busy로 분류","결과":"Provider(Overloaded)","http_status":503,"클라이언트":"결과 단계 502"},
  {"상황":"200의 제공자 오류 본문","결과":"Provider(Overloaded)","http_status":200,"클라이언트":"결과 단계 502"},
  {"상황":"비성공 HTTP를 어댑터가 잘못 성공 처리","결과":"Provider(Unknown)","클라이언트":"결과 단계 502; 원래 상태 보존"},
  {"상황":"200의 잘못된 성공 본문","결과":"Decode","클라이언트":"결과 단계 502"},
  {"상황":"스트림 HTTP 429, 빈 본문을 classify","결과":"Provider(Overloaded) 또는 Provider(Unknown)","http_status":429,"클라이언트":"응답 시작 전 결과 단계 502"},
  {"상황":"정상 데이터 뒤 제공자 오류 프레임","결과":"이전 이벤트 뒤 Provider(Overloaded)","http_status":null,"클라이언트":"이전 프레임 뒤 relay-error; 합성 stop 없음"},
  {"상황":"같은 정상 프레임 안에서 이벤트 해석 실패","결과":"이전 이벤트 뒤 Decode","클라이언트":"이전 프레임 뒤 relay-error"},
  {"상황":"정상 프레임 뒤 framing 실패","결과":"이전 이벤트 뒤 Decode","클라이언트":"이전 프레임 뒤 relay-error"},
  {"상황":"정상 EOF인데 stop 없음","결과":"버퍼 이벤트 뒤 IncompleteStream","클라이언트":"이전 프레임 뒤 relay-error; 합성 완료 없음"},
  {"상황":"클라이언트 인코더 push가 일부 출력 후 실패","결과":"Conversion","클라이언트":"부분 출력 뒤 relay-error"},
  {"상황":"클라이언트 인코더 finish가 꼬리 출력 후 실패","결과":"Conversion","클라이언트":"꼬리 출력 뒤 relay-error"},
  {"상황":"정상 stop·finish, 허용한 생략 있음","결과":"Outcome::Completed와 손실 목록","클라이언트":"정상 프레임과 인코더 꼬리 출력"},
  {"상황":"전송 단절·취소, finish 없이 drop","outcome":null,"클라이언트":"자동 프레임 없음; Transport/Cancelled는 소비자 처리"},
  {"상황":"모델 탐색 미지원","결과":"UnsupportedOperation(list_models)","클라이언트":"소비자 미지원 정책"},
  {"상황":"빈 모델 페이지와 다음 커서","결과":"ModelPage","클라이언트":"정상 목록 응답"}
]
```
