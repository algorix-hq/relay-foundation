//! Replays recorded exchanges with real providers through the built-in adapters.
//! Each request body was produced by the adapter and accepted by the provider.

use std::fs;

use relay_adapter::{
    Adapter, AdapterExt, AnthropicAdapter, Headers, OpenAiAdapter, OpenAiCompatibleAdapter,
};
use relay_protocol::{
    Delivery, Message, Parameters, Request, StopReason, StreamEvent, TextPart, UserContent, Version,
};
use serde_json::Value;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/e2e/");

fn request(model: &str, stream: bool) -> Request {
    Request {
        version: Version::V1,
        model: model.into(),
        messages: vec![Message::User {
            content: vec![UserContent::Text(TextPart {
                text: "Say hi in two words.".into(),
                cache_control: None,
            })],
        }],
        tools: None,
        parameters: Some(Parameters {
            max_output_tokens: Some(16),
            temperature: None,
            top_p: None,
            stop: vec![],
        }),
        response_format: None,
        reasoning: None,
        delivery: Some(Delivery {
            stream,
            stream_usage: stream,
        }),
    }
}

fn recorded_headers(name: &str) -> (u16, Headers) {
    let text = fs::read_to_string(format!("{DIR}{name}.response.headers")).expect("headers");
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let pairs = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    (status, Headers(pairs))
}

fn json(path: String) -> Value {
    serde_json::from_slice(&fs::read(path).expect("fixture")).expect("JSON")
}

fn check(name: &str, adapter: &dyn Adapter, model: &str, plain_stop: StopReason) {
    let recorded = format!("{name}_plain");
    let (built, losses) = adapter
        .build_request(&request(model, false))
        .expect("plain");
    assert!(losses.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&built.body).expect("body"),
        json(format!("{DIR}{recorded}.request.json")),
        "{name}: the provider accepted exactly this body"
    );
    let (status, headers) = recorded_headers(&recorded);
    let body = fs::read(format!("{DIR}{recorded}.response.json")).expect("body");
    let response = adapter
        .parse_response(status, &headers, &body)
        .expect("parse");
    assert_eq!(response.model, model);
    assert_eq!(response.provider_id.as_deref(), Some(response.id.as_str()));
    assert_eq!(response.stop_reason, plain_stop);
    assert!(response.usage.is_some());

    let recorded = format!("{name}_stream");
    let (built, _) = adapter
        .build_request(&request(model, true))
        .expect("stream");
    assert_eq!(
        serde_json::from_slice::<Value>(&built.body).expect("body"),
        json(format!("{DIR}{recorded}.request.json"))
    );
    let (status, headers) = recorded_headers(&recorded);
    let bytes = fs::read(format!("{DIR}{recorded}.response.sse")).expect("stream");
    let mut stream = adapter.open_stream(status, &headers).expect("open");
    let mut events = Vec::new();
    for chunk in bytes.chunks(37) {
        events.extend(stream.push(chunk).map(|item| item.expect("event")));
    }
    events.extend(stream.finish().map(|item| item.expect("event")));
    assert!(matches!(events.first(), Some(StreamEvent::Start { .. })));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop { usage: Some(_), .. })
    ));
}

#[test]
fn openai_responses_exchange() {
    // gpt-6-luna spent all 16 tokens on reasoning and stopped incomplete.
    check(
        "openai",
        &OpenAiAdapter::new("sk-test"),
        "gpt-6-luna",
        StopReason::MaxTokens,
    );
}

#[test]
fn anthropic_messages_exchange() {
    check(
        "anthropic",
        &AnthropicAdapter::new("sk-test"),
        "claude-haiku-5-5",
        StopReason::EndTurn,
    );
}

#[test]
fn ollama_cloud_chat_completions_exchange() {
    check(
        "ollama",
        &OpenAiCompatibleAdapter::new("https://ollama.com/v1")
            .api_key("sk-test")
            .without_tool_choice(),
        "gemma4:31b",
        StopReason::EndTurn,
    );
}
