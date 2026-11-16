use std::{fs, path::Path, sync::Arc};

use relay_adapter::{
    Adapter, AdapterExt, Conversion, EncodedError, ErrorEncoder, FrameDecoder, FrameItem, Headers,
    Method, Outcome, ProviderRequest, Stage,
};
use relay_bridge::{
    ApiFormat, Bridge, ChatCompletionsBridge, ConversionOptions, DecodeContext, Frame, LossScope,
    MessagesBridge, ResponsesBridge,
};
use relay_protocol::{
    AssistantContent, ConfigurationError, ConversionErrorKind, DecodeError, Delivery, Error,
    Message, ProviderError, ProviderErrorCategory, StreamEvent,
};
use serde_json::{Value, json};

const FALLBACK_ID: &str = "relay-composition";
const OBSERVED_AT: u64 = 1_700_000_123;

struct CustomAdapter {
    conversion: Conversion,
}

impl CustomAdapter {
    fn new(bridge: Arc<dyn Bridge>, options: ConversionOptions) -> Self {
        let formats = [bridge.format().clone()];
        Self::with_formats(bridge, &formats, options).expect("compatible bridge")
    }

    fn with_formats(
        bridge: Arc<dyn Bridge>,
        formats: &[ApiFormat],
        options: ConversionOptions,
    ) -> Result<Self, ConfigurationError> {
        Ok(Self {
            conversion: Conversion::new(bridge, formats, options)?
                .with_id_generator(|| FALLBACK_ID.into())
                .with_clock(|| OBSERVED_AT),
        })
    }
}

impl Adapter for CustomAdapter {
    fn conversion(&self) -> &Conversion {
        &self.conversion
    }

    fn assemble_request(
        &self,
        _model: &str,
        body: Value,
        _stream: bool,
    ) -> Result<ProviderRequest, ConfigurationError> {
        Ok(ProviderRequest {
            method: Method::Post,
            url: "https://custom.invalid/generate".into(),
            headers: Headers(vec![("x-custom-token".into(), "fixture-token".into())]),
            body: serde_json::to_vec(&body).map_err(|error| ConfigurationError {
                message: error.to_string(),
            })?,
        })
    }

    fn check_response(
        &self,
        status: u16,
        _headers: &Headers,
        body: &[u8],
    ) -> Result<(), ProviderError> {
        if !(200..300).contains(&status) {
            let payload = serde_json::from_slice(body).unwrap_or(Value::Null);
            return Err(provider_error(&payload, Some(status)));
        }
        Ok(())
    }

    fn frame_decoder(&self, _headers: &Headers) -> Box<dyn FrameDecoder> {
        Box::new(SseDecoder::default())
    }
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    fn line(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        let line = std::str::from_utf8(bytes).map_err(decode_error)?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.into()),
            "data" => self.data.push(value.into()),
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<FrameItem>) {
        let event = self.event.take();
        if self.data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let payload: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        if event.as_deref() == Some("error") || payload.get("error").is_some() {
            out.push(FrameItem::Error(provider_error(&payload, None)));
        } else {
            out.push(FrameItem::Data(Frame { event, data }));
        }
    }
}

impl FrameDecoder for SseDecoder {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            self.line(&line[..end], out)?;
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<FrameItem>) -> Result<(), DecodeError> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.line(&line, out)?;
        }
        self.dispatch(out);
        Ok(())
    }
}

fn decode_error(error: impl std::fmt::Display) -> DecodeError {
    DecodeError {
        path: String::new(),
        message: error.to_string(),
    }
}

fn provider_error(payload: &Value, http_status: Option<u16>) -> ProviderError {
    ProviderError {
        category: ProviderErrorCategory::Unknown,
        message: "custom provider failed".into(),
        code: payload["error"]["type"].as_str().map(str::to_owned),
        http_status,
        request_id: None,
        retry_after: None,
    }
}

struct TestErrors;

impl ErrorEncoder for TestErrors {
    fn encode_error(&self, error: &Error, stage: Stage) -> EncodedError {
        EncodedError {
            status: if stage == Stage::Request { 400 } else { 502 },
            headers: Headers::default(),
            body: serde_json::to_vec(error).expect("error JSON"),
        }
    }

    fn encode_stream_error(&self, error: &Error) -> Vec<Frame> {
        vec![Frame {
            event: Some("relay-error".into()),
            data: serde_json::to_string(error).expect("error JSON"),
        }]
    }
}

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../relay-bridge/tests/fixtures")
        .join(name);
    serde_json::from_slice(&fs::read(path).expect("bridge fixture")).expect("fixture JSON")
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON")
}

fn frame(value: Value) -> Frame {
    Frame {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn messages_frames() -> Vec<Frame> {
    [
        "00-start",
        "01-text-start",
        "02-text-delta",
        "03-text-stop",
        "04-thinking-start",
        "05-thinking-delta",
        "06-signature-delta",
        "07-thinking-stop",
        "08-redacted-start",
        "09-redacted-stop",
        "10-tool-start",
        "11-tool-delta",
        "12-tool-delta",
        "13-tool-stop",
        "14-message-delta",
        "15-stop",
    ]
    .map(|name| frame(fixture(&format!("messages/stream/{name}.json"))))
    .into()
}

fn sse(frames: &[Frame]) -> Vec<u8> {
    let mut wire = String::from(": keepalive\r\n\r\n");
    for frame in frames {
        if let Some(event) = &frame.event {
            wire.push_str(&format!("event: {event}\r\n"));
        }
        for line in frame.data.lines() {
            wire.push_str(&format!("data: {line}\r\n"));
        }
        wire.push_str(": comment inside frame\r\n\r\n");
    }
    wire.into_bytes()
}

fn direct_events(bridge: &dyn Bridge, frames: &[Frame], finish: bool) -> Vec<StreamEvent> {
    let mut decoder = bridge.stream_decoder(&DecodeContext {
        response_id: Some(FALLBACK_ID.into()),
        observed_at: OBSERVED_AT,
    });
    let mut events = Vec::new();
    for frame in frames {
        decoder
            .push(frame.clone(), &mut events)
            .expect("bridge frame");
    }
    if finish {
        decoder.finish(&mut events).expect("bridge EOF");
    }
    events
}

#[test]
fn custom_adapter_preserves_each_builtin_bridges_requests_responses_and_identity() {
    let cases: [(Arc<dyn Bridge>, &str, &str); 3] = [
        (
            Arc::new(ChatCompletionsBridge::new()),
            "chat_completions/request_tokens.json",
            "chat_completions/response.json",
        ),
        (
            Arc::new(MessagesBridge::new()),
            "messages/request-minimal.json",
            "messages/response-full.json",
        ),
        (
            Arc::new(ResponsesBridge::new()),
            "responses/request-empty-replay.json",
            "responses/response-full.json",
        ),
    ];
    for (bridge, request_fixture, response_fixture) in cases {
        let adapter = CustomAdapter::new(bridge.clone(), ConversionOptions::default());
        let input = bridge
            .decode_request(&bytes(&fixture(request_fixture)))
            .expect("client request");
        let encoded = bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("bridge request");
        let (request, losses) = adapter.build_request(&input).expect("adapter request");
        assert_eq!(request.body, bytes(&encoded.body), "{request_fixture}");
        assert_eq!(losses, encoded.losses);
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.url, "https://custom.invalid/generate");
        assert_eq!(request.headers.get("x-custom-token"), Some("fixture-token"));

        let response = fixture(response_fixture);
        for provider_id in [response["id"].as_str().map(str::to_owned), None] {
            let mut response = response.clone();
            if provider_id.is_none() {
                response
                    .as_object_mut()
                    .expect("response object")
                    .remove("id");
            }
            let body = bytes(&response);
            let actual = adapter
                .parse_response(200, &Headers::default(), &body)
                .expect("adapter response");
            let expected = bridge
                .decode_response(
                    &body,
                    &DecodeContext {
                        response_id: Some(FALLBACK_ID.into()),
                        observed_at: OBSERVED_AT,
                    },
                )
                .expect("bridge response");
            assert_eq!(actual, expected, "{response_fixture}");
            assert_eq!(actual.id, provider_id.as_deref().unwrap_or(FALLBACK_ID));
            assert_eq!(actual.provider_id, provider_id);
            assert_eq!(actual.created_at.observed_at, OBSERVED_AT);
        }
        let error = adapter
            .parse_response(503, &Headers::default(), br#"{"error":{"type":"x"}}"#)
            .expect_err("provider rejection");
        assert_eq!(
            error,
            Error::Provider(ProviderError {
                code: Some("x".into()),
                http_status: Some(503),
                ..provider_error(&Value::Null, None)
            })
        );
    }
}

#[test]
fn fragmented_sse_matches_each_builtin_bridges_direct_stream() {
    let mut chat_frames: Vec<_> = (0..6)
        .map(|index| frame(fixture(&format!("chat_completions/chunk_{index:02}.json"))))
        .collect();
    chat_frames.push(Frame {
        event: None,
        data: "[DONE]".into(),
    });
    let cases: [(Arc<dyn Bridge>, Vec<Frame>); 3] = [
        (Arc::new(ChatCompletionsBridge::new()), chat_frames),
        (Arc::new(MessagesBridge::new()), messages_frames()),
        (
            Arc::new(ResponsesBridge::new()),
            (0..27)
                .map(|index| frame(fixture(&format!("responses/stream-{index:02}.json"))))
                .collect(),
        ),
    ];
    for (bridge, mut frames) in cases {
        let first: Value = serde_json::from_str(&frames[0].data).expect("first event JSON");
        frames[0].data = serde_json::to_string_pretty(&first).expect("multiline SSE data");
        let expected = direct_events(bridge.as_ref(), &frames, true);
        let adapter = CustomAdapter::new(bridge.clone(), ConversionOptions::default());
        let mut stream = adapter
            .open_stream(200, &Headers::default())
            .expect("stream");
        let mut actual = Vec::new();
        for chunk in sse(&frames).chunks(17) {
            actual.extend(stream.push(chunk));
        }
        actual.extend(stream.finish());
        assert_eq!(
            actual,
            expected.into_iter().map(Ok).collect::<Vec<_>>(),
            "{}",
            bridge.format()
        );
    }
}

#[test]
fn provider_error_frames_bypass_the_bridge_and_terminate_after_prior_events() {
    let bridge = Arc::new(MessagesBridge::new());
    let frames = messages_frames();
    let prefix = &frames[..3];
    let expected = direct_events(bridge.as_ref(), prefix, false);
    for error in [
        Frame {
            event: None,
            data: r#"{"error":{"type":"x"}}"#.into(),
        },
        Frame {
            event: Some("error".into()),
            data: "{}".into(),
        },
    ] {
        let adapter = CustomAdapter::new(bridge.clone(), ConversionOptions::default());
        let mut stream = adapter
            .open_stream(200, &Headers::default())
            .expect("stream");
        let payload: Value = serde_json::from_str(&error.data).expect("error JSON");
        let failure = Error::Provider(provider_error(&payload, None));
        let wire = sse(&[prefix.to_vec(), vec![error.clone()], frames[3..].to_vec()].concat());
        let mut actual = Vec::new();
        for chunk in wire.chunks(19) {
            actual.extend(stream.push(chunk));
        }
        let mut expected_items: Vec<_> = expected.iter().cloned().map(Ok).collect();
        expected_items.push(Err(failure));
        assert_eq!(actual, expected_items);
        assert!(stream.push(&sse(&frames)).next().is_none());
        assert!(stream.finish().next().is_none());

        let mut decoder = bridge.stream_decoder(&DecodeContext {
            response_id: Some(FALLBACK_ID.into()),
            observed_at: OBSERVED_AT,
        });
        let mut events = Vec::new();
        for frame in prefix {
            decoder.push(frame.clone(), &mut events).expect("prefix");
        }
        assert!(decoder.push(error, &mut events).is_err());
        assert_eq!(events, expected);
    }
}

#[test]
fn chat_client_receives_messages_response_with_relay_identity_and_observed_time() {
    let chat = ChatCompletionsBridge::new();
    let input = chat
        .decode_request(&bytes(&fixture("chat_completions/request_loss.json")))
        .expect("Chat Completions client request");
    let adapter = CustomAdapter::new(
        Arc::new(MessagesBridge::with_default_max_tokens(32)),
        ConversionOptions::default(),
    );
    let (provider_request, losses) = adapter.build_request(&input).expect("Messages request");
    assert!(losses.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&provider_request.body).expect("body"),
        json!({
            "model":"test-model", "max_tokens":32,
            "messages":[{"role":"assistant","content":[{"type":"text","text":"ok"}]}]
        })
    );
    let response = adapter
        .parse_response(
            200,
            &Headers::default(),
            &bytes(&fixture("messages/response-encoded.json")),
        )
        .expect("Messages response");
    let encoded = chat
        .encode_response(
            &input,
            &response,
            &ConversionOptions {
                allow_loss: [LossScope::Reasoning].into(),
            },
        )
        .expect("Chat Completions response");
    assert_eq!(encoded.body["id"], "msg_full");
    assert_eq!(encoded.body["id"], response.id);
    assert_eq!(encoded.body["created"], OBSERVED_AT);
    assert_eq!(response.created_at.provider_created_at, None);
    assert_eq!(
        encoded
            .losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        ["/content/2", "/content/3"]
    );
    let expected: Value = serde_json::from_str(include_str!("fixtures/composition/response.json"))
        .expect("SDK-checked ChatCompletion");
    assert_eq!(encoded.body, expected);
}

#[test]
fn messages_gateway_stream_emits_chat_chunks_and_done() {
    let chat = ChatCompletionsBridge::new();
    let mut input = chat
        .decode_request(&bytes(&fixture("chat_completions/request_tokens.json")))
        .expect("client request");
    input.delivery = Some(Delivery {
        stream: true,
        stream_usage: true,
    });
    let adapter = CustomAdapter::new(
        Arc::new(MessagesBridge::with_default_max_tokens(32)),
        ConversionOptions::default(),
    );
    let (request, losses) = adapter.build_request(&input).expect("stream request");
    assert!(losses.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).expect("body")["stream"],
        true
    );
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &chat,
            &input,
            &ConversionOptions {
                allow_loss: [LossScope::Reasoning].into(),
            },
            Arc::new(TestErrors),
        );
    let mut frames = Vec::new();
    for chunk in sse(&messages_frames()).chunks(23) {
        frames.extend(transcoder.push(chunk));
    }
    frames.extend(transcoder.finish());
    assert_eq!(transcoder.report().outcome, Some(Outcome::Completed));
    assert_eq!(frames.last().expect("terminal frame").data, "[DONE]");
    let chunks: Vec<Value> = frames[..frames.len() - 1]
        .iter()
        .map(|frame| {
            assert_eq!(frame.event, None);
            serde_json::from_str(&frame.data).expect("ChatCompletionChunk")
        })
        .collect();
    let expected: Vec<Value> = (0..8)
        .map(|index| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/composition/chunk_{index:02}.json"));
            serde_json::from_slice(&fs::read(path).expect("SDK-checked chunk")).expect("chunk JSON")
        })
        .collect();
    assert_eq!(chunks, expected);
    assert_eq!(
        transcoder
            .report()
            .losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        ["/items/1", "/items/2"]
    );
    assert!(transcoder.finish().is_empty());
}

#[test]
fn messages_gateway_stream_encodes_one_provider_error_after_earlier_chat_chunks() {
    let chat = ChatCompletionsBridge::new();
    let input = chat
        .decode_request(&bytes(&fixture("chat_completions/request_tokens.json")))
        .expect("client request");
    let adapter = CustomAdapter::new(
        Arc::new(MessagesBridge::with_default_max_tokens(32)),
        ConversionOptions::default(),
    );
    let mut transcoder = adapter
        .open_stream(200, &Headers::default())
        .expect("stream")
        .transcode(
            &chat,
            &input,
            &ConversionOptions::default(),
            Arc::new(TestErrors),
        );
    let frames = messages_frames();
    let failure = frame(json!({"type":"error","error":{"type":"x"}}));
    let wire = sse(&[frames[..3].to_vec(), vec![failure], frames[3..].to_vec()].concat());
    let actual = transcoder.push(&wire);
    let expected_error = Error::Provider(provider_error(&json!({"error":{"type":"x"}}), None));
    assert_eq!(
        transcoder.report().outcome,
        Some(Outcome::Failed(expected_error.clone()))
    );
    assert_eq!(actual.len(), 4);
    for (index, frame) in actual[..3].iter().enumerate() {
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/composition/chunk_{index:02}.json"));
        assert_eq!(
            serde_json::from_str::<Value>(&frame.data).expect("earlier chunk"),
            serde_json::from_slice::<Value>(&fs::read(expected).expect("SDK-checked chunk"))
                .expect("fixture JSON")
        );
    }
    assert_eq!(actual[3].event.as_deref(), Some("relay-error"));
    assert_eq!(
        serde_json::from_str::<Error>(&actual[3].data).expect("normalized error"),
        expected_error
    );
    assert!(transcoder.push(&sse(&frames)).is_empty());
    assert!(transcoder.finish().is_empty());
}

#[test]
fn provisional_usage_is_skipped_without_loss_and_never_becomes_final_usage() {
    let chat = ChatCompletionsBridge::new();
    let mut client = fixture("chat_completions/request_tokens.json");
    client["stream"] = json!(true);
    client["stream_options"] = json!({"include_usage": true});
    let input = chat
        .decode_request(&bytes(&client))
        .expect("client request");
    let adapter = CustomAdapter::new(
        Arc::new(MessagesBridge::with_default_max_tokens(32)),
        ConversionOptions::default(),
    );
    let mut end = fixture("messages/stream/14-message-delta.json");
    end.as_object_mut().expect("message delta").remove("usage");
    let frames = messages_frames();
    let without_final = sse(&[frames[..4].to_vec(), vec![frame(end), frames[15].clone()]].concat());
    for (wire, final_usage) in [(sse(&frames), true), (without_final, false)] {
        // Reasoning is allowed so only usage handling is under test.
        let options = ConversionOptions {
            allow_loss: [LossScope::Reasoning].into(),
        };
        let mut transcoder = adapter
            .open_stream(200, &Headers::default())
            .expect("stream")
            .transcode(&chat, &input, &options, Arc::new(TestErrors));
        let mut output = transcoder.push(&wire);
        output.extend(transcoder.finish());
        assert_eq!(transcoder.report().outcome, Some(Outcome::Completed));
        assert!(
            transcoder
                .report()
                .losses
                .iter()
                .all(|loss| loss.path.starts_with("/items/"))
        );
        assert_eq!(output.last().expect("done").data, "[DONE]");
        let usage_chunks = output[..output.len() - 1]
            .iter()
            .map(|frame| serde_json::from_str::<Value>(&frame.data).expect("chunk"))
            .filter(|chunk| chunk.get("usage").is_some_and(|usage| !usage.is_null()))
            .count();
        assert_eq!(usage_chunks, usize::from(final_usage));
    }
}

#[test]
fn messages_composition_requires_explicit_loss_for_foreign_reasoning_and_sampling() {
    let reasoning = ResponsesBridge::new().decode_request(&bytes(&json!({
        "model":"test-model", "input":[
            {"role":"user","content":"Hi"},
            {"type":"reasoning","id":"rs_foreign","summary":[],"encrypted_content":"foreign-opaque"}
        ]
    }))).expect("Responses request");
    assert!(
        matches!(&reasoning.messages[1], Message::Assistant { content } if matches!(&content[0], AssistantContent::Reasoning { provider_id: Some(id), .. } if id == "rs_foreign"))
    );
    let mut sampling = fixture("chat_completions/request_tokens.json");
    sampling["temperature"] = json!(0.5);
    let sampling = ChatCompletionsBridge::new()
        .decode_request(&bytes(&sampling))
        .expect("Chat Completions request");
    for (request, scope, path) in [
        (reasoning, LossScope::Reasoning, "/messages/1/content/0"),
        (sampling, LossScope::Sampling, "/parameters/temperature"),
    ] {
        let strict = CustomAdapter::new(
            Arc::new(MessagesBridge::with_default_max_tokens(32)),
            ConversionOptions::default(),
        );
        let error = strict
            .build_request(&request)
            .expect_err("strict cross-format conversion");
        let Error::Conversion(error) = error else {
            panic!("expected Conversion")
        };
        assert_eq!(error.kind, ConversionErrorKind::Unrepresentable);
        assert_eq!(error.path, path);
        let permitted = CustomAdapter::new(
            Arc::new(MessagesBridge::with_default_max_tokens(32)),
            ConversionOptions {
                allow_loss: [scope].into(),
            },
        );
        let (provider_request, losses) = permitted
            .build_request(&request)
            .expect("explicit omission");
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].path, path);
        let body: Value = serde_json::from_slice(&provider_request.body).expect("provider body");
        match scope {
            LossScope::Reasoning => assert_eq!(body["messages"][1]["content"], json!([])),
            LossScope::Sampling => assert!(body.get("temperature").is_none()),
            _ => unreachable!("table contains only reasoning and sampling"),
        }
    }
}

#[test]
fn messages_only_custom_adapter_rejects_chat_completions_composition() {
    let error: Error = CustomAdapter::with_formats(
        Arc::new(ChatCompletionsBridge::new()),
        &[ApiFormat::MESSAGES],
        ConversionOptions::default(),
    )
    .err()
    .expect("incompatible composition")
    .into();
    assert!(matches!(error, Error::Configuration(_)));
}
