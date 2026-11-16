use std::sync::Arc;

use relay_adapter::{AdapterExt, BaseAdapter, Classify, Headers, Method};
use relay_bridge::{Bridge, ChatCompletionsBridge};
use relay_protocol::{Error, ProviderError, ProviderErrorCategory};
use serde_json::{Value, json};

#[test]
fn shared_base_assembles_requests_and_classifies_http_and_stream_failures() {
    fn classify(
        payload: &Value,
        envelope: &Value,
        http_status: Option<u16>,
        headers: &Headers,
    ) -> ProviderError {
        ProviderError {
            category: ProviderErrorCategory::Unknown,
            message: payload.to_string(),
            code: envelope["marker"].as_str().map(str::to_owned),
            http_status,
            request_id: headers.get("x-request-id").map(str::to_owned),
            retry_after: None,
        }
    }

    let classify: Classify = classify;
    let bridge = Arc::new(ChatCompletionsBridge::new());
    let input = bridge
        .decode_request(br#"{"model":"custom-model","messages":[{"role":"user","content":"Hi"}]}"#)
        .expect("client request");
    let adapter = BaseAdapter::new(bridge, "https://custom.example/generate", classify)
        .header("x-custom-token", "old-token")
        .header("x-custom-token", "custom-token");
    let (request, losses) = adapter.build_request(&input).expect("provider request");
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.url, "https://custom.example/generate");
    assert_eq!(
        request.headers,
        Headers(vec![
            ("content-type".into(), "application/json".into()),
            ("x-custom-token".into(), "custom-token".into()),
        ])
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).expect("JSON request"),
        json!({"model":"custom-model","messages":[{"role":"user","content":"Hi"}]})
    );
    assert!(losses.is_empty());

    let headers = Headers(vec![("x-request-id".into(), "custom-request".into())]);
    let http_error = adapter
        .parse_response(
            418,
            &headers,
            br#"{"error":{"code":"http_failure"},"marker":"http-envelope"}"#,
        )
        .expect_err("HTTP error");
    assert_eq!(
        http_error,
        Error::Provider(ProviderError {
            category: ProviderErrorCategory::Unknown,
            message: r#"{"code":"http_failure"}"#.into(),
            code: Some("http-envelope".into()),
            http_status: Some(418),
            request_id: Some("custom-request".into()),
            retry_after: None,
        })
    );

    let mut stream = adapter.open_stream(200, &headers).expect("stream");
    assert_eq!(
        stream
            .push(b"data: {\"error\":{\"code\":\"stream_failure\"},\"marker\":\"stream-envelope\"}\n\n")
            .collect::<Vec<_>>(),
        vec![Err(Error::Provider(ProviderError {
            category: ProviderErrorCategory::Unknown,
            message: r#"{"code":"stream_failure"}"#.into(),
            code: Some("stream-envelope".into()),
            http_status: None,
            request_id: Some("custom-request".into()),
            retry_after: None,
        }))]
    );
}
