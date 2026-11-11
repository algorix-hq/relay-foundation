use relay_bridge::{
    Bridge, BridgeError, ConversionOptions, DecodeContext, Frame, LossScope, ResponsesBridge,
    responses::ResponsesBridgeConfig,
};
use relay_protocol::{
    AssistantContent, ConversionErrorKind, Delivery, Message, MetadataScope, Request, StopReason,
    StreamDelta, StreamEvent, StreamItem, Timestamp, Usage,
};
use serde_json::{Value, json};

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

fn input(extra: Value) -> Request {
    let mut value = json!({"version":"1", "model":"gpt-5-nano", "messages":[]});
    value
        .as_object_mut()
        .expect("request")
        .extend(extra.as_object().expect("fields").clone());
    serde_json::from_value(value).expect("protocol request")
}

fn context() -> DecodeContext {
    DecodeContext {
        response_id: Some("fallback".into()),
        observed_at: 1700000001,
    }
}

fn conversion(error: BridgeError, kind: ConversionErrorKind, path: &str) {
    let BridgeError::Conversion(error) = error else {
        panic!("expected Conversion: {error}")
    };
    assert_eq!(error.kind, kind);
    assert_eq!(error.path, path);
}

#[test]
fn ingress_normalizes_instructions_and_preserves_tools_media_and_reasoning() {
    let bridge = ResponsesBridge::new();
    let request = bridge
        .decode_request(&bytes(&fixture("request-ingress")))
        .expect("ingress");
    assert!(matches!(request.messages[0], Message::System { .. }));
    assert_eq!(
        request.delivery,
        Some(Delivery {
            stream: true,
            stream_usage: false
        })
    );
    let encoded = bridge
        .encode_request(&request, &ConversionOptions::default())
        .expect("encode");
    assert_eq!(encoded.body, fixture("request-encoded"));
    assert!(encoded.losses.is_empty());
    let request = bridge
        .decode_request(&bytes(&json!({"model":"m", "input":"hello"})))
        .expect("string input");
    assert!(
        matches!(&request.messages[0], Message::User { content } if matches!(&content[0], relay_protocol::UserContent::Text(part) if part.text == "hello"))
    );
}

#[test]
fn ingress_rejects_unrepresented_fields_and_reports_malformed_fields() {
    use ConversionErrorKind::{Invalid, Unrepresentable};
    let cases = [
        (
            json!({"previous_response_id":"resp_previous"}),
            Unrepresentable,
            "/previous_response_id",
        ),
        (
            json!({"include":["reasoning.encrypted_content"]}),
            Unrepresentable,
            "/include",
        ),
        (json!({"store":false}), Unrepresentable, "/store"),
        (json!({"metadata":{}}), Unrepresentable, "/metadata"),
        (
            json!({"future_option":true}),
            Unrepresentable,
            "/future_option",
        ),
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
            json!({"input":[{"role":"assistant","content":"x","phase":"commentary"}]}),
            Unrepresentable,
            "/input/0/phase",
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
            ResponsesBridge::new()
                .decode_request(&bytes(&body))
                .expect_err(path),
            kind,
            path,
        );
    }
    conversion(
        ResponsesBridge::new()
            .decode_request(b"not JSON")
            .expect_err("malformed"),
        Invalid,
        "",
    );
}

#[test]
fn encoding_rejects_unsupported_semantics_and_missing_schema_names() {
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
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"relay:0","summary":["one"]}]}]}),
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","id":"relay:0","value":{"type":"future"}}]}]}),
            "/messages/0/content/0",
        ),
        (
            json!({"messages":[{"role":"user","content":[{"type":"document","source":{"type":"url","url":"https://example.com/a.pdf"},"title":"Title"}]}]}),
            "/messages/0/content/0/title",
        ),
        (
            json!({"response_format":{"type":"json_schema","schema":{}}}),
            "/response_format/name",
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
fn empty_messages_stored_reasoning_and_simple_formats_keep_their_wire_structure() {
    for name in [
        "request-empty-replay",
        "request-format-text",
        "request-format-json-object",
    ] {
        let bridge = ResponsesBridge::new();
        let value = fixture(name);
        let request = bridge.decode_request(&bytes(&value)).expect("ingress");
        assert_eq!(
            bridge
                .encode_request(&request, &ConversionOptions::default())
                .expect("encode")
                .body,
            value
        );
    }
}

#[test]
fn explicit_losses_drop_only_allowed_content_and_reasoning_replays_only_provider_ids() {
    for (scope, request, path, expected) in [
        (
            LossScope::CacheControl,
            json!({"messages":[{"role":"system","content":[{"type":"text","text":"policy","cache_control":{}}]}]}),
            "/messages/0/content/0/cache_control",
            "request-text",
        ),
        (
            LossScope::Reasoning,
            json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"relay:0","text":"thinking","opaque":"opaque"}]}]}),
            "/messages/0/content/0",
            "request-store-default",
        ),
        (
            LossScope::UnknownContent,
            json!({"messages":[{"role":"assistant","content":[{"type":"unknown","id":"relay:0","value":{"type":"future"}}]}]}),
            "/messages/0/content/0",
            "request-store-default",
        ),
    ] {
        let encoded = ResponsesBridge::new()
            .encode_request(
                &input(request),
                &ConversionOptions {
                    allow_loss: [scope].into(),
                },
            )
            .expect("allowed omission");
        assert_eq!(encoded.body, fixture(expected));
        assert_eq!(encoded.losses.len(), 1);
        assert_eq!(encoded.losses[0].path, path);
    }
    let request = input(
        json!({"messages":[{"role":"assistant","content":[{"type":"reasoning","id":"not-the-provider-id","provider_id":"rs_original","summary":[]}]}]}),
    );
    let encoded = ResponsesBridge::new()
        .encode_request(&request, &ConversionOptions::default())
        .expect("provider replay");
    assert_eq!(encoded.body["input"][0]["id"], "rs_original");
}

#[test]
fn store_is_only_a_constructor_option_and_usage_delivery_does_not_add_include() {
    let mut request = input(json!({}));
    for (store, expected) in [
        (None, "request-store-default"),
        (Some(false), "request-store-false"),
        (Some(true), "request-store-true"),
    ] {
        let bridge = ResponsesBridge::with_config(ResponsesBridgeConfig { store });
        let encoded = bridge
            .encode_request(&request, &ConversionOptions::default())
            .expect("store");
        assert_eq!(encoded.body, fixture(expected));
    }
    request.delivery = Some(Delivery {
        stream: true,
        stream_usage: true,
    });
    let body = ResponsesBridge::new()
        .encode_request(&request, &ConversionOptions::default())
        .expect("delivery")
        .body;
    assert_eq!(body["stream"], true);
    assert!(body.get("include").is_none());
    assert!(body.get("stream_options").is_none());
}

#[test]
fn responses_preserve_item_identity_calls_reasoning_unknown_output_and_usage() {
    let bridge = ResponsesBridge::new();
    let value = fixture("response-full");
    let output = bridge
        .decode_response(&bytes(&value), &context())
        .expect("response");
    assert_eq!(output.id, "resp_1");
    assert_eq!(output.provider_id.as_deref(), Some("resp_1"));
    assert_eq!(
        output.created_at,
        Timestamp {
            observed_at: 1700000001,
            provider_created_at: Some(1700000000)
        }
    );
    assert_eq!(output.stop_reason, StopReason::ToolUse);
    assert_eq!(output.content.len(), 5);
    assert!(
        matches!(&output.content[0], AssistantContent::Text { id, provider_id, text, .. } if id == "msg_1" && provider_id.as_deref() == Some("msg_1") && text == "Answer")
    );
    assert!(
        matches!(&output.content[1], AssistantContent::ToolCall { id, provider_id, call_id, name, arguments, .. } if id == "fc_1" && provider_id.as_deref() == Some("fc_1") && call_id == "call_1" && name == "lookup" && arguments == "{ \"key\":1, \"key\": 2 }")
    );
    assert!(
        matches!(&output.content[2], AssistantContent::Reasoning { id, provider_id, text, summary, opaque } if id == "rs_1" && provider_id.as_deref() == Some("rs_1") && text.as_deref() == Some("Thinking") && summary == &["One", "Two"] && opaque.as_deref() == Some("opaque-value"))
    );
    assert_eq!(
        output.content[3],
        AssistantContent::Unknown {
            id: "msg_refusal".into(),
            provider_id: Some("msg_refusal".into()),
            value: value["output"][3]["content"][0].clone()
        }
    );
    assert_eq!(
        output.content[4],
        AssistantContent::Unknown {
            id: "ws_1".into(),
            provider_id: Some("ws_1".into()),
            value: value["output"][4].clone()
        }
    );
    assert_eq!(output.usage, Some(usage()));
    conversion(
        bridge
            .encode_response(&input(json!({})), &output, &ConversionOptions::default())
            .expect_err("unknown content"),
        ConversionErrorKind::Unrepresentable,
        "/content/3",
    );
    let encoded = bridge
        .encode_response(
            &input(json!({})),
            &output,
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
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
fn absent_ids_use_context_and_item_indices_and_client_encoding_uses_relay_ids_and_time() {
    let bridge = ResponsesBridge::new();
    let mut value = fixture("response-full");
    value.as_object_mut().expect("response").remove("id");
    value
        .as_object_mut()
        .expect("response")
        .remove("created_at");
    for item in value["output"].as_array_mut().expect("items") {
        item.as_object_mut().expect("item").remove("id");
    }
    let mut output = bridge
        .decode_response(&bytes(&value), &context())
        .expect("fallback");
    assert_eq!(output.id, "fallback");
    assert_eq!(output.provider_id, None);
    assert!(
        matches!(&output.content[1], AssistantContent::ToolCall { id, provider_id: None, call_id, .. } if id == "fallback:1" && call_id == "call_1")
    );
    assert!(
        matches!(&output.content[2], AssistantContent::Reasoning { id, provider_id: None, .. } if id == "fallback:2")
    );
    output.id = "relay-response".into();
    let encoded = bridge
        .encode_response(
            &input(json!({})),
            &output,
            &ConversionOptions {
                allow_loss: [LossScope::UnknownContent].into(),
            },
        )
        .expect("relay identity");
    assert_eq!(encoded.body["id"], "relay-response");
    assert_eq!(encoded.body["created_at"], 1700000001);
    assert_eq!(encoded.body["output"][0]["id"], "fallback:0");
    assert_eq!(encoded.body["output"][1]["id"], "fallback:1");
    assert!(matches!(
        bridge.decode_response(
            &bytes(&value),
            &DecodeContext {
                response_id: None,
                observed_at: 1
            }
        ),
        Err(BridgeError::Decode(_))
    ));
}

#[test]
fn fractional_provider_timestamps_fail_instead_of_being_truncated_or_dropped() {
    let bridge = ResponsesBridge::new();
    let error = bridge
        .decode_response(&bytes(&fixture("response-fractional-time")), &context())
        .expect_err("fractional timestamp");
    assert!(matches!(error, BridgeError::Decode(error) if error.path == "/created_at"));
    for name in ["stream-00", "stream-in-progress", "stream-26"] {
        let mut decoder = bridge.stream_decoder(&context());
        let mut out = Vec::new();
        if name != "stream-00" {
            decoder
                .push(frame(fixture("stream-00")), &mut out)
                .expect("integer timestamp");
        }
        let before = out.clone();
        let mut value = fixture(name);
        value["response"]["created_at"] = json!(1700000000.25);
        let error = decoder
            .push(frame(value), &mut out)
            .expect_err("fractional stream timestamp");
        assert!(
            matches!(error, BridgeError::Decode(error) if error.path == "/response/created_at")
        );
        assert_eq!(out, before);
    }
}

#[test]
fn client_response_echoes_use_the_same_settings_conversion_as_request_encoding() {
    let bridge = ResponsesBridge::new();
    let request = bridge
        .decode_request(&bytes(&fixture("request-ingress")))
        .expect("client request");
    let output = bridge
        .decode_response(&bytes(&fixture("response-incomplete")), &context())
        .expect("response");
    let outgoing = bridge
        .encode_request(&request, &ConversionOptions::default())
        .expect("provider request");
    let encoded = bridge
        .encode_response(&request, &output, &ConversionOptions::default())
        .expect("client response");
    for key in [
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "max_output_tokens",
        "reasoning",
        "text",
    ] {
        assert_eq!(encoded.body[key], outgoing.body[key], "{key}");
    }
    assert_eq!(encoded.body, fixture("response-echo"));
    assert_eq!(encoded.body["instructions"], Value::Null);
    assert_eq!(encoded.body["access_programs"], Value::Null);
    assert_eq!(encoded.body["metadata"], json!({}));
    assert!(encoded.body.get("previous_response_id").is_none());
}

#[test]
fn incomplete_responses_are_normal_but_failures_and_malformed_successes_are_decode_errors() {
    let bridge = ResponsesBridge::new();
    for (name, expected) in [
        ("response-incomplete", StopReason::MaxTokens),
        ("response-content-filter", StopReason::ContentFilter),
        (
            "response-max-messages",
            StopReason::Other("max_messages".into()),
        ),
        ("response-steered", StopReason::Other("steered".into())),
    ] {
        let output = bridge
            .decode_response(&bytes(&fixture(name)), &context())
            .expect("normal incomplete");
        assert_eq!(output.stop_reason, expected);
        let encoded = bridge
            .encode_response(&input(json!({})), &output, &ConversionOptions::default())
            .expect("exact reason");
        assert_eq!(
            encoded.body["incomplete_details"],
            fixture(name)["incomplete_details"]
        );
        assert_eq!(encoded.body["status"], "incomplete");
    }
    let mut future = fixture("response-incomplete");
    future["incomplete_details"]["reason"] = json!("future_reason");
    let output = bridge
        .decode_response(&bytes(&future), &context())
        .expect("normal stop");
    assert_eq!(
        output.stop_reason,
        StopReason::Other("future_reason".into())
    );
    conversion(
        bridge
            .encode_response(&input(json!({})), &output, &ConversionOptions::default())
            .expect_err("unknown target reason"),
        ConversionErrorKind::Unrepresentable,
        "/stop_reason",
    );
    for name in ["response-failed", "response-error"] {
        assert!(
            matches!(bridge.decode_response(&bytes(&fixture(name)), &context()), Err(BridgeError::Decode(error)) if error.message.contains("removed by the adapter"))
        );
    }
    for body in [b"not JSON".as_slice(), b"{}", b"{\"output\":null}"] {
        assert!(matches!(
            bridge.decode_response(body, &context()),
            Err(BridgeError::Decode(_))
        ));
    }
}

#[test]
fn gateway_usage_requires_unknown_required_counters_instead_of_assuming_zero() {
    let bridge = ResponsesBridge::new();
    let mut output = bridge
        .decode_response(&bytes(&fixture("response-encoded")), &context())
        .expect("response");
    output.usage.as_mut().expect("usage").cache_read_tokens = None;
    conversion(
        bridge
            .encode_response(&input(json!({})), &output, &ConversionOptions::default())
            .expect_err("unknown cached count"),
        ConversionErrorKind::Unrepresentable,
        "/usage/input_tokens_details/cached_tokens",
    );
    output.usage.as_mut().expect("usage").cache_read_tokens = Some(5);
    output.usage.as_mut().expect("usage").reasoning_tokens = None;
    conversion(
        bridge
            .encode_response(&input(json!({})), &output, &ConversionOptions::default())
            .expect_err("unknown reasoning count"),
        ConversionErrorKind::Unrepresentable,
        "/usage/output_tokens_details/reasoning_tokens",
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
        StreamEvent::Start {
            response_id: "resp_stream".into(),
            provider_id: Some("resp_stream".into()),
            model: Some("gpt-5-nano".into()),
            created_at: Timestamp {
                observed_at: 1700000001,
                provider_created_at: Some(1700000000),
            },
        },
        StreamEvent::ItemStart {
            index: 0,
            id: "rs_stream".into(),
            provider_id: Some("rs_stream".into()),
            item: StreamItem::Reasoning,
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
            id: "msg_stream".into(),
            provider_id: Some("msg_stream".into()),
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
            id: "fc_stream".into(),
            provider_id: Some("fc_stream".into()),
            item: StreamItem::ToolCall {
                call_id: Some("call_stream".into()),
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
fn stream_decoding_preserves_ids_order_and_only_completed_opaque_reasoning() {
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
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
fn complete_echoes_fill_missing_deltas_before_a_later_failure_without_completion() {
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
    let mut out = Vec::new();
    for number in 0..=18 {
        if number != 17 {
            decoder
                .push(frame(fixture(&format!("stream-{number:02}"))), &mut out)
                .expect("event");
        }
    }
    let expected = StreamEvent::Delta {
        index: 1,
        delta: StreamDelta::Text {
            text: "Answer".into(),
        },
    };
    assert_eq!(out.last(), Some(&expected));
    assert!(matches!(
        decoder.push(frame(fixture("stream-error")), &mut out),
        Err(BridgeError::Decode(_))
    ));
    assert_eq!(out.last(), Some(&expected));
}

#[test]
fn item_snapshots_fill_missing_reasoning_and_argument_suffixes_without_duplicates() {
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
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
    let mut decoder = bridge.stream_decoder(&context());
    let mut out = Vec::new();
    for number in 0..26 {
        decoder
            .push(frame(fixture(&format!("stream-{number:02}"))), &mut out)
            .expect("event");
    }
    decoder
        .push(frame(fixture("stream-incomplete")), &mut out)
        .expect("normal incomplete");
    decoder.finish(&mut out).expect("terminal");
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Stop {
            stop_reason: StopReason::MaxTokens,
            usage: Some(usage())
        })
    );
    for failure in [None, Some("stream-error"), Some("stream-failed")] {
        let mut decoder = bridge.stream_decoder(&context());
        let mut out = Vec::new();
        decoder
            .push(frame(fixture("stream-00")), &mut out)
            .expect("created");
        if let Some(failure) = failure {
            assert!(
                matches!(decoder.push(frame(fixture(failure)), &mut out), Err(BridgeError::Decode(error)) if error.message.contains("removed by the adapter"))
            );
        }
        assert!(matches!(
            decoder.finish(&mut out),
            Err(BridgeError::IncompleteStream(_))
        ));
        assert_eq!(out, vec![events()[0].clone()]);
    }
}

#[test]
fn unknown_output_is_preserved_once_and_new_output_events_get_standalone_items() {
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
    let mut out = Vec::new();
    decoder
        .push(frame(fixture("stream-00")), &mut out)
        .expect("created");
    for name in [
        "stream-hosted-added",
        "stream-hosted-progress",
        "stream-hosted-done",
    ] {
        decoder
            .push(frame(fixture(name)), &mut out)
            .expect("hosted event");
    }
    decoder.push(frame(json!({"type":"response.future_audio.delta","sequence_number":4,"output_index":77,"data":"audio"})), &mut out).expect("future event");
    assert_eq!(
        out[1],
        StreamEvent::ItemStart {
            index: 0,
            id: "ws_1".into(),
            provider_id: Some("ws_1".into()),
            item: StreamItem::Unknown
        }
    );
    assert_eq!(
        out[2],
        StreamEvent::Delta {
            index: 0,
            delta: StreamDelta::Unknown {
                value: fixture("response-full")["output"][4].clone()
            }
        }
    );
    assert_eq!(out[3], StreamEvent::ItemEnd { index: 0 });
    assert_eq!(
        out[4],
        StreamEvent::ItemStart {
            index: 1,
            id: "resp_stream:1".into(),
            provider_id: None,
            item: StreamItem::Unknown
        }
    );
    assert!(
        matches!(&out[5], StreamEvent::Delta { index: 1, delta: StreamDelta::Unknown { value } } if value["data"] == "audio")
    );
    assert_eq!(out.len(), 7);
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
    let mut out = Vec::new();
    for name in ["stream-00", "stream-refusal-added", "stream-refusal-done"] {
        decoder
            .push(frame(fixture(name)), &mut out)
            .expect("refusal");
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

#[test]
fn late_call_identity_is_item_metadata_and_missing_item_id_uses_start_order() {
    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
    let mut out = Vec::new();
    decoder
        .push(frame(fixture("stream-00")), &mut out)
        .expect("created");
    decoder.push(frame(json!({"type":"response.output_item.added","output_index":9,"item":{"type":"function_call","arguments":""}})), &mut out).expect("partial start");
    decoder.push(frame(json!({"type":"response.function_call_arguments.delta","output_index":9,"item_id":"fc_late","call_id":"call_late","name":"lookup","delta":"{}"})), &mut out).expect("late metadata");
    assert_eq!(
        out[1],
        StreamEvent::ItemStart {
            index: 0,
            id: "resp_stream:0".into(),
            provider_id: None,
            item: StreamItem::ToolCall {
                call_id: None,
                name: None
            }
        }
    );
    assert!(out.contains(&StreamEvent::Metadata(MetadataScope::Item {
        index: 0,
        provider_id: None,
        call_id: Some("call_late".into()),
        name: Some("lookup".into())
    })));
    assert!(out.contains(&StreamEvent::Metadata(MetadataScope::Item {
        index: 0,
        provider_id: Some("fc_late".into()),
        call_id: None,
        name: None
    })));

    let mut decoder = ResponsesBridge::new().stream_decoder(&context());
    let mut out = Vec::new();
    decoder
        .push(frame(fixture("stream-00")), &mut out)
        .expect("created");
    decoder.push(frame(json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","arguments":""}})), &mut out).expect("partial start");
    decoder
        .push(frame(fixture("stream-25")), &mut out)
        .expect("completed snapshot");
    assert!(out.contains(&StreamEvent::Metadata(MetadataScope::Item {
        index: 0,
        provider_id: Some("fc_stream".into()),
        call_id: None,
        name: None
    })));
    assert!(out.contains(&StreamEvent::Metadata(MetadataScope::Item {
        index: 0,
        provider_id: None,
        call_id: Some("call_stream".into()),
        name: Some("lookup".into())
    })));
    assert_eq!(out.last(), Some(&StreamEvent::ItemEnd { index: 0 }));
}

#[test]
fn stream_items_can_interleave_and_empty_items_are_not_lost() {
    let bridge = ResponsesBridge::new();
    let expected = vec![
        events()[0].clone(),
        StreamEvent::ItemStart {
            index: 0,
            id: "msg_empty".into(),
            provider_id: Some("msg_empty".into()),
            item: StreamItem::Text,
        },
        StreamEvent::ItemStart {
            index: 1,
            id: "msg_answer".into(),
            provider_id: Some("msg_answer".into()),
            item: StreamItem::Text,
        },
        StreamEvent::Delta {
            index: 1,
            delta: StreamDelta::Text {
                text: "Answer".into(),
            },
        },
        StreamEvent::ItemEnd { index: 1 },
        StreamEvent::ItemEnd { index: 0 },
        StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            usage: None,
        },
    ];
    let mut encoder = bridge.stream_encoder(&input(json!({})), &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in &expected {
        encoder
            .push(event, &mut frames, &mut losses)
            .expect("interleaved event");
    }
    encoder.finish(&mut frames, &mut losses).expect("complete");
    let mut decoder = bridge.stream_decoder(&context());
    let mut actual = Vec::new();
    for frame in frames {
        decoder.push(frame, &mut actual).expect("interleaved frame");
    }
    decoder.finish(&mut actual).expect("complete");
    assert_eq!(actual, expected);
    assert!(losses.is_empty());
}

#[test]
fn gateway_stream_matches_sdk_fixtures_and_decodes_back_to_events() {
    let bridge = ResponsesBridge::new();
    for (stop_reason, terminal) in [
        (StopReason::ToolUse, "stream-26"),
        (StopReason::MaxTokens, "stream-incomplete"),
    ] {
        let mut expected = events();
        *expected.last_mut().expect("stop") = StreamEvent::Stop {
            stop_reason,
            usage: Some(usage()),
        };
        let mut encoder = bridge.stream_encoder(&input(json!({})), &ConversionOptions::default());
        let mut frames = Vec::new();
        let mut losses = Vec::new();
        for event in &expected {
            encoder
                .push(event, &mut frames, &mut losses)
                .expect("encode event");
        }
        encoder.finish(&mut frames, &mut losses).expect("complete");
        assert!(losses.is_empty());
        assert_eq!(frames.len(), 28);
        let mut decoder = bridge.stream_decoder(&context());
        let mut decoded = Vec::new();
        for (number, frame) in frames.into_iter().enumerate() {
            let value: Value = serde_json::from_str(&frame.data).expect("frame JSON");
            assert_eq!(value["sequence_number"], number);
            assert_eq!(frame.event.as_deref(), value["type"].as_str());
            let name = if number == 0 {
                "stream-00".into()
            } else if number == 1 {
                "stream-in-progress".into()
            } else if number == 27 {
                terminal.into()
            } else {
                format!("stream-{:02}", number - 1)
            };
            let mut expected_wire = fixture(&name);
            expected_wire["sequence_number"] = json!(number);
            assert_eq!(value, expected_wire);
            decoder
                .push(frame, &mut decoded)
                .expect("decode generated frame");
        }
        decoder.finish(&mut decoded).expect("complete");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn stream_echoes_share_request_settings_and_provisional_usage_is_not_final() {
    let bridge = ResponsesBridge::new();
    let request = bridge
        .decode_request(&bytes(&fixture("request-ingress")))
        .expect("client request");
    let settings = bridge
        .encode_request(&request, &ConversionOptions::default())
        .expect("request")
        .body;
    let mut encoder = bridge.stream_encoder(&request, &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(&events()[0], &mut frames, &mut losses)
        .expect("start");
    let first: Value = serde_json::from_str(&frames[0].data).expect("JSON");
    assert_eq!(first, fixture("stream-echo"));
    for key in [
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "max_output_tokens",
        "reasoning",
        "text",
    ] {
        assert_eq!(first["response"][key], settings[key], "{key}");
    }
    encoder
        .push(
            &StreamEvent::Metadata(MetadataScope::Response {
                usage: Some(usage()),
                model: None,
            }),
            &mut frames,
            &mut losses,
        )
        .expect("provisional usage");
    encoder
        .push(
            &StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
            &mut frames,
            &mut losses,
        )
        .expect("SDK allows absent usage");
    let final_frame: Value =
        serde_json::from_str(&frames.last().expect("stop").data).expect("JSON");
    assert!(final_frame["response"].get("usage").is_none());
    let mut decoder = bridge.stream_decoder(&context());
    let mut out = Vec::new();
    for frame in frames {
        decoder.push(frame, &mut out).expect("frame");
    }
    decoder.finish(&mut out).expect("complete");
    assert!(
        out.contains(&StreamEvent::Metadata(MetadataScope::Response {
            usage: Some(usage()),
            model: None
        }))
    );
    assert_eq!(
        out.last(),
        Some(&StreamEvent::Stop {
            stop_reason: StopReason::EndTurn,
            usage: None
        })
    );
}

#[test]
fn stream_encoding_reports_unknown_drops_immediately_and_rejects_missing_facts() {
    let bridge = ResponsesBridge::new();
    let options = ConversionOptions {
        allow_loss: [LossScope::UnknownContent].into(),
    };
    let mut encoder = bridge.stream_encoder(&input(json!({})), &options);
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    encoder
        .push(&events()[0], &mut frames, &mut losses)
        .expect("start");
    encoder
        .push(
            &StreamEvent::ItemStart {
                index: 0,
                id: "relay:0".into(),
                provider_id: None,
                item: StreamItem::Unknown,
            },
            &mut frames,
            &mut losses,
        )
        .expect("explicit drop");
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].path, "/items/0");
    encoder
        .push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses)
        .expect("end");
    conversion(
        encoder
            .finish(&mut frames, &mut losses)
            .expect_err("no stop"),
        ConversionErrorKind::Unrepresentable,
        "/stop_reason",
    );
    let mut encoder = bridge.stream_encoder(&input(json!({})), &ConversionOptions::default());
    encoder
        .push(&events()[0], &mut Vec::new(), &mut Vec::new())
        .expect("start");
    conversion(
        encoder
            .push(
                &StreamEvent::ItemStart {
                    index: 0,
                    id: "relay:0".into(),
                    provider_id: None,
                    item: StreamItem::ToolCall {
                        call_id: None,
                        name: Some("lookup".into()),
                    },
                },
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .expect_err("call id"),
        ConversionErrorKind::Unrepresentable,
        "/items/0/call_id",
    );
}
