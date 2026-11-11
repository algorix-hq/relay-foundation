use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use relay_adapter::{
    Adapter, AdapterExt, Conversion, EncodedError, ErrorEncoder, FrameDecoder, FrameItem, Headers,
    Method, ModelDiscovery, ModelPage, Outcome, ProviderRequest, Stage,
};
use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, Encoded, Frame, Input, LossScope,
    NativeFields, Output, StreamDecoder, StreamEncoder,
};
use relay_protocol::{
    AssistantContent, ConfigurationError, ConversionError, ConversionErrorKind, ConversionLoss,
    DecodeError, IncompleteStream, Message, ProviderError, ProviderErrorCategory, RelayError,
    Request, Response, StopReason, StreamEvent, Version,
};
use serde_json::{Value, json};

struct TestBridge {
    format: ApiFormat,
    decoded_responses: AtomicUsize,
    fail_encoder_finish: bool,
}

impl TestBridge {
    fn client() -> Self {
        Self {
            format: ApiFormat::new("client-wire").expect("nonempty format"),
            decoded_responses: AtomicUsize::new(0),
            fail_encoder_finish: false,
        }
    }

    fn provider() -> Self {
        Self {
            format: ApiFormat::new("provider-wire").expect("nonempty format"),
            ..Self::client()
        }
    }
}

impl Bridge for TestBridge {
    fn format(&self) -> &ApiFormat {
        &self.format
    }

    fn decode_request(&self, body: &[u8]) -> Result<Input, BridgeError> {
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
    ) -> Result<Encoded, BridgeError> {
        let mut request = input.request.clone();
        let mut losses = Vec::new();
        for (message_index, message) in request.messages.iter_mut().enumerate() {
            if let Message::Assistant { content } = message {
                let mut retained = Vec::new();
                for (index, part) in content.drain(..).enumerate() {
                    if matches!(part, AssistantContent::Unknown { .. }) {
                        if !options.allow_loss.contains(&LossScope::UnknownContent) {
                            return Err(unrepresentable("unknown content").into());
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
            body.extend(native.clone());
        }
        Ok(Encoded {
            body: Value::Object(body),
            losses,
        })
    }

    fn decode_response(&self, body: &[u8]) -> Result<Output, BridgeError> {
        self.decoded_responses.fetch_add(1, Ordering::Relaxed);
        let mut native: NativeFields = serde_json::from_slice(body).map_err(decode_error)?;
        let response = serde_json::from_value(native.remove("response").unwrap_or(Value::Null))
            .map_err(decode_error)?;
        Ok(Output::with_native(self.format.clone(), response, native))
    }

    fn encode_response(
        &self,
        output: &Output,
        _options: &ConversionOptions,
    ) -> Result<Encoded, BridgeError> {
        let mut body = NativeFields::from_iter([("response".into(), json!(output.response))]);
        if let Some(native) = output.native_for(self.format()) {
            body.extend(native.clone());
        }
        Ok(Encoded {
            body: Value::Object(body),
            losses: Vec::new(),
        })
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(TestStreamDecoder { stopped: false })
    }

    fn stream_encoder(&self, options: &ConversionOptions) -> Box<dyn StreamEncoder> {
        Box::new(TestStreamEncoder {
            allow_unknown: options.allow_loss.contains(&LossScope::UnknownContent),
            fail_finish: self.fail_encoder_finish,
        })
    }
}

struct TestStreamDecoder {
    stopped: bool,
}

impl StreamDecoder for TestStreamDecoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        let values: Vec<Value> = serde_json::from_str(&frame.data).map_err(decode_error)?;
        for value in values {
            let event = serde_json::from_value(value).map_err(decode_error)?;
            self.stopped |= matches!(event, StreamEvent::Stop { .. });
            out.push(event);
        }
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(IncompleteStream {
                message: "stream ended before stop".into(),
            }
            .into());
        }
        Ok(())
    }
}

struct TestStreamEncoder {
    allow_unknown: bool,
    fail_finish: bool,
}

impl StreamEncoder for TestStreamEncoder {
    fn push(
        &mut self,
        event: &StreamEvent,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if matches!(
            event,
            StreamEvent::ItemStart {
                item: relay_protocol::StreamItem::Unknown,
                ..
            }
        ) {
            if !self.allow_unknown {
                out.push(Frame {
                    event: Some("partial".into()),
                    data: "before failure".into(),
                });
                return Err(unrepresentable("unknown stream item").into());
            }
            losses.push(ConversionLoss {
                path: "/items/0".into(),
                message: "unknown item omitted".into(),
            });
            return Ok(());
        }
        out.push(Frame {
            event: None,
            data: serde_json::to_string(event).map_err(invalid)?,
        });
        Ok(())
    }

    fn finish(
        &mut self,
        out: &mut Vec<Frame>,
        _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        out.push(Frame {
            event: Some("tail".into()),
            data: "encoder tail".into(),
        });
        if self.fail_finish {
            return Err(unrepresentable("unrepresentable encoder tail").into());
        }
        Ok(())
    }
}

struct TestAdapter {
    conversion: Conversion,
    discovery: bool,
    lenient: bool,
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
            lenient: false,
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
    ) -> Result<ProviderRequest, ConfigurationError> {
        Ok(ProviderRequest {
            method: Method::Post,
            url: format!(
                "https://test.invalid/{}/{model}",
                self.conversion.bridge().format()
            ),
            headers: Headers(vec![("Authorization".into(), "test-token".into())]),
            body: serde_json::to_vec(&json!({"wire": body, "stream": stream})).map_err(
                |error| ConfigurationError {
                    message: error.to_string(),
                },
            )?,
        })
    }

    fn check_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError> {
        if !self.lenient && (!(200..300).contains(&status) || body == b"provider-failure") {
            return Err(ProviderError {
                http_status: Some(status),
                request_id: headers.get("request-id").map(str::to_owned),
                ..provider_failure()
            });
        }
        Ok(())
    }

    fn frame_decoder(&self, headers: &Headers) -> Box<dyn FrameDecoder> {
        Box::new(TestFrameDecoder {
            pending: Vec::new(),
            request_id: headers.get("request-id").map(str::to_owned),
        })
    }

    fn model_discovery(&self) -> Option<&dyn ModelDiscovery> {
        if self.discovery { Some(self) } else { None }
    }
}

impl ModelDiscovery for TestAdapter {
    fn build_list_models(
        &self,
        cursor: Option<&str>,
    ) -> Result<ProviderRequest, ConfigurationError> {
        Ok(ProviderRequest {
            method: Method::Get,
            url: format!("https://test.invalid/models/{}", cursor.unwrap_or_default()),
            headers: Headers::default(),
            body: Vec::new(),
        })
    }

    fn decode_models(&self, body: &[u8]) -> Result<ModelPage, DecodeError> {
        let (model_ids, next_cursor) = serde_json::from_slice(body).map_err(decode_error)?;
        Ok(ModelPage {
            model_ids,
            next_cursor,
        })
    }
}

struct TestFrameDecoder {
    pending: Vec<u8>,
    request_id: Option<String>,
}

impl TestFrameDecoder {
    fn frame(&self, line: Vec<u8>) -> Result<FrameItem, DecodeError> {
        let data = String::from_utf8(line).map_err(decode_error)?;
        if data == "{\"type\":\"failure\",\"code\":\"busy\"}" {
            return Ok(FrameItem::Error(ProviderError {
                request_id: self.request_id.clone(),
                ..provider_failure()
            }));
        }
        Ok(FrameItem::Data(Frame { event: None, data }))
    }
}

impl FrameDecoder for TestFrameDecoder {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=end).collect();
            line.pop();
            out.push(self.frame(line)?);
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            out.push(self.frame(line)?);
        }
        Ok(())
    }
}

struct TestErrorEncoder;

impl ErrorEncoder for TestErrorEncoder {
    fn encode_error(&self, error: &RelayError, stage: Stage) -> EncodedError {
        EncodedError {
            status: if stage == Stage::Request { 400 } else { 502 },
            headers: Headers::default(),
            body: error.to_string().into_bytes(),
        }
    }

    fn encode_stream_error(&self, error: &RelayError) -> Vec<Frame> {
        vec![Frame {
            event: Some("relay-error".into()),
            data: error.to_string(),
        }]
    }
}

fn invalid(error: impl std::fmt::Display) -> ConversionError {
    ConversionError {
        kind: ConversionErrorKind::Invalid,
        path: String::new(),
        message: error.to_string(),
    }
}

fn unrepresentable(message: &str) -> ConversionError {
    ConversionError {
        kind: ConversionErrorKind::Unrepresentable,
        path: String::new(),
        message: message.into(),
    }
}

fn decode_error(error: impl std::fmt::Display) -> DecodeError {
    DecodeError {
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
        messages: Vec::new(),
        tools: None,
        parameters: None,
        response_format: None,
        reasoning: None,
    }
}

const START: &[u8] = b"[{\"type\":\"response_start\"}]\n";
const STOP: &[u8] = b"[{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}]\n";
const FAILURE: &[u8] = b"{\"type\":\"failure\",\"code\":\"busy\"}\n";
const UNKNOWN: &[u8] = b"[{\"type\":\"item_start\",\"index\":0,\"item\":{\"type\":\"unknown\"}}]\n";

#[test]
fn gateway_converts_between_client_and_provider_formats() {
    let client: Box<dyn Bridge> = Box::new(TestBridge::client());
    let adapter: Box<dyn Adapter> = Box::new(TestAdapter::default());
    let body =
        serde_json::to_vec(&json!({"request": request(), "stream": false})).expect("client body");
    let input = client.decode_request(&body).expect("client ingress");
    let (req, losses) = adapter.build_request(&input).expect("provider request");
    assert_eq!(req.method, Method::Post);
    assert_eq!(req.url, "https://test.invalid/provider-wire/test-model");
    assert_eq!(req.headers.get("authorization"), Some("test-token"));
    assert_eq!(
        serde_json::from_slice::<Value>(&req.body).expect("provider body"),
        json!({"wire": {"request": request(), "stream": false}, "stream": false})
    );
    assert!(losses.is_empty());

    let response = Response {
        version: Version::V1,
        id: Some("response-1".into()),
        model: "test-model".into(),
        content: vec![AssistantContent::Text(relay_protocol::TextPart {
            text: "world".into(),
            cache_control: None,
        })],
        stop_reason: StopReason::EndTurn,
        usage: None,
    };
    let response_body = serde_json::to_vec(&json!({"response": response, "provider-tier": "test"}))
        .expect("fake transport");
    let output = adapter
        .parse_response(200, &Headers::default(), &response_body)
        .expect("provider response");
    let encoded = client
        .encode_response(&output, &ConversionOptions::default())
        .expect("client response");
    assert_eq!(encoded.body, json!({"response": response}));
    assert_eq!(
        adapter
            .conversion()
            .bridge()
            .encode_response(&output, adapter.conversion().options())
            .expect("same format")
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
            .expect("ingress");
        let (req, _) = adapter.build_request(&input).expect("request");
        let body: Value = serde_json::from_slice(&req.body).expect("provider body");
        assert_eq!(
            body["wire"].get("native-option"),
            forwarded.then_some(&json!(7))
        );
    }
}

#[test]
fn incompatible_bridge_is_a_configuration_failure_naming_both_formats() {
    let error: RelayError = Conversion::new(
        Arc::new(TestBridge::client()),
        &[ApiFormat::MESSAGES],
        ConversionOptions::default(),
    )
    .err()
    .expect("incompatible")
    .into();
    let RelayError::Configuration(error) = error else {
        panic!("expected configuration error")
    };
    assert!(error.message.contains("client-wire"));
    assert!(error.message.contains("messages"));
    assert!(ApiFormat::new("").is_err());
}

#[test]
fn provider_rejection_bypasses_response_decoding_even_on_success_status() {
    let bridge = Arc::new(TestBridge::provider());
    let adapter = TestAdapter::new(bridge.clone(), ConversionOptions::default());
    let headers = Headers(vec![
        ("Request-ID".into(), "request-1".into()),
        ("Content-Type".into(), "text/plain".into()),
    ]);
    for (status, body) in [(503, b"not-json".as_slice()), (200, b"provider-failure")] {
        assert_eq!(
            adapter.parse_response(status, &headers, body),
            Err(RelayError::Provider(ProviderError {
                http_status: Some(status),
                request_id: Some("request-1".into()),
                ..provider_failure()
            }))
        );
    }
    assert_eq!(bridge.decoded_responses.load(Ordering::Relaxed), 0);
}

#[test]
fn confirmed_http_failure_never_becomes_a_decode_failure() {
    let adapter = TestAdapter {
        lenient: true,
        discovery: true,
        ..TestAdapter::default()
    };
    for error in [
        adapter
            .parse_response(502, &Headers::default(), b"")
            .expect_err("HTTP failure"),
        adapter
            .parse_models(502, &Headers::default(), b"")
            .expect_err("discovery HTTP failure"),
    ] {
        let RelayError::Provider(error) = error else {
            panic!("expected provider failure")
        };
        assert_eq!(error.category, ProviderErrorCategory::Unknown);
        assert_eq!(error.http_status, Some(502));
        assert_eq!(
            (error.code, error.request_id, error.retry_after),
            (None, None, None)
        );
    }
}

#[test]
fn malformed_success_response_is_a_decode_failure() {
    assert!(matches!(
        TestAdapter::default().parse_response(200, &Headers::default(), b"not-json"),
        Err(RelayError::Decode(_))
    ));
}

#[test]
fn rejected_stream_is_classified_after_its_body_is_read() {
    for lenient in [false, true] {
        let adapter = TestAdapter {
            lenient,
            ..TestAdapter::default()
        };
        let rejected = match adapter.open_stream(429, &Headers::default()) {
            Ok(_) => panic!("rejected status opened a stream"),
            Err(rejected) => rejected,
        };
        let RelayError::Provider(error) = rejected.classify(b"") else {
            panic!("expected provider error")
        };
        assert_eq!(error.http_status, Some(429));
        assert_eq!(
            error.category,
            if lenient {
                ProviderErrorCategory::Unknown
            } else {
                ProviderErrorCategory::Overloaded
            }
        );
    }
}

#[test]
fn mid_stream_provider_failure_follows_data_and_keeps_header_request_id() {
    let adapter = TestAdapter::default();
    let mut stream = adapter
        .open_stream(
            200,
            &Headers(vec![("Request-ID".into(), "stream-request".into())]),
        )
        .expect("stream");
    assert!(stream.push(b"[{\"type\":\"response_").next().is_none());
    let bytes = [b"start\"}]\n".as_slice(), FAILURE, STOP].concat();
    assert_eq!(
        stream.push(&bytes).collect::<Vec<_>>(),
        vec![
            Ok(StreamEvent::ResponseStart {
                id: None,
                model: None
            }),
            Err(RelayError::Provider(ProviderError {
                request_id: Some("stream-request".into()),
                ..provider_failure()
            })),
        ]
    );
    assert!(stream.push(START).next().is_none());
    assert!(stream.finish().next().is_none());
}

#[test]
fn bridge_decode_failure_preserves_events_from_the_same_frame() {
    let adapter = TestAdapter::default();
    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    let items = stream
        .push(b"[{\"type\":\"response_start\"},{\"type\":\"invalid\"}]\n")
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        Ok(StreamEvent::ResponseStart {
            id: None,
            model: None
        })
    );
    assert!(matches!(items[1], Err(RelayError::Decode(_))));
    assert!(stream.push(STOP).next().is_none());
    assert!(stream.finish().next().is_none());
}

#[test]
fn malformed_framing_preserves_preceding_frames() {
    let adapter = TestAdapter::default();
    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    let items = stream
        .push(&[START, b"\xff\n"].concat())
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        Ok(StreamEvent::ResponseStart {
            id: None,
            model: None
        })
    );
    assert!(matches!(items[1], Err(RelayError::Decode(_))));
}

#[test]
fn eof_flushes_buffered_events_then_reports_incomplete_stream_without_a_stop() {
    let adapter = TestAdapter::default();
    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    assert!(stream.push(&START[..START.len() - 1]).next().is_none());
    let items = stream.finish().collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        Ok(StreamEvent::ResponseStart {
            id: None,
            model: None
        })
    );
    assert!(matches!(items[1], Err(RelayError::IncompleteStream(_))));
    assert!(stream.push(STOP).next().is_none());
    assert!(stream.finish().next().is_none());
}

#[test]
fn transcoder_emits_prior_frames_then_one_provider_error_and_stops() {
    let adapter = TestAdapter::default();
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &TestBridge::client(),
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
                ..Default::default()
            },
            Arc::new(TestErrorEncoder),
        );
    let frames = transcoder.push(&[START, UNKNOWN, FAILURE, STOP].concat());
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].data, "{\"type\":\"response_start\"}");
    assert_eq!(frames[1].event.as_deref(), Some("relay-error"));
    assert!(frames[1].data.contains("provider is busy"));
    assert_eq!(
        transcoder.report().losses,
        vec![ConversionLoss {
            path: "/items/0".into(),
            message: "unknown item omitted".into(),
        }]
    );
    assert_eq!(
        transcoder.report().outcome,
        Some(Outcome::Failed(RelayError::Provider(provider_failure())))
    );
    assert!(transcoder.push(START).is_empty());
    assert!(transcoder.finish().is_empty());
}

#[test]
fn transcoder_encodes_client_push_failure_after_partial_encoder_output() {
    let adapter = TestAdapter::default();
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &TestBridge::client(),
            &ConversionOptions::default(),
            Arc::new(TestErrorEncoder),
        );
    let frames = transcoder.push(&[START, UNKNOWN, STOP].concat());
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.event.as_deref())
            .collect::<Vec<_>>(),
        vec![None, Some("partial"), Some("relay-error")]
    );
    assert!(matches!(
        transcoder.report().outcome,
        Some(Outcome::Failed(RelayError::Conversion(_)))
    ));
    assert!(transcoder.push(START).is_empty());
    assert!(transcoder.finish().is_empty());
}

#[test]
fn transcoder_encodes_client_finish_failure_after_encoder_tail() {
    let adapter = TestAdapter::default();
    let client = TestBridge {
        fail_encoder_finish: true,
        ..TestBridge::client()
    };
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &client,
            &ConversionOptions::default(),
            Arc::new(TestErrorEncoder),
        );
    assert_eq!(transcoder.push(STOP).len(), 1);
    let frames = transcoder.finish();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("tail"), Some("relay-error")]
    );
    assert!(matches!(
        transcoder.report().outcome,
        Some(Outcome::Failed(RelayError::Conversion(_)))
    ));
    assert!(transcoder.finish().is_empty());
    assert!(transcoder.push(START).is_empty());
}

#[test]
fn successful_transcode_flushes_stop_reports_losses_and_completes() {
    let adapter = TestAdapter::default();
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &TestBridge::client(),
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
                ..Default::default()
            },
            Arc::new(TestErrorEncoder),
        );
    assert!(transcoder.push(UNKNOWN).is_empty());
    assert!(transcoder.push(&STOP[..STOP.len() - 1]).is_empty());
    let frames = transcoder.finish();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0].data,
        "{\"type\":\"stop\",\"stop_reason\":\"end_turn\"}"
    );
    assert_eq!(frames[1].event.as_deref(), Some("tail"));
    assert_eq!(
        transcoder.report().losses,
        vec![ConversionLoss {
            path: "/items/0".into(),
            message: "unknown item omitted".into()
        }]
    );
    assert_eq!(transcoder.report().outcome, Some(Outcome::Completed));
    assert!(transcoder.push(START).is_empty());
    assert!(transcoder.finish().is_empty());
}

#[test]
fn abandoning_a_transcode_does_not_report_incomplete_or_completed() {
    let adapter = TestAdapter::default();
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &TestBridge::client(),
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
                ..Default::default()
            },
            Arc::new(TestErrorEncoder),
        );
    assert_eq!(transcoder.push(START).len(), 1);
    assert!(transcoder.push(UNKNOWN).is_empty());
    assert_eq!(
        transcoder.report().losses,
        vec![ConversionLoss {
            path: "/items/0".into(),
            message: "unknown item omitted".into(),
        }]
    );
    assert_eq!(transcoder.report().outcome, None);
    drop(transcoder);
}

#[test]
fn discovery_distinguishes_unsupported_empty_pages_and_provider_errors() {
    let unsupported = TestAdapter::default();
    for error in [
        unsupported.list_models(None).expect_err("unsupported"),
        unsupported
            .parse_models(200, &Headers::default(), b"[]")
            .expect_err("unsupported"),
    ] {
        let RelayError::UnsupportedOperation(error) = error else {
            panic!("expected unsupported")
        };
        assert_eq!(error.operation, "list_models");
    }
    let adapter = TestAdapter {
        discovery: true,
        ..TestAdapter::default()
    };
    let req = adapter
        .list_models(Some("page-2"))
        .expect("discovery request");
    assert_eq!(req.method, Method::Get);
    assert_eq!(req.url, "https://test.invalid/models/page-2");
    assert!(req.body.is_empty());
    assert_eq!(
        adapter.parse_models(200, &Headers::default(), b"[[],\"page-3\"]"),
        Ok(ModelPage {
            model_ids: Vec::new(),
            next_cursor: Some("page-3".into())
        })
    );
    for (status, body) in [(429, b"not-json".as_slice()), (200, b"provider-failure")] {
        assert_eq!(
            adapter.parse_models(status, &Headers::default(), body),
            Err(RelayError::Provider(ProviderError {
                http_status: Some(status),
                ..provider_failure()
            }))
        );
    }
}

#[test]
fn build_request_requires_loss_permission_and_returns_encode_losses() {
    let mut request = request();
    request.messages.push(Message::Assistant {
        content: vec![AssistantContent::Unknown {
            value: json!({"future": 1}),
        }],
    });
    let input = Input::new(request);
    assert!(matches!(
        TestAdapter::default().build_request(&input),
        Err(RelayError::Conversion(ConversionError {
            kind: ConversionErrorKind::Unrepresentable,
            ..
        }))
    ));
    let adapter = TestAdapter::new(
        Arc::new(TestBridge::provider()),
        ConversionOptions {
            allow_loss: [LossScope::UnknownContent].into(),
            ..Default::default()
        },
    );
    let (req, losses) = adapter.build_request(&input).expect("explicit omission");
    assert_eq!(
        losses,
        vec![ConversionLoss {
            path: "/messages/0/content/0".into(),
            message: "unknown content omitted".into()
        }]
    );
    let body: Value = serde_json::from_slice(&req.body).expect("provider body");
    assert_eq!(body["wire"]["request"]["messages"][0]["content"], json!([]));
}
