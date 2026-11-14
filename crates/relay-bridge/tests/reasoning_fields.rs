use relay_bridge::{
    Bridge, BridgeError, ConversionOptions, DecodeContext, Frame, LossScope,
    chat_completions::{ChatCompletionsBridge, MaxTokensField, ReasoningField},
};
use relay_protocol::*;
use serde_json::{Value, json};

fn fields() -> [(ReasoningField, &'static str, &'static str); 2] {
    [
        (
            ReasoningField::ReasoningContent,
            "reasoning_content",
            "reasoning",
        ),
        (ReasoningField::Reasoning, "reasoning", "reasoning_content"),
    ]
}

fn context() -> DecodeContext {
    DecodeContext {
        response_id: Some("fallback".into()),
        observed_at: 20,
    }
}

fn request(content: Value) -> Request {
    serde_json::from_value(json!({
        "version":"1","model":"m",
        "messages":[{"role":"assistant","content":content}],
        "parameters":{"max_output_tokens":32},
        "reasoning":{"effort":"high"},
        "delivery":{"stream":true,"stream_usage":true}
    }))
    .expect("protocol request")
}

fn response(content: Value) -> Response {
    serde_json::from_value(json!({
        "version":"1","id":"r","model":"m",
        "created_at":{"observed_at":20,"provider_created_at":10},
        "content":content,"stop_reason":"end_turn",
        "usage":{"input_tokens":11,"output_tokens":7,"reasoning_tokens":5}
    }))
    .expect("protocol response")
}

fn wire_usage() -> Value {
    json!({
        "prompt_tokens":11,"completion_tokens":7,"total_tokens":18,
        "completion_tokens_details":{"reasoning_tokens":5}
    })
}

fn wire_response(message: Value) -> Value {
    json!({
        "id":"r","object":"chat.completion","model":"m","created":10,
        "choices":[{"index":0,"message":message,"finish_reason":"stop"}],
        "usage":wire_usage()
    })
}

fn chunk(delta: Value) -> Frame {
    Frame {
        event: None,
        data: json!({
            "id":"r","object":"chat.completion.chunk","model":"m","created":10,
            "choices":[{"index":0,"delta":delta,"finish_reason":null}]
        })
        .to_string(),
    }
}

fn start() -> StreamEvent {
    StreamEvent::Start {
        response_id: "r".into(),
        provider_id: None,
        created_at: Timestamp {
            observed_at: 20,
            provider_created_at: Some(10),
        },
        model: Some("m".into()),
    }
}

fn reasoning_start(index: u32) -> StreamEvent {
    StreamEvent::ItemStart {
        index,
        id: format!("r:{index}"),
        provider_id: None,
        item: StreamItem::Reasoning,
    }
}

fn reasoning_text(index: u32, text: &str) -> StreamEvent {
    StreamEvent::Delta {
        index,
        delta: StreamDelta::ReasoningText { text: text.into() },
    }
}

fn stop() -> StreamEvent {
    StreamEvent::Stop {
        stop_reason: StopReason::EndTurn,
        usage: response(json!([])).usage,
    }
}

fn chunks(frames: &[Frame]) -> Vec<Value> {
    frames
        .iter()
        .filter(|frame| frame.data != "[DONE]")
        .map(|frame| serde_json::from_str(&frame.data).expect("chunk JSON"))
        .collect()
}

fn assert_unrepresentable(error: BridgeError) {
    assert!(
        matches!(
            error,
            BridgeError::Conversion(ConversionError {
                kind: ConversionErrorKind::Unrepresentable,
                ..
            })
        ),
        "{error:?}"
    );
}

#[test]
fn selected_history_field_preserves_exact_text_and_independent_request_settings() {
    for (field, key, other) in fields() {
        let bridge = ChatCompletionsBridge::with_max_tokens_field(MaxTokensField::MaxTokens)
            .with_reasoning_field(field);
        for text in [" \n考える\t", ""] {
            let mut message = json!({"role":"assistant","content":"answer"});
            message[key] = json!(text);
            let wire = json!({
                "model":"m","messages":[message.clone(), message],
                "reasoning_effort":"high","max_tokens":32
            });
            let decoded = bridge
                .decode_request(&serde_json::to_vec(&wire).expect("JSON"))
                .expect("reasoning history");
            for (index, message) in decoded.messages.iter().enumerate() {
                let Message::Assistant { content } = message else {
                    panic!("assistant history")
                };
                assert_eq!(
                    content,
                    &[
                        AssistantContent::Reasoning {
                            id: format!("/messages/{index}:0"),
                            provider_id: None,
                            text: Some(text.into()),
                            summary: Vec::new(),
                            opaque: None,
                        },
                        AssistantContent::Text {
                            id: format!("/messages/{index}:1"),
                            provider_id: None,
                            text: "answer".into(),
                            cache_control: None,
                        },
                    ]
                );
            }
            assert_eq!(
                decoded.reasoning.as_ref().expect("reasoning").effort,
                Some(ReasoningEffort("high".into()))
            );

            let input = request(json!([
                {"type":"reasoning","id":"thought","text":text},
                {"type":"text","id":"answer","text":"answer"}
            ]));
            let encoded = bridge
                .encode_request(&input, &ConversionOptions::default())
                .expect("encode history");
            assert_eq!(encoded.body["messages"][0][key], text);
            assert!(encoded.body["messages"][0].get(other).is_none());
            assert_eq!(encoded.body["messages"][0]["content"], "answer");
            assert_eq!(encoded.body["reasoning_effort"], "high");
            assert_eq!(encoded.body["max_tokens"], 32);
            assert!(encoded.body.get("max_completion_tokens").is_none());
            assert!(encoded.losses.is_empty());
        }
    }
}

#[test]
fn selected_response_field_preserves_exact_text_and_token_usage() {
    for (field, key, other) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        for text in [" \n考える\t", ""] {
            let mut message = json!({"role":"assistant","content":"answer"});
            message[key] = json!(text);
            let decoded = bridge
                .decode_response(
                    &serde_json::to_vec(&wire_response(message)).expect("JSON"),
                    &context(),
                )
                .expect("reasoning response");
            let expected = response(json!([
                {"type":"reasoning","id":"thought","text":text},
                {"type":"text","id":"answer","text":"answer"}
            ]));
            assert_eq!(decoded.usage, expected.usage);
            assert_eq!(
                decoded
                    .content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::Reasoning { text, .. } => Some(text.as_deref()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                [Some(text)]
            );
            let encoded = bridge
                .encode_response(
                    &request(json!([])),
                    &expected,
                    &ConversionOptions::default(),
                )
                .expect("encode reasoning response");
            assert_eq!(encoded.body["choices"][0]["message"][key], text);
            assert!(encoded.body["choices"][0]["message"].get(other).is_none());
            assert_eq!(encoded.body["choices"][0]["message"]["content"], "answer");
            assert_eq!(encoded.body["usage"], wire_usage());
            assert!(encoded.losses.is_empty());
        }
    }
}

#[test]
fn ingress_rejects_disabled_unselected_conflicting_and_structured_reasoning() {
    for selection in [
        None,
        Some(ReasoningField::Disabled),
        Some(ReasoningField::ReasoningContent),
        Some(ReasoningField::Reasoning),
    ] {
        let bridge = match selection {
            Some(field) => ChatCompletionsBridge::new().with_reasoning_field(field),
            None => ChatCompletionsBridge::new(),
        };
        let mut invalid = vec![
            json!({"reasoning_content":"same","reasoning":"same"}),
            json!({"reasoning_details":[{"type":"reasoning.text","text":"private"}]}),
        ];
        for (field, key, _) in fields() {
            if selection != Some(field) {
                for text in ["private", ""] {
                    let mut data = json!({});
                    data[key] = json!(text);
                    invalid.push(data);
                }
            } else {
                for role in ["user", "system", "developer", "tool"] {
                    let mut message = json!({"role":role,"content":"answer"});
                    if role == "tool" {
                        message["tool_call_id"] = json!("call");
                    }
                    message[key] = json!("private");
                    assert!(matches!(
                        bridge.decode_request(
                            &serde_json::to_vec(&json!({"model":"m","messages":[message]}))
                                .expect("JSON")
                        ),
                        Err(BridgeError::Conversion(_))
                    ));
                }
                for value in [json!(1), json!([]), json!({})] {
                    let mut data = json!({});
                    data[key] = value;
                    invalid.push(data);
                }
            }
        }
        for data in invalid {
            let mut message = data.clone();
            message["role"] = json!("assistant");
            message["content"] = json!("answer");
            let wire = json!({"model":"m","messages":[message.clone()]});
            assert!(
                matches!(
                    bridge.decode_request(&serde_json::to_vec(&wire).expect("JSON")),
                    Err(BridgeError::Conversion(_))
                ),
                "{selection:?}: {data}"
            );
            assert!(
                matches!(
                    bridge.decode_response(
                        &serde_json::to_vec(&wire_response(message)).expect("JSON"),
                        &context()
                    ),
                    Err(BridgeError::Decode(_))
                ),
                "{selection:?}: {data}"
            );
            let mut decoder = bridge.stream_decoder(&context());
            assert!(
                matches!(
                    decoder.push(chunk(data.clone()), &mut Vec::new()),
                    Err(BridgeError::Decode(_))
                ),
                "{selection:?}: {data}"
            );
        }
    }
}

#[test]
fn null_reasoning_fields_and_empty_details_are_absent_even_when_disabled() {
    for field in [
        ReasoningField::Disabled,
        ReasoningField::ReasoningContent,
        ReasoningField::Reasoning,
    ] {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        let delta = json!({
            "reasoning":null,"reasoning_content":null,"reasoning_details":[],
            "content":"answer"
        });
        let mut message = delta.clone();
        message["role"] = json!("assistant");
        let input = bridge
            .decode_request(
                &serde_json::to_vec(&json!({"model":"m","messages":[message.clone()]}))
                    .expect("JSON"),
            )
            .expect("absent history reasoning");
        assert!(matches!(
            input.messages.as_slice(),
            [Message::Assistant { content }]
                if matches!(content.as_slice(), [AssistantContent::Text { text, .. }] if text == "answer")
        ));
        let output = bridge
            .decode_response(
                &serde_json::to_vec(&wire_response(message)).expect("JSON"),
                &context(),
            )
            .expect("absent response reasoning");
        assert!(matches!(
            output.content.as_slice(),
            [AssistantContent::Text { text, .. }] if text == "answer"
        ));
        let mut decoder = bridge.stream_decoder(&context());
        let mut events = Vec::new();
        decoder
            .push(chunk(delta), &mut events)
            .expect("absent stream reasoning");
        assert!(matches!(
            events.as_slice(),
            [
                StreamEvent::Start { .. },
                StreamEvent::ItemStart { item: StreamItem::Text, .. },
                StreamEvent::Delta { delta: StreamDelta::Text { text }, .. }
            ] if text == "answer"
        ));
    }
}

#[test]
fn unsupported_reasoning_is_rejected_or_omitted_as_a_whole_item() {
    let options = ConversionOptions {
        allow_loss: [LossScope::Reasoning].into(),
    };
    for (field, key, _) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        for extra in [
            json!({"summary":[""]}),
            json!({"opaque":""}),
            json!({"provider_id":""}),
            json!({"text":null}),
        ] {
            let mut reasoning = json!({"type":"reasoning","id":"thought","text":"must not leak"});
            reasoning
                .as_object_mut()
                .expect("reasoning object")
                .extend(extra.as_object().expect("extra fields").clone());
            let content = json!([reasoning, {"type":"text","id":"answer","text":"answer"}]);
            let input = request(content.clone());
            let output = response(content);
            assert_unrepresentable(
                bridge
                    .encode_request(&input, &ConversionOptions::default())
                    .expect_err("strict history"),
            );
            assert_unrepresentable(
                bridge
                    .encode_response(&input, &output, &ConversionOptions::default())
                    .expect_err("strict response"),
            );
            let encoded_request = bridge
                .encode_request(&input, &options)
                .expect("permitted history loss");
            let encoded_response = bridge
                .encode_response(&input, &output, &options)
                .expect("permitted response loss");
            for (encoded, message, path) in [
                (
                    &encoded_request,
                    &encoded_request.body["messages"][0],
                    "/messages/0/content/0",
                ),
                (
                    &encoded_response,
                    &encoded_response.body["choices"][0]["message"],
                    "/content/0",
                ),
            ] {
                assert!(message.get(key).is_none(), "{extra}");
                assert_eq!(message["content"], "answer");
                assert_eq!(
                    encoded
                        .losses
                        .iter()
                        .map(|loss| loss.path.as_str())
                        .collect::<Vec<_>>(),
                    [path]
                );
            }
        }
    }
}

#[test]
fn additional_reasoning_blocks_require_loss_permission_instead_of_concatenation() {
    for (field, key, _) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        let content = json!([
            {"type":"reasoning","id":"first","text":"first"},
            {"type":"reasoning","id":"second","text":"second"}
        ]);
        let input = request(content.clone());
        let output = response(content);
        assert_unrepresentable(
            bridge
                .encode_request(&input, &ConversionOptions::default())
                .expect_err("multiple history blocks"),
        );
        assert_unrepresentable(
            bridge
                .encode_response(&input, &output, &ConversionOptions::default())
                .expect_err("multiple response blocks"),
        );
        let options = ConversionOptions {
            allow_loss: [LossScope::Reasoning].into(),
        };
        let encoded_request = bridge
            .encode_request(&input, &options)
            .expect("history loss");
        let encoded_response = bridge
            .encode_response(&input, &output, &options)
            .expect("response loss");
        assert_eq!(encoded_request.body["messages"][0][key], "first");
        assert_eq!(encoded_response.body["choices"][0]["message"][key], "first");
        assert_eq!(encoded_request.losses.len(), 1);
        assert_eq!(encoded_request.losses[0].path, "/messages/0/content/1");
        assert_eq!(encoded_response.losses.len(), 1);
        assert_eq!(encoded_response.losses[0].path, "/content/1");
    }
}

#[test]
fn stream_decoder_preserves_empty_and_fragmented_reasoning_with_late_usage() {
    for (field, key, _) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        let mut decoder = bridge.stream_decoder(&context());
        let mut events = Vec::new();
        for (index, text) in ["", " \n", "考える\t"].into_iter().enumerate() {
            let mut delta = json!({});
            delta[key] = json!(text);
            decoder
                .push(chunk(delta), &mut events)
                .expect("reasoning fragment");
            if index == 0 {
                decoder
                    .push(chunk(json!({"content":"answer"})), &mut events)
                    .expect("interleaved answer");
            }
            if index == 1 {
                decoder
                    .push(
                        chunk(json!({"tool_calls":[{
                            "index":0,"id":"call","type":"function",
                            "function":{"name":"lookup","arguments":"{\"x\":"}
                        }]})),
                        &mut events,
                    )
                    .expect("interleaved tool");
            }
        }
        decoder
            .push(
                chunk(json!({"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]})),
                &mut events,
            )
            .expect("tool continuation");
        let mut finish = chunks(&[chunk(json!({}))]).remove(0);
        finish["choices"][0]["finish_reason"] = json!("stop");
        decoder
            .push(
                Frame {
                    event: None,
                    data: finish.to_string(),
                },
                &mut events,
            )
            .expect("finish reason");
        finish["choices"] = json!([]);
        finish["usage"] = wire_usage();
        decoder
            .push(
                Frame {
                    event: None,
                    data: finish.to_string(),
                },
                &mut events,
            )
            .expect("late usage");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::Stop { .. }))
        );
        decoder
            .push(
                Frame {
                    event: None,
                    data: "[DONE]".into(),
                },
                &mut events,
            )
            .expect("done");
        decoder.finish(&mut events).expect("complete stream");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    StreamEvent::ItemStart {
                        item: StreamItem::Reasoning,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::Delta {
                        index: 0,
                        delta: StreamDelta::ReasoningText { text },
                    } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["", " \n", "考える\t"]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::ItemStart {
                index: 2,
                item: StreamItem::ToolCall { call_id: Some(call_id), name: Some(name) },
                ..
            } if call_id == "call" && name == "lookup"
        )));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::Delta {
                        index: 2,
                        delta: StreamDelta::ToolCallArguments { text },
                    } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "{\"x\":1}"
        );
        assert!(events.iter().any(|event| matches!(event, StreamEvent::Delta { delta: StreamDelta::Text { text }, .. } if text == "answer")));
        assert_eq!(events.last(), Some(&stop()));
    }
}

#[test]
fn stream_encoder_preserves_reasoning_presence_text_and_token_usage() {
    for (field, key, other) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        for text in [" \n考える\t", ""] {
            let mut encoder =
                bridge.stream_encoder(&request(json!([])), &ConversionOptions::default());
            let mut frames = Vec::new();
            let mut losses = Vec::new();
            for event in [
                start(),
                reasoning_start(0),
                reasoning_text(0, text),
                StreamEvent::ItemStart {
                    index: 1,
                    id: "r:1".into(),
                    provider_id: None,
                    item: StreamItem::ToolCall {
                        call_id: Some("call".into()),
                        name: Some("lookup".into()),
                    },
                },
                StreamEvent::Delta {
                    index: 1,
                    delta: StreamDelta::ToolCallArguments { text: "{}".into() },
                },
                StreamEvent::ItemEnd { index: 1 },
                StreamEvent::ItemEnd { index: 0 },
                stop(),
            ] {
                encoder
                    .push(&event, &mut frames, &mut losses)
                    .expect("reasoning event");
            }
            encoder
                .finish(&mut frames, &mut losses)
                .expect("complete stream");
            assert!(losses.is_empty());
            assert_eq!(frames.last().expect("done").data, "[DONE]");
            let values = chunks(&frames);
            let fragments = values
                .iter()
                .filter_map(|value| value["choices"][0]["delta"].get(key))
                .collect::<Vec<_>>();
            assert!(!fragments.is_empty(), "empty reasoning is present");
            assert_eq!(
                fragments
                    .iter()
                    .map(|value| value.as_str().expect("reasoning string"))
                    .collect::<String>(),
                text
            );
            assert!(
                values
                    .iter()
                    .all(|value| value["choices"][0]["delta"].get(other).is_none())
            );
            assert_eq!(
                values.iter().find_map(|value| value.get("usage")),
                Some(&wire_usage())
            );
            let tool_calls = values
                .iter()
                .filter_map(|value| value["choices"][0]["delta"]["tool_calls"].as_array())
                .flatten()
                .collect::<Vec<_>>();
            assert!(!tool_calls.is_empty());
            assert!(tool_calls.iter().all(|call| call["index"] == 0));
            assert_eq!(
                tool_calls
                    .iter()
                    .filter_map(|call| call["function"]["arguments"].as_str())
                    .collect::<String>(),
                "{}"
            );
        }
    }
}

#[test]
fn later_unsupported_stream_data_never_leaks_partial_reasoning() {
    for (field, key, _) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        for unsupported in [
            StreamEvent::Delta {
                index: 0,
                delta: StreamDelta::ReasoningSummary {
                    summary_index: 0,
                    text: "".into(),
                },
            },
            StreamEvent::Delta {
                index: 0,
                delta: StreamDelta::ReasoningOpaque { value: "".into() },
            },
            StreamEvent::Metadata(MetadataScope::Item {
                index: 0,
                provider_id: Some("provider-item".into()),
                call_id: None,
                name: None,
            }),
        ] {
            for allow_loss in [false, true] {
                let options = ConversionOptions {
                    allow_loss: if allow_loss {
                        [LossScope::Reasoning].into()
                    } else {
                        Default::default()
                    },
                };
                let mut encoder = bridge.stream_encoder(&request(json!([])), &options);
                let mut frames = Vec::new();
                let mut losses = Vec::new();
                for event in [
                    start(),
                    reasoning_start(0),
                    reasoning_text(0, "must not leak"),
                ] {
                    encoder
                        .push(&event, &mut frames, &mut losses)
                        .expect("representable prefix");
                }
                if allow_loss {
                    assert!(
                        chunks(&frames)
                            .iter()
                            .all(|value| value["choices"][0]["delta"].get(key).is_none())
                    );
                }
                let result = encoder.push(&unsupported, &mut frames, &mut losses);
                if allow_loss {
                    result.expect("permitted whole-item loss");
                    for event in [
                        reasoning_text(0, "also omitted"),
                        StreamEvent::ItemEnd { index: 0 },
                        StreamEvent::ItemStart {
                            index: 1,
                            id: "r:1".into(),
                            provider_id: None,
                            item: StreamItem::Text,
                        },
                        StreamEvent::Delta {
                            index: 1,
                            delta: StreamDelta::Text {
                                text: "answer".into(),
                            },
                        },
                        StreamEvent::ItemEnd { index: 1 },
                        stop(),
                    ] {
                        encoder
                            .push(&event, &mut frames, &mut losses)
                            .expect("remaining stream");
                    }
                    encoder
                        .finish(&mut frames, &mut losses)
                        .expect("complete stream");
                    assert_eq!(losses.len(), 1);
                    assert_eq!(losses[0].path, "/items/0");
                    assert!(
                        chunks(&frames)
                            .iter()
                            .any(|value| value["choices"][0]["delta"]["content"] == "answer")
                    );
                } else {
                    assert_unrepresentable(result.expect_err("strict unsupported stream item"));
                }
                if allow_loss {
                    assert!(
                        chunks(&frames)
                            .iter()
                            .all(|value| value["choices"][0]["delta"].get(key).is_none())
                    );
                }
            }
        }
    }
}

#[test]
fn streams_reject_or_report_reasoning_without_text_identity_and_extra_blocks() {
    for (field, key, _) in fields() {
        let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
        let mut identified_start = reasoning_start(0);
        if let StreamEvent::ItemStart { provider_id, .. } = &mut identified_start {
            *provider_id = Some("provider-item".into());
        }
        for (events, omitted_index, expected_text) in [
            (
                vec![
                    start(),
                    reasoning_start(0),
                    StreamEvent::ItemEnd { index: 0 },
                ],
                0,
                None,
            ),
            (
                vec![
                    start(),
                    identified_start,
                    reasoning_text(0, "must not leak"),
                    StreamEvent::ItemEnd { index: 0 },
                ],
                0,
                None,
            ),
            (
                vec![
                    start(),
                    reasoning_start(0),
                    reasoning_text(0, "first"),
                    StreamEvent::ItemEnd { index: 0 },
                    reasoning_start(1),
                    reasoning_text(1, "second"),
                    StreamEvent::ItemEnd { index: 1 },
                ],
                1,
                Some("first"),
            ),
        ] {
            let mut strict =
                bridge.stream_encoder(&request(json!([])), &ConversionOptions::default());
            let mut frames = Vec::new();
            let mut losses = Vec::new();
            let error = events
                .iter()
                .find_map(|event| strict.push(event, &mut frames, &mut losses).err())
                .expect("strict unsupported reasoning");
            assert_unrepresentable(error);

            let options = ConversionOptions {
                allow_loss: [LossScope::Reasoning].into(),
            };
            let mut encoder = bridge.stream_encoder(&request(json!([])), &options);
            frames.clear();
            losses.clear();
            for event in events.iter().chain(std::iter::once(&stop())) {
                encoder
                    .push(event, &mut frames, &mut losses)
                    .expect("permitted stream loss");
            }
            encoder
                .finish(&mut frames, &mut losses)
                .expect("complete stream");
            assert_eq!(losses.len(), 1);
            assert_eq!(losses[0].path, format!("/items/{omitted_index}"));
            let values = chunks(&frames);
            let fragments = values
                .iter()
                .filter_map(|value| value["choices"][0]["delta"].get(key))
                .collect::<Vec<_>>();
            match expected_text {
                Some(text) => assert_eq!(
                    fragments
                        .iter()
                        .map(|value| value.as_str().expect("reasoning text"))
                        .collect::<String>(),
                    text
                ),
                None => assert!(fragments.is_empty()),
            }
        }
    }
}
