use std::path::Path;

use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ChatCompletionsBridge, ConversionOptions, Frame, Input,
    LossScope, NativeFields, Output, chat_completions::MaxTokensField,
};
use relay_protocol::*;
use serde_json::{Value, json};

// Request, response and chunk fixtures are checked against OpenAI 7.55.0's
// ChatCompletionCreateParams, ChatCompletion and ChatCompletionChunk respectively.
fn fixture(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/chat_completions")
                .join(name),
        )
        .expect("read fixture"),
    )
    .expect("fixture JSON")
}

fn decode_request(value: &Value) -> Input {
    ChatCompletionsBridge::new()
        .decode_request(&serde_json::to_vec(value).expect("JSON"))
        .expect("request")
}

fn protocol_input(value: Value) -> Input {
    Input::new(serde_json::from_value(value).expect("protocol request"))
}

fn assert_conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected conversion, got {error:?}")
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_preserves_messages_options_and_same_format_native_fields() {
    let bridge = ChatCompletionsBridge::new();
    for name in [
        "request.json",
        "request_required.json",
        "request_auto.json",
        "request_none.json",
        "request_legacy_mapped.json",
    ] {
        let wire = fixture(name);
        let input = decode_request(&wire);
        let encoded = bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("encode request");
        assert_eq!(encoded.body, wire, "{name}");
        assert!(encoded.losses.is_empty());
    }
    let input = decode_request(&fixture("request.json"));
    assert!(input.stream);
    assert!(matches!(input.request.messages[0], Message::System { .. }));
    assert!(matches!(
        input.request.messages[1],
        Message::Developer { .. }
    ));
    let Message::User { content } = &input.request.messages[2] else {
        panic!("user")
    };
    assert!(
        matches!(&content[1], UserContent::Image(ImagePart { detail: Some(ImageDetail(detail)), .. }) if detail == "high")
    );
    assert!(
        matches!(&content[3], UserContent::Document(DocumentPart { source: MediaSource::Base64 { media_type, .. }, filename: Some(filename), .. }) if media_type == "application/pdf" && filename == "report.pdf")
    );
    let Message::Assistant { content } = &input.request.messages[3] else {
        panic!("assistant")
    };
    assert!(
        matches!(&content[0], AssistantContent::ToolCall { arguments, .. } if arguments == "{ \"a\":1, \"a\": 2 }")
    );
    let native = input.native_for(bridge.format()).expect("native fields");
    assert_eq!(native.len(), 4);
    assert_eq!(native["seed"], 42);
    assert_eq!(native["metadata"], json!({"consumer":"fixture"}));
    assert!(!native.contains_key("model"));
    assert!(input.native_for(&ApiFormat::MESSAGES).is_none());
}

#[test]
fn ingress_rejects_unrepresentable_nested_fields_and_malformed_values() {
    let bridge = ChatCompletionsBridge::new();
    for (message, path, kind) in [
        (
            json!({"role":"user","content":"Hi","future":1}),
            "/messages/0/future",
            ConversionErrorKind::Unrepresentable,
        ),
        (
            json!({"role":"user","content":[{"type":"text","text":"Hi","future":1}]}),
            "/messages/0/content/0/future",
            ConversionErrorKind::Unrepresentable,
        ),
        (
            json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"u","future":1}}]}),
            "/messages/0/content/0/image_url/future",
            ConversionErrorKind::Unrepresentable,
        ),
        (
            json!({"role":"assistant","function_call":{"name":"f","arguments":"{}"}}),
            "/messages/0/function_call",
            ConversionErrorKind::Unrepresentable,
        ),
        (
            json!({"role":"function","name":"f","content":"result"}),
            "/messages/0",
            ConversionErrorKind::Unrepresentable,
        ),
        (
            json!({"role":"user","content":12}),
            "/messages/0/content",
            ConversionErrorKind::Invalid,
        ),
    ] {
        let wire = json!({"model":"m","messages":[message]});
        assert_conversion(
            bridge
                .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
                .expect_err("strict ingress"),
            kind,
            path,
        );
    }
    let mut wire = fixture("request.json");
    wire["tools"][0]["function"]["future"] = json!(1);
    assert_conversion(
        bridge
            .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
            .expect_err("strict tool"),
        ConversionErrorKind::Unrepresentable,
        "/tools/0/function/future",
    );
    assert_conversion(
        bridge.decode_request(b"{").expect_err("malformed JSON"),
        ConversionErrorKind::Invalid,
        "",
    );
    for (pointer, path) in [
        (
            "/tools/0/function/parameters",
            "/tools/0/function/parameters",
        ),
        (
            "/response_format/json_schema/schema",
            "/response_format/json_schema/schema",
        ),
    ] {
        let mut wire = fixture("request.json");
        *wire.pointer_mut(pointer).expect("fixture field") = json!(true);
        assert_conversion(
            bridge
                .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
                .expect_err("non-object schema"),
            ConversionErrorKind::Invalid,
            path,
        );
    }
}

#[test]
fn max_token_aliases_require_agreement_and_constructor_selects_wire_field() {
    let bridge = ChatCompletionsBridge::new();
    let mut wire = fixture("request_max_tokens.json");
    let input = decode_request(&wire);
    assert_eq!(
        bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("modern field")
            .body,
        fixture("request_tokens.json")
    );
    let old = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens);
    assert_eq!(
        old.encode_request(&input, &ConversionOptions::default())
            .expect("compatible field")
            .body,
        wire
    );
    wire["max_completion_tokens"] = json!(16);
    assert_eq!(
        bridge
            .encode_request(&decode_request(&wire), &ConversionOptions::default())
            .expect("equal aliases")
            .body,
        fixture("request_tokens.json")
    );
    wire["max_completion_tokens"] = json!(17);
    assert_conversion(
        bridge
            .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
            .expect_err("conflict"),
        ConversionErrorKind::Invalid,
        "/max_completion_tokens",
    );
}

#[test]
fn legacy_definitions_and_choice_map_without_inventing_call_ids() {
    let bridge = ChatCompletionsBridge::new();
    let input = decode_request(&fixture("request_legacy.json"));
    assert_eq!(
        input.request.tools.as_ref().expect("tools").choice,
        Some(ToolChoice::Function {
            name: "lookup".into()
        })
    );
    assert!(
        !input
            .native_for(bridge.format())
            .expect("native")
            .contains_key("functions")
    );
    assert_eq!(
        bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("legacy conversion")
            .body,
        fixture("request_legacy_mapped.json")
    );
}

#[test]
fn native_fields_never_override_generated_fields() {
    let request = decode_request(&fixture("request_tokens.json")).request;
    let native = NativeFields::from_iter([("model".into(), json!("override"))]);
    let input = Input::with_native(ApiFormat::CHAT_COMPLETIONS, request, false, native);
    assert_conversion(
        ChatCompletionsBridge::new()
            .encode_request(&input, &ConversionOptions::default())
            .expect_err("collision"),
        ConversionErrorKind::Invalid,
        "/model",
    );
}

#[test]
fn unrepresentable_request_features_have_precise_paths_even_with_loss_permissions() {
    let bridge = ChatCompletionsBridge::new();
    let options = ConversionOptions {
        allow_loss: [
            LossScope::Reasoning,
            LossScope::CacheControl,
            LossScope::UnknownContent,
        ]
        .into(),
        ..Default::default()
    };
    for (part, suffix) in [
        (
            json!({"type":"tool_result","tool_call_id":"c","content":[],"is_error":true}),
            "/is_error",
        ),
        (
            json!({"type":"tool_result","tool_call_id":"c","content":[{"type":"image","source":{"type":"url","url":"u"}}]}),
            "/content/0",
        ),
        (
            json!({"type":"tool_result","tool_call_id":"c","content":[{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"cGRm"}}]}),
            "/content/0",
        ),
        (
            json!({"type":"document","source":{"type":"url","url":"u"}}),
            "/source",
        ),
        (
            json!({"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"cGRm"},"title":"title"}),
            "/title",
        ),
    ] {
        let input = protocol_input(
            json!({"version":"1","model":"m","messages":[{"role":"user","content":[part]}]}),
        );
        assert_conversion(
            bridge
                .encode_request(&input, &options)
                .expect_err("unrepresentable"),
            ConversionErrorKind::Unrepresentable,
            &format!("/messages/0/content/0{suffix}"),
        );
    }
    for (field, value) in [
        ("budget_tokens", json!(1)),
        ("summary", json!("auto")),
        ("thinking", json!("adaptive")),
    ] {
        let mut request = json!({"version":"1","model":"m","messages":[],"reasoning":{}});
        request["reasoning"][field] = value;
        assert_conversion(
            bridge
                .encode_request(&protocol_input(request), &options)
                .expect_err("reasoning option"),
            ConversionErrorKind::Unrepresentable,
            &format!("/reasoning/{field}"),
        );
    }
    for (temperature, top_p, path) in [
        (Some(f64::NAN), None, "/parameters/temperature"),
        (None, Some(f64::INFINITY), "/parameters/top_p"),
    ] {
        let mut input = decode_request(&fixture("request_tokens.json"));
        input.request.parameters = Some(Parameters {
            max_output_tokens: None,
            temperature,
            top_p,
            stop: Vec::new(),
        });
        assert_conversion(
            bridge
                .encode_request(&input, &options)
                .expect_err("non-JSON number"),
            ConversionErrorKind::Invalid,
            path,
        );
    }
    for (field, value, path) in [
        (
            "tools",
            json!({"definitions":[{"type":"function","name":"f","parameters":true}]}),
            "/tools/definitions/0/parameters",
        ),
        (
            "response_format",
            json!({"type":"json_schema","name":"s","schema":true}),
            "/response_format/schema",
        ),
    ] {
        let mut request = json!({"version":"1","model":"m","messages":[]});
        request[field] = value;
        assert_conversion(
            bridge
                .encode_request(&protocol_input(request), &options)
                .expect_err("schema cannot be expressed"),
            ConversionErrorKind::Unrepresentable,
            path,
        );
    }
}

#[test]
fn losses_require_matching_scopes_and_report_each_dropped_item() {
    let bridge = ChatCompletionsBridge::new();
    let input = protocol_input(
        json!({"version":"1","model":"test-model","messages":[{"role":"assistant","content":[
            {"type":"reasoning","text":"private","opaque":"opaque"},
            {"type":"unknown","value":{"future":true}},
            {"type":"text","text":"ok","cache_control":{}}
        ]}]}),
    );
    for (scopes, path) in [
        (vec![], "/messages/0/content/0"),
        (vec![LossScope::Reasoning], "/messages/0/content/1"),
        (
            vec![LossScope::Reasoning, LossScope::UnknownContent],
            "/messages/0/content/2/cache_control",
        ),
    ] {
        let options = ConversionOptions {
            allow_loss: scopes.into_iter().collect(),
            ..Default::default()
        };
        assert_conversion(
            bridge
                .encode_request(&input, &options)
                .expect_err("missing loss scope"),
            ConversionErrorKind::Unrepresentable,
            path,
        );
    }
    let options = ConversionOptions {
        allow_loss: [
            LossScope::Reasoning,
            LossScope::UnknownContent,
            LossScope::CacheControl,
        ]
        .into(),
        ..Default::default()
    };
    let encoded = bridge
        .encode_request(&input, &options)
        .expect("permitted omissions");
    assert_eq!(encoded.body, fixture("request_loss.json"));
    assert_eq!(
        encoded
            .losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/messages/0/content/0",
            "/messages/0/content/1",
            "/messages/0/content/2/cache_control"
        ]
    );
}

fn output(name: &str) -> Output {
    ChatCompletionsBridge::new()
        .decode_response(&serde_json::to_vec(&fixture(name)).expect("JSON"))
        .expect("response")
}

#[test]
fn responses_preserve_text_tools_usage_and_native_metadata_without_inventing_refusal_text() {
    let bridge = ChatCompletionsBridge::new();
    let output = output("response.json");
    assert_eq!(output.response.id.as_deref(), Some("chatcmpl_fixture"));
    assert_eq!(output.response.stop_reason, StopReason::ToolUse);
    assert_eq!(
        output.response.content,
        vec![
            AssistantContent::Text(TextPart {
                text: "Checking".into(),
                cache_control: None
            }),
            AssistantContent::ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{ \"x\": 1 }".into(),
                cache_control: None
            }
        ]
    );
    assert_eq!(output.response.usage, Some(usage()));
    assert_eq!(
        output.native_for(bridge.format()).expect("native")["created"],
        1234
    );
    assert_eq!(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect("same-format response")
            .body,
        fixture("response.json")
    );
    let refusal_wire = fixture("response_refusal.json");
    let refusal = bridge
        .decode_response(&serde_json::to_vec(&refusal_wire).expect("JSON"))
        .expect("refusal");
    assert_eq!(
        refusal.response.content,
        vec![AssistantContent::Unknown {
            value: refusal_wire["choices"][0]["message"].clone()
        }]
    );
    assert_eq!(refusal.response.stop_reason, StopReason::ContentFilter);
    assert_eq!(refusal.response.usage, None);
    let unknown_wire = fixture("response_unknown.json");
    let unknown = bridge
        .decode_response(&serde_json::to_vec(&unknown_wire).expect("JSON"))
        .expect("unknown tool output");
    assert_eq!(
        unknown.response.content,
        vec![AssistantContent::Unknown {
            value: unknown_wire["choices"][0]["message"]["tool_calls"][0].clone()
        }]
    );
    assert!(
        matches!(bridge.decode_response(&serde_json::to_vec(&fixture("response_multiple.json")).expect("JSON")), Err(BridgeError::Decode(error)) if error.message.contains("multiple choices"))
    );
    for malformed in [b"{".as_slice(), b"{}"] {
        assert!(matches!(
            bridge.decode_response(malformed),
            Err(BridgeError::Decode(_))
        ));
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 20,
        output_tokens: 9,
        cache_read_tokens: Some(10),
        cache_write_tokens: Some(4),
        reasoning_tokens: Some(3),
    }
}

fn chunk(name: &str) -> Frame {
    Frame {
        event: None,
        data: fixture(name).to_string(),
    }
}

#[test]
fn stream_waits_for_done_preserves_parallel_arguments_and_late_usage() {
    let bridge = ChatCompletionsBridge::new();
    let mut decoder = bridge.stream_decoder();
    let mut events = Vec::new();
    for name in [
        "chunk_00.json",
        "chunk_01.json",
        "chunk_02.json",
        "chunk_03.json",
        "chunk_04.json",
        "chunk_05.json",
    ] {
        decoder.push(chunk(name), &mut events).expect("chunk");
    }
    assert_eq!(
        events,
        vec![
            StreamEvent::ResponseStart {
                id: Some("chatcmpl_fixture".into()),
                model: Some("test-model".into())
            },
            StreamEvent::ItemStart {
                index: 0,
                item: StreamItem::Text
            },
            StreamEvent::Delta {
                index: 0,
                delta: StreamDelta::Text {
                    text: "Checking".into()
                }
            },
            StreamEvent::ItemStart {
                index: 1,
                item: StreamItem::ToolCall {
                    id: Some("call_1".into()),
                    name: Some("lookup".into())
                }
            },
            StreamEvent::Delta {
                index: 1,
                delta: StreamDelta::ToolCallArguments {
                    text: "{\"x\":".into()
                }
            },
            StreamEvent::ItemStart {
                index: 2,
                item: StreamItem::ToolCall {
                    id: Some("call_2".into()),
                    name: Some("weather".into())
                }
            },
            StreamEvent::Delta {
                index: 2,
                delta: StreamDelta::ToolCallArguments {
                    text: "{\"city\":".into()
                }
            },
            StreamEvent::Delta {
                index: 2,
                delta: StreamDelta::ToolCallArguments {
                    text: "\"Seoul\"}".into()
                }
            },
            StreamEvent::Delta {
                index: 1,
                delta: StreamDelta::ToolCallArguments { text: "1}".into() }
            },
        ]
    );
    let before = events.len();
    assert!(matches!(
        decoder.finish(&mut events),
        Err(BridgeError::IncompleteStream(_))
    ));
    assert_eq!(events.len(), before);
    decoder
        .push(
            Frame {
                event: None,
                data: "[DONE]".into(),
            },
            &mut events,
        )
        .expect("terminal signal");
    assert_eq!(
        &events[before..],
        &[
            StreamEvent::ItemEnd { index: 0 },
            StreamEvent::ItemEnd { index: 1 },
            StreamEvent::ItemEnd { index: 2 },
            StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                usage: Some(usage())
            }
        ]
    );
    decoder.finish(&mut events).expect("complete stream");
    let mut decoder = bridge.stream_decoder();
    assert!(
        matches!(decoder.push(Frame { event:None, data:json!({"error":{"message":"failure"}}).to_string() }, &mut Vec::new()), Err(BridgeError::Decode(error)) if error.message.contains("removed by the adapter"))
    );
}

#[test]
fn response_encoding_requires_provider_facts_or_explicit_created_time() {
    let bridge = ChatCompletionsBridge::new();
    let mut response = output("response.json").response;
    response.content = vec![
        AssistantContent::Text(TextPart {
            text: "Hello ".into(),
            cache_control: None,
        }),
        AssistantContent::Text(TextPart {
            text: "world".into(),
            cache_control: None,
        }),
    ];
    response.stop_reason = StopReason::EndTurn;
    let mut output = Output::new(response);
    assert_conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("missing created"),
        ConversionErrorKind::Unrepresentable,
        "/created",
    );
    let options = ConversionOptions {
        created_at: Some(1234),
        ..Default::default()
    };
    let encoded = bridge
        .encode_response(&output, &options)
        .expect("explicit timestamp");
    assert_eq!(encoded.body, fixture("response_encoded.json"));
    assert!(encoded.losses.is_empty());
    output.response.id = None;
    let native = NativeFields::from_iter([
        ("id".into(), json!("chatcmpl_fixture")),
        ("created".into(), json!(1234)),
    ]);
    let same_format =
        Output::with_native(ApiFormat::CHAT_COMPLETIONS, output.response.clone(), native);
    assert_eq!(
        bridge
            .encode_response(
                &same_format,
                &ConversionOptions {
                    created_at: Some(999),
                    ..Default::default()
                }
            )
            .expect("native id and created take precedence")
            .body,
        fixture("response_encoded.json")
    );
    assert_conversion(
        bridge
            .encode_response(&output, &options)
            .expect_err("missing id"),
        ConversionErrorKind::Unrepresentable,
        "/id",
    );
}

#[test]
fn stream_encoder_emits_sdk_chunks_and_done_and_reports_losses_immediately() {
    let bridge = ChatCompletionsBridge::new();
    let options = ConversionOptions {
        created_at: Some(1234),
        allow_loss: [LossScope::Reasoning, LossScope::UnknownContent].into(),
    };
    let mut encoder = bridge.stream_encoder(&options);
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    let events = vec![
        StreamEvent::ResponseStart {
            id: Some("chatcmpl_fixture".into()),
            model: Some("test-model".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            item: StreamItem::Reasoning { id: None },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningOpaque {
                value: "secret".into(),
            },
        },
        StreamEvent::ItemEnd { index: 0 },
        StreamEvent::ItemStart {
            index: 1,
            item: StreamItem::Text,
        },
        StreamEvent::Delta {
            index: 1,
            delta: StreamDelta::Text {
                text: "Hello world".into(),
            },
        },
        StreamEvent::ItemEnd { index: 1 },
        StreamEvent::ItemStart {
            index: 2,
            item: StreamItem::ToolCall {
                id: Some("call_1".into()),
                name: Some("lookup".into()),
            },
        },
        StreamEvent::Delta {
            index: 2,
            delta: StreamDelta::ToolCallArguments {
                text: "{ \"x\": 1 }".into(),
            },
        },
        StreamEvent::ItemEnd { index: 2 },
        StreamEvent::ItemStart {
            index: 3,
            item: StreamItem::Unknown,
        },
        StreamEvent::Delta {
            index: 3,
            delta: StreamDelta::Unknown {
                value: json!({"future":true}),
            },
        },
        StreamEvent::ItemEnd { index: 3 },
        StreamEvent::Stop {
            stop_reason: StopReason::ToolUse,
            usage: Some(usage()),
        },
    ];
    for event in &events {
        encoder
            .push(event, &mut frames, &mut losses)
            .expect("encode event");
        if matches!(event, StreamEvent::ItemStart { index: 0, .. }) {
            assert_eq!(losses.len(), 1);
        }
    }
    encoder.finish(&mut frames, &mut losses).expect("complete");
    assert_eq!(
        losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        ["/items/0", "/items/3"]
    );
    assert!(frames.iter().all(|frame| frame.event.is_none()));
    assert_eq!(frames.last().expect("done").data, "[DONE]");
    let chunks = frames[..frames.len() - 1]
        .iter()
        .map(|frame| serde_json::from_str::<Value>(&frame.data).expect("chunk JSON"))
        .collect::<Vec<_>>();
    let expected = [
        "chunk_00.json",
        "chunk_encoded_text.json",
        "chunk_encoded_tool_start.json",
        "chunk_encoded_arguments.json",
        "chunk_04.json",
        "chunk_05.json",
    ]
    .map(fixture);
    assert_eq!(chunks, expected);
    let start = &events[0];
    assert_conversion(
        bridge
            .stream_encoder(&ConversionOptions::default())
            .push(start, &mut Vec::new(), &mut Vec::new())
            .expect_err("missing created"),
        ConversionErrorKind::Unrepresentable,
        "/created",
    );
    let mut strict = bridge.stream_encoder(&ConversionOptions {
        created_at: Some(1234),
        ..Default::default()
    });
    strict
        .push(start, &mut Vec::new(), &mut Vec::new())
        .expect("start");
    assert_conversion(
        strict
            .push(&events[1], &mut Vec::new(), &mut Vec::new())
            .expect_err("reasoning scope"),
        ConversionErrorKind::Unrepresentable,
        "/items/0",
    );
}
