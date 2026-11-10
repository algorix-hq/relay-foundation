use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use relay_adapter::{
    Adapter, DiscoveryError, FrameDecoder, FrameItem, Headers, HttpRequest, Method, ModelDiscovery,
    ModelPage, Pipeline, PipelineError, StreamChunk,
};
use relay_bridge::{
    ApiFormat, Bridge, ConversionOptions, Encoded, Frame, LossScope, NativeFields, StreamDecoder,
};
use relay_protocol::{
    AssistantContent, ConversionError, ConversionErrorKind, ConversionLoss, Message, ProviderError,
    ProviderErrorCategory, Request, Response, RetryAfter, StopReason, StreamEvent, TextPart,
    UserContent, Version,
};
use serde_json::{Value, json};

struct TestBridge {
    format: ApiFormat,
    losses: Vec<ConversionLoss>,
    decoded_responses: AtomicUsize,
}

impl Default for TestBridge {
    fn default() -> Self {
        Self {
            format: ApiFormat::new("test-wire").expect("nonempty format"),
            losses: Vec::new(),
            decoded_responses: AtomicUsize::new(0),
        }
    }
}

impl Bridge for TestBridge {
    fn format(&self) -> &ApiFormat {
        &self.format
    }

    fn decode_request(&self, body: &[u8]) -> Result<(Request, NativeFields), ConversionError> {
        let (request, native, _stream): (Request, NativeFields, bool) =
            serde_json::from_slice(body).map_err(invalid)?;
        Ok((request, native))
    }

    fn encode_request(
        &self,
        request: &Request,
        native: &NativeFields,
        stream: bool,
        options: &ConversionOptions,
    ) -> Result<Encoded, ConversionError> {
        if !self.losses.is_empty() && !options.allow_loss.contains(&LossScope::UnknownContent) {
            return Err(ConversionError {
                kind: ConversionErrorKind::Unrepresentable,
                path: "/messages/0/content/0".into(),
                message: "unknown content requires explicit loss permission".into(),
            });
        }
        Ok(Encoded {
            body: json!([request, native, stream]),
            losses: self.losses.clone(),
        })
    }

    fn decode_response(&self, body: &[u8]) -> Result<(Response, NativeFields), ConversionError> {
        self.decoded_responses.fetch_add(1, Ordering::Relaxed);
        serde_json::from_slice(body).map_err(invalid)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(TestStreamDecoder { stopped: false })
    }
}

struct TestStreamDecoder {
    stopped: bool,
}

impl StreamDecoder for TestStreamDecoder {
    fn push(&mut self, frame: Frame) -> Result<Vec<StreamEvent>, ConversionError> {
        let events: Vec<StreamEvent> = serde_json::from_str(&frame.data).map_err(invalid)?;
        self.stopped |= events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. }));
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ConversionError> {
        if !self.stopped {
            return Err(invalid("stream ended before stop"));
        }
        Ok(Vec::new())
    }
}

struct TestAdapter {
    formats: Vec<ApiFormat>,
    discovery: bool,
}

impl Default for TestAdapter {
    fn default() -> Self {
        Self {
            formats: vec![ApiFormat::new("test-wire").expect("nonempty format")],
            discovery: false,
        }
    }
}

impl Adapter for TestAdapter {
    fn supported_formats(&self) -> &[ApiFormat] {
        &self.formats
    }

    fn build_request(
        &self,
        format: &ApiFormat,
        model: &str,
        body: &Value,
        stream: bool,
    ) -> Result<HttpRequest, ConversionError> {
        Ok(HttpRequest {
            method: Method::Post,
            url: format!("https://test.invalid/{format}/{model}"),
            headers: Headers(vec![("Authorization".into(), "test-token".into())]),
            body: Some(
                serde_json::to_vec(&json!({"wire": body, "stream": stream})).map_err(invalid)?,
            ),
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

    fn frame_decoder(&self, _format: &ApiFormat) -> Box<dyn FrameDecoder> {
        Box::new(TestFrameDecoder(Vec::new()))
    }

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        if self.discovery { Some(self) } else { None }
    }
}

impl ModelDiscovery for TestAdapter {
    fn build_list_models(&self, cursor: Option<&str>) -> Result<HttpRequest, ConversionError> {
        Ok(HttpRequest {
            method: Method::Get,
            url: format!("https://test.invalid/models/{}", cursor.unwrap_or_default()),
            headers: Headers::default(),
            body: None,
        })
    }

    fn parse_list_models(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<ModelPage, DiscoveryError> {
        self.check_response(status, headers, body)?;
        Ok(ModelPage {
            model_ids: serde_json::from_slice(body).map_err(invalid)?,
            next_cursor: headers.get("next-cursor").map(str::to_owned),
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

fn pipeline() -> Pipeline {
    Pipeline::new(
        Arc::new(TestBridge::default()),
        Arc::new(TestAdapter::default()),
    )
    .expect("compatible custom formats")
}

#[test]
fn custom_extensions_round_trip_through_consumer_transport() {
    let bridge = Arc::new(TestBridge::default());
    let pipeline = Pipeline::new(bridge.clone(), Arc::new(TestAdapter::default()))
        .expect("compatible custom formats");
    let request = request();
    let native = NativeFields::from_iter([("custom-option".into(), json!(7))]);
    let (http, losses) = pipeline
        .build_request(&request, &native, true, &ConversionOptions::default())
        .expect("request builds");
    assert_eq!(http.method, Method::Post);
    assert_eq!(http.url, "https://test.invalid/test-wire/test-model");
    assert_eq!(http.headers.get("authorization"), Some("test-token"));
    let body: Value =
        serde_json::from_slice(http.body.as_deref().expect("request body")).expect("JSON body");
    assert_eq!(
        body,
        json!({"wire": [&request, &native, true], "stream": true})
    );
    assert!(losses.is_empty());
    let ingress = bridge
        .decode_request(&serde_json::to_vec(&body["wire"]).expect("wire body"))
        .expect("strict ingress");
    assert_eq!(ingress, (request, native));

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
    let metadata = NativeFields::from_iter([("tier".into(), json!("test"))]);
    let response_body = serde_json::to_vec(&(&response, &metadata)).expect("response body");
    assert_eq!(
        pipeline
            .parse_response(200, &Headers::default(), &response_body)
            .expect("response decodes"),
        (response, metadata),
    );
}

#[test]
fn incompatible_formats_report_both_sides() {
    let adapter = TestAdapter {
        formats: vec![ApiFormat::MESSAGES],
        ..TestAdapter::default()
    };
    let result = Pipeline::new(Arc::new(TestBridge::default()), Arc::new(adapter));
    let error = result.err().expect("incompatible formats rejected");
    assert_eq!(error.bridge_format.as_str(), "test-wire");
    assert_eq!(error.supported_formats, vec![ApiFormat::MESSAGES]);
}

#[test]
fn provider_rejection_bypasses_response_conversion_including_on_success_status() {
    let bridge = Arc::new(TestBridge::default());
    let pipeline = Pipeline::new(bridge.clone(), Arc::new(TestAdapter::default()))
        .expect("compatible formats");
    let headers = Headers(vec![
        ("Request-ID".into(), "request-1".into()),
        ("Retry-After".into(), "Wed, 11 Nov 2026 00:00:00 GMT".into()),
    ]);
    for status in [503, 200] {
        let expected = ProviderError {
            http_status: Some(status),
            request_id: Some("request-1".into()),
            retry_after: Some(RetryAfter::HttpDate {
                date: "Wed, 11 Nov 2026 00:00:00 GMT".into(),
            }),
            ..provider_failure()
        };
        assert_eq!(
            pipeline.parse_response(status, &headers, b"provider-failure"),
            Err(PipelineError::Provider(expected.clone())),
        );
        assert_eq!(
            pipeline.check_stream_start(status, &headers, b"provider-failure"),
            Err(PipelineError::Provider(expected)),
        );
    }
    assert_eq!(bridge.decoded_responses.load(Ordering::Relaxed), 0);
}

#[test]
fn mid_stream_provider_failure_preserves_earlier_events_and_is_terminal() {
    let pipeline = pipeline();
    pipeline
        .check_stream_start(200, &Headers::default(), b"")
        .expect("stream accepted");
    let mut stream = pipeline.stream();
    assert_eq!(
        stream.push(b"[{\"type\":\"response_"),
        StreamChunk::default()
    );
    let chunk = stream.push(b"start\",\"model\":\"test-model\"}]\nerror\n[]\n");
    assert_eq!(
        chunk.events,
        vec![StreamEvent::ResponseStart {
            id: None,
            model: Some("test-model".into())
        }]
    );
    assert_eq!(
        chunk.failure,
        Some(PipelineError::Provider(provider_failure()))
    );
    assert_eq!(stream.push(b"not valid framing"), StreamChunk::default());
    assert_eq!(stream.finish(), StreamChunk::default());
}

#[test]
fn normal_finish_flushes_frames_and_missing_stop_is_a_conversion_failure() {
    let pipeline = pipeline();
    let mut complete = pipeline.stream();
    assert_eq!(
        complete.push(b"[{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}]"),
        StreamChunk::default()
    );
    assert_eq!(
        complete.finish(),
        StreamChunk {
            events: vec![StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: None
            }],
            failure: None,
        }
    );
    assert_eq!(complete.finish(), StreamChunk::default());

    let mut truncated = pipeline.stream();
    assert_eq!(
        truncated.push(b"[{\"type\":\"response_start\"}]"),
        StreamChunk::default()
    );
    let chunk = truncated.finish();
    assert_eq!(
        chunk.events,
        vec![StreamEvent::ResponseStart {
            id: None,
            model: None
        }]
    );
    assert!(matches!(
        chunk.failure,
        Some(PipelineError::Conversion(ConversionError {
            kind: ConversionErrorKind::Invalid,
            ..
        }))
    ));
    assert_eq!(truncated.finish(), StreamChunk::default());
}

#[test]
fn bridge_conversion_failure_preserves_events_from_earlier_frames() {
    let mut stream = pipeline().stream();
    let chunk = stream.push(b"[{\"type\":\"response_start\"}]\nnot-json\n");
    assert_eq!(
        chunk.events,
        vec![StreamEvent::ResponseStart {
            id: None,
            model: None
        }]
    );
    assert!(matches!(chunk.failure, Some(PipelineError::Conversion(_))));
    assert_eq!(stream.push(b"[]\n"), StreamChunk::default());
    assert_eq!(stream.finish(), StreamChunk::default());
}

#[test]
fn unsupported_discovery_differs_from_an_empty_page() {
    assert!(TestAdapter::default().model_discovery().is_none());
    let adapter = TestAdapter {
        discovery: true,
        ..TestAdapter::default()
    };
    let discovery = adapter.model_discovery().expect("discovery supported");
    let http = discovery
        .build_list_models(Some("page-2"))
        .expect("discovery request");
    assert_eq!(http.method, Method::Get);
    assert_eq!(http.url, "https://test.invalid/models/page-2");
    assert_eq!(http.body, None);
    assert_eq!(
        discovery
            .parse_list_models(200, &Headers::default(), b"[]")
            .expect("empty page is valid"),
        ModelPage {
            model_ids: Vec::new(),
            next_cursor: None,
        }
    );
}

#[test]
fn consumer_loss_permission_and_report_cross_the_composition_boundary() {
    let loss = ConversionLoss {
        path: "/messages/0/content/0".into(),
        message: "unknown content omitted".into(),
    };
    let bridge = TestBridge {
        losses: vec![loss.clone()],
        ..TestBridge::default()
    };
    let pipeline = Pipeline::new(Arc::new(bridge), Arc::new(TestAdapter::default()))
        .expect("compatible formats");
    let native = NativeFields::new();
    assert!(matches!(
        pipeline.build_request(&request(), &native, false, &ConversionOptions::default()),
        Err(PipelineError::Conversion(ConversionError {
            kind: ConversionErrorKind::Unrepresentable,
            ..
        }))
    ));
    let options = ConversionOptions {
        allow_loss: [LossScope::UnknownContent].into(),
    };
    let (_, losses) = pipeline
        .build_request(&request(), &native, false, &options)
        .expect("explicit loss permitted");
    assert_eq!(losses, vec![loss]);
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
