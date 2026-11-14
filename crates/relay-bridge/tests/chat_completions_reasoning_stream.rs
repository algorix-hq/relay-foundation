use relay_bridge::{
    Bridge, ConversionOptions, LossScope,
    chat_completions::{ChatCompletionsBridge, ReasoningField},
};
use relay_protocol::{Request, StopReason, StreamDelta, StreamEvent, StreamItem, Timestamp};
use serde_json::{Value, json};

#[test]
fn strict_reasoning_streams_fragments_before_item_end_without_fabricating_initial_text() {
    let request: Request = serde_json::from_value(json!({
        "version":"1", "model":"model", "messages":[]
    }))
    .expect("protocol request");
    let bridge =
        ChatCompletionsBridge::new().with_reasoning_field(ReasoningField::ReasoningContent);
    let mut encoder = bridge.stream_encoder(&request, &ConversionOptions::default());
    let mut frames = Vec::new();
    let mut losses = Vec::new();
    for event in [
        StreamEvent::Start {
            response_id: "response".into(),
            provider_id: None,
            created_at: Timestamp {
                observed_at: 1,
                provider_created_at: None,
            },
            model: Some("model".into()),
        },
        StreamEvent::ItemStart {
            index: 0,
            id: "response:0".into(),
            provider_id: None,
            item: StreamItem::Reasoning,
        },
    ] {
        encoder
            .push(&event, &mut frames, &mut losses)
            .expect("start reasoning");
    }
    assert_eq!(frames.len(), 1, "start alone must not fabricate text");
    for text in ["fragment", " continuation"] {
        frames.clear();
        encoder
            .push(
                &StreamEvent::Delta {
                    index: 0,
                    delta: StreamDelta::ReasoningText { text: text.into() },
                },
                &mut frames,
                &mut losses,
            )
            .expect("incremental strict reasoning");
        let fragments: String = frames
            .iter()
            .filter_map(|frame| {
                let chunk: Value = serde_json::from_str(&frame.data).expect("chunk JSON");
                chunk["choices"][0]["delta"]["reasoning_content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        assert_eq!(fragments, text, "fragment must precede ItemEnd");
    }
    assert!(losses.is_empty());
}

#[test]
fn buffered_reasoning_retains_interleaved_frame_order_when_kept_or_omitted() {
    let request: Request = serde_json::from_value(json!({
        "version":"1", "model":"model", "messages":[]
    }))
    .expect("protocol request");
    for (field, name) in [
        (ReasoningField::ReasoningContent, "reasoning_content"),
        (ReasoningField::Reasoning, "reasoning"),
    ] {
        for omitted in [false, true] {
            let bridge = ChatCompletionsBridge::new().with_reasoning_field(field);
            let mut encoder = bridge.stream_encoder(
                &request,
                &ConversionOptions {
                    allow_loss: [LossScope::Reasoning].into(),
                },
            );
            let mut frames = Vec::new();
            let mut losses = Vec::new();
            for event in [
                StreamEvent::Start {
                    response_id: "response".into(),
                    provider_id: None,
                    created_at: Timestamp {
                        observed_at: 1,
                        provider_created_at: None,
                    },
                    model: Some("model".into()),
                },
                StreamEvent::ItemStart {
                    index: 0,
                    id: "response:0".into(),
                    provider_id: None,
                    item: StreamItem::Reasoning,
                },
                StreamEvent::Delta {
                    index: 0,
                    delta: StreamDelta::ReasoningText {
                        text: String::new(),
                    },
                },
                StreamEvent::ItemStart {
                    index: 1,
                    id: "response:1".into(),
                    provider_id: None,
                    item: StreamItem::Text,
                },
                StreamEvent::Delta {
                    index: 1,
                    delta: StreamDelta::Text {
                        text: "answer".into(),
                    },
                },
                StreamEvent::Delta {
                    index: 0,
                    delta: StreamDelta::ReasoningText {
                        text: "thought".into(),
                    },
                },
            ] {
                encoder
                    .push(&event, &mut frames, &mut losses)
                    .expect("buffer interleaved content");
                assert_eq!(frames.len(), 1);
            }
            if omitted {
                encoder
                    .push(
                        &StreamEvent::Delta {
                            index: 0,
                            delta: StreamDelta::ReasoningOpaque {
                                value: "opaque".into(),
                            },
                        },
                        &mut frames,
                        &mut losses,
                    )
                    .expect("omit reasoning");
            }
            encoder
                .push(&StreamEvent::ItemEnd { index: 0 }, &mut frames, &mut losses)
                .expect("end reasoning");
            let deltas: Vec<Value> = frames
                .iter()
                .map(|frame| {
                    let chunk: Value = serde_json::from_str(&frame.data).expect("chunk JSON");
                    chunk["choices"][0]["delta"].clone()
                })
                .collect();
            let expected = if omitted {
                vec![
                    json!({"role":"assistant"}),
                    json!({"content":""}),
                    json!({"content":"answer"}),
                ]
            } else {
                vec![
                    json!({"role":"assistant"}),
                    json!({name:""}),
                    json!({name:""}),
                    json!({"content":""}),
                    json!({"content":"answer"}),
                    json!({name:"thought"}),
                ]
            };
            assert_eq!(deltas, expected);
            for event in [
                StreamEvent::ItemEnd { index: 1 },
                StreamEvent::Stop {
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                },
            ] {
                encoder
                    .push(&event, &mut frames, &mut losses)
                    .expect("complete stream");
            }
            encoder
                .finish(&mut frames, &mut losses)
                .expect("finished stream");
        }
    }
}
