//! Covers acceptance criteria from the design doc that other tests do not cover.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use relay_adapter::providers::anthropic::AnthropicAdapter;
use relay_adapter::{Adapter, AdapterExt, BaseAdapter, Headers, Method};
use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ChatCompletionsBridge, ConversionOptions, DecodeContext,
    Encoded, MessagesBridge, ResponsesBridge, StreamDecoder, StreamEncoder,
};
use relay_protocol::{
    ConversionError, ConversionErrorKind, DecodeError, Error, ProviderError, ProviderErrorCategory,
    Request, Response, StreamEvent,
};
use serde_json::{Value, json};

struct PassthroughBridge {
    decoded_responses: AtomicUsize,
}

impl Bridge for PassthroughBridge {
    fn format(&self) -> &ApiFormat {
        &ApiFormat::CHAT_COMPLETIONS
    }

    fn decode_request(&self, body: &[u8]) -> Result<Request, BridgeError> {
        serde_json::from_slice(body).map_err(|error| {
            ConversionError {
                kind: ConversionErrorKind::Invalid,
                path: String::new(),
                message: error.to_string(),
            }
            .into()
        })
    }

    fn encode_request(
        &self,
        request: &Request,
        _options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        Ok(Encoded {
            body: json!({"custom_request": request}),
            losses: Vec::new(),
        })
    }

    fn decode_response(
        &self,
        body: &[u8],
        _context: &DecodeContext,
    ) -> Result<Response, BridgeError> {
        self.decoded_responses.fetch_add(1, Ordering::Relaxed);
        serde_json::from_slice(body).map_err(|error| {
            DecodeError {
                path: String::new(),
                message: error.to_string(),
            }
            .into()
        })
    }

    fn encode_response(
        &self,
        _request: &Request,
        response: &Response,
        _options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        Ok(Encoded {
            body: json!(response),
            losses: Vec::new(),
        })
    }

    fn stream_decoder(&self, _context: &DecodeContext) -> Box<dyn StreamDecoder> {
        unreachable!("this custom bridge only supports non-streaming exchanges")
    }

    fn stream_encoder(
        &self,
        _request: &Request,
        _options: &ConversionOptions,
    ) -> Box<dyn StreamEncoder> {
        unreachable!("this custom bridge only supports non-streaming exchanges")
    }
}

fn request() -> Request {
    serde_json::from_value(json!({
        "version": "1", "model": "claude-haiku-5-5",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
        "parameters": {"max_output_tokens": 16}
    }))
    .expect("structurally valid request")
}

fn recorded_headers(raw: &str) -> (u16, Headers) {
    let mut lines = raw.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .expect("recorded status");
    let headers = Headers(
        lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.into(), value.trim().into()))
            .collect(),
    );
    (status, headers)
}

fn status_classify(
    _payload: &Value,
    _envelope: &Value,
    http_status: Option<u16>,
    _headers: &Headers,
) -> ProviderError {
    ProviderError {
        category: match http_status {
            Some(401) => ProviderErrorCategory::Authentication,
            _ => ProviderErrorCategory::Unknown,
        },
        message: "provider returned an error".into(),
        code: None,
        http_status,
        request_id: None,
        retry_after: None,
    }
}

#[test]
fn custom_bridge_bodies_keep_adapter_provider_error_ownership() {
    let bridge = Arc::new(PassthroughBridge {
        decoded_responses: AtomicUsize::new(0),
    });
    let adapter = BaseAdapter::new(
        bridge.clone(),
        "https://ollama.com/v1/chat/completions",
        status_classify,
    )
    .header("authorization", "Bearer fixture-key");
    let input = request();
    let (built, losses) = adapter.build_request(&input).expect("custom request");
    assert_eq!(built.method, Method::Post);
    assert_eq!(built.url, "https://ollama.com/v1/chat/completions");
    assert_eq!(
        built.headers.get("authorization"),
        Some("Bearer fixture-key")
    );
    assert_eq!(built.headers.get("content-type"), Some("application/json"));
    assert_eq!(
        serde_json::from_slice::<Value>(&built.body).expect("custom body"),
        json!({"custom_request": input})
    );
    assert!(losses.is_empty());

    let success = json!({
        "version": "1", "id": "custom-response", "created_at": {"observed_at": 7},
        "model": "claude-haiku-5-5", "stop_reason": "end_turn",
        "content": [{"type": "text", "id": "custom-response:0", "text": "Custom answer"}]
    });
    let expected: Response = serde_json::from_value(success.clone()).expect("custom response");
    assert_eq!(
        adapter
            .parse_response(200, &Headers::default(), success.to_string().as_bytes())
            .expect("custom decoding"),
        expected
    );

    let (status, headers) =
        recorded_headers(include_str!("fixtures/live/ollama_invalid_key.headers"));
    let body = include_bytes!("fixtures/live/ollama_invalid_key.json");
    let classified = adapter
        .check_response(status, &headers, body)
        .expect_err("adapter classifies authentication failure");
    assert_eq!(classified.category, ProviderErrorCategory::Authentication);
    assert_eq!(
        adapter.parse_response(status, &headers, body),
        Err(Error::Provider(classified))
    );
    assert_eq!(bridge.decoded_responses.load(Ordering::Relaxed), 1);
}

#[test]
fn dropping_partial_stream_preserves_events_without_errors_but_normal_eof_is_incomplete() {
    let adapter = AnthropicAdapter::new("fixture-key").with_clock(|| 7);
    let (status, headers) = recorded_headers(include_str!(
        "fixtures/e2e/anthropic_stream.response.headers"
    ));
    let recording = include_str!("fixtures/e2e/anthropic_stream.response.sse");
    let cutoff = recording
        .find("event: content_block_stop")
        .expect("recorded text precedes item end");
    let partial = &recording.as_bytes()[..cutoff];
    let expected: Vec<StreamEvent> = serde_json::from_value(json!([
        {"type": "start", "response_id": "msg_011CgwC1uzyBxfJtpptpZ4mG",
         "provider_id": "msg_011CgwC1uzyBxfJtpptpZ4mG", "created_at": {"observed_at": 7},
         "model": "claude-haiku-5-5"},
        {"type": "metadata", "scope": "response", "usage": {
            "input_tokens": 16, "output_tokens": 3, "cache_read_tokens": 0, "cache_write_tokens": 0}},
        {"type": "item_start", "index": 0, "id": "msg_011CgwC1uzyBxfJtpptpZ4mG:0",
         "item": {"type": "text"}},
        {"type": "delta", "index": 0, "delta": {"type": "text", "text": "Hello"}},
        {"type": "delta", "index": 0, "delta": {"type": "text", "text": " there"}},
        {"type": "delta", "index": 0, "delta": {"type": "text", "text": "!"}}
    ]))
    .expect("expected partial events");
    let expected: Vec<_> = expected.into_iter().map(Ok).collect();

    // Transport failure and cancellation are consumer-owned, never Error variants.
    // Stopping input must not fabricate completion or an incomplete-stream error.
    {
        let mut stream = adapter.open_stream(status, &headers).expect("stream");
        let mut events = Vec::new();
        for chunk in partial.chunks(37) {
            events.extend(stream.push(chunk));
        }
        drop(stream);
        assert_eq!(events, expected);
        assert!(events.iter().all(Result::is_ok));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::Stop { .. })))
        );
    }

    {
        let mut stream = adapter.open_stream(status, &headers).expect("stream");
        let mut events = Vec::new();
        for chunk in partial.chunks(37) {
            events.extend(stream.push(chunk));
        }
        assert_eq!(events, expected);
        let terminal: Vec<_> = stream.finish().collect();
        assert!(matches!(
            terminal.as_slice(),
            [Err(Error::IncompleteStream(_))]
        ));
    }
}

#[test]
fn messages_adapter_rejects_unrepresentable_system_position_without_reordering() {
    let mut input = request();
    input.messages = serde_json::from_value(json!([
        {"role": "system", "content": [{"type": "text", "text": "Initial policy"}]},
        {"role": "user", "content": [{"type": "text", "text": "First turn"}]},
        {"role": "assistant", "content": [{"type": "text", "id": "r:0", "text": "Answer"}]},
        {"role": "system", "content": [{"type": "text", "text": "Later policy"}]},
        {"role": "user", "content": [{"type": "text", "text": "Next turn"}]}
    ]))
    .expect("ordered conversation");
    let error = AnthropicAdapter::new("fixture-key")
        .build_request(&input)
        .expect_err("must reject instead of producing a reordered body");
    let Error::Conversion(error) = error else {
        panic!("expected conversion failure, got {error:?}")
    };
    assert_eq!(error.kind, ConversionErrorKind::Invalid);
    assert_eq!(error.path, "/messages/3");
}

#[test]
fn model_ids_do_not_change_encoding_and_unknown_model_rejection_remains_a_provider_error() {
    let adapter = AnthropicAdapter::new("fixture-key");
    let mut input = request();
    for model in [
        "claude-haiku-5-5",
        "relay-no-such-model",
        "consumer-invented-model",
    ] {
        input.model = model.into();
        let (built, losses) = adapter
            .build_request(&input)
            .expect("model-independent encoding");
        let mut body: Value = serde_json::from_slice(&built.body).expect("request body");
        assert_eq!(
            body.as_object_mut().expect("object").remove("model"),
            Some(json!(model))
        );
        assert_eq!(
            body,
            json!({
                "max_tokens": 16,
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}]
            })
        );
        assert!(losses.is_empty());
    }

    let (status, headers) = recorded_headers(include_str!(
        "fixtures/live/anthropic_bearer_model_not_found.headers"
    ));
    let error = adapter
        .parse_response(
            status,
            &headers,
            include_bytes!("fixtures/live/anthropic_bearer_model_not_found.json"),
        )
        .expect_err("provider rejects unknown model");
    let Error::Provider(error) = error else {
        panic!("expected normalized provider failure, got {error:?}")
    };
    assert_eq!(error.http_status, Some(404));
    assert_eq!(error.code.as_deref(), Some("not_found_error"));
    assert_eq!(
        error.request_id.as_deref(),
        Some("req_011Cgw7NBGy8NHYiMbB5TVoE")
    );
}

#[test]
fn builtin_bridges_preserve_schema_invalid_arguments_and_business_invalid_tool_results() {
    let arguments = r#"{"count":"wrong-type"}"#;
    let mut input = request();
    input.messages = serde_json::from_value(json!([
        {"role": "assistant", "content": [{"type": "tool_call", "id": "r:0",
            "call_id": "call_1", "name": "lookup", "arguments": arguments}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_call_id": "call_1",
            "content": [{"type": "text", "text": ""}]}]}
    ]))
    .expect("structurally valid tool exchange");
    input.tools = Some(serde_json::from_value(json!({"definitions": [{
        "type": "function", "name": "lookup", "parameters": {
            "type": "object", "properties": {"count": {"type": "integer"}, "key": {"type": "string"}},
            "required": ["count", "key"]
        }
    }]})).expect("declared tool schema"));

    let cases: [(Box<dyn Bridge>, &str, &str); 3] = [
        (
            Box::new(ChatCompletionsBridge::new()),
            "/messages/0/tool_calls/0/function/arguments",
            "/messages/1/content",
        ),
        (
            Box::new(ResponsesBridge::new()),
            "/input/0/arguments",
            "/input/1/output/0/text",
        ),
        (
            Box::new(MessagesBridge::new()),
            "/messages/0/content/0/input",
            "/messages/1/content/0/content/0/text",
        ),
    ];
    for (bridge, arguments_path, result_path) in cases {
        let encoded = bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("schema and business rules belong to the consumer");
        let wire_arguments = encoded
            .body
            .pointer(arguments_path)
            .expect("tool arguments");
        let argument_bytes = match wire_arguments.as_str() {
            Some(text) => text.as_bytes().to_vec(),
            None => serde_json::to_vec(wire_arguments).expect("object-form arguments"),
        };
        assert_eq!(argument_bytes, arguments.as_bytes(), "{}", bridge.format());
        assert_eq!(
            encoded.body.pointer(result_path),
            Some(&json!("")),
            "{}",
            bridge.format()
        );
        assert!(encoded.losses.is_empty(), "{}", bridge.format());
    }
}
