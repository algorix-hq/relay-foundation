use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use relay_adapter::{
    Adapter, AdapterError, AdapterExt, Conversion, FrameDecoder, FrameItem, Headers, Method,
    ModelDiscovery, ModelPage, ProviderRequest,
};
use relay_bridge::{
    ApiFormat, Bridge, ConversionOptions, Encoded, Frame, Input, LossScope, NativeFields, Output,
    StreamDecoder, StreamEncoder,
};
use relay_protocol::{
    AssistantContent, ConversionError, ConversionErrorKind, ConversionLoss, Message, ProviderError,
    ProviderErrorCategory, Request, Response, RetryAfter, StopReason, StreamEvent, TextPart,
    UserContent, Version,
};
use serde_json::{Value, json};

struct TestBridge {
    format: ApiFormat,
    decoded_responses: AtomicUsize,
}

impl TestBridge {
    fn client() -> Self {
        Self {
            format: ApiFormat::new("client-wire").expect("nonempty format"),
            decoded_responses: AtomicUsize::new(0),
        }
    }

    fn provider() -> Self {
        Self {
            format: ApiFormat::new("provider-wire").expect("nonempty format"),
            decoded_responses: AtomicUsize::new(0),
        }
    }
}

impl Bridge for TestBridge {
    fn format(&self) -> &ApiFormat {
        &self.format
    }

    fn decode_request(&self, body: &[u8]) -> Result<Input, ConversionError> {
        let mut native: NativeFields = serde_json::from_slice(body).map_err(invalid)?;
        let request = serde_json::from_value(native.remove("request").unwrap_or(Value::Null))
            .map_err(invalid)?;
        let stream = serde_json::from_value(native.remove("stream").unwrap_or(Value::Null))
            .map_err(invalid)?;
        Ok(Input::with_native(
            self.format.clone(),
            request,
            stream,
            native,
        ))
    }

    fn encode_request(
        &self,
        input: &Input,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError> {
        let mut request = input.request.clone();
        let mut losses = Vec::new();
        for (message_index, message) in request.messages.iter_mut().enumerate() {
            if let Message::Assistant { content } = message {
                let mut retained = Vec::new();
                for (index, part) in content.drain(..).enumerate() {
                    if matches!(part, AssistantContent::Unknown { .. }) {
                        if !options.allow_loss.contains(&LossScope::UnknownContent) {
                            return Err(ConversionError {
                                kind: ConversionErrorKind::Unrepresentable,
                                path: format!("/messages/{message_index}/content/{index}"),
                                message: "unknown content requires explicit loss permission".into(),
                            });
                        }
                        losses.push(ConversionLoss {
                            path: format!("/messages/{message_index}/content/{index}"),
                            message: "unknown content omitted".into(),
                        });
                    } else {
                        retained.push(part);
                    }
                }
                *content = retained;
            }
        }
        let mut body = NativeFields::from_iter([
            ("request".into(), json!(request)),
            ("stream".into(), json!(input.stream)),
        ]);
        if let Some(native) = input.native_for(self.format()) {
            for (key, value) in native {
                if body.contains_key(key) {
                    return Err(invalid("native field collides with a generated field"));
                }
                body.insert(key.clone(), value.clone());
            }
        }
        Ok(Encoded {
            body: Value::Object(body),
            losses,
        })
    }

    fn decode_response(&self, body: &[u8]) -> Result<Output, ConversionError> {
        self.decoded_responses.fetch_add(1, Ordering::Relaxed);
        let mut native: NativeFields = serde_json::from_slice(body).map_err(invalid)?;
        let response = serde_json::from_value(native.remove("response").unwrap_or(Value::Null))
            .map_err(invalid)?;
        Ok(Output::with_native(self.format.clone(), response, native))
    }

    fn encode_response(
        &self,
        output: &Output,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError> {
        let mut response = output.response.clone();
        let mut retained = Vec::new();
        let mut losses = Vec::new();
        for (index, part) in response.content.drain(..).enumerate() {
            if matches!(part, AssistantContent::Unknown { .. }) {
                if !options.allow_loss.contains(&LossScope::UnknownContent) {
                    return Err(ConversionError {
                        kind: ConversionErrorKind::Unrepresentable,
                        path: format!("/content/{index}"),
                        message: "unknown content requires explicit loss permission".into(),
                    });
                }
                losses.push(ConversionLoss {
                    path: format!("/content/{index}"),
                    message: "unknown content omitted".into(),
                });
            } else {
                retained.push(part);
            }
        }
        response.content = retained;
        let mut body = NativeFields::from_iter([("response".into(), json!(response))]);
        if let Some(native) = output.native_for(self.format()) {
            for (key, value) in native {
                if body.contains_key(key) {
                    return Err(invalid("native field collides with a generated field"));
                }
                body.insert(key.clone(), value.clone());
            }
        }
        Ok(Encoded {
            body: Value::Object(body),
            losses,
        })
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(TestStreamDecoder { stopped: false })
    }

    fn stream_encoder(&self, _options: &ConversionOptions) -> Box<dyn StreamEncoder> {
        Box::new(TestStreamEncoder)
    }
}

struct TestStreamDecoder {
    stopped: bool,
}

impl StreamDecoder for TestStreamDecoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), ConversionError> {
        let values: Vec<Value> = serde_json::from_str(&frame.data).map_err(invalid)?;
        for value in values {
            let event = serde_json::from_value(value).map_err(invalid)?;
            self.stopped |= matches!(event, StreamEvent::Stop { .. });
            out.push(event);
        }
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), ConversionError> {
        if !self.stopped {
            return Err(invalid("stream ended before stop"));
        }
        Ok(())
    }
}

struct TestStreamEncoder;

impl StreamEncoder for TestStreamEncoder {
    fn push(&mut self, event: &StreamEvent, out: &mut Vec<Frame>) -> Result<(), ConversionError> {
        out.push(Frame {
            event: None,
            data: serde_json::to_string(event).map_err(invalid)?,
        });
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<Frame>) -> Result<Vec<ConversionLoss>, ConversionError> {
        Ok(Vec::new())
    }
}

struct TestAdapter {
    conversion: Conversion,
    discovery: bool,
}

impl Default for TestAdapter {
    fn default() -> Self {
        Self::new(
            Arc::new(TestBridge::provider()),
            ConversionOptions::default(),
        )
    }
}

impl TestAdapter {
    fn new(bridge: Arc<dyn Bridge>, options: ConversionOptions) -> Self {
        Self {
            conversion: Conversion::new(
                bridge,
                &[ApiFormat::new("provider-wire").expect("nonempty format")],
                options,
            )
            .expect("compatible provider bridge"),
            discovery: false,
        }
    }
}

impl Adapter for TestAdapter {
    fn conversion(&self) -> &Conversion {
        &self.conversion
    }

    fn assemble_request(
        &self,
        model: &str,
        body: Value,
        stream: bool,
    ) -> Result<ProviderRequest, ConversionError> {
        Ok(ProviderRequest {
            method: Method::Post,
            url: format!(
                "https://test.invalid/{}/{model}",
                self.conversion().bridge().format()
            ),
            headers: Headers(vec![("Authorization".into(), "test-token".into())]),
            body: serde_json::to_vec(&json!({"wire": body, "stream": stream})).map_err(invalid)?,
        })
    }

    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError> {
        if !(200..300).contains(&status) || body == b"provider-failure" {
            return Err(ProviderError {
                http_status: Some(status),
                request_id: headers.get("request-id").map(str::to_owned),
                retry_after: headers
                    .get("retry-after")
                    .map(|date| RetryAfter::HttpDate { date: date.into() }),
                ..provider_failure()
            });
        }
        Ok(())
    }

    fn frame_decoder(&self) -> Box<dyn FrameDecoder> {
        Box::new(TestFrameDecoder(Vec::new()))
    }

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        if self.discovery { Some(self) } else { None }
    }
}

impl ModelDiscovery for TestAdapter {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<ProviderRequest, ConversionError> {
        Ok(ProviderRequest {
            method: Method::Get,
            url: format!("https://test.invalid/models/{}", cursor.unwrap_or_default()),
            headers: Headers::default(),
            body: Vec::new(),
        })
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, ConversionError> {
        let (model_ids, next_cursor) = serde_json::from_slice(body).map_err(invalid)?;
        Ok(ModelPage {
            model_ids,
            next_cursor,
        })
    }
}

struct TestFrameDecoder(Vec<u8>);

impl TestFrameDecoder {
    fn frame(line: Vec<u8>) -> Result<FrameItem, ConversionError> {
        let data = String::from_utf8(line).map_err(invalid)?;
        if data == "error" {
            return Ok(FrameItem::Error(provider_failure()));
        }
        Ok(FrameItem::Data(Frame { event: None, data }))
    }
}

impl FrameDecoder for TestFrameDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<FrameItem>, ConversionError> {
        self.0.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = self.0.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.0.drain(..=end).collect();
            line.pop();
            frames.push(Self::frame(line)?);
        }
        Ok(frames)
    }

    fn finish(&mut self) -> Result<Vec<FrameItem>, ConversionError> {
        if self.0.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Self::frame(std::mem::take(&mut self.0))?])
    }
}

fn invalid(error: impl std::fmt::Display) -> ConversionError {
    ConversionError {
        kind: ConversionErrorKind::Invalid,
        path: String::new(),
        message: error.to_string(),
    }
}

fn provider_failure() -> ProviderError {
    ProviderError {
        category: ProviderErrorCategory::Overloaded,
        message: "provider is busy".into(),
        code: Some("busy".into()),
        http_status: None,
        request_id: None,
        retry_after: None,
    }
}

fn request() -> Request {
    Request {
        version: Version::V1,
        model: "test-model".into(),
        messages: vec![Message::User {
            content: vec![UserContent::Text(TextPart {
                text: "hello".into(),
                cache_control: None,
            })],
        }],
        tools: None,
        parameters: None,
        response_format: None,
        reasoning: None,
    }
}

#[test]
fn gateway_converts_between_client_and_provider_formats() {
    let client: Box<dyn Bridge> = Box::new(TestBridge::client());
    let adapter: Box<dyn Adapter> = Box::new(TestAdapter::default());
    let body = serde_json::to_vec(&json!({"request": request(), "stream": true}))
        .expect("client request body");
    let input = client.decode_request(&body).expect("strict client ingress");
    let (req, losses) = adapter.build_request(&input).expect("provider request");
    assert_eq!(req.method, Method::Post);
    assert_eq!(req.url, "https://test.invalid/provider-wire/test-model");
    assert_eq!(req.headers.get("authorization"), Some("test-token"));
    assert_eq!(
        serde_json::from_slice::<Value>(&req.body).expect("provider body"),
        json!({"wire": {"request": request(), "stream": true}, "stream": true})
    );
    assert!(losses.is_empty());

    let response = Response {
        version: Version::V1,
        id: Some("response-1".into()),
        model: "test-model".into(),
        content: vec![AssistantContent::Text(TextPart {
            text: "world".into(),
            cache_control: None,
        })],
        stop_reason: StopReason::EndTurn,
        usage: None,
    };
    let response_body = serde_json::to_vec(&json!({"response": response, "provider-tier": "test"}))
        .expect("fake transport response");
    let output = adapter
        .parse_response(200, &Headers::default(), &response_body)
        .expect("provider response");
    let encoded = client
        .encode_response(&output, &ConversionOptions::default())
        .expect("client response");
    assert_eq!(encoded.body, json!({"response": response}));
    assert!(encoded.losses.is_empty());
    assert_eq!(
        adapter
            .conversion()
            .bridge()
            .encode_response(&output, adapter.conversion().options())
            .expect("same-format response")
            .body,
        json!({"response": response, "provider-tier": "test"})
    );
}

#[test]
fn native_request_fields_only_reach_a_same_format_bridge() {
    let adapter = TestAdapter::default();
    for (bridge, forwarded) in [
        (TestBridge::client(), false),
        (TestBridge::provider(), true),
    ] {
        let input = bridge
            .decode_request(
                &serde_json::to_vec(&json!({
                    "request": request(), "stream": false, "native-option": 7
                }))
                .expect("wire request"),
            )
            .expect("request ingress");
        let (req, _) = adapter.build_request(&input).expect("request builds");
        let body: Value = serde_json::from_slice(&req.body).expect("provider body");
        assert_eq!(
            body["wire"].get("native-option"),
            forwarded.then_some(&json!(7))
        );
    }
}

#[test]
fn incompatible_formats_report_both_sides() {
    let error = Conversion::new(
        Arc::new(TestBridge::client()),
        &[ApiFormat::MESSAGES],
        ConversionOptions::default(),
    )
    .err()
    .expect("incompatible formats rejected");
    assert_eq!(error.bridge_format.as_str(), "client-wire");
    assert_eq!(error.supported_formats, vec![ApiFormat::MESSAGES]);
}

#[test]
fn provider_rejection_bypasses_conversion_including_on_success_status() {
    let bridge = Arc::new(TestBridge::provider());
    let adapter = TestAdapter::new(bridge.clone(), ConversionOptions::default());
    let headers = Headers(vec![
        ("Request-ID".into(), "request-1".into()),
        ("Retry-After".into(), "Wed, 11 Nov 2026 00:00:00 GMT".into()),
    ]);
    for (status, body) in [(503, b"not-json".as_slice()), (200, b"provider-failure")] {
        assert_eq!(
            adapter.parse_response(status, &headers, body),
            Err(AdapterError::Provider(ProviderError {
                http_status: Some(status),
                request_id: Some("request-1".into()),
                retry_after: Some(RetryAfter::HttpDate {
                    date: "Wed, 11 Nov 2026 00:00:00 GMT".into(),
                }),
                ..provider_failure()
            })),
        );
    }
    assert_eq!(bridge.decoded_responses.load(Ordering::Relaxed), 0);
}

#[test]
fn provider_stream_failure_preserves_earlier_events_and_is_terminal() {
    let mut decoder = TestAdapter::default().stream_decoder();
    let mut events = Vec::new();
    decoder
        .push(b"[{\"type\":\"response_", &mut events)
        .expect("partial frame");
    assert!(events.is_empty());
    assert_eq!(
        decoder.push(
            b"start\"}]\nerror\n[{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}]\n",
            &mut events
        ),
        Err(AdapterError::Provider(provider_failure()))
    );
    let expected = vec![StreamEvent::ResponseStart {
        id: None,
        model: None,
    }];
    assert_eq!(events, expected);
    decoder
        .push(b"not valid framing", &mut events)
        .expect("terminal push");
    decoder.finish(&mut events).expect("terminal finish");
    assert_eq!(events, expected);
}

#[test]
fn bridge_failure_mid_frame_preserves_appended_events_and_is_terminal() {
    let mut decoder = TestAdapter::default().stream_decoder();
    let mut events = vec![StreamEvent::ResponseStart {
        id: Some("prior".into()),
        model: None,
    }];
    let result = decoder.push(
        b"[{\"type\":\"response_start\"},{\"type\":\"invalid\"}]\n",
        &mut events,
    );
    assert!(matches!(result, Err(AdapterError::Conversion(_))));
    let expected = vec![
        StreamEvent::ResponseStart {
            id: Some("prior".into()),
            model: None,
        },
        StreamEvent::ResponseStart {
            id: None,
            model: None,
        },
    ];
    assert_eq!(events, expected);
    decoder.push(b"[]\n", &mut events).expect("terminal push");
    decoder.finish(&mut events).expect("terminal finish");
    assert_eq!(events, expected);
}

#[test]
fn finish_flushes_frames_and_requires_a_stop() {
    let adapter = TestAdapter::default();
    let mut truncated = adapter.stream_decoder();
    let mut events = Vec::new();
    truncated
        .push(b"[{\"type\":\"response_start\"}]", &mut events)
        .expect("buffered frame");
    assert!(matches!(
        truncated.finish(&mut events),
        Err(AdapterError::Conversion(ConversionError {
            kind: ConversionErrorKind::Invalid,
            ..
        }))
    ));
    let expected = vec![StreamEvent::ResponseStart {
        id: None,
        model: None,
    }];
    assert_eq!(events, expected);
    truncated.finish(&mut events).expect("terminal finish");
    truncated
        .push(b"not-json\n", &mut events)
        .expect("terminal push");
    assert_eq!(events, expected);

    let mut complete = adapter.stream_decoder();
    let mut events = Vec::new();
    complete
        .push(
            b"[{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}]",
            &mut events,
        )
        .expect("buffered stop");
    complete.finish(&mut events).expect("normal finish");
    assert_eq!(
        events,
        vec![StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            usage: None
        }]
    );
    let client = TestBridge::client();
    let mut encoder = client.stream_encoder(&ConversionOptions::default());
    let mut frames = Vec::new();
    for event in events.drain(..) {
        encoder.push(&event, &mut frames).expect("client frame");
    }
    assert!(
        encoder
            .finish(&mut frames)
            .expect("encoder finishes")
            .is_empty()
    );
    assert_eq!(
        frames,
        vec![Frame {
            event: None,
            data: "{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}".into()
        }]
    );
    complete
        .push(b"not-json\n", &mut events)
        .expect("terminal push");
    complete.finish(&mut events).expect("terminal finish");
    assert!(events.is_empty());
}

#[test]
fn discovery_distinguishes_unsupported_empty_pages_and_provider_errors() {
    let unsupported = TestAdapter::default();
    assert!(unsupported.list_models(None).is_none());
    assert!(
        unsupported
            .parse_models(503, &Headers::default(), b"not-json")
            .is_none()
    );
    let adapter = TestAdapter {
        discovery: true,
        ..TestAdapter::default()
    };
    let req = adapter
        .list_models(Some("page-2"))
        .expect("supported")
        .expect("discovery request");
    assert_eq!(req.method, Method::Get);
    assert_eq!(req.url, "https://test.invalid/models/page-2");
    assert!(req.body.is_empty());
    assert_eq!(
        adapter
            .parse_models(200, &Headers::default(), b"[[],\"page-3\"]")
            .expect("supported")
            .expect("empty page is valid"),
        ModelPage {
            model_ids: Vec::new(),
            next_cursor: Some("page-3".into())
        }
    );
    assert_eq!(
        adapter
            .parse_models(429, &Headers::default(), b"not-json")
            .expect("supported"),
        Err(AdapterError::Provider(ProviderError {
            http_status: Some(429),
            ..provider_failure()
        }))
    );
    assert_eq!(
        adapter
            .parse_models(200, &Headers::default(), b"provider-failure")
            .expect("supported"),
        Err(AdapterError::Provider(ProviderError {
            http_status: Some(200),
            ..provider_failure()
        }))
    );
}

#[test]
fn build_request_propagates_loss_permission_and_reports_actual_omissions() {
    let mut request = request();
    request.messages.push(Message::Assistant {
        content: vec![AssistantContent::Unknown {
            value: json!({"future": 1}),
        }],
    });
    let input = Input::new(request);
    assert!(matches!(
        TestAdapter::default().build_request(&input),
        Err(ConversionError {
            kind: ConversionErrorKind::Unrepresentable,
            ..
        })
    ));
    let adapter = TestAdapter::new(
        Arc::new(TestBridge::provider()),
        ConversionOptions {
            allow_loss: [LossScope::UnknownContent].into(),
        },
    );
    let (req, losses) = adapter
        .build_request(&input)
        .expect("explicit omission permitted");
    assert_eq!(
        losses,
        vec![ConversionLoss {
            path: "/messages/1/content/0".into(),
            message: "unknown content omitted".into()
        }]
    );
    let body: Value = serde_json::from_slice(&req.body).expect("provider body");
    assert_eq!(body["wire"]["request"]["messages"][1]["content"], json!([]));
}

#[test]
fn api_format_rejects_empty_identifiers() {
    assert!(matches!(
        ApiFormat::new(String::new()),
        Err(ConversionError {
            kind: ConversionErrorKind::Invalid,
            ..
        })
    ));
}
