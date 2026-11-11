use relay_bridge::{
    ApiFormat, Bridge, BridgeError, ConversionOptions, Frame, Input, LossScope, NativeFields,
    Output, ResponsesBridge, responses::ResponsesBridgeConfig,
};
use relay_protocol::{
    AssistantContent, ConversionErrorKind, Message, Request, StopReason, StreamDelta, StreamEvent,
    StreamItem, Usage,
};
use serde_json::{Value, json};

// request-*.json: ResponseCreateParams; response-*.json: Response;
// stream-*.json: ResponseStreamEvent, checked with the openai 7.55.0 SDK harness.
fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/responses")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).expect("fixture file")).expect("fixture JSON")
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON body")
}

fn frame(value: Value) -> Frame {
    Frame {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn input(extra: Value) -> Input {
    let mut value = json!({"version":"1", "model":"gpt-5-nano", "messages":[]});
    value
        .as_object_mut()
        .expect("request")
        .extend(extra.as_object().expect("fields").clone());
    Input::new(serde_json::from_value::<Request>(value).expect("protocol request"))
}

fn conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected Conversion: {error}");
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_preserves_native_options_and_normalizes_instructions_to_system_input() {
    let bridge = ResponsesBridge::new();
    let input = bridge
        .decode_request(&bytes(&fixture("request-ingress")))
        .expect("ingress");
    assert!(matches!(input.request.messages[0], Message::System { .. }));
    let native = input.native_for(&ApiFormat::RESPONSES).expect("native");
    assert_eq!(native["previous_response_id"], "resp_previous");
    assert_eq!(native["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(native["store"], false);
    assert_eq!(native["metadata"], json!({"consumer":"test"}));
    for key in [
        "model",
        "input",
        "stream",
        "instructions",
        "tools",
        "tool_choice",
        "reasoning",
        "text",
    ] {
        assert!(!native.contains_key(key));
    }
    let encoded = bridge
        .encode_request(&input, &ConversionOptions::default())
        .expect("encode");
    assert_eq!(encoded.body, fixture("request-encoded"));
    assert!(encoded.losses.is_empty());
    let decoded = bridge
        .decode_request(&bytes(
            &json!({"model":"m", "input":"hello", "future_option":{"untouched":true}}),
        ))
        .expect("future top-level fields");
    assert_eq!(
        decoded.native_for(&ApiFormat::RESPONSES).expect("native")["future_option"],
        json!({"untouched":true})
    );
    assert!(
        matches!(&decoded.request.messages[0], Message::User { content } if matches!(&content[0], relay_protocol::UserContent::Text(part) if part.text == "hello"))
    );
}

#[test]
fn ingress_rejects_unrepresentable_nested_input_and_reports_malformed_fields() {
    use ConversionErrorKind::{Invalid, Unrepresentable};
    let bridge = ResponsesBridge::new();
    let cases = [
        (
            json!({"tools":[{"type":"web_search"}]}),
            Unrepresentable,
            "/tools/0/type",
        ),
        (
            json!({"input":[{"type":"item_reference","id":"item_1"}]}),
            Unrepresentable,
            "/input/0/type",
        ),
        (
            json!({"input":[{"role":"user","content":[{"type":"input_text","text":"x","extra":true}]}]}),
            Unrepresentable,
            "/input/0/content/0/extra",
        ),
        (
            json!({"input":[{"role":"user","content":"x","extra":true}]}),
            Unrepresentable,
            "/input/0/extra",
        ),
        (
            json!({"input":[{"type":"reasoning","id":"rs_1","summary":[],"status":"completed"}]}),
            Unrepresentable,
            "/input/0/status",
        ),
        (
            json!({"input":[{"type":"reasoning","summary":[]}]}),
            Invalid,
            "/input/0/id",
        ),
        (
            json!({"input":[{"type":"reasoning","id":"rs_1","summary":[],"content":[{"type":"reasoning_text","text":"one"},{"type":"reasoning_text","text":"two"}]}]}),
            Unrepresentable,
            "/input/0/content",
        ),
        (
            json!({"text":{"verbosity":"low"}}),
            Unrepresentable,
            "/text/verbosity",
        ),
        (json!({"stop":["x"]}), Unrepresentable, "/stop"),
        (json!({"temperature":"warm"}), Invalid, "/temperature"),
        (
            json!({"input":[{"role":"user","content":[{"type":"input_image","detail":"high","image_url":"data:image/png,raw"}]}]}),
            Invalid,
            "/input/0/content/0/image_url",
        ),
    ];
    for (extra, kind, path) in cases {
        let mut body = json!({"model":"gpt-5-nano"});
        body.as_object_mut()
            .expect("body")
            .extend(extra.as_object().expect("extra").clone());
        conversion(
            bridge.decode_request(&bytes(&body)).expect_err(path),
            kind,
            path,
        );
    }
    conversion(
        bridge.decode_request(b"not JSON").expect_err("malformed"),
        Invalid,
        "",
    );
}

#[test]
fn encoding_rejects_every_unsupported_semantic_field_without_a_loss_escape() {
    let cases = [
        (json!({"parameters":{"stop":["x"]}}), "/parameters/stop"),
        (
            json!({"reasoning":{"budget_tokens":32}}),
            "/reasoning/budget_tokens",
        ),
        (
            json!({"reasoning":{"thinking":"adaptive"}}),
            "/reasoning/thinking",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_call_id":"c","content":[],"is_error":true}]}]}),
            "/messages/0/content/0/is_error",
        ),
        (
            json!({"messages":[{"role":"system","content":[{"type":"text","text":"policy","cache_control":{}}]}]}),
            "/messages/0/content/0/cache_control",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","summary":["one"]}]}]}),
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","value":{"type":"future"}}]}]}),
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"document","source":{"type":"url","url":"https://example.com/a.pdf"},"title":"Title"}]}]}),
            "/messages/0/content/0/title",
        ),
    ];
    for (request, path) in cases {
        conversion(
            ResponsesBridge::new()
                .encode_request(&input(request), &ConversionOptions::default())
                .expect_err(path),
            ConversionErrorKind::Unrepresentable,
            path,
        );
    }
}

#[test]
fn empty_messages_and_stored_reasoning_keep_their_wire_structure() {
    let bridge = ResponsesBridge::new();
    let value = fixture("request-empty-replay");
    let input = bridge
        .decode_request(&bytes(&value))
        .expect("valid empty content");
    assert_eq!(
        bridge
            .encode_request(&input, &ConversionOptions::default())
            .expect("encode")
            .body,
        value
    );
}

#[test]
fn simple_response_formats_use_the_text_format_envelope() {
    for (name, format) in [
        ("request-format-text", relay_protocol::ResponseFormat::Text),
        (
            "request-format-json-object",
            relay_protocol::ResponseFormat::JsonObject,
        ),
    ] {
        let bridge = ResponsesBridge::new();
        let input = bridge
            .decode_request(&bytes(&fixture(name)))
            .expect("format");
        assert_eq!(input.request.response_format, Some(format));
        assert_eq!(
            bridge
                .encode_request(&input, &ConversionOptions::default())
                .expect("encode")
                .body,
            fixture(name)
        );
    }
}

#[test]
fn explicit_losses_drop_only_the_allowed_content_and_report_each_location() {
    for (scope, request, path, expected) in [
        (
            LossScope::CacheControl,
            json!({"messages":[{"role":"system","content":[{"type":"text","text":"policy","cache_control":{}}]}]}),
            "/messages/0/content/0/cache_control",
            "request-text",
        ),
        (
            LossScope::Reasoning,
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","text":"thinking","opaque":"opaque"}]}]}),
            "/messages/0/content/0",
            "request-store-default",
        ),
        (
            LossScope::UnknownContent,
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","value":{"type":"future"}}]}]}),
            "/messages/0/content/0",
            "request-store-default",
        ),
    ] {
        let encoded = ResponsesBridge::new()
            .encode_request(
                &input(request),
                &ConversionOptions {
                    allow_loss: [scope].into(),
                    ..Default::default()
                },
            )
            .expect("allowed omission");
        assert_eq!(encoded.body, fixture(expected));
        assert_eq!(encoded.losses.len(), 1);
        assert_eq!(encoded.losses[0].path, path);
    }
}

#[test]
fn store_configuration_and_native_fields_never_override_each_other() {
    let bridge = ResponsesBridge::new();
    let stateless = ResponsesBridge::with_config(ResponsesBridgeConfig {
        store: Some(false),
        ..Default::default()
    });
    let plain = input(json!({}));
    assert_eq!(
        bridge
            .encode_request(&plain, &ConversionOptions::default())
            .expect("default")
            .body,
        fixture("request-store-default")
    );
    assert_eq!(
        stateless
            .encode_request(&plain, &ConversionOptions::default())
            .expect("stateless")
            .body,
        fixture("request-store-false")
    );
    let native = Input::with_native(
        ApiFormat::RESPONSES,
        plain.request.clone(),
        false,
        NativeFields::from_iter([("store".into(), json!(true))]),
    );
    assert_eq!(
        bridge
            .encode_request(&native, &ConversionOptions::default())
            .expect("native wins when unset")
            .body,
        fixture("request-store-true")
    );
    conversion(
        stateless
            .encode_request(&native, &ConversionOptions::default())
            .expect_err("collision"),
        ConversionErrorKind::Invalid,
        "/store",
    );
    let foreign = Input::with_native(
        ApiFormat::MESSAGES,
        plain.request.clone(),
        false,
        NativeFields::from_iter([("store".into(), json!(true))]),
    );
    assert_eq!(
        stateless
            .encode_request(&foreign, &ConversionOptions::default())
            .expect("foreign ignored")
            .body,
        fixture("request-store-false")
    );
    let collision = Input::with_native(
        ApiFormat::RESPONSES,
        plain.request,
        false,
        NativeFields::from_iter([("model".into(), json!("different"))]),
    );
    conversion(
        bridge
            .encode_request(&collision, &ConversionOptions::default())
            .expect_err("collision"),
        ConversionErrorKind::Invalid,
        "/model",
    );
}

#[test]
fn responses_preserve_text_calls_reasoning_unknown_output_and_usage() {
    let bridge = ResponsesBridge::new();
    let value = fixture("response-full");
    let output = bridge.decode_response(&bytes(&value)).expect("response");
    assert_eq!(output.response.stop_reason, StopReason::ToolUse);
    assert_eq!(output.response.content.len(), 5);
    assert!(
        matches!(&output.response.content[0], AssistantContent::Text(part) if part.text == "Answer")
    );
    assert!(
        matches!(&output.response.content[1], AssistantContent::ToolCall { id, name, arguments, .. } if id == "call_1" && name == "lookup" && arguments == "{ \"key\":1, \"key\": 2 }")
    );
    assert!(
        matches!(&output.response.content[2], AssistantContent::Reasoning { id, text, summary, opaque } if id.as_deref() == Some("rs_1") && text.as_deref() == Some("Thinking") && summary == &["One", "Two"] && opaque.as_deref() == Some("opaque-value"))
    );
    assert_eq!(
        output.response.content[3],
        AssistantContent::Unknown {
            value: value["output"][3]["content"][0].clone()
        }
    );
    assert_eq!(
        output.response.content[4],
        AssistantContent::Unknown {
            value: value["output"][4].clone()
        }
    );
    assert_eq!(output.response.usage, Some(usage()));
    assert_eq!(
        output.native_for(&ApiFormat::RESPONSES).expect("native")["usage"]["input_tokens_details"]
            ["cache_write_tokens_details"],
        json!({"ttl_12h_tokens":2,"ttl_30m_tokens":0})
    );
    conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("unknown content"),
        ConversionErrorKind::Unrepresentable,
        "/content/3",
    );
    let encoded = bridge
        .encode_response(
            &output,
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
                ..Default::default()
            },
        )
        .expect("explicit loss");
    assert_eq!(encoded.body, fixture("response-encoded"));
    assert_eq!(
        encoded
            .losses
            .iter()
            .map(|loss| loss.path.as_str())
            .collect::<Vec<_>>(),
        ["/content/3", "/content/4"]
    );
}

#[test]
fn incomplete_responses_are_normal_but_failures_and_malformed_successes_are_decode_errors() {
    let bridge = ResponsesBridge::new();
    let value = fixture("response-incomplete");
    let output = bridge
        .decode_response(&bytes(&value))
        .expect("incomplete response");
    assert_eq!(output.response.stop_reason, StopReason::MaxTokens);
    assert_eq!(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect("encode")
            .body,
        value
    );
    for (name, expected) in [
        ("response-content-filter", StopReason::ContentFilter),
        (
            "response-max-messages",
            StopReason::Other("max_messages".into()),
        ),
        ("response-steered", StopReason::Other("steered".into())),
    ] {
        let value = fixture(name);
        let output = bridge
            .decode_response(&bytes(&value))
            .expect("normal incomplete response");
        assert_eq!(output.response.stop_reason, expected);
        assert_eq!(
            bridge
                .encode_response(&output, &ConversionOptions::default())
                .expect("exact SDK reason")
                .body,
            value
        );
    }
    let mut future = value.clone();
    future["incomplete_details"]["reason"] = json!("future_reason");
    let future_output = bridge
        .decode_response(&bytes(&future))
        .expect("normal stop");
    assert_eq!(
        future_output.response.stop_reason,
        StopReason::Other("future_reason".into())
    );
    conversion(
        bridge
            .encode_response(&future_output, &ConversionOptions::default())
            .expect_err("unknown target reason"),
        ConversionErrorKind::Unrepresentable,
        "/stop_reason",
    );
    for value in [fixture("response-failed"), fixture("response-error")] {
        let BridgeError::Decode(error) = bridge
            .decode_response(&bytes(&value))
            .expect_err("adapter boundary")
        else {
            panic!("expected Decode");
        };
        assert!(error.message.contains("adapter should have removed"));
    }
    for body in [b"not JSON".as_slice(), b"{}", b"{\"output\":null}"] {
        assert!(matches!(
            bridge.decode_response(body),
            Err(BridgeError::Decode(_))
        ));
    }
}

#[test]
fn gateway_requires_missing_facts_and_accepts_explicit_ids_and_creation_times() {
    let bridge = ResponsesBridge::new();
    let output = bridge
        .decode_response(&bytes(&fixture("response-incomplete")))
        .expect("response");
    let native = output
        .native_for(&ApiFormat::RESPONSES)
        .expect("native")
        .clone();
    for key in [
        "created_at",
        "access_programs",
        "instructions",
        "metadata",
        "parallel_tool_calls",
        "temperature",
        "tool_choice",
        "tools",
        "top_p",
    ] {
        let mut native = native.clone();
        native.remove(key);
        let missing = Output::with_native(ApiFormat::RESPONSES, output.response.clone(), native);
        conversion(
            bridge
                .encode_response(&missing, &ConversionOptions::default())
                .expect_err(key),
            ConversionErrorKind::Unrepresentable,
            &format!("/{key}"),
        );
    }
    let mut no_id = output.clone();
    no_id.response.id = None;
    conversion(
        bridge
            .encode_response(&no_id, &ConversionOptions::default())
            .expect_err("missing ID"),
        ConversionErrorKind::Unrepresentable,
        "/id",
    );
    let mut explicit_native = native.clone();
    explicit_native.insert("id".into(), json!("resp_incomplete"));
    explicit_native.remove("created_at");
    no_id.response.id = Some("must_not_override_native".into());
    let explicit = Output::with_native(ApiFormat::RESPONSES, no_id.response, explicit_native);
    assert_eq!(
        bridge
            .encode_response(
                &explicit,
                &ConversionOptions {
                    created_at: Some(1700000000),
                    ..Default::default()
                }
            )
            .expect("explicit context")
            .body,
        fixture("response-incomplete")
    );
    let mut unknown_stop = output.clone();
    unknown_stop.response.stop_reason = StopReason::ContextWindowExceeded;
    conversion(
        bridge
            .encode_response(&unknown_stop, &ConversionOptions::default())
            .expect_err("no exact mapping"),
        ConversionErrorKind::Unrepresentable,
        "/stop_reason",
    );
    let value = fixture("response-native-time");
    let timed = bridge
        .decode_response(&bytes(&value))
        .expect("native timestamp");
    assert_eq!(
        bridge
            .encode_response(
                &timed,
                &ConversionOptions {
                    created_at: Some(1),
                    ..Default::default()
                }
            )
            .expect("native time wins without truncation")
            .body,
        value
    );
}

#[test]
fn gateway_usage_requires_unknown_counters_instead_of_assuming_zero() {
    let bridge = ResponsesBridge::new();
    let mut output = bridge
        .decode_response(&bytes(&fixture("response-encoded")))
        .expect("response");
    output
        .response
        .usage
        .as_mut()
        .expect("usage")
        .cache_read_tokens = None;
    conversion(
        bridge
            .encode_response(&output, &ConversionOptions::default())
            .expect_err("unknown cached count"),
        ConversionErrorKind::Unrepresentable,
        "/usage/input_tokens_details/cached_tokens",
    );
    let mut native = output
        .native_for(&ApiFormat::RESPONSES)
        .expect("native")
        .clone();
    native["usage"]["input_tokens_details"]["cached_tokens"] = json!(5);
    let explicit = Output::with_native(ApiFormat::RESPONSES, output.response, native);
    assert_eq!(
        bridge
            .encode_response(&explicit, &ConversionOptions::default())
            .expect("explicit count")
            .body,
        fixture("response-encoded")
    );
}

fn usage() -> Usage {
    Usage {
        input_tokens: 20,
        output_tokens: 9,
        cache_read_tokens: Some(5),
        cache_write_tokens: Some(2),
        reasoning_tokens: Some(4),
    }
}

fn events() -> Vec<StreamEvent> {
    vec![
        StreamEvent::ResponseStart {
            id: Some("resp_stream".into()),
            model: Some("gpt-5-nano".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            item: StreamItem::Reasoning {
                id: Some("rs_stream".into()),
            },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningText {
                text: "Thinking".into(),
            },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningSummary {
                summary_index: 0,
                text: "One".into(),
            },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningSummary {
                summary_index: 1,
                text: "Two".into(),
            },
        },
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::ReasoningOpaque {
                value: "opaque-value".into(),
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
                text: "Answer".into(),
            },
        },
        StreamEvent::ItemEnd { index: 1 },
        StreamEvent::ItemStart {
            index: 2,
            item: StreamItem::ToolCall {
                id: Some("call_stream".into()),
                name: Some("lookup".into()),
            },
        },
        StreamEvent::Delta {
            index: 2,
            delta: StreamDelta::ToolCallArguments {
                text: "{ \"key\":".into(),
            },
        },
        StreamEvent::Delta {
            index: 2,
            delta: StreamDelta::ToolCallArguments {
                text: "1, \"key\": 2 }".into(),
            },
        },
        StreamEvent::ItemEnd { index: 2 },
        StreamEvent::Stop {
            stop_reason: StopReason::ToolUse,
            usage: Some(usage()),
        },
    ]
}

#[test]
fn stream_decoding_preserves_ids_indices_raw_deltas_and_only_completed_opaque_reasoning() {
    let mut decoder = ResponsesBridge::new().stream_decoder();
    let mut out = Vec::new();
    for number in 0..27 {
        let mut value = fixture(&format!("stream-{number:02}"));
        if number == 1 {
            value["item"]["encrypted_content"] = json!("incomplete-do-not-replay");
        }
        if let Some(index) = value.get_mut("output_index") {
            *index = json!(index.as_u64().expect("index") * 7 + 5);
        }
        decoder.push(frame(value), &mut out).expect("event");
    }
    decoder.finish(&mut out).expect("terminal event");
    assert_eq!(out, events());
}

#[test]
fn complete_text_echoes_fill_missing_deltas_before_a_later_provider_failure() {
    let mut decoder = ResponsesBridge::new().stream_decoder();
    let mut out = Vec::new();
    for number in 0..=18 {
        if number != 17 {
            decoder
                .push(frame(fixture(&format!("stream-{number:02}"))), &mut out)
                .expect("event");
        }
    }
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Delta {
            index: 1,
            delta: StreamDelta::Text {
                text: "Answer".into()
            }
        })
    );
    assert!(matches!(
        decoder.push(frame(fixture("stream-error")), &mut out),
        Err(BridgeError::Decode(_))
    ));
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Delta {
            index: 1,
            delta: StreamDelta::Text {
                text: "Answer".into()
            }
        })
    );
}

#[test]
fn final_item_snapshots_fill_missing_reasoning_and_argument_suffixes_without_duplicates() {
    let mut decoder = ResponsesBridge::new().stream_decoder();
    let mut out = Vec::new();
    for number in 0..27 {
        if ![3, 5, 7, 8, 10, 12, 17, 18, 23, 24].contains(&number) {
            decoder
                .push(frame(fixture(&format!("stream-{number:02}"))), &mut out)
                .expect("event");
        }
    }
    decoder.finish(&mut out).expect("complete");
    assert_eq!(out, events());
}

#[test]
fn incomplete_stream_stops_normally_and_eof_and_adapter_failures_never_synthesize_completion() {
    let bridge = ResponsesBridge::new();
    let mut decoder = bridge.stream_decoder();
    let mut out = Vec::new();
    for number in 0..26 {
        decoder
            .push(frame(fixture(&format!("stream-{number:02}"))), &mut out)
            .expect("event");
    }
    decoder
        .push(frame(fixture("stream-incomplete")), &mut out)
        .expect("incomplete is normal");
    decoder.finish(&mut out).expect("terminal");
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Stop {
            stop_reason: StopReason::MaxTokens,
            usage: Some(usage())
        })
    );
    for failure in [None, Some("stream-error"), Some("stream-failed")] {
        let mut decoder = bridge.stream_decoder();
        let mut out = Vec::new();
        decoder
            .push(frame(fixture("stream-00")), &mut out)
            .expect("created");
        if let Some(failure) = failure {
            let BridgeError::Decode(error) = decoder
                .push(frame(fixture(failure)), &mut out)
                .expect_err("adapter boundary")
            else {
                panic!("expected Decode");
            };
            assert!(error.message.contains("adapter should have removed"));
        }
        assert!(matches!(
            decoder.finish(&mut out),
            Err(BridgeError::IncompleteStream(_))
        ));
        assert_eq!(out, vec![events()[0].clone()]);
    }
}

#[test]
fn unknown_output_is_preserved_while_lifecycle_events_carry_no_duplicate_content() {
    let mut decoder = ResponsesBridge::new().stream_decoder();
    let mut out = Vec::new();
    decoder
        .push(frame(fixture("stream-00")), &mut out)
        .expect("created");
    let hosted = fixture("response-full")["output"][4].clone();
    for name in [
        "stream-hosted-added",
        "stream-hosted-progress",
        "stream-hosted-done",
    ] {
        decoder
            .push(frame(fixture(name)), &mut out)
            .expect("hosted event");
    }
    decoder
        .push(
            frame(json!({"type":"response.future_audio.delta","sequence_number":4,"output_index":77,"data":"audio"})),
            &mut out,
        )
        .expect("new output event");
    assert_eq!(
        out[1],
        StreamEvent::ItemStart {
            index: 0,
            item: StreamItem::Unknown
        }
    );
    assert_eq!(
        out[2],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown { value: hosted }
        }
    );
    assert_eq!(out[3], StreamEvent::ItemEnd { index: 0 });
    assert!(
        matches!(&out[5], StreamEvent::Delta { index: 1, delta: StreamDelta::Unknown { value } } if value["data"] == "audio")
    );
    assert_eq!(out.len(), 7);
    let mut decoder = ResponsesBridge::new().stream_decoder();
    let mut out = Vec::new();
    for name in ["stream-00", "stream-refusal-added", "stream-refusal-done"] {
        decoder
            .push(frame(fixture(name)), &mut out)
            .expect("refusal event");
    }
    assert_eq!(
        out[2],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("response-full")["output"][3]["content"][0].clone()
            }
        }
    );
    assert_eq!(out[3], StreamEvent::ItemEnd { index: 0 });
    assert_eq!(out.len(), 4);
}

fn gateway() -> ResponsesBridge {
    let mut native = fixture("stream-00")["response"]
        .as_object()
        .expect("response")
        .clone();
    for key in ["id", "model", "created_at", "status"] {
        native.remove(key);
    }
    native.insert(
        "output".into(),
        json!([{}, {"id":"msg_stream","status":"completed"}, {"id":"fc_stream"}]),
    );
    ResponsesBridge::with_config(ResponsesBridgeConfig {
        store: None,
        stream_native: native,
    })
}

#[test]
fn gateway_stream_frames_match_sdk_checked_shapes_and_decode_back_to_the_original_events() {
    let options = ConversionOptions {
        created_at: Some(1700000000),
        ..Default::default()
    };
    for (stop_reason, terminal) in [
        (StopReason::ToolUse, "stream-26"),
        (StopReason::MaxTokens, "stream-incomplete"),
    ] {
        let mut expected = events();
        *expected.last_mut().expect("stop") = StreamEvent::Stop {
            stop_reason,
            usage: Some(usage()),
        };
        let mut encoder = gateway().stream_encoder(&options);
        let mut frames = Vec::new();
        let mut losses = Vec::new();
        for event in &expected {
            encoder
                .push(event, &mut frames, &mut losses)
                .expect("encode event");
        }
        encoder.finish(&mut frames, &mut losses).expect("complete");
        assert_eq!(frames.len(), 27);
        assert!(losses.is_empty());
        let mut decoder = ResponsesBridge::new().stream_decoder();
        let mut decoded = Vec::new();
        for (number, frame) in frames.into_iter().enumerate() {
            let value: Value = serde_json::from_str(&frame.data).expect("frame JSON");
            assert_eq!(frame.event.as_deref(), value["type"].as_str());
            let name = if number == 26 {
                terminal.into()
            } else {
                format!("stream-{number:02}")
            };
            assert_eq!(value, fixture(&name));
            decoder
                .push(frame, &mut decoded)
                .expect("decode generated frame");
        }
        decoder.finish(&mut decoded).expect("complete");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn stream_encoding_reports_drops_immediately_and_rejects_missing_ids_or_context() {
    let options = ConversionOptions {
        created_at: Some(1700000000),
        allow_loss: [LossScope::Reasoning, LossScope::UnknownContent].into(),
    };
    let mut encoder = gateway().stream_encoder(&options);
    let mut out = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(&events()[0], &mut out, &mut losses)
        .expect("start");
    for (index, item) in [
        (0, StreamItem::Reasoning { id: None }),
        (1, StreamItem::Unknown),
    ] {
        encoder
            .push(
                &StreamEvent::ItemStart { index, item },
                &mut out,
                &mut losses,
            )
            .expect("explicit drop");
        assert_eq!(losses.len(), usize::try_from(index + 1).expect("count"));
        assert_eq!(losses.last().expect("loss").path, format!("/items/{index}"));
        encoder
            .push(&StreamEvent::ItemEnd { index }, &mut out, &mut losses)
            .expect("end omitted item");
    }
    encoder
        .push(
            &StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
            &mut out,
            &mut losses,
        )
        .expect("stop");
    encoder.finish(&mut out, &mut losses).expect("complete");
    assert_eq!(out.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&out[0].data).expect("created"),
        fixture("stream-00")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&out[1].data).expect("completed"),
        fixture("stream-loss-stop")
    );
    let strict = ConversionOptions {
        created_at: Some(1700000000),
        ..Default::default()
    };
    let mut encoder = gateway().stream_encoder(&strict);
    let mut out = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(&events()[0], &mut out, &mut losses)
        .expect("start");
    conversion(
        encoder
            .push(
                &StreamEvent::ItemStart {
                    index: 0,
                    item: StreamItem::Reasoning { id: None },
                },
                &mut out,
                &mut losses,
            )
            .expect_err("identity"),
        ConversionErrorKind::Unrepresentable,
        "/items/0",
    );
    conversion(
        encoder.finish(&mut out, &mut losses).expect_err("no stop"),
        ConversionErrorKind::Unrepresentable,
        "/stop_reason",
    );
    let mut encoder = ResponsesBridge::new().stream_encoder(&strict);
    conversion(
        encoder
            .push(&events()[0], &mut out, &mut losses)
            .expect_err("no context"),
        ConversionErrorKind::Unrepresentable,
        "/access_programs",
    );
}
