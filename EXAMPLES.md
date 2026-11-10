# 0. 머리말

세 crate의 공개 API를 사용하는 예제다. 이 파일은 컴파일하거나 실행하지 않는다. 공개 API를 바꾸면 같은 커밋에서 예제도 갱신한다.

`ChatCompletionsBridge`, `MessagesBridge`, `ResponsesBridge`는 M3 예정 이름이다. `OpenAiAdapter`, `AnthropicAdapter`, `OpenAiCompatibleAdapter`, `ChatCompletionsErrors`는 M4 예정 이름이다. 아직 구현이나 export가 없으며, 아래의 간단한 생성자 모양도 가정이다. 그 외 Relay 타입과 메서드는 현재 공개 API를 따른다.

코드 블록은 독립 실행 파일이 아니라 연결해서 읽는 조각이다. 앞에서 만든 `request`, `input`, `output` 등을 재사용한다. 커스텀 구현의 `todo!`는 구현할 wire 변환 자리다. HTTP 클라이언트와 클라이언트 응답 전송은 소비자가 맡는다.

# 1. Protocol (`relay-protocol`)

## Request 만들기

텍스트 요청에 생성 옵션과 추론 설정을 붙인다.

```rust
use relay_protocol::{
    Message, Parameters, Reasoning, ReasoningEffort, ReasoningSummary,
    Request, ResponseFormat, TextPart, ThinkingMode, UserContent, Version,
};

let mut request = Request {
    version: Version::V1,
    model: "target-model".into(),
    messages: vec![Message::User {
        content: vec![UserContent::Text(TextPart {
            text: "사진과 조회 결과를 설명해 줘".into(), cache_control: None,
        })],
    }],
    tools: None,
    parameters: Some(Parameters {
        max_output_tokens: Some(4096), temperature: Some(0.7),
        top_p: Some(0.9), top_k: None, stop: vec![],
    }),
    response_format: Some(ResponseFormat::JsonObject),
    reasoning: Some(Reasoning {
        effort: Some(ReasoningEffort("high".into())), budget_tokens: Some(1024),
        summary: Some(ReasoningSummary("auto".into())),
        thinking: Some(ThinkingMode::Adaptive),
    }),
};
```

이미지와 도구 호출 이력을 추가하고, 도구 결과는 사용자 콘텐츠로 넣는다.

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
        id: "call_1".into(), name: "lookup".into(),
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

도구 정의와 선택 정책은 `ToolConfig` 안에 함께 둔다.

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

위 요청의 JSON은 다음과 같고, 필드 이름은 `tool_choice`가 아니라 `tools.choice`다.

```json
{
  "version": "1",
  "model": "target-model",
  "messages": [
    {"role": "user", "content": [
      {"type": "text", "text": "사진과 조회 결과를 설명해 줘"},
      {"type": "image", "source": {"type": "url", "url": "https://example.com/photo.png"}, "detail": "high"}
    ]},
    {"role": "assistant", "content": [
      {"type": "tool_call", "id": "call_1", "name": "lookup", "arguments": "{ \"key\": \"photo\" }"}
    ]},
    {"role": "user", "content": [
      {"type": "tool_result", "tool_call_id": "call_1", "content": [
        {"type": "text", "text": "서울"}
      ], "is_error": false}
    ]}
  ],
  "tools": {
    "definitions": [{
      "type": "function", "name": "lookup", "description": "키로 조회",
      "parameters": {
        "type": "object", "properties": {"key": {"type": "string"}},
        "required": ["key"], "additionalProperties": false
      },
      "strict": true
    }],
    "choice": "auto", "parallel": false
  },
  "parameters": {"max_output_tokens": 4096, "temperature": 0.7, "top_p": 0.9},
  "response_format": "json_object",
  "reasoning": {"effort": "high", "budget_tokens": 1024, "summary": "auto", "thinking": "adaptive"}
}
```

선택 정책과 응답 형식은 단순 값일 때 문자열이고, 이름이나 스키마가 필요할 때만 객체다.

```json
{
  "tools": {"definitions": [], "choice": {"type": "function", "name": "lookup"}},
  "response_format": {
    "type": "json_schema", "name": "result", "description": "결과",
    "schema": {"type": "object"}, "strict": true
  }
}
```

`ToolChoice::{Auto, None, Required}`는 `"auto"`, `"none"`, `"required"`다. `ResponseFormat::{Text, JsonObject}`는 `"text"`, `"json_object"`다. `ReasoningEffort`, `ReasoningSummary`, `ImageDetail`은 문자열을 보존하며 대상 모델의 지원 여부를 보장하지 않는다. JSON 역직렬화는 빈 문자열과 0인 캐시 TTL을 거부한다. 구조체 리터럴 자체는 검증하지 않는다. `CacheControl { ttl_seconds: None }`은 기본 TTL의 캐시 경계이며 `{}`로 직렬화된다.

## Response 읽기

응답은 도구 인자 원문과 해석할 수 없는 출력까지 보존한다.

```json
{
  "version": "1", "id": "resp_1", "model": "target-model",
  "content": [
    {"type": "text", "text": "조회했어"},
    {"type": "tool_call", "id": "call_2", "name": "lookup", "arguments": "{ \"key\":1, \"key\": 2 }"},
    {"type": "reasoning", "summary": ["조회 필요"], "opaque": "opaque-value"},
    {"type": "reasoning", "text": ""},
    {"type": "unknown", "value": {"type": "future_output", "payload": 7}}
  ],
  "stop_reason": "pause_turn",
  "usage": {
    "input_tokens": 900, "output_tokens": 200,
    "cache_read_tokens": 500, "cache_write_tokens": 100, "reasoning_tokens": 120
  }
}
```

텍스트 유무와 빈 텍스트를 구분하고, `opaque`는 다음 요청에서 그대로 돌려준다.

```rust
use relay_protocol::{AssistantContent, Response, StopReason};

let response: Response = serde_json::from_slice(&response_bytes)?;
for part in &response.content {
    match part {
        AssistantContent::Text(part) => println!("{}", part.text),
        AssistantContent::ToolCall { id, name, arguments, .. } => {
            println!("{id}: {name}({arguments})");
            // 실행 시 JSON을 해석해도, 재전송할 원문은 덮어쓰지 않는다.
        }
        AssistantContent::Reasoning { text, summary, opaque } => {
            match text.as_deref() {
                None => println!("추론 텍스트 없음"),
                Some("") => println!("빈 추론 텍스트 있음"),
                Some(text) => println!("{text}"),
            }
            println!("요약: {summary:?}, 보존할 값: {opaque:?}");
        }
        AssistantContent::Unknown { value } => println!("미해석 출력: {value}"),
    }
}
if let StopReason::Other(value) = &response.stop_reason {
    println!("알 수 없는 정상 종료 사유: {value}");
}
```

`Unknown`은 이미 생성된 출력을 잃지 않기 위한 보관 값이다. 요청으로 다시 인코딩할 중립 의미는 없다. bridge는 표현할 수 없는 콘텐츠를 거부하며, 소비자가 허용한 손실만 보고 후 생략한다.

`Usage.input_tokens`에는 캐시 읽기와 쓰기가 포함된다. 위의 500과 100을 900에 더하지 않는다. `reasoning_tokens`도 `output_tokens`의 부분집합이다. 별도 집계가 없으면 각 선택 필드는 `None`이며, 0이라는 뜻이 아니다. `StopReason::ContextWindowExceeded`도 정상 종료 사유이며 오류가 아니다.

## StreamEvent 순서

각 항목은 시작 뒤에 delta가 오고, 정상 종료 때만 항목 끝과 stop이 온다.

```json
{"type":"response_start","id":"resp_1","model":"target-model"}
{"type":"item_start","index":0,"item":{"type":"reasoning"}}
{"type":"delta","index":0,"delta":{"type":"reasoning_text","text":"확인 중"}}
{"type":"delta","index":0,"delta":{"type":"reasoning_summary","summary_index":0,"text":"조회"}}
{"type":"delta","index":0,"delta":{"type":"reasoning_opaque","value":"opaque-value"}}
{"type":"item_end","index":0}
{"type":"item_start","index":1,"item":{"type":"text"}}
{"type":"delta","index":1,"delta":{"type":"text","text":"답변"}}
{"type":"item_end","index":1}
{"type":"item_start","index":2,"item":{"type":"tool_call","id":"call_1","name":"lookup"}}
{"type":"delta","index":2,"delta":{"type":"tool_call_arguments","text":"{\"key\":"}}
{"type":"delta","index":2,"delta":{"type":"tool_call_arguments","text":"1}"}}
{"type":"item_end","index":2}
{"type":"stop","stop_reason":"tool_use","usage":{"input_tokens":20,"output_tokens":9}}
```

이는 이벤트 JSON 줄 목록이지 SSE 바이트가 아니다. index는 응답 안의 항목 순서로 bridge가 정한다. `reasoning_opaque.value`는 조각이 아니라 항목의 완전한 값이다. 실패는 `StreamEvent`가 아니며 오류를 `Stop`으로 바꾸거나 실패 뒤에 `ItemEnd`를 합성하지 않는다.

## RelayError 분기

소비자는 원인별로 분기하되 재시도 여부와 지연 시간은 직접 결정한다.

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

`ProviderError`는 `code`, `http_status`, `request_id`, `retry_after`를 별도로 보존한다. `RetryAfter::Seconds { seconds }`와 `RetryAfter::HttpDate { date }`를 해석하고 재시도 예산·중복 실행 위험·이미 전송한 출력을 판단하는 일은 소비자 몫이다.

# 2. Bridge (`relay-bridge`)

## Gateway ingress

클라이언트 요청을 `Input`으로 읽고, 어댑터에서 받은 `Output`을 클라이언트 형식으로 쓴다.

```rust
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, Input};

let client = ChatCompletionsBridge::new(); // M3 예정
let input: Input = client.decode_request(&client_body)?;
println!("모델: {}, 스트림: {}", input.request.model, input.stream);

// output은 어댑터의 parse_response 결과다.
let encoded = client.encode_response(&output, &ConversionOptions::default())?;
let client_json = encoded.body;
let response_losses = encoded.losses;
```

`Input::new(request)`는 native 필드 없이 만들며 `stream`의 기본값은 `false`다. 스트리밍은 `input.stream = true`로 선택한다. 성공 응답만 bridge에 전달하며, 제공자 오류 판별은 어댑터가 먼저 한다.

## Native fields

같은 API 형식에만 native 필드를 돌려주며, 다른 형식에는 전달하지 않는다.

```rust
use relay_bridge::{ApiFormat, Input, NativeFields};
use serde_json::json;

let native = NativeFields::from_iter([("seed".into(), json!(42))]);
let input = Input::with_native(
    ApiFormat::CHAT_COMPLETIONS, request, false, native,
);
let same_format = input.native_for(&ApiFormat::CHAT_COMPLETIONS);
let other_format = input.native_for(&ApiFormat::MESSAGES);
println!("같은 형식: {same_format:?}, 다른 형식: {other_format:?}");
```

`same_format`은 `Some(&NativeFields)`, `other_format`은 `None`이다. 제공자가 아니라 **API 형식**이 기준이다. `Output::with_native`와 `Output::native_for`도 같다. 생성 필드와 native 키가 충돌하면 bridge는 `ConversionErrorKind::Invalid`를 반환하며 덮어쓰지 않는다. native 필드에 넣을 수 있는 항목은 각 bridge가 정한다.

## ConversionOptions와 손실 보고

기본은 엄격 변환이며, 필요한 손실 범위만 명시적으로 허용한다.

```rust
use relay_bridge::{ConversionOptions, LossScope};
use relay_protocol::ConversionLoss;

let options = ConversionOptions {
    allow_loss: [LossScope::UnknownContent].into(),
};
let encoded = client.encode_response(&output, &options)?;
for ConversionLoss { path, message } in encoded.losses {
    eprintln!("생략: {path}: {message}");
}
let client_json = encoded.body;
```

`LossScope`에는 `CacheControl`, `ImageDetail`, `Reasoning`, `UnknownContent`가 있다. `Reasoning`은 opaque 값을 포함한 콘텐츠 항목에 관한 것이며 요청의 추론 설정을 뜻하지 않는다. 허용은 생략 의무가 아니다. 실제로 생략했을 때만 `ConversionLoss`가 나온다. 허용하지 않은 생략은 `BridgeError::Conversion`이다. 스트림의 손실 목록은 `StreamEncoder::finish`가 반환한다.

## Custom Bridge 구현

커스텀 wire 변환의 골격은 요청 변환 실패와 성공 응답 해석 실패를 구분한다.

```rust
use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, Encoded,
    Input, Output, StreamDecoder, StreamEncoder,
};

struct GatewayJsonBridge { format: ApiFormat }

impl Bridge for GatewayJsonBridge {
    fn format(&self) -> &ApiFormat { &self.format }

    fn decode_request(&self, body: &[u8]) -> Result<Input, BridgeError> {
        todo!("요청 문법·표현 불가 오류는 BridgeError::Conversion")
    }
    fn encode_request(
        &self, input: &Input, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("native 충돌·표현 불가는 Conversion, 허용한 생략은 losses")
    }
    fn decode_response(&self, body: &[u8]) -> Result<Output, BridgeError> {
        todo!("성공 응답만 해석하고 해석 실패는 BridgeError::Decode")
    }
    fn encode_response(
        &self, output: &Output, options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        todo!("표현 불가는 Conversion, 허용한 생략은 losses")
    }
    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(GatewayJsonDecoder { stopped: false })
    }
    fn stream_encoder(&self, options: &ConversionOptions) -> Box<dyn StreamEncoder> {
        Box::new(GatewayJsonEncoder { options: options.clone(), losses: Vec::new() })
    }
}
```

요청 오류는 `Invalid`, 유효하지만 표현할 수 없는 값은 `Unrepresentable`, 아직 구현하지 않은 기능은 `Unsupported`로 구분한다.

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

예제의 커스텀 wire는 정상 프레임 하나에 `StreamEvent` JSON 하나를 담으며, 정상 EOF에 stop이 없으면 미완료다.

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

스트림 인코더는 오류 전에 만든 프레임을 유지하고, 정상 finish에서 생략 목록을 반환한다.

```rust
use relay_bridge::{Frame, StreamEncoder};
use relay_protocol::{ConversionLoss, StreamEvent};

struct GatewayJsonEncoder { options: ConversionOptions, losses: Vec<ConversionLoss> }

impl StreamEncoder for GatewayJsonEncoder {
    fn push(&mut self, event: &StreamEvent, out: &mut Vec<Frame>) -> Result<(), BridgeError> {
        todo!("허용한 손실만 누적하고, 표현 불가 시 BridgeError::Conversion")
    }
    fn finish(&mut self, out: &mut Vec<Frame>) -> Result<Vec<ConversionLoss>, BridgeError> {
        // 최종 변환이 실패해도 이미 out에 넣은 프레임을 지우지 않는다.
        Ok(std::mem::take(&mut self.losses))
    }
}
```

이는 wire 변환 골격이며 전체 이벤트 순서 검증은 생략했다. bridge는 HTTP 오류나 오류 프레임을 감지·분류하지 않는다. `BridgeError`에는 `Provider` 변형이 없다. 어댑터의 `FrameDecoder`가 오류 프레임을 제거한 뒤 정상 `Frame`만 `StreamDecoder`로 보낸다.

# 3. Adapter (`relay-adapter`)

## 소비자 HTTP 전송

HTTP 실패와 취소는 Relay 오류 밖에 두고, 아래 공통 헬퍼를 뒤의 요청 예제에서 재사용한다.

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

전송 헬퍼는 오류 상태도 본문을 읽을 수 있게 남겨 두고, 중복 헤더를 보존한다.

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

`error_for_status`는 호출하지 않는다. 헤더 이름과 순서는 reqwest가 정규화할 수 있으며, `Headers` 자체는 순서와 중복을 보존한다. 문자열로 읽을 수 없는 헤더는 이 소비자 헬퍼가 전송 오류로 처리한다. 타임아웃·리다이렉트·연결 풀·취소 대기는 소비자의 HTTP 설정이다.

## 일반 요청

`stream: false`인 Chat Completions 입력을 Anthropic 요청으로 바꾸고, 응답을 원래 형식으로 돌린다.

```rust
use relay_adapter::{AdapterExt, AnthropicAdapter};
use relay_bridge::{Bridge, ChatCompletionsBridge, ConversionOptions, Encoded};

async fn complete(
    http: &reqwest::Client, client_body: &[u8],
) -> Result<Encoded, GatewayError> {
    let client = ChatCompletionsBridge::new();
    let adapter = AnthropicAdapter::new("sk-test")?; // M4 예정
    let input = client.decode_request(client_body)?;
    let (request, request_losses) = adapter.build_request(&input)?;
    for loss in request_losses { eprintln!("요청 생략: {loss}"); }
    let response = send(http, request).await?;
    let status = response.status().as_u16();
    let headers = headers_of(&response)?;
    let body = response.bytes().await?;
    let output = adapter.parse_response(status, &headers, &body)?;
    let encoded = client.encode_response(&output, &ConversionOptions::default())?;
    Ok(encoded)
}
```

`Encoded.body`는 JSON 값이다. 소비자가 직렬화하고 클라이언트에 전송하며 `Encoded.losses`도 기록한다. `AdapterExt`는 `Adapter`의 blanket 구현이므로 `Box<dyn Adapter>`에서도 같은 흐름을 쓴다. API 형식 호환성은 어댑터 생성 시 `Conversion::new`에서 확인하며 protocol에는 형식을 넣지 않는다.

## 스트리밍 A: Transcoder

비성공 HTTP 응답은 본문을 읽고 `classify`하며, 성공 응답만 transcoder로 연다.

```rust
use std::sync::Arc;
use relay_adapter::{AdapterExt, ChatCompletionsErrors, ErrorEncoder};

let mut input = client.decode_request(&client_body)?;
input.stream = true;
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
let errors: Arc<dyn ErrorEncoder> = Arc::new(ChatCompletionsErrors::new()); // M4 예정
let mut transcoder = stream.transcode(&client, &ConversionOptions::default(), errors);
```

`open_stream`은 상태 코드로만 스트림을 열며 `check_response`를 호출하지 않는다. 따라서 2xx 안의 제공자 오류도 `FrameDecoder`가 오류 payload나 event 이름으로 판별해야 한다. 거절 본문을 읽는 동안 전송이 실패하면 분류할 본문이 없으므로 소비자의 전송 오류로 남긴다.

정상 EOF에서만 `finish`하고, 전송 단절이나 취소에서는 finish 없이 버린다.

```rust
use relay_adapter::Outcome;
use relay_bridge::Frame;

// emit은 소비자가 제공하는 FnMut(Frame) -> Result<(), GatewayError>다.
// cancelled는 소비자가 제공하는 Fn() -> bool이며, 실제 취소 대기도 소비자가 맡는다.
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

`Frame`의 SSE 등 실제 바이트 직렬화는 `emit` 쪽에서 한다. 오류를 만나면 앞선 프레임 뒤에 `encode_stream_error`가 만든 프레임 묶음을 한 번 붙이고 종료한다. 이후 `push`와 `finish`는 빈 결과다. `report.losses`는 성공한 인코더 finish의 보고다. 실패·중단 시 부분 손실의 완전한 감사 목록으로 볼 수 없다. 전송 실패로 `?`가 반환되면 `outcome`은 소비자 전송 오류로 갱신되지 않는다.

## 스트리밍 B: AdapterStream 직접

클라이언트에 보내기 전 재시도·fallback을 판단하려면 이벤트를 먼저 보관한다.

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

`open_stream`과 거절 본문 분류는 A와 같다. 각 `push`/`finish` 결과는 정상 이벤트들 뒤에 최대 한 개의 마지막 오류를 낸다. 스트림 전체에서도 오류는 한 번뿐이다. 위 함수는 설명을 위해 전체 응답을 보관한다. 실제 게이트웨이는 메모리 상한과 클라이언트 전송 시작점을 정한다. 오류 이전 이벤트를 버리고 재시도할지는 소비자가 결정한다. 전송 오류나 취소는 `GatewayError`로 반환되며 `stream.finish()`를 호출하지 않는다.

## 오류 응답

응답 시작 전에는 실패가 요청 처리 중인지 결과 처리 중인지 구분해서 인코딩한다.

```rust
use relay_adapter::{EncodedError, ErrorEncoder, Stage};
use relay_bridge::{Bridge, ConversionOptions};
use relay_protocol::RelayError;

fn ingress_error(client: &dyn Bridge, errors: &dyn ErrorEncoder, body: &[u8])
    -> Result<relay_bridge::Input, EncodedError>
{
    client.decode_request(body).map_err(|error| {
        errors.encode_error(&RelayError::from(error), Stage::Request)
    })
}

let output = adapter.parse_response(status, &headers, &body)
    .map_err(|error| errors.encode_error(&error, Stage::Response))?;
let encoded = client.encode_response(&output, &ConversionOptions::default())
    .map_err(|error| errors.encode_error(&error.into(), Stage::Response))?;
```

`build_request`의 요청 변환 실패는 `Stage::Request`로 전달한다. `Stage`는 네트워크 단계 enum이 아니라 오류를 클라이언트 형식으로 매핑하는 문맥이다. HTTP 응답 전송이 시작된 뒤에는 상태 코드를 다시 쓰지 않는다.

이미 스트림을 시작했다면 별도의 오류 프레임만 보낸다.

```rust
use relay_adapter::ErrorEncoder;
use relay_protocol::RelayError;

fn frames_for_failure(
    errors: &dyn ErrorEncoder, error: &RelayError,
) -> Vec<relay_bridge::Frame> {
    errors.encode_stream_error(error)
}
```

`Transcoder`를 쓰면 위 호출을 직접 반복하지 않는다. 이미 어댑터가 분류한 `RelayError`를 인코더가 wire로 바꾸는 것이며, 인코더가 제공자 오류를 다시 판별하지 않는다.

## Model 목록

모델 탐색을 지원하지 않는 경우와 정상적인 빈 페이지를 구분한다.

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

`model_discovery()`가 `None`이면 두 메서드 모두 `RelayError::UnsupportedOperation`이며 `operation`은 `"list_models"`다. 지원한다면 빈 `model_ids`도 성공이며 `next_cursor`가 있을 수 있다. `parse_models`는 성공 본문 해석 전에 제공자 오류를 분류한다. 가격·컨텍스트 크기·기능 정보는 반환하지 않는다.

## Custom Adapter 구현

아래 소비자 정의 어댑터는 `gateway-json` bridge와 인증·엔드포인트를 조합한다.

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

커스텀 wire는 JSON 요청과 NDJSON 스트림을 쓰며, 모델과 스트림 선택을 헤더로 받는다고 정의한다.

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

오류 payload는 `type`, `code`, `message`를 쓰며, HTTP 본문과 스트림에서 같은 분류 함수를 사용한다.

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

`Content-Type`을 검사하지 않아 `text/plain`의 JSON 오류도 분류한다. 비성공 상태의 비JSON 본문은 `Provider(Unknown)`으로 남고, 성공한 비JSON 본문은 bridge가 `Decode`로 처리한다. `unwrap_or`는 의도적인 미분류 경로이며 JSON 해석 실패를 성공 데이터로 바꾸는 것이 아니다. `Retry-After`의 날짜 문자열은 시계를 읽거나 정규화하지 않고 보존한다.

프레임 디코더는 정상 JSON 줄만 bridge에 넘기고, 오류에는 응답 헤더의 요청 ID를 붙인다.

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

이 커스텀 NDJSON 형식은 빈 줄 없이 줄마다 JSON 하나를 보낸다. 앞선 `out` 항목은 뒤의 잘못된 줄 때문에 지우지 않는다. mid-stream 오류의 `http_status`는 `None`이며 스트림을 연 HTTP 200을 복사하지 않는다. SSE를 쓰는 제공자는 event 이름과 payload 양쪽의 오류 신호를 해당 어댑터에서 분류해야 한다.

선택적 모델 탐색은 별도 구현을 주입하며, wire 응답을 ID와 커서로만 변환한다.

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

이 소비자 정의 목록 API는 `[model_ids, next_cursor]`를 반환하고, 커서는 `x-cursor`로 받는다고 정의했다. 실제 인증이 필요한 목록 API라면 `GatewayDiscovery`가 자기 인증 설정도 소유해야 한다.

커스텀 구현도 별도 소비자 메서드 없이 `AdapterExt`를 통해 사용한다.

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

bridge 골격의 `todo!`를 채운 뒤에 사용하는 흐름이다. `Conversion::new`의 지원 형식 배열은 허용 목록이며, 일치하지 않으면 양쪽 형식 이름을 포함한 `ConfigurationError`가 나온다.

# 4. 오류 결과 표

아래 Relay 결과는 `crates/relay-adapter/tests/adapter.rs`의 동작을 따른다. 마지막 열은 그 테스트의 `TestErrorEncoder` 정책이다. 응답 시작 전에는 `Stage::Request`에 400, `Stage::Response`에 502와 오류 문자열 본문을 낸다. 스트림에서는 `event: relay-error` 프레임 하나를 낸다. 내장 인코더의 확정된 상태 코드 정책을 뜻하지 않는다.

| 상황 | RelayError 결과 | 클라이언트 인코더 출력 |
| --- | --- | --- |
| 요청의 Unknown 콘텐츠를 손실 허용 없이 인코딩 | `Conversion(Unrepresentable)` | 요청 단계 400 |
| bridge와 어댑터의 API 형식 불일치 | `Configuration` | 응답 전이라면 요청 단계 400 또는 소비자 설정 오류 정책 |
| 503의 비JSON 오류 본문, 테스트 어댑터가 busy로 분류 | `Provider(Overloaded)`, 상태 503·요청 ID 보존 | 결과 단계 502 |
| 200의 제공자 오류 본문, `text/plain`이어도 동일 | `Provider(Overloaded)`, 상태 200·요청 ID 보존 | 결과 단계 502 |
| 비성공 HTTP를 어댑터가 잘못 성공 처리 | fallback `Provider(Unknown)`, 원래 상태 보존 | 결과 단계 502; Decode로 바뀌지 않음 |
| 200의 잘못된 성공 본문 | `Decode` | 결과 단계 502 |
| 스트림 HTTP 429, 빈 본문을 `classify` | `Provider(Overloaded)` 또는 미분류 fallback `Provider(Unknown)`, 상태 429 | 응답 시작 전 결과 단계 502 |
| 정상 데이터 뒤 제공자 오류 프레임 | 이전 이벤트 뒤 `Provider(Overloaded)`, 요청 ID 보존·상태 `None` | 이전 프레임 뒤 `relay-error`; 합성 stop 없음 |
| 같은 정상 프레임 안에서 이벤트 해석 실패 | 이전 이벤트 뒤 `Decode` | 이전 프레임 뒤 `relay-error` |
| 정상 프레임 뒤 UTF-8 등 framing 실패 | 이전 이벤트 뒤 `Decode` | 이전 프레임 뒤 `relay-error` |
| 정상 EOF인데 stop 없음 | 버퍼 이벤트 뒤 `IncompleteStream` | 이전 프레임 뒤 `relay-error`; 합성 완료 없음 |
| 클라이언트 인코더 push가 일부 출력 후 실패 | `Conversion` | 부분 출력 뒤 `relay-error` |
| 클라이언트 인코더 finish가 꼬리 출력 후 실패 | `Conversion` | 꼬리 출력 뒤 `relay-error` |
| 정상 stop·finish, 허용한 생략 있음 | 오류 없음, `Outcome::Completed`와 손실 목록 | 정상 프레임과 인코더 꼬리 출력 |
| 전송 단절·취소, finish 없이 drop | Relay 오류 없음, 진행 중 report의 outcome은 `None` | 자동 프레임 없음; `GatewayError::Transport` / `Cancelled`는 소비자 처리 |
| 모델 탐색 미지원 | `UnsupportedOperation`, operation `list_models` | 결과 단계 502 또는 소비자 미지원 정책 |
| 빈 모델 페이지와 다음 커서 | 오류 없음, 정상 `ModelPage` | 정상 목록 응답 |

`Conversion(Unrepresentable)` 등은 표의 축약 표기다. 실제 변형은 `RelayError::Conversion(ConversionError { kind: ConversionErrorKind::Unrepresentable, .. })`처럼 원인 구조체를 담는다. 상태 코드와 재시도 정책은 Relay 분류와 별개이며, 이미 클라이언트 응답이 시작됐다면 오류 프레임 또는 연결 종료 정책만 적용할 수 있다.
