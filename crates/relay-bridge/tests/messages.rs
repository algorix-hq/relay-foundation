use std::{fs, path::PathBuf};

use relay_bridge::{
    Bridge, BridgeError, ConversionOptions, DecodeContext, Frame, LossScope, MessagesBridge,
};
use relay_protocol::{
    AssistantContent, ConversionErrorKind, Delivery, Message, MetadataScope, Request, StopReason,
    StreamDelta, StreamEvent, StreamItem, Timestamp, Usage,
};
use serde_json::{Value, json};

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

fn protocol_input(overrides: Value) -> Request {
    let mut request = json!({"version":"1", "model":"consumer-model", "messages":[{"role":"user", "content":[{"type":"text", "text":"Hi"}]}], "parameters":{"max_output_tokens":32}});
    request
        .as_object_mut()
        .expect("request")
        .extend(overrides.as_object().expect("overrides").clone());
    serde_json::from_value::<Request>(request).expect("neutral request")
}

fn context() -> DecodeContext {
    DecodeContext {
        response_id: Some("fallback".into()),
        observed_at: 1234,
    }
}

fn start() -> StreamEvent {
    StreamEvent::Start {
        response_id: "msg_stream".into(),
        provider_id: None,
        created_at: Timestamp {
            observed_at: 1234,
            provider_created_at: None,
        },
        model: Some("consumer-model".into()),
    }
}

fn item_start(index: u32, item: StreamItem) -> StreamEvent {
    StreamEvent::ItemStart {
        index,
        id: format!("msg_stream:{index}"),
        provider_id: None,
        item,
    }
}

fn provisional_usage(input_tokens: u64, output_tokens: u64) -> StreamEvent {
    StreamEvent::Metadata(MetadataScope::Response {
        usage: Some(Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }),
        model: None,
    })
}

fn assert_conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected conversion error: {error}")
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_preserves_system_positions_tools_reasoning_and_delivery() {
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
        if name == "request-full.json" {
            assert_eq!(
                input.delivery,
                Some(Delivery {
                    stream: true,
                    stream_usage: false
                })
            );
            assert!(matches!(
                input.response_format.as_ref(),
                Some(relay_protocol::ResponseFormat::JsonSchema {
                    name: None,
                    description: None,
                    strict: None,
                    ..
                })
            ));
            assert!(matches!(input.messages[4], Message::System { .. }));
            let Message::Assistant { content } = &input.messages[2] else {
                panic!("assistant")
            };
            assert!(
                matches!(&content[0], AssistantContent::Reasoning { provider_id: None, text: Some(text), opaque: Some(opaque), .. } if text.is_empty() && opaque == "SIG")
            );
            assert!(
                matches!(&content[1], AssistantContent::Reasoning { provider_id: None, text: None, opaque: Some(opaque), .. } if opaque == "ENC")
            );
        }
        let encoded = bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("strict encoding");
        assert_eq!(encoded.body, body, "{name}");
        assert!(encoded.losses.is_empty());

        if name == "request-full.json" {
            let Message::System { content } = input.messages.remove(0) else {
                panic!("leading system")
            };
            for part in content.into_iter().rev() {
                input.messages.insert(
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
            input.parameters = None;
            let encoded = MessagesBridge::with_default_max_tokens(32)
                .encode_request(&input, &ConversionOptions::default())
                .expect("consumer default");
            assert_eq!(encoded.body, body);
        }
    }
}

#[test]
fn ingress_rejects_top_level_fields_without_protocol_counterparts() {
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
        let mut body = fixture("request-minimal.json");
        body[key] = json!({"uninterpreted":true});
        assert_conversion(
            MessagesBridge::new()
                .decode_request(&serde_json::to_vec(&body).expect("JSON"))
                .expect_err("strict ingress"),
            ConversionErrorKind::Unrepresentable,
            &format!("/{key}"),
        );
    }
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
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"r:0","text":"x","summary":["summary"],"opaque":"SIG"}]}]}),
            Unrepresentable,
            "/messages/0/content/0/summary",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"https://example.com/a.png"},"detail":"high"}]}]}),
            Unrepresentable,
            "/messages/0/content/0/detail",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"r:0","text":"unsigned"}]}]}),
            Unrepresentable,
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","id":"r:0","value":{"type":"future"}}]}]}),
            Unrepresentable,
            "/messages/0/content/0",
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
            json!({"messages":[{"role":"assistant","content":[{"type":"tool_call","id":"r:0","call_id":"t","name":"f","arguments":"{"}]}]}),
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
        (
            json!({"response_format":{"type":"json_schema","name":"","schema":{}}}),
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
fn replay_uses_call_ids_and_opaque_values_never_item_ids() {
    let input = protocol_input(json!({"messages":[{"role":"assistant","content":[
        {"type":"tool_call","id":"relay:0","provider_id":"provider-item","call_id":"tool_1","name":"lookup","arguments":"{}"},
        {"type":"reasoning","id":"relay:1","provider_id":"provider-reasoning","text":"Thought","opaque":"SIG"}
    ]}]}));
    let encoded = MessagesBridge::new()
        .encode_request(&input, &ConversionOptions::default())
        .expect("replay");
    assert_eq!(
        encoded.body["messages"][0]["content"],
        json!([
            {"type":"tool_use","id":"tool_1","name":"lookup","input":{}},
            {"type":"thinking","thinking":"Thought","signature":"SIG"}
        ])
    );
    assert!(encoded.losses.is_empty());
}

#[test]
fn responses_keep_known_meaning_unknown_blocks_and_normalized_cache_usage() {
    let bridge = MessagesBridge::new();
    let body = fixture("response-full.json");
    let output = bridge
        .decode_response(&serde_json::to_vec(&body).expect("JSON"), &context())
        .expect("response");
    assert!(matches!(&output.content[0], AssistantContent::Text { text, .. } if text == "Answer"));
    assert!(
        matches!(&output.content[1], AssistantContent::ToolCall { arguments, .. } if arguments == "{\"q\":1}")
    );
    assert!(
        matches!(&output.content[2], AssistantContent::Reasoning { provider_id: None, text: Some(text), opaque: Some(signature), .. } if text == "Thought" && signature == "SIG")
    );
    assert!(
        matches!(&output.content[3], AssistantContent::Reasoning { provider_id: None, text: None, opaque: Some(data), .. } if data == "ENC")
    );
    assert_eq!(
        output.content[4],
        AssistantContent::Unknown {
            id: format!("{}:4", output.id),
            provider_id: None,
            value: body["content"][4].clone()
        }
    );
    let usage = output.usage.as_ref().expect("usage");
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
    assert_eq!(output.id, body["id"]);
    assert_eq!(output.provider_id.as_deref(), body["id"].as_str());
    assert_eq!(
        output.created_at,
        Timestamp {
            observed_at: 1234,
            provider_created_at: None
        }
    );
    assert_conversion(
        bridge
            .encode_response(
                &protocol_input(json!({})),
                &output,
                &ConversionOptions::default(),
            )
            .expect_err("unknown output requires permission"),
        ConversionErrorKind::Unrepresentable,
        "/content/4",
    );
    let encoded = bridge
        .encode_response(
            &protocol_input(json!({})),
            &output,
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
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
            .decode_response(&serde_json::to_vec(&body).expect("JSON"), &context())
            .expect("response");
        assert_eq!(output.stop_reason, expected);
        let mut expected_body = body;
        expected_body["stop_sequence"] = Value::Null;
        assert_eq!(
            bridge
                .encode_response(
                    &protocol_input(json!({})),
                    &output,
                    &ConversionOptions::default()
                )
                .expect("gateway response")
                .body,
            expected_body
        );
    }
    let mut future = fixture("response-end-turn.json");
    future["stop_reason"] = json!("future_stop");
    let output = bridge
        .decode_response(&serde_json::to_vec(&future).expect("JSON"), &context())
        .expect("forward-compatible stop reason");
    assert_eq!(output.stop_reason, StopReason::Other("future_stop".into()));
    for stop in [
        StopReason::ContentFilter,
        StopReason::Other("future_stop".into()),
    ] {
        let mut output = output.clone();
        output.stop_reason = stop;
        assert_conversion(
            bridge
                .encode_response(
                    &protocol_input(json!({})),
                    &output,
                    &ConversionOptions::default(),
                )
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
            bridge.decode_response(body, &context()),
            Err(BridgeError::Decode(_))
        ));
    }
    let mut output = bridge
        .decode_response(
            &serde_json::to_vec(&fixture("response-end-turn.json")).expect("JSON"),
            &context(),
        )
        .expect("response");
    let mut missing_id = fixture("response-end-turn.json");
    missing_id.as_object_mut().expect("object").remove("id");
    let fallback = bridge
        .decode_response(&serde_json::to_vec(&missing_id).expect("JSON"), &context())
        .expect("fallback identity");
    assert_eq!(fallback.id, "fallback");
    assert_eq!(fallback.provider_id, None);
    assert!(matches!(
        bridge.decode_response(
            &serde_json::to_vec(&missing_id).expect("JSON"),
            &DecodeContext {
                response_id: None,
                observed_at: 1
            }
        ),
        Err(BridgeError::Decode(_))
    ));
    output.id = "relay-id".into();
    assert_eq!(
        bridge
            .encode_response(
                &protocol_input(json!({})),
                &output,
                &ConversionOptions::default()
            )
            .expect("Relay ID")
            .body["id"],
        "relay-id"
    );
    let mut without_usage = output.clone();
    without_usage.usage = None;
    assert_conversion(
        bridge
            .encode_response(
                &protocol_input(json!({})),
                &without_usage,
                &ConversionOptions::default(),
            )
            .expect_err("usage required"),
        ConversionErrorKind::Unrepresentable,
        "/usage",
    );
    let usage = output.usage.as_mut().expect("usage");
    usage.cache_read_tokens = Some(1);
    assert_conversion(
        bridge
            .encode_response(
                &protocol_input(json!({})),
                &output,
                &ConversionOptions::default(),
            )
            .expect_err("cache subtraction"),
        ConversionErrorKind::Invalid,
        "/usage/input_tokens",
    );
}

#[test]
fn stream_preserves_text_tool_fragments_signatures_and_cumulative_usage() {
    let bridge = MessagesBridge::new();
    let fixtures = stream_fixtures();
    let mut decoder = bridge.stream_decoder(&context());
    let mut events = Vec::new();
    for value in &fixtures {
        decoder
            .push(wire_frame(value), &mut events)
            .expect("stream frame");
    }
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
            {"type":"start","response_id":"msg_stream","provider_id":"msg_stream","created_at":{"observed_at":1234},"model":"consumer-model"},
            {"type":"metadata","scope":"response","usage":{"input_tokens":35,"output_tokens":1,"cache_read_tokens":20,"cache_write_tokens":5}},
            {"type":"item_start","index":0,"id":"msg_stream:0","item":{"type":"text"}},
            {"type":"delta","index":0,"delta":{"type":"text","text":"Hello"}},
            {"type":"item_end","index":0},
            {"type":"item_start","index":1,"id":"msg_stream:1","item":{"type":"reasoning"}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_text","text":""}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_text","text":"Thought"}},
            {"type":"delta","index":1,"delta":{"type":"reasoning_opaque","value":"SIG"}},
            {"type":"item_end","index":1},
            {"type":"item_start","index":2,"id":"msg_stream:2","item":{"type":"reasoning"}},
            {"type":"delta","index":2,"delta":{"type":"reasoning_opaque","value":"ENC"}},
            {"type":"item_end","index":2},
            {"type":"item_start","index":3,"id":"msg_stream:3","item":{"type":"tool_call","call_id":"tool_1","name":"lookup"}},
            {"type":"delta","index":3,"delta":{"type":"tool_call_arguments","text":"{\"q\":"}},
            {"type":"delta","index":3,"delta":{"type":"tool_call_arguments","text":"1}"}},
            {"type":"item_end","index":3},
            {"type":"stop","stop_reason":"tool_use","usage":{"input_tokens":35,"output_tokens":12,"cache_read_tokens":20,"cache_write_tokens":5,"reasoning_tokens":4}}
        ])
    );
    let mut encoder =
        bridge.stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in &events {
        encoder
            .push(event, &mut frames, &mut losses)
            .expect("gateway event");
        if matches!(event, StreamEvent::Metadata(MetadataScope::Response { .. })) {
            assert_eq!(frames, [wire_frame(&fixture("stream/00-start.json"))]);
        }
        if matches!(event, StreamEvent::Delta { delta: StreamDelta::ReasoningText { text }, .. } if text == "Thought")
        {
            let value: Value =
                serde_json::from_str(&frames.last().expect("thinking frame").data).expect("JSON");
            assert_eq!(
                value["delta"],
                json!({"type":"thinking_delta", "thinking":"Thought"})
            );
            assert!(
                !frames
                    .iter()
                    .any(|frame| frame.data.contains("signature_delta"))
            );
        }
    }
    encoder
        .finish(&mut frames, &mut losses)
        .expect("gateway EOF");
    assert!(losses.is_empty());
    let mut expected = fixtures;
    expected[14] = fixture("stream-encoded-delta.json");
    assert_eq!(frames, expected.iter().map(wire_frame).collect::<Vec<_>>());
    let mut encoder =
        bridge.stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut interleaved_frames = Vec::new();
    for event in events[..6]
        .iter()
        .chain(&events[10..13])
        .chain(&events[6..10])
        .chain(&events[13..])
    {
        encoder
            .push(event, &mut interleaved_frames, &mut losses)
            .expect("interleaved reasoning");
    }
    encoder
        .finish(&mut interleaved_frames, &mut losses)
        .expect("interleaved EOF");
    let mut interleaved_decoder = bridge.stream_decoder(&context());
    let mut interleaved_events = Vec::new();
    for frame in interleaved_frames {
        interleaved_decoder
            .push(frame, &mut interleaved_events)
            .expect("interleaved frame");
    }
    interleaved_decoder
        .finish(&mut interleaved_events)
        .expect("interleaved finish");
    assert_eq!(interleaved_events.last(), events.last());
    assert!(interleaved_events.contains(&StreamEvent::Delta {
        index: 1,
        delta: StreamDelta::ReasoningOpaque {
            value: "SIG".into()
        }
    }));
}

#[test]
fn stream_unknowns_keep_raw_values_with_bridge_assigned_indices() {
    let mut decoder = MessagesBridge::new().stream_decoder(&context());
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
        events[2],
        StreamEvent::ItemStart {
            index: 0,
            id: "msg_stream:0".into(),
            provider_id: None,
            item: StreamItem::Unknown
        }
    );
    assert_eq!(
        events[3],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-server-start.json")["content_block"].clone()
            }
        }
    );
    assert_eq!(
        events[5],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-citations-delta.json")["delta"].clone()
            }
        }
    );
    assert_eq!(
        events[4],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("stream-server-delta.json")["delta"].clone()
            }
        }
    );
    assert_eq!(events[6], StreamEvent::ItemEnd { index: 0 });
}

#[test]
fn incomplete_stream_and_leaked_provider_errors_never_synthesize_completion() {
    let bridge = MessagesBridge::new();
    let mut decoder = bridge.stream_decoder(&context());
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
    let mut encoder = bridge.stream_encoder(
        &protocol_input(json!({})),
        &ConversionOptions {
            allow_loss: [LossScope::UnknownContent, LossScope::Reasoning].into(),
        },
    );
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(&start(), &mut frames, &mut losses)
        .expect("start");
    encoder
        .push(
            &item_start(0, StreamItem::Unknown),
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
            &item_start(1, StreamItem::Reasoning),
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
        item_start(2, StreamItem::Text),
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
    let mut encoder = MessagesBridge::new()
        .stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        start(),
        item_start(0, StreamItem::Text),
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

#[test]
fn live_stream_reports_provisional_then_final_usage_without_promoting_metadata() {
    let recording = include_str!("fixtures/messages_live/stream_usage.sse");
    let mut frames = Vec::new();
    for record in recording.split("\n\n").filter(|record| !record.is_empty()) {
        let event = record
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .map(str::to_owned);
        let data = record
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("recorded data")
            .into();
        frames.push(Frame { event, data });
    }
    let bridge = MessagesBridge::new();
    let mut decoder = bridge.stream_decoder(&context());
    let mut events = Vec::new();
    for frame in &frames {
        decoder
            .push(frame.clone(), &mut events)
            .expect("recorded frame");
    }
    decoder.finish(&mut events).expect("normal finish");
    assert!(
        matches!(&events[0], StreamEvent::Start { response_id, provider_id: Some(provider_id), created_at: Timestamp { observed_at: 1234, provider_created_at: None }, .. } if response_id == "msg_011CgvtpzYn9bzLNJr3SVTuh" && response_id == provider_id)
    );
    assert!(matches!(
        &events[1],
        StreamEvent::Metadata(MetadataScope::Response {
            usage: Some(Usage {
                input_tokens: 13,
                output_tokens: 2,
                ..
            }),
            model: None
        })
    ));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            usage: Some(Usage {
                input_tokens: 13,
                output_tokens: 8,
                ..
            }),
            ..
        })
    ));

    let mut decoder = bridge.stream_decoder(&context());
    let mut without_final = Vec::new();
    for frame in frames {
        let mut value: Value = serde_json::from_str(&frame.data).expect("JSON");
        if value["type"] == "message_delta" {
            value.as_object_mut().expect("object").remove("usage");
        }
        decoder
            .push(wire_frame(&value), &mut without_final)
            .expect("no final usage");
    }
    decoder.finish(&mut without_final).expect("normal finish");
    assert!(matches!(
        without_final.last(),
        Some(StreamEvent::Stop { usage: None, .. })
    ));
}

#[test]
fn cross_format_stream_waits_for_final_usage_even_if_metadata_arrives_after_content() {
    let bridge = MessagesBridge::new();
    let mut encoder =
        bridge.stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        start(),
        item_start(0, StreamItem::Text),
        provisional_usage(99, 1),
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
            .expect("buffered event");
        assert!(frames.is_empty());
    }
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
        .expect("final usage");
    encoder.finish(&mut frames, &mut losses).expect("complete");
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
        .map(|name| wire_frame(&fixture(name)))
    );
    assert!(losses.is_empty());
}

#[test]
fn provisional_usage_does_not_make_a_stop_without_final_usage_encodable() {
    let bridge = MessagesBridge::new();
    let mut encoder =
        bridge.stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [start(), provisional_usage(13, 2)] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("early usage");
    }
    assert_eq!(frames.len(), 1);
    let prior = frames.clone();
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
            .expect_err("final usage required"),
        ConversionErrorKind::Unrepresentable,
        "/usage",
    );
    assert_eq!(frames, prior);
}

#[test]
fn unsigned_reasoning_errors_after_streaming_or_is_omitted_atomically_when_allowed() {
    let bridge = MessagesBridge::new();
    for allowed in [false, true] {
        let options = ConversionOptions {
            allow_loss: if allowed {
                [LossScope::Reasoning].into()
            } else {
                Default::default()
            },
        };
        let mut encoder = bridge.stream_encoder(&protocol_input(json!({})), &options);
        let mut frames = Vec::new();
        let mut losses = Vec::new();
        for event in [
            start(),
            provisional_usage(13, 2),
            item_start(0, StreamItem::Reasoning),
            StreamEvent::Delta {
                index: 0,
                delta: StreamDelta::ReasoningText {
                    text: "Unsigned".into(),
                },
            },
        ] {
            encoder
                .push(&event, &mut frames, &mut losses)
                .expect("reasoning text");
        }
        assert_eq!(frames.len(), if allowed { 1 } else { 3 });
        let prior = frames.clone();
        let end = encoder.push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses);
        if allowed {
            end.expect("permitted omission");
            assert_eq!(losses.len(), 1);
            assert_eq!(losses[0].path, "/items/0");
        } else {
            assert_conversion(
                end.expect_err("unsigned thinking"),
                ConversionErrorKind::Unrepresentable,
                "/items/0",
            );
            assert!(losses.is_empty());
        }
        assert_eq!(frames, prior);
    }
}

#[test]
fn signed_reasoning_with_loss_permission_waits_until_item_end_and_preserves_empty_text() {
    let bridge = MessagesBridge::new();
    let mut encoder = bridge.stream_encoder(
        &protocol_input(json!({})),
        &ConversionOptions {
            allow_loss: [LossScope::Reasoning].into(),
        },
    );
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        start(),
        provisional_usage(13, 2),
        item_start(0, StreamItem::Reasoning),
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningText {
                text: String::new(),
            },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningOpaque {
                value: "SIG".into(),
            },
        },
    ] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("buffered signed thinking");
        assert!(frames.len() <= 1);
    }
    encoder
        .push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses)
        .expect("release reasoning");
    let values: Vec<Value> = frames
        .iter()
        .map(|frame| serde_json::from_str(&frame.data).expect("JSON"))
        .collect();
    assert_eq!(values[1]["content_block"]["type"], "thinking");
    assert_eq!(
        values[2]["delta"],
        json!({"type":"thinking_delta","thinking":""})
    );
    assert_eq!(
        values[3]["delta"],
        json!({"type":"signature_delta","signature":"SIG"})
    );
    assert!(losses.is_empty());
}

#[test]
fn tool_start_waits_for_partial_metadata_and_uses_call_id_not_provider_item_id() {
    let bridge = MessagesBridge::new();
    let mut encoder =
        bridge.stream_encoder(&protocol_input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        start(),
        provisional_usage(13, 2),
        item_start(
            0,
            StreamItem::ToolCall {
                call_id: None,
                name: None,
            },
        ),
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ToolCallArguments { text: "{}".into() },
        },
        StreamEvent::Metadata(MetadataScope::Item {
            index: 0,
            provider_id: Some("provider-item".into()),
            call_id: Some("tool_1".into()),
            name: None,
        }),
    ] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("partial tool metadata");
        assert!(frames.len() <= 1);
    }
    encoder
        .push(
            &StreamEvent::Metadata(MetadataScope::Item {
                index: 0,
                provider_id: None,
                call_id: None,
                name: Some("lookup".into()),
            }),
            &mut frames,
            &mut losses,
        )
        .expect("tool name");
    assert_eq!(frames.len(), 3);
    let block: Value = serde_json::from_str(&frames[1].data).expect("JSON");
    assert_eq!(block["content_block"]["id"], "tool_1");
    assert_eq!(block["content_block"]["name"], "lookup");
    encoder
        .push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses)
        .expect("tool end");
}
