use std::{fs, path::Path, sync::Arc};

use relay_adapter::providers::ollama_cloud::OllamaCloudAdapter;
use relay_adapter::{Adapter, AdapterExt, Headers, Method};
use relay_bridge::{ApiFormat, ChatCompletionsBridge, MessagesBridge, ResponsesBridge};
use relay_protocol::{
    AssistantContent, Error, ProviderErrorCategory as Category, Request, StopReason, StreamDelta,
    StreamEvent,
};
use serde_json::{Value, json};

fn live(name: &str, body_extension: &str) -> (u16, Headers, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live");
    let raw = fs::read_to_string(path.join(format!("{name}.headers"))).expect("recorded headers");
    let mut lines = raw.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers = Headers(
        lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.into(), value.trim().into()))
            .collect(),
    );
    let body = fs::read(path.join(format!("{name}.{body_extension}"))).expect("recorded body");
    (status, headers, body)
}

fn input() -> Request {
    serde_json::from_value(json!({"version":"1","model":"gpt-oss:20b","messages":[{"role":"user","content":[{"type":"text","text":"Hi"}]}],"parameters":{"max_output_tokens":64}})).expect("request")
}

#[test]
fn requests_target_ollama_cloud_with_bearer_key_and_max_tokens() {
    let (request, losses) = OllamaCloudAdapter::new("ollama-key")
        .build_request(&input())
        .expect("request");
    assert!(losses.is_empty());
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.url, "https://ollama.com/v1/chat/completions");
    assert_eq!(
        request.headers,
        Headers(vec![
            ("content-type".into(), "application/json".into()),
            ("authorization".into(), "Bearer ollama-key".into()),
        ])
    );
    let body: Value = serde_json::from_slice(&request.body).expect("JSON");
    assert_eq!(body["max_tokens"], 64);
    assert!(body.get("max_completion_tokens").is_none());
}

#[test]
fn any_explicit_tool_choice_is_rejected() {
    let mut value = serde_json::to_value(input()).expect("input");
    value["tools"] = json!({"definitions":[],"choice":"required"});
    let request: Request = serde_json::from_value(value).expect("choice");
    let Err(Error::Configuration(error)) = OllamaCloudAdapter::new("key").build_request(&request)
    else {
        panic!("configuration failure")
    };
    assert!(error.message.contains("tool_choice"));
}

#[test]
fn recorded_reasoning_response_decodes_reasoning_and_text() {
    let (status, headers, body) = live("ollama_reasoning", "json");
    let response = OllamaCloudAdapter::new("key")
        .parse_response(status, &headers, &body)
        .expect("response");
    assert_eq!(response.model, "gpt-oss:20b");
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert!(matches!(
        response.content.as_slice(),
        [
            AssistantContent::Reasoning { text: Some(reasoning), .. },
            AssistantContent::Text { text, .. },
        ] if reasoning == "User just says: \"Say hi.\" Provide a greeting. Should be brief."
            && text == "Hi there! 👋"
    ));
}

#[test]
fn recorded_reasoning_stream_emits_reasoning_before_text_and_ends_with_usage() {
    let (status, headers, body) = live("ollama_reasoning_stream", "sse");
    let mut stream = OllamaCloudAdapter::new("key")
        .open_stream(status, &headers)
        .expect("stream");
    let mut events = Vec::new();
    for chunk in body.chunks(29) {
        events.extend(stream.push(chunk).map(|item| item.expect("event")));
    }
    events.extend(stream.finish().map(|item| item.expect("event")));
    let (mut reasoning, mut text) = (String::new(), String::new());
    for event in &events {
        match event {
            StreamEvent::Delta {
                delta: StreamDelta::ReasoningText { text: delta },
                ..
            } => {
                assert!(text.is_empty(), "reasoning after text");
                reasoning.push_str(delta);
            }
            StreamEvent::Delta {
                delta: StreamDelta::Text { text: delta },
                ..
            } => text.push_str(delta),
            _ => {}
        }
    }
    assert!(reasoning.starts_with("User just says"), "{reasoning}");
    assert!(!text.is_empty());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop { usage: Some(_), .. })
    ));
}

#[test]
fn recorded_errors_classify_model_not_found_but_not_unknown_paths() {
    let adapter = OllamaCloudAdapter::new("key");
    for (name, category) in [
        ("ollama_invalid_key", Category::Authentication),
        ("ollama_model_not_found", Category::ModelNotFound),
        ("ollama_unknown_path", Category::Unknown),
        ("ollama_image_url", Category::InvalidRequest),
    ] {
        let (status, headers, body) = live(name, "json");
        let error = adapter
            .check_response(status, &headers, &body)
            .expect_err(name);
        assert_eq!(error.category, category, "{name}");
        assert_eq!(error.http_status, Some(status), "{name}");
        assert_eq!(error.request_id.as_deref(), headers.get("x-request-id"));
    }
}

#[test]
fn mid_stream_not_found_without_http_status_stays_unknown() {
    let mut stream = OllamaCloudAdapter::new("key")
        .open_stream(200, &Headers::default())
        .expect("stream");
    let items: Vec<_> = stream
        .push(
            br#"data: {"error":{"type":"not_found_error","code":null,"message":"gone"}}

"#,
        )
        .collect();
    let Some(Err(Error::Provider(error))) = items.last() else {
        panic!("provider error: {items:?}")
    };
    assert_eq!(error.category, Category::Unknown);
}

#[test]
fn bridge_replacement_accepts_chat_completions_only() {
    assert!(
        OllamaCloudAdapter::new("key")
            .with_bridge(Arc::new(ResponsesBridge::new()))
            .is_err()
    );
    assert!(
        OllamaCloudAdapter::new("key")
            .with_bridge(Arc::new(MessagesBridge::new()))
            .is_err()
    );
    let adapter = OllamaCloudAdapter::new("key")
        .with_bridge(Arc::new(ChatCompletionsBridge::new()))
        .expect("replacement");
    assert_eq!(
        adapter.conversion().bridge().format(),
        &ApiFormat::CHAT_COMPLETIONS
    );
}

#[test]
fn model_discovery_lists_ollama_cloud_models() {
    let adapter = OllamaCloudAdapter::new("key");
    let request = adapter.list_models(None).expect("models");
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.url, "https://ollama.com/v1/models");
    assert_eq!(request.headers.get("authorization"), Some("Bearer key"));
    let page = adapter
        .parse_models(
            200,
            &Headers::default(),
            br#"{"object":"list","data":[{"id":"gpt-oss:20b"}]}"#,
        )
        .expect("page");
    assert_eq!(page.model_ids, vec!["gpt-oss:20b".to_owned()]);
    assert_eq!(page.next_cursor, None);
}
