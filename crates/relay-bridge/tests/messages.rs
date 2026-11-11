use std::{fs, path::PathBuf};

use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, Frame, Input, LossScope, MessagesBridge,
    Output,
};
use relay_protocol::{
    AssistantContent, ConversionErrorKind, Message, Request, StopReason, StreamDelta, StreamEvent,
    StreamItem, Usage,
};
use serde_json::{Value, json};

// SDK 1.7.0 check.sh types: request-*.json => MessageCreateParams,
// response-*.json => Message, stream*.json and stream/*.json => RawMessageStreamEvent.
fn fixture(name: &str) -> Value {
    serde_json::from_slice(
        &fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/messages")
                .join(name),
        )
        .expect("fixture file"),
    )
    .expect("fixture JSON")
}

fn wire_frame(value: &Value) -> Frame {
    Frame {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn stream_fixtures() -> Vec<Value> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/messages/stream");
    let mut paths = fs::read_dir(dir)
        .expect("stream fixtures")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| serde_json::from_slice(&fs::read(path).expect("fixture")).expect("JSON"))
        .collect()
}

fn protocol_input(overrides: Value) -> Input {
    let mut request = json!({"version":"1", "model":"consumer-model", "messages":[{"role":"user", "content":[{"type":"text", "text":"Hi"}]}], "parameters":{"max_output_tokens":32}});
    request
        .as_object_mut()
        .expect("request")
        .extend(overrides.as_object().expect("overrides").clone());
    Input::new(serde_json::from_value::<Request>(request).expect("neutral request"))
}

fn assert_conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected conversion error: {error}")
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_preserves_system_positions_tools_reasoning_and_native_fields() {
    let bridge = MessagesBridge::new();
    for name in [
        "request-full.json",
        "request-minimal.json",
        "request-adaptive.json",
        "request-disabled.json",
        "request-grouped-system.json",
        "request-tools-auto.json",
        "request-tools-none.json",
        "request-tools-function.json",
        "request-thinking-zero-budget.json",
        "request-thinking-equal-limit.json",
        "request-thinking-above-limit.json",
    ] {
        let body = fixture(name);
        let mut input = bridge
            .decode_request(&serde_json::to_vec(&body).expect("JSON"))
            .expect("ingress");
        let native = input
            .native_for(&ApiFormat::MESSAGES)
            .expect("format-scoped native fields");
        assert!(!native.contains_key("max_tokens"));
        assert!(!native.contains_key("messages"));
        assert!(!native.contains_key("thinking"));
        if name == "request-full.json" {
            assert!(input.stream);
            assert!(
                matches!(input.request.response_format.as_ref(), Some(relay_protocol::ResponseFormat::JsonSchema { name, description: None, strict: None, .. }) if name.is_empty())
            );
            assert_eq!(native["metadata"], body["metadata"]);
            assert_eq!(native["service_tier"], body["service_tier"]);
            assert!(input.native_for(&ApiFormat::RESPONSES).is_none());
            assert!(matches!(input.request.messages[4], Message::System { .. }));
            let Message::Assistant { content } = &input.request.messages[2] else {
                panic!("assistant")
            };
            assert!(
                matches!(&content[0], AssistantContent::Reasoning { id: None, text: Some(text), opaque: Some(opaque), .. } if text.is_empty() && opaque == "SIG")
            );
            assert!(
                matches!(&content[1], AssistantContent::Reasoning { id: None, text: None, opaque: Some(opaque), .. } if opaque == "ENC")
            );
        }
        let encoded = bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("strict encoding");
        assert_eq!(encoded.body, body, "{name}");
        assert!(encoded.losses.is_empty());

        if name == "request-full.json" {
            let Message::System { content } = input.request.messages.remove(0) else {
                panic!("leading system")
            };
            for part in content.into_iter().rev() {
                input.request.messages.insert(
                    0,
                    Message::System {
                        content: vec![part],
                    },
                );
            }
            assert_eq!(
                bridge
                    .encode_request(&input, &ConversionOptions::default())
                    .expect("leading system run")
                    .body,
                body
            );
        }

        if name == "request-minimal.json" {
            input.request.parameters = None;
            let encoded = MessagesBridge::with_default_max_tokens(32)
                .encode_request(&input, &ConversionOptions::default())
                .expect("consumer default");
            assert_eq!(encoded.body, body);
        }
    }
}

#[test]
fn top_level_fields_without_neutral_counterparts_are_not_allowlisted() {
    let mut body = fixture("request-minimal.json");
    for key in [
        "metadata",
        "service_tier",
        "container",
        "speed",
        "inference_geo",
        "cache_control",
        "diagnostics",
        "future_option",
    ] {
        body[key] = json!({"uninterpreted":true});
    }
    let input = MessagesBridge::new()
        .decode_request(&serde_json::to_vec(&body).expect("JSON"))
        .expect("native ingress");
    let native = input.native_for(&ApiFormat::MESSAGES).expect("same format");
    assert_eq!(native.len(), 8);
    assert_eq!(native["future_option"], json!({"uninterpreted":true}));
}

#[test]
fn strict_ingress_rejects_unrepresented_fields_and_invalid_system_positions() {
    use ConversionErrorKind::{Invalid, Unrepresentable};
    let cases = [
        (
            json!({"messages":[{"role":"user","content":[{"type":"text","text":"x","extra":1}]}]}),
            Unrepresentable,
            "/messages/0/content/0/extra",
        ),
        (
            json!({"tools":[{"type":"web_search_20250305","name":"web_search"}]}),
            Unrepresentable,
            "/tools/0/type",
        ),
        (
            json!({"messages":[{"role":"user","content":"x","extra":1}]}),
            Unrepresentable,
            "/messages/0/extra",
        ),
        (
            json!({"thinking":{"type":"adaptive","display":"omitted"}}),
            Unrepresentable,
            "/thinking/display",
        ),
        (
            json!({"output_config":{"other":true}}),
            Unrepresentable,
            "/output_config/other",
        ),
        (
            json!({"messages":[{"role":"system","content":"x"},{"role":"assistant","content":"a"}]}),
            Invalid,
            "/messages/0",
        ),
        (
            json!({"messages":[{"role":"system","content":[]},{"role":"system","content":"x"},{"role":"assistant","content":"a"}]}),
            Invalid,
            "/messages/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":"a"},{"role":"system","content":"x"}]}),
            Invalid,
            "/messages/1",
        ),
        (
            json!({"messages":[{"role":"user","content":"u"},{"role":"system","content":"x"},{"role":"user","content":"v"}]}),
            Invalid,
            "/messages/1",
        ),
        (
            json!({"messages":[{"role":"user","content":"u"},{"role":"system","content":"x"},{"role":"system","content":"y"},{"role":"user","content":"v"}]}),
            Invalid,
            "/messages/1",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"f","input":{}}]},{"role":"system","content":"x"},{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[]}]}]}),
            Invalid,
            "/messages/1",
        ),
        (
            json!({"thinking":{"type":"disabled","budget_tokens":1024}}),
            Invalid,
            "/thinking/budget_tokens",
        ),
        (
            json!({"thinking":{"type":"enabled"}}),
            Invalid,
            "/thinking/budget_tokens",
        ),
    ];
    for (overrides, kind, path) in cases {
        let mut body = fixture("request-minimal.json");
        body.as_object_mut()
            .expect("object")
            .extend(overrides.as_object().expect("object").clone());
        assert_conversion(
            MessagesBridge::new()
                .decode_request(&serde_json::to_vec(&body).expect("JSON"))
                .expect_err(path),
            kind,
            path,
        );
    }
    assert_conversion(
        MessagesBridge::new()
            .decode_request(b"not JSON")
            .expect_err("malformed request"),
        Invalid,
        "",
    );
}

#[test]
fn strict_encoding_names_each_unrepresentable_input_and_structural_error() {
    use ConversionErrorKind::{Invalid, Unrepresentable};
    let cases = [
        (
            json!({"parameters":{}}),
            Unrepresentable,
            "/parameters/max_output_tokens",
        ),
        (
            json!({"parameters":{"max_output_tokens":32,"temperature":0.5}}),
            Unrepresentable,
            "/parameters/temperature",
        ),
        (
            json!({"messages":[{"role":"developer","content":[{"type":"text","text":"x"}]}]}),
            Unrepresentable,
            "/messages/0/role",
        ),
        (
            json!({"response_format":"json_object"}),
            Unrepresentable,
            "/response_format",
        ),
        (
            json!({"reasoning":{"summary":"auto"}}),
            Unrepresentable,
            "/reasoning/summary",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","text":"x","summary":["summary"],"opaque":"SIG"}]}]}),
            Unrepresentable,
            "/messages/0/content/0/summary",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"https://example.com/a.png"},"detail":"high"}]}]}),
            Unrepresentable,
            "/messages/0/content/0/detail",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","text":"unsigned"}]}]}),
            Unrepresentable,
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","value":{"type":"future"}}]}]}),
            Unrepresentable,
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"item_1","text":"x","opaque":"SIG"}]}]}),
            Unrepresentable,
            "/messages/0/content/0/id",
        ),
        (
            json!({"reasoning":{"thinking":"enabled"}}),
            Invalid,
            "/reasoning/budget_tokens",
        ),
        (
            json!({"reasoning":{"thinking":"adaptive","budget_tokens":1024}}),
            Invalid,
            "/reasoning/budget_tokens",
        ),
        (
            json!({"messages":[{"role":"user","content":[]},{"role":"assistant","content":[]},{"role":"system","content":[{"type":"text","text":"x"}]}]}),
            Invalid,
            "/messages/2",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"tool_call","id":"t","name":"f","arguments":"{"}]}]}),
            Invalid,
            "/messages/0/content/0/arguments",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"document","source":{"type":"base64","media_type":"text/plain","data":"/w=="}}]}]}),
            Invalid,
            "/messages/0/content/0/source/data",
        ),
        (
            json!({"response_format":{"type":"json_schema","name":"named","schema":{}}}),
            Unrepresentable,
            "/response_format/name",
        ),
    ];
    for (overrides, kind, path) in cases {
        assert_conversion(
            MessagesBridge::new()
                .encode_request(&protocol_input(overrides), &ConversionOptions::default())
                .expect_err(path),
            kind,
            path,
        );
    }
}

#[test]
fn permitted_sampling_and_developer_losses_are_reported_separately() {
    let input = protocol_input(json!({
        "messages":[{"role":"developer","content":[{"type":"text","text":"Instructions"}]},{"role":"user","content":[{"type":"text","text":"Hi"}]}],
        "parameters":{"max_output_tokens":32,"temperature":0.5,"top_p":0.9}
    }));
    let encoded = MessagesBridge::new()
        .encode_request(
            &input,
            &ConversionOptions {
                allow_loss: [LossScope::Sampling, LossScope::DeveloperRole].into(),
                ..Default::default()
            },
        )
        .expect("explicit loss permission");
    assert_eq!(encoded.body, fixture("request-lossy.json"));
    assert_eq!(
        encoded
            .losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/parameters/temperature",
            "/parameters/top_p",
            "/messages/0/role"
        ]
    );
}

#[test]
fn native_fields_never_override_generated_request_or_response_fields() {
    let input = protocol_input(json!({}));
    let input = Input::with_native(
        ApiFormat::MESSAGES,
        input.request,
        false,
        serde_json::Map::from_iter([("model".into(), json!("override"))]),
    );
    assert_conversion(
        MessagesBridge::new()
            .encode_request(&input, &ConversionOptions::default())
            .expect_err("collision"),
        ConversionErrorKind::Invalid,
        "/model",
    );
    let output = MessagesBridge::new()
        .decode_response(&serde_json::to_vec(&fixture("response-end-turn.json")).expect("JSON"))
        .expect("response");
    let output = Output::with_native(
        ApiFormat::MESSAGES,
        output.response,
        serde_json::Map::from_iter([("model".into(), json!("override"))]),
    );
    assert_conversion(
        MessagesBridge::new()
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("collision"),
        ConversionErrorKind::Invalid,
        "/model",
    );
}

#[test]
fn responses_keep_known_meaning_unknown_blocks_and_normalized_cache_usage() {
    let bridge = MessagesBridge::new();
    let body = fixture("response-full.json");
    let output = bridge
        .decode_response(&serde_json::to_vec(&body).expect("JSON"))
        .expect("response");
    assert!(
        matches!(&output.response.content[0], AssistantContent::Text(part) if part.text == "Answer")
    );
    assert!(
        matches!(&output.response.content[1], AssistantContent::ToolCall { arguments, .. } if arguments == "{\"q\":1}")
    );
    assert!(
        matches!(&output.response.content[2], AssistantContent::Reasoning { id: None, text: Some(text), opaque: Some(signature), .. } if text == "Thought" && signature == "SIG")
    );
    assert!(
        matches!(&output.response.content[3], AssistantContent::Reasoning { id: None, text: None, opaque: Some(data), .. } if data == "ENC")
    );
    assert_eq!(
        output.response.content[4],
        AssistantContent::Unknown {
            value: body["content"][4].clone()
        }
    );
    let usage = output.response.usage.as_ref().expect("usage");
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.reasoning_tokens
        ),
        (35, 12, Some(20), Some(5), Some(4))
    );
    let native = output.native_for(&ApiFormat::MESSAGES).expect("native");
    assert_eq!(native["type"], "message");
    assert!(native.contains_key("container"));
    assert!(!native.contains_key("content"));
    assert_conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("unknown output requires permission"),
        ConversionErrorKind::Unrepresentable,
        "/content/4",
    );
    let encoded = bridge
        .encode_response(
            &output,
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
                ..Default::default()
            },
        )
        .expect("gateway response");
    assert_eq!(encoded.body, fixture("response-encoded.json"));
    assert_eq!(encoded.losses.len(), 1);
    assert_eq!(encoded.losses[0].path, "/content/4");
}

#[test]
fn stop_reasons_round_trip_without_equating_filtering_and_refusal() {
    let bridge = MessagesBridge::new();
    for (name, expected) in [
        ("response-end-turn.json", StopReason::EndTurn),
        ("response-max-tokens.json", StopReason::MaxTokens),
        ("response-stop-sequence.json", StopReason::StopSequence),
        ("response-tool-use.json", StopReason::ToolUse),
        ("response-refusal.json", StopReason::Refusal),
        (
            "response-context-window.json",
            StopReason::ContextWindowExceeded,
        ),
        (
            "response-pause-turn.json",
            StopReason::Other("pause_turn".into()),
        ),
    ] {
        let body = fixture(name);
        let output = bridge
            .decode_response(&serde_json::to_vec(&body).expect("JSON"))
            .expect("response");
        assert_eq!(output.response.stop_reason, expected);
        assert_eq!(
            bridge
                .encode_response(&output, &ConversionOptions::default())
                .expect("gateway response")
                .body,
            body
        );
    }
    let mut future = fixture("response-end-turn.json");
    future["stop_reason"] = json!("future_stop");
    let output = bridge
        .decode_response(&serde_json::to_vec(&future).expect("JSON"))
        .expect("forward-compatible stop reason");
    assert_eq!(
        output.response.stop_reason,
        StopReason::Other("future_stop".into())
    );
    for stop in [
        StopReason::ContentFilter,
        StopReason::Other("future_stop".into()),
    ] {
        let mut output = output.clone();
        output.response.stop_reason = stop;
        assert_conversion(
            bridge
                .encode_response(&output, &ConversionOptions::default())
                .expect_err("unrepresentable stop"),
            ConversionErrorKind::Unrepresentable,
            "/stop_reason",
        );
    }
}

#[test]
fn response_failures_distinguish_bad_bodies_missing_ids_and_impossible_usage() {
    let bridge = MessagesBridge::new();
    for body in [b"not JSON".as_slice(), b"{}", b"{\"content\":42}"] {
        assert!(matches!(
            bridge.decode_response(body),
            Err(BridgeError::Decode(_))
        ));
    }
    let mut output = bridge
        .decode_response(&serde_json::to_vec(&fixture("response-end-turn.json")).expect("JSON"))
        .expect("response");
    output.response.id = None;
    assert_conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("ID required"),
        ConversionErrorKind::Unrepresentable,
        "/id",
    );
    let mut native = output
        .native_for(&ApiFormat::MESSAGES)
        .expect("native")
        .clone();
    native.insert("id".into(), json!("msg_stop"));
    let mut output = Output::with_native(ApiFormat::MESSAGES, output.response, native);
    assert_eq!(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect("native ID")
            .body,
        fixture("response-end-turn.json")
    );
    let mut without_usage = output.clone();
    without_usage.response.usage = None;
    assert_conversion(
        bridge
            .encode_response(&without_usage, &ConversionOptions::default())
            .expect_err("usage required"),
        ConversionErrorKind::Unrepresentable,
        "/usage",
    );
    let usage = output.response.usage.as_mut().expect("usage");
    usage.cache_read_tokens = Some(1);
    assert_conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("cache subtraction"),
        ConversionErrorKind::Invalid,
        "/usage/input_tokens",
    );
}

#[test]
fn stream_preserves_text_tool_fragments_signatures_and_cumulative_usage() {
    let bridge = MessagesBridge::new();
    let fixtures = stream_fixtures();
    let mut decoder = bridge.stream_decoder();
    let mut events = Vec::new();
    for value in &fixtures {
        decoder
            .push(wire_frame(value), &mut events)
            .expect("stream frame");
    }
    // Ping is documented framing-level keepalive, not a member of SDK RawMessageStreamEvent.
    decoder
        .push(
            Frame {
                event: Some("ping".into()),
                data: "{\"type\":\"ping\"}".into(),
            },
            &mut events,
        )
        .expect("keepalive ignored");
    decoder.finish(&mut events).expect("normal EOF");
    assert_eq!(
        serde_json::to_value(&events).expect("neutral events"),
        json!([
            {"type":"response_start","id":"msg_stream","model":"consumer-model"},
            {"type":"item_start","index":0,"item":{"type":"text"}},
            {"type":"delta","index":0,"delta":{"type":"text","text":"Hello"}},
            {"type":"item_end","index":0},
            {"type":"item_start","index":1,"item":{"type":"reasoning"}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_text","text":""}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_text","text":"Thought"}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_opaque","value":"SIG"}},
            {"type":"item_end","index":1},
            {"type":"item_start","index":2,"item":{"type":"reasoning"}},
            {"type":"delta","index":2,"delta":{"type":"reasoning_opaque","value":"ENC"}},
            {"type":"item_end","index":2},
            {"type":"item_start","index":3,"item":{"type":"tool_call","id":"tool_1","name":"lookup"}},
            {"type":"delta","index":3,"delta":{"type":"tool_call_arguments","text":"{\"q\":"}},
            {"type":"delta","index":3,"delta":{"type":"tool_call_arguments","text":"1}"}},
            {"type":"item_end","index":3},
            {"type":"stop","stop_reason":"tool_use","usage":{"input_tokens":35,"output_tokens":12,"cache_read_tokens":20,"cache_write_tokens":5,"reasoning_tokens":4}}
        ])
    );
    let mut encoder = bridge.stream_encoder(&ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in &events {
        encoder
            .push(event, &mut frames, &mut losses)
            .expect("gateway event");
        if !matches!(event, StreamEvent::Stop { .. }) {
            assert!(
                frames.is_empty(),
                "message_start must wait for actual usage"
            );
        }
    }
    encoder
        .finish(&mut frames, &mut losses)
        .expect("gateway EOF");
    assert!(losses.is_empty());
    let mut expected = fixtures;
    expected[0] = fixture("stream-encoded-start.json");
    expected[14] = fixture("stream-encoded-delta.json");
    assert_eq!(frames, expected.iter().map(wire_frame).collect::<Vec<_>>());
    let mut encoder = bridge.stream_encoder(&ConversionOptions::default());
    let mut interleaved_frames = Vec::new();
    for event in events[..5]
        .iter()
        .chain(&events[9..12])
        .chain(&events[5..9])
        .chain(&events[12..])
    {
        encoder
            .push(event, &mut interleaved_frames, &mut losses)
            .expect("interleaved reasoning");
        if !matches!(event, StreamEvent::Stop { .. }) {
            assert!(
                interleaved_frames.is_empty(),
                "interleaved frames must wait for actual usage"
            );
        }
    }
    encoder
        .finish(&mut interleaved_frames, &mut losses)
        .expect("interleaved EOF");
    assert_eq!(interleaved_frames, frames);
}

#[test]
fn stream_unknowns_keep_raw_values_with_bridge_assigned_indices() {
    let mut decoder = MessagesBridge::new().stream_decoder();
    let mut events = Vec::new();
    for name in [
        "stream/00-start.json",
        "stream-server-start.json",
        "stream-server-delta.json",
        "stream-citations-delta.json",
        "stream-server-stop.json",
        "stream/14-message-delta.json",
        "stream/15-stop.json",
    ] {
        decoder
            .push(wire_frame(&fixture(name)), &mut events)
            .expect("forward-compatible event");
    }
    decoder.finish(&mut events).expect("stop");
    assert_eq!(
        events[1],
        StreamEvent::ItemStart {
            index: 0,
            item: StreamItem::Unknown
        }
    );
    assert_eq!(
        events[2],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-server-start.json")["content_block"].clone()
            }
        }
    );
    assert_eq!(
        events[4],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-citations-delta.json")["delta"].clone()
            }
        }
    );
    assert_eq!(
        events[3],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-server-delta.json")["delta"].clone()
            }
        }
    );
    assert_eq!(events[5], StreamEvent::ItemEnd { index: 0 });
}

#[test]
fn incomplete_stream_and_leaked_provider_errors_never_synthesize_completion() {
    let bridge = MessagesBridge::new();
    let mut decoder = bridge.stream_decoder();
    let mut events = Vec::new();
    for value in stream_fixtures().iter().take(3) {
        decoder
            .push(wire_frame(value), &mut events)
            .expect("partial output");
    }
    let prior = events.clone();
    assert!(matches!(
        decoder.finish(&mut events),
        Err(BridgeError::IncompleteStream(_))
    ));
    assert_eq!(events, prior);
    let error = decoder
        .push(
            Frame {
                event: Some("error".into()),
                data: "{\"type\":\"error\"}".into(),
            },
            &mut events,
        )
        .expect_err("adapter boundary violation");
    let BridgeError::Decode(error) = error else {
        panic!("bridge must not classify provider errors")
    };
    assert!(error.message.contains("adapter should have removed"));
    assert_eq!(events, prior);
}

#[test]
fn stream_encoder_reports_losses_when_the_omission_becomes_known() {
    let bridge = MessagesBridge::new();
    let mut encoder = bridge.stream_encoder(&ConversionOptions {
        allow_loss: [LossScope::UnknownContent, LossScope::Reasoning].into(),
        ..Default::default()
    });
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(
            &StreamEvent::ResponseStart {
                id: Some("msg_stream".into()),
                model: Some("consumer-model".into()),
            },
            &mut frames,
            &mut losses,
        )
        .expect("start");
    encoder
        .push(
            &StreamEvent::ItemStart {
                index: 0,
                item: StreamItem::Unknown,
            },
            &mut frames,
            &mut losses,
        )
        .expect("permitted unknown loss");
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].path, "/items/0");
    encoder
        .push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses)
        .expect("dropped item end");
    encoder
        .push(
            &StreamEvent::ItemStart {
                index: 1,
                item: StreamItem::Reasoning { id: None },
            },
            &mut frames,
            &mut losses,
        )
        .expect("reasoning start");
    encoder
        .push(
            &StreamEvent::Delta {
                index: 1,
                delta: StreamDelta::ReasoningText {
                    text: "unsigned".into(),
                },
            },
            &mut frames,
            &mut losses,
        )
        .expect("buffered reasoning");
    assert_eq!(losses.len(), 1);
    encoder
        .push(&StreamEvent::ItemEnd { index: 1 }, &mut frames, &mut losses)
        .expect("unsigned reasoning omitted");
    assert_eq!(losses.len(), 2);
    assert_eq!(losses[1].path, "/items/1");
    assert!(frames.is_empty());
    for event in [
        StreamEvent::ItemStart {
            index: 2,
            item: StreamItem::Text,
        },
        StreamEvent::Delta {
            index: 2,
            delta: StreamDelta::Text {
                text: "Hello".into(),
            },
        },
        StreamEvent::ItemEnd { index: 2 },
    ] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("output after a dropped item");
    }
    assert!(frames.is_empty());
    encoder
        .push(
            &StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                usage: Some(Usage {
                    input_tokens: 35,
                    output_tokens: 12,
                    cache_read_tokens: Some(20),
                    cache_write_tokens: Some(5),
                    reasoning_tokens: Some(4),
                }),
            },
            &mut frames,
            &mut losses,
        )
        .expect("provider-supplied usage");
    encoder
        .finish(&mut frames, &mut losses)
        .expect("complete stream");
    assert_eq!(
        frames,
        [
            "stream-encoded-start.json",
            "stream/01-text-start.json",
            "stream/02-text-delta.json",
            "stream/03-text-stop.json",
            "stream-encoded-delta.json",
            "stream/15-stop.json"
        ]
        .iter()
        .map(|name| wire_frame(&fixture(name)))
        .collect::<Vec<_>>()
    );
}

#[test]
fn stream_encoder_never_releases_frames_without_usage() {
    let mut encoder = MessagesBridge::new().stream_encoder(&ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        StreamEvent::ResponseStart {
            id: Some("msg_stream".into()),
            model: Some("consumer-model".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            item: StreamItem::Text,
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Text {
                text: "Hello".into(),
            },
        },
        StreamEvent::ItemEnd { index: 0 },
    ] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("waiting for usage");
        assert!(frames.is_empty());
    }
    assert_conversion(
        encoder
            .push(
                &StreamEvent::Stop {
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                },
                &mut frames,
                &mut losses,
            )
            .expect_err("missing usage"),
        ConversionErrorKind::Unrepresentable,
        "/usage",
    );
    assert!(frames.is_empty());
    assert!(losses.is_empty());
}
