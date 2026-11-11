use std::path::Path;

use relay_bridge::{
    Bridge, BridgeError, ChatCompletionsBridge, ConversionOptions, DecodeContext, Frame, LossScope,
    chat_completions::MaxTokensField,
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

fn decode_request(value: &Value) -> Request {
    ChatCompletionsBridge::new()
        .decode_request(&serde_json::to_vec(value).expect("JSON"))
        .expect("request")
}

fn protocol_input(value: Value) -> Request {
    serde_json::from_value(value).expect("protocol request")
}

fn context() -> DecodeContext {
    DecodeContext {
        response_id: Some("fallback".into()),
        observed_at: 5678,
    }
}

fn client_request() -> Request {
    protocol_input(json!({"version":"1","model":"test-model","messages":[]}))
}

fn assert_conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected conversion, got {error:?}")
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_preserves_messages_options_and_delivery_usage() {
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
    assert_eq!(
        input.delivery,
        Some(Delivery {
            stream: true,
            stream_usage: true
        })
    );
    assert!(matches!(input.messages[0], Message::System { .. }));
    assert!(matches!(input.messages[1], Message::Developer { .. }));
    let Message::User { content } = &input.messages[2] else {
        panic!("user")
    };
    assert!(
        matches!(&content[1], UserContent::Image(ImagePart { detail: Some(ImageDetail(detail)), .. }) if detail == "high")
    );
    assert!(
        matches!(&content[3], UserContent::Document(DocumentPart { source: MediaSource::Base64 { media_type, .. }, filename: Some(filename), .. }) if media_type == "application/pdf" && filename == "report.pdf")
    );
    let Message::Assistant { content } = &input.messages[3] else {
        panic!("assistant")
    };
    assert!(
        matches!(&content[0], AssistantContent::ToolCall { arguments, .. } if arguments == "{ \"a\":1, \"a\": 2 }")
    );
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
        input.tools.as_ref().expect("tools").choice,
        Some(ToolChoice::Function {
            name: "lookup".into()
        })
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
fn ingress_rejects_seed_and_other_unrepresentable_top_level_fields() {
    let bridge = ChatCompletionsBridge::new();
    for (key, value) in [
        ("seed", json!(42)),
        ("service_tier", json!("default")),
        ("metadata", json!({"consumer":"fixture"})),
        ("user", json!("u")),
        ("logprobs", json!(true)),
        ("n", json!(1)),
        ("prediction", json!({})),
    ] {
        let mut wire = fixture("request_tokens.json");
        wire[key] = value;
        assert_conversion(
            bridge
                .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
                .expect_err("strict ingress"),
            ConversionErrorKind::Unrepresentable,
            &format!("/{key}"),
        );
    }
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
        input.parameters = Some(Parameters {
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
        (
            "response_format",
            json!({"type":"json_schema","schema":{}}),
            "/response_format/name",
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
            {"type":"reasoning","id":"r:0","text":"private","opaque":"opaque"},
            {"type":"unknown","id":"r:1","value":{"future":true}},
            {"type":"text","id":"r:2","text":"ok","cache_control":{}}
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

fn output(name: &str) -> Response {
    ChatCompletionsBridge::new()
        .decode_response(
            &serde_json::to_vec(&fixture(name)).expect("JSON"),
            &context(),
        )
        .expect("response")
}

#[test]
fn responses_preserve_text_tools_usage_and_ignore_unrepresented_metadata() {
    let bridge = ChatCompletionsBridge::new();
    let output = output("response.json");
    assert_eq!(output.stop_reason, StopReason::ToolUse);
    assert_eq!(
        output.content,
        vec![
            AssistantContent::Text {
                id: "chatcmpl_fixture:0".into(),
                provider_id: None,
                text: "Checking".into(),
                cache_control: None
            },
            AssistantContent::ToolCall {
                id: "chatcmpl_fixture:1".into(),
                provider_id: None,
                call_id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{ \"x\": 1 }".into(),
                cache_control: None
            }
        ]
    );
    assert_eq!(output.usage, Some(usage()));
    assert_eq!(
        output.created_at,
        Timestamp {
            observed_at: 5678,
            provider_created_at: Some(1234)
        }
    );
    let mut expected = fixture("response.json");
    for key in ["system_fingerprint", "service_tier", "metadata"] {
        expected.as_object_mut().expect("object").remove(key);
    }
    assert_eq!(
        bridge
            .encode_response(&client_request(), &output, &ConversionOptions::default())
            .expect("same-format response")
            .body,
        expected
    );
    let refusal_wire = fixture("response_refusal.json");
    let refusal = bridge
        .decode_response(
            &serde_json::to_vec(&refusal_wire).expect("JSON"),
            &context(),
        )
        .expect("refusal");
    assert_eq!(
        refusal.content,
        vec![AssistantContent::Unknown {
            id: "chatcmpl_refusal:0".into(),
            provider_id: None,
            value: refusal_wire["choices"][0]["message"].clone()
        }]
    );
    assert_eq!(refusal.stop_reason, StopReason::ContentFilter);
    assert_eq!(refusal.usage, None);
    let unknown_wire = fixture("response_unknown.json");
    let unknown = bridge
        .decode_response(
            &serde_json::to_vec(&unknown_wire).expect("JSON"),
            &context(),
        )
        .expect("unknown tool output");
    assert_eq!(
        unknown.content,
        vec![AssistantContent::Unknown {
            id: "chatcmpl_custom:0".into(),
            provider_id: None,
            value: unknown_wire["choices"][0]["message"]["tool_calls"][0].clone()
        }]
    );
    assert!(
        matches!(bridge.decode_response(&serde_json::to_vec(&fixture("response_multiple.json")).expect("JSON"), &context()), Err(BridgeError::Decode(error)) if error.message.contains("multiple choices"))
    );
    for malformed in [b"{".as_slice(), b"{}"] {
        assert!(matches!(
            bridge.decode_response(malformed, &context()),
            Err(BridgeError::Decode(_))
        ));
    }
}

#[test]
fn relay_identity_uses_provider_ids_when_present() {
    let response = output("response_unknown.json");
    assert_eq!(response.id, "chatcmpl_custom");
    assert_eq!(response.provider_id.as_deref(), Some("chatcmpl_custom"));
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
    let mut decoder = bridge.stream_decoder(&context());
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
            StreamEvent::Start {
                response_id: "chatcmpl_fixture".into(),
                provider_id: Some("chatcmpl_fixture".into()),
                created_at: Timestamp {
                    observed_at: 5678,
                    provider_created_at: Some(1234)
                },
                model: Some("test-model".into())
            },
            StreamEvent::ItemStart {
                index: 0,
                id: "chatcmpl_fixture:0".into(),
                provider_id: None,
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
                id: "chatcmpl_fixture:1".into(),
                provider_id: None,
                item: StreamItem::ToolCall {
                    call_id: Some("call_1".into()),
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
                id: "chatcmpl_fixture:2".into(),
                provider_id: None,
                item: StreamItem::ToolCall {
                    call_id: None,
                    name: None,
                }
            },
            StreamEvent::Delta {
                index: 2,
                delta: StreamDelta::ToolCallArguments {
                    text: "{\"city\":".into()
                }
            },
            StreamEvent::Metadata(MetadataScope::Item {
                index: 2,
                provider_id: None,
                call_id: Some("call_2".into()),
                name: Some("weather".into()),
            }),
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
    let mut decoder = bridge.stream_decoder(&context());
    assert!(
        matches!(decoder.push(Frame { event:None, data:json!({"error":{"message":"failure"}}).to_string() }, &mut Vec::new()), Err(BridgeError::Decode(error)) if error.message.contains("removed by the adapter"))
    );
}

#[test]
fn response_encoding_prefers_provider_time_and_falls_back_to_observed_time() {
    let bridge = ChatCompletionsBridge::new();
    let mut response = output("response.json");
    response.content = vec![
        AssistantContent::Text {
            id: "chatcmpl_fixture:0".into(),
            provider_id: None,
            text: "Hello ".into(),
            cache_control: None,
        },
        AssistantContent::Text {
            id: "chatcmpl_fixture:1".into(),
            provider_id: None,
            text: "world".into(),
            cache_control: None,
        },
    ];
    response.stop_reason = StopReason::EndTurn;
    let encoded = bridge
        .encode_response(&client_request(), &response, &ConversionOptions::default())
        .expect("provider timestamp");
    assert_eq!(encoded.body, fixture("response_encoded.json"));
    assert!(encoded.losses.is_empty());
    response.created_at.provider_created_at = None;
    let mut expected = fixture("response_encoded.json");
    expected["created"] = json!(5678);
    assert_eq!(
        bridge
            .encode_response(&client_request(), &response, &ConversionOptions::default())
            .expect("observed timestamp")
            .body,
        expected
    );
}

#[test]
fn stream_encoder_emits_sdk_chunks_and_done_and_reports_losses_immediately() {
    let bridge = ChatCompletionsBridge::new();
    let options = ConversionOptions {
        allow_loss: [LossScope::Reasoning, LossScope::UnknownContent].into(),
    };
    let mut encoder = bridge.stream_encoder(&client_request(), &options);
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    let events = vec![
        StreamEvent::Start {
            response_id: "chatcmpl_fixture".into(),
            provider_id: None,
            created_at: Timestamp {
                observed_at: 1234,
                provider_created_at: None,
            },
            model: Some("test-model".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            id: "chatcmpl_fixture:0".into(),
            provider_id: None,
            item: StreamItem::Reasoning,
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
            id: "chatcmpl_fixture:1".into(),
            provider_id: None,
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
            id: "chatcmpl_fixture:2".into(),
            provider_id: None,
            item: StreamItem::ToolCall {
                call_id: Some("call_1".into()),
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
            id: "chatcmpl_fixture:3".into(),
            provider_id: None,
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
        "chunk_encoded_text_start.json",
        "chunk_encoded_text.json",
        "chunk_encoded_tool_start.json",
        "chunk_encoded_arguments.json",
        "chunk_04.json",
        "chunk_05.json",
    ]
    .map(fixture);
    assert_eq!(chunks, expected);
    let start = &events[0];
    let mut strict = bridge.stream_encoder(&client_request(), &ConversionOptions::default());
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

#[test]
fn stream_usage_requires_stream_and_stream_options_are_strict() {
    let bridge = ChatCompletionsBridge::new();
    let mut request = client_request();
    request.delivery = Some(Delivery {
        stream: false,
        stream_usage: true,
    });
    assert_conversion(
        bridge
            .encode_request(&request, &ConversionOptions::default())
            .expect_err("stream required"),
        ConversionErrorKind::Invalid,
        "/delivery/stream_usage",
    );
    for (options, kind, path) in [
        (
            json!({"include_usage":true}),
            ConversionErrorKind::Invalid,
            "/stream_options/include_usage",
        ),
        (
            json!({"future":true}),
            ConversionErrorKind::Unrepresentable,
            "/stream_options/future",
        ),
    ] {
        let body = json!({"model":"m","messages":[],"stream":false,"stream_options":options});
        assert_conversion(
            bridge
                .decode_request(&serde_json::to_vec(&body).expect("JSON"))
                .expect_err("invalid delivery"),
            kind,
            path,
        );
    }
}

#[test]
fn an_item_with_start_and_end_only_remains_present() {
    let bridge = ChatCompletionsBridge::new();
    let mut encoder = bridge.stream_encoder(&client_request(), &ConversionOptions::default());
    let events = [
        StreamEvent::Start {
            response_id: "r".into(),
            provider_id: None,
            created_at: Timestamp {
                observed_at: 5,
                provider_created_at: None,
            },
            model: Some("m".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            id: "r:0".into(),
            provider_id: None,
            item: StreamItem::Text,
        },
        StreamEvent::ItemEnd { index: 0 },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ];
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in &events {
        encoder
            .push(event, &mut frames, &mut losses)
            .expect("event");
    }
    encoder.finish(&mut frames, &mut losses).expect("complete");
    let mut decoder = bridge.stream_decoder(&context());
    let mut decoded = Vec::new();
    for frame in frames {
        decoder.push(frame, &mut decoded).expect("frame");
    }
    decoder.finish(&mut decoded).expect("complete");
    assert!(
        matches!(&decoded[1], StreamEvent::ItemStart { index: 0, id, provider_id: None, item: StreamItem::Text } if id == "r:0")
    );
    assert_eq!(
        decoded[decoded.len() - 2],
        StreamEvent::ItemEnd { index: 0 }
    );
    assert_eq!(decoded.last(), events.last());
    assert!(losses.is_empty());
}
