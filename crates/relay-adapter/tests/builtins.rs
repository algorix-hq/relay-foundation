use std::{fs, path::Path, sync::Arc};

use relay_adapter::{
    Adapter, AdapterExt, AnthropicAdapter, Headers, Method, ModelPage, OpenAiAdapter,
    OpenAiCompatibleAdapter,
};
use relay_bridge::{ApiFormat, ChatCompletionsBridge, LossScope, MessagesBridge, ResponsesBridge};
use relay_protocol::{
    ProviderError, ProviderErrorCategory as Category, RelayError, Request, RetryAfter, StreamEvent,
};
use serde_json::{Value, json};

fn live(name: &str) -> (u16, Headers, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live");
    let raw = fs::read_to_string(path.join(format!("{name}.headers"))).expect("recorded headers");
    let mut lines = raw.lines();
    let status = lines
        .next()
        .expect("status")
        .split_whitespace()
        .nth(1)
        .expect("status number")
        .parse()
        .expect("u16");
    let headers = Headers(
        lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.into(), value.trim().into()))
            .collect(),
    );
    (
        status,
        headers,
        fs::read(path.join(format!("{name}.json"))).expect("recorded body"),
    )
}

fn adapters() -> [Box<dyn Adapter>; 3] {
    [
        Box::new(OpenAiAdapter::new("key")),
        Box::new(AnthropicAdapter::new("key")),
        Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1")),
    ]
}

#[test]
fn recorded_errors_preserve_diagnostics_even_with_non_json_content_types() {
    let adapters = adapters();
    for (index, name, category, code) in [
        (
            0,
            "openai_invalid_key",
            Category::Authentication,
            Some("invalid_api_key"),
        ),
        (
            0,
            "openai_model_not_found",
            Category::ModelNotFound,
            Some("model_not_found"),
        ),
        (
            0,
            "openai_responses_model_not_found",
            Category::ModelNotFound,
            Some("model_not_found"),
        ),
        (
            1,
            "anthropic_invalid_key",
            Category::Authentication,
            Some("authentication_error"),
        ),
        (
            1,
            "anthropic_model_not_found",
            Category::Unknown,
            Some("not_found_error"),
        ),
        (
            1,
            "anthropic_bearer_model_not_found",
            Category::Unknown,
            Some("not_found_error"),
        ),
        (
            1,
            "anthropic_get_unknown_model",
            Category::Unknown,
            Some("not_found_error"),
        ),
        (
            1,
            "anthropic_unknown_batch",
            Category::Unknown,
            Some("not_found_error"),
        ),
        (
            1,
            "anthropic_unknown_path",
            Category::Unknown,
            Some("not_found_error"),
        ),
        (2, "ollama_invalid_key", Category::Authentication, None),
        (2, "ollama_model_not_found", Category::Unknown, None),
        (2, "ollama_image_url", Category::InvalidRequest, None),
    ] {
        let (status, headers, body) = live(name);
        let mut retries = headers.clone();
        retries.0.push(("Retry-After".into(), "7".into()));
        for headers in [headers, retries] {
            let error = adapters[index]
                .check_response(status, &headers, &body)
                .expect_err(name);
            assert_eq!(error.category, category, "{name}");
            assert_eq!(error.http_status, Some(status), "{name}");
            assert_eq!(error.code.as_deref(), code, "{name}");
            assert_eq!(
                error.request_id.as_deref(),
                headers.get(if index == 1 {
                    "request-id"
                } else {
                    "x-request-id"
                }),
                "{name}"
            );
            assert_eq!(
                error.retry_after,
                headers
                    .get("retry-after")
                    .map(|_| RetryAfter::Seconds { seconds: 7 })
            );
        }
    }
}

#[test]
fn openai_and_compatible_codes_win_over_types_and_unknown_types_use_status() {
    let adapters: [Box<dyn Adapter>; 2] = [
        Box::new(OpenAiAdapter::new("key")),
        Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1")),
    ];
    for adapter in adapters {
        for (code, category) in [
            ("invalid_api_key", Category::Authentication),
            ("model_not_found", Category::ModelNotFound),
            ("insufficient_quota", Category::InsufficientQuota),
            ("credit_balance_exhausted", Category::InsufficientQuota),
            (
                "organization_spend_limit_exceeded",
                Category::InsufficientQuota,
            ),
            ("project_spend_limit_exceeded", Category::InsufficientQuota),
            (
                "organization_usage_limit_exceeded",
                Category::InsufficientQuota,
            ),
            ("rate_limit_exceeded", Category::RateLimited),
            ("context_length_exceeded", Category::ContextLengthExceeded),
            ("server_is_overloaded", Category::Overloaded),
            ("server_error", Category::ServerError),
        ] {
            let body = json!({"error":{"code":code, "type":"invalid_request_error", "message":"not classified by text"}});
            let error = adapter
                .check_response(418, &Headers::default(), body.to_string().as_bytes())
                .expect_err(code);
            assert_eq!(error.category, category, "{code}");
            assert_eq!(error.code.as_deref(), Some(code));
        }
        for (kind, code, status, category) in [
            ("insufficient_quota", None, 418, Category::InsufficientQuota),
            ("rate_limit_error", None, 418, Category::RateLimited),
            ("service_unavailable_error", None, 418, Category::Overloaded),
            ("server_error", None, 418, Category::ServerError),
            ("invalid_request_error", None, 418, Category::InvalidRequest),
            (
                "invalid_request_error",
                Some("future"),
                418,
                Category::InvalidRequest,
            ),
            ("api_error", None, 401, Category::Authentication),
            ("not_found_error", None, 404, Category::Unknown),
            ("context_length_exceeded", None, 418, Category::Unknown),
            ("future", Some("future"), 403, Category::PermissionDenied),
        ] {
            let body = json!({"error":{"type":kind,"code":code,"message":"invalid_api_key insufficient_quota"}});
            let error = adapter
                .check_response(status, &Headers::default(), body.to_string().as_bytes())
                .expect_err(kind);
            assert_eq!(error.category, category, "{kind} {code:?}");
            assert_eq!(error.code.as_deref(), code);
        }
    }
}

#[test]
fn anthropic_types_spend_limit_override_and_body_request_id_are_preserved() {
    let adapter = AnthropicAdapter::new("key");
    for (kind, category) in [
        ("invalid_request_error", Category::InvalidRequest),
        ("authentication_error", Category::Authentication),
        ("permission_error", Category::PermissionDenied),
        ("rate_limit_error", Category::RateLimited),
        ("api_error", Category::ServerError),
        ("overloaded_error", Category::Overloaded),
        ("request_too_large", Category::InvalidRequest),
        ("billing_error", Category::Unknown),
        ("not_found_error", Category::Unknown),
        ("conflict_error", Category::Unknown),
        ("timeout_error", Category::Unknown),
    ] {
        let body = json!({"type":"error","error":{"type":kind,"message":"model not found"},"request_id":"body-id"});
        let error = adapter
            .check_response(418, &Headers::default(), body.to_string().as_bytes())
            .expect_err(kind);
        assert_eq!(error.category, category, "{kind}");
        assert_eq!(error.code.as_deref(), Some(kind));
        assert_eq!(error.request_id.as_deref(), Some("body-id"));
    }
    let body = br#"{"type":"error","error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}},"request_id":"body-id"}"#;
    let error = adapter
        .check_response(
            429,
            &Headers(vec![("request-id".into(), "header-id".into())]),
            body,
        )
        .expect_err("spend limit");
    assert_eq!(error.category, Category::InsufficientQuota);
    assert_eq!(error.code.as_deref(), Some("enforced_spend_limit_reached"));
    assert_eq!(error.request_id.as_deref(), Some("header-id"));
}

#[test]
fn missing_or_undecodable_error_bodies_use_status_and_keep_retry_headers() {
    for adapter in adapters() {
        for (status, expected) in [
            (401, Category::Authentication),
            (403, Category::PermissionDenied),
            (429, Category::RateLimited),
            (500, Category::ServerError),
            (599, Category::ServerError),
            (418, Category::Unknown),
        ] {
            for body in [b"".as_slice(), b"<html>overloaded</html>"] {
                let error = adapter
                    .check_response(status, &Headers::default(), body)
                    .expect_err("HTTP failure");
                assert_eq!(error.category, expected);
                assert_eq!(error.http_status, Some(status));
                assert_eq!(error.code, None);
            }
        }
        for (headers, expected) in [
            (
                vec![("Retry-After", "Wed, 11 Nov 2026 12:00:00 GMT")],
                RetryAfter::HttpDate {
                    date: "Wed, 11 Nov 2026 12:00:00 GMT".into(),
                },
            ),
            (
                vec![("retry-after-ms", "1001")],
                RetryAfter::Seconds { seconds: 2 },
            ),
            (
                vec![("retry-after-ms", "1000")],
                RetryAfter::Seconds { seconds: 1 },
            ),
            (
                vec![("retry-after-ms", "0")],
                RetryAfter::Seconds { seconds: 0 },
            ),
            (
                vec![("retry-after-ms", "9000"), ("retry-after", "3")],
                RetryAfter::Seconds { seconds: 3 },
            ),
        ] {
            let headers = Headers(
                headers
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            );
            assert_eq!(
                adapter
                    .check_response(429, &headers, b"")
                    .expect_err("failure")
                    .retry_after,
                Some(expected)
            );
        }
    }
}

fn input() -> Request {
    serde_json::from_value(json!({"version":"1","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"Hi"}]}],"parameters":{"max_output_tokens":16}})).expect("request")
}

#[test]
fn requests_use_exact_endpoints_authentication_and_bridge_selected_paths() {
    let cases: [(Box<dyn Adapter>, &str, Headers, &str); 5] = [
        (
            Box::new(AnthropicAdapter::new("anthropic-key")),
            "https://api.anthropic.com/v1/messages",
            Headers(vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer anthropic-key".into()),
                ("anthropic-version".into(), "2026-10-21".into()),
            ]),
            "max_tokens",
        ),
        (
            Box::new(
                OpenAiAdapter::new("openai-key")
                    .organization("org")
                    .project("project"),
            ),
            "https://api.openai.com/v1/responses",
            Headers(vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer openai-key".into()),
                ("openai-organization".into(), "org".into()),
                ("openai-project".into(), "project".into()),
            ]),
            "max_output_tokens",
        ),
        (
            Box::new(
                OpenAiAdapter::new("key")
                    .base_url("https://proxy.invalid/v1/")
                    .with_bridge(Arc::new(ChatCompletionsBridge::new()))
                    .expect("chat bridge"),
            ),
            "https://proxy.invalid/v1/chat/completions",
            Headers(vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer key".into()),
            ]),
            "max_completion_tokens",
        ),
        (
            Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1/")),
            "https://ollama.com/v1/chat/completions",
            Headers(vec![("content-type".into(), "application/json".into())]),
            "max_tokens",
        ),
        (
            Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1").api_key("ollama-key")),
            "https://ollama.com/v1/chat/completions",
            Headers(vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer ollama-key".into()),
            ]),
            "max_tokens",
        ),
    ];
    for (adapter, url, headers, tokens_field) in cases {
        let (request, losses) = adapter.build_request(&input()).expect("provider request");
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.url, url);
        assert_eq!(request.headers, headers);
        assert!(losses.is_empty());
        let body: Value = serde_json::from_slice(&request.body).expect("JSON");
        assert_eq!(body[tokens_field], 16);
        let expected = adapter
            .conversion()
            .bridge()
            .encode_request(&input(), adapter.conversion().options())
            .expect("bridge body");
        assert_eq!(body, expected.body);
    }
    let custom = AnthropicAdapter::new("key")
        .base_url("https://proxy.invalid/")
        .version("custom");
    let (request, _) = custom.build_request(&input()).expect("override");
    assert_eq!(request.url, "https://proxy.invalid/v1/messages");
    assert_eq!(request.headers.get("anthropic-version"), Some("custom"));
    assert!(matches!(
        OpenAiCompatibleAdapter::new("").build_request(&input()),
        Err(RelayError::Configuration(_))
    ));
}

#[test]
fn compatible_server_declaration_rejects_any_choice_before_conversion() {
    let adapter = OpenAiCompatibleAdapter::new("https://ollama.com/v1").without_tool_choice();
    for choice in [
        json!("required"),
        json!("auto"),
        json!("none"),
        json!({"type":"function","name":"f"}),
    ] {
        let mut value = serde_json::to_value(input()).expect("input");
        value["tools"] = json!({"definitions":[],"choice":choice});
        let request: Request = serde_json::from_value(value).expect("choice");
        let RelayError::Configuration(error) = adapter
            .build_request(&request)
            .expect_err("unsupported server feature")
        else {
            panic!("configuration failure")
        };
        assert!(error.message.contains("tool_choice"));
    }
    assert!(adapter.build_request(&input()).is_ok());
    let mut request = input();
    request.tools = Some(relay_protocol::ToolConfig {
        definitions: Vec::new(),
        choice: None,
        parallel: Some(false),
    });
    assert!(adapter.build_request(&request).is_ok());
}

#[test]
fn bridge_replacement_validates_formats_and_retains_loss_policy_and_identity_injection() {
    assert!(
        AnthropicAdapter::new("key")
            .with_bridge(Arc::new(ResponsesBridge::new()))
            .is_err()
    );
    assert!(
        OpenAiCompatibleAdapter::new("https://ollama.com/v1")
            .with_bridge(Arc::new(MessagesBridge::new()))
            .is_err()
    );
    assert!(
        OpenAiAdapter::new("key")
            .with_bridge(Arc::new(MessagesBridge::new()))
            .is_err()
    );
    let adapter = OpenAiAdapter::new("key")
        .with_id_generator(|| "injected".into())
        .with_clock(|| 123)
        .allow_loss([LossScope::CacheControl])
        .with_bridge(Arc::new(ChatCompletionsBridge::new()))
        .expect("replacement");
    assert_eq!(
        adapter.conversion().bridge().format(),
        &ApiFormat::CHAT_COMPLETIONS
    );
    let response = adapter.parse_response(200, &Headers::default(), br#"{"object":"chat.completion","model":"m","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#).expect("response");
    assert_eq!(response.id, "injected");
    assert_eq!(response.created_at.observed_at, 123);
    let mut request = input();
    if let relay_protocol::Message::User { content } = &mut request.messages[0]
        && let relay_protocol::UserContent::Text(text) = &mut content[0]
    {
        text.cache_control = Some(relay_protocol::CacheControl { ttl_seconds: None });
    }
    let (_, losses) = adapter
        .build_request(&request)
        .expect("retained loss permission");
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].path, "/messages/0/content/0/cache_control");
}

#[test]
fn model_discovery_preserves_ids_and_only_anthropic_paginates() {
    let anthropic = AnthropicAdapter::new("key");
    let req = anthropic
        .list_models(Some("model /+&한"))
        .expect("list request");
    assert_eq!(req.method, Method::Get);
    assert_eq!(
        req.url,
        "https://api.anthropic.com/v1/models?limit=1000&after_id=model%20%2F%2B%26%ED%95%9C"
    );
    assert!(req.body.is_empty());
    assert_eq!(req.headers.get("authorization"), Some("Bearer key"));
    for (has_more, expected) in [(true, Some("last".to_owned())), (false, None)] {
        let body = json!({"data":[{"id":"m"}],"has_more":has_more,"last_id":"last"});
        assert_eq!(
            anthropic
                .parse_models(200, &Headers::default(), body.to_string().as_bytes())
                .expect("page"),
            ModelPage {
                model_ids: vec!["m".into()],
                next_cursor: expected
            }
        );
    }
    for adapter in [
        Box::new(OpenAiAdapter::new("key")) as Box<dyn Adapter>,
        Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1")),
    ] {
        let req = adapter.list_models(Some("ignored")).expect("models");
        assert!(req.url.ends_with("/v1/models"));
        assert!(!req.url.contains('?'));
        let page = adapter
            .parse_models(
                200,
                &Headers::default(),
                br#"{"data":[{"id":"m"}],"has_more":true,"last_id":"not-a-cursor"}"#,
            )
            .expect("page");
        assert_eq!(
            page,
            ModelPage {
                model_ids: vec!["m".into()],
                next_cursor: None
            }
        );
        assert!(matches!(
            adapter.parse_models(200, &Headers::default(), br#"{"data":[{"id":7}]}"#),
            Err(RelayError::Decode(_))
        ));
    }
}

fn bridge_fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../relay-bridge/tests/fixtures")
            .join(name),
    )
    .expect("bridge fixture")
}

#[test]
fn provider_stream_failures_follow_prior_events_and_never_reach_bridges() {
    struct StreamFailureCase {
        adapter: Box<dyn Adapter>,
        prefix: Vec<u8>,
        error: Value,
        event: Option<&'static str>,
        category: Category,
        code: Option<&'static str>,
    }
    let cases = [
        StreamFailureCase {
            adapter: Box::new(AnthropicAdapter::new("key")),
            prefix: bridge_fixture("messages/stream/00-start.json"),
            error: json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}),
            event: Some("error"),
            category: Category::Overloaded,
            code: Some("overloaded_error"),
        },
        StreamFailureCase {
            adapter: Box::new(OpenAiAdapter::new("key")),
            prefix: bridge_fixture("responses/stream-00.json"),
            error: json!({"type":"error","code":"rate_limit_exceeded","message":"busy","param":null}),
            event: Some("error"),
            category: Category::RateLimited,
            code: Some("rate_limit_exceeded"),
        },
        StreamFailureCase {
            adapter: Box::new(OpenAiAdapter::new("key")),
            prefix: bridge_fixture("responses/stream-00.json"),
            error: json!({"type":"response.failed","response":{"error":{"code":"server_error","message":"busy"}}}),
            event: Some("response.failed"),
            category: Category::ServerError,
            code: Some("server_error"),
        },
        StreamFailureCase {
            adapter: Box::new(
                OpenAiAdapter::new("key")
                    .with_bridge(Arc::new(ChatCompletionsBridge::new()))
                    .expect("chat"),
            ),
            prefix: bridge_fixture("chat_completions/chunk_00.json"),
            error: json!({"error":{"type":"invalid_request_error","code":"context_length_exceeded","message":"too big"}}),
            event: None,
            category: Category::ContextLengthExceeded,
            code: Some("context_length_exceeded"),
        },
        StreamFailureCase {
            adapter: Box::new(OpenAiCompatibleAdapter::new("https://ollama.com/v1")),
            prefix: Vec::new(),
            error: json!({"error":{"type":"api_error","code":null,"message":"Unauthorized"}}),
            event: None,
            category: Category::Unknown,
            code: None,
        },
    ];
    for StreamFailureCase {
        adapter,
        prefix,
        error,
        event,
        category,
        code,
    } in cases
    {
        let headers = Headers(vec![
            ("x-request-id".into(), "upstream".into()),
            ("request-id".into(), "upstream".into()),
            ("retry-after".into(), "4".into()),
        ]);
        let prefix = if prefix.is_empty() {
            include_str!("fixtures/live/ollama_stream_usage.sse")
                .split("\n\n")
                .next()
                .expect("recorded first frame")
                .to_owned()
                + "\n\n"
        } else {
            format!(
                "data: {}\n\n",
                serde_json::from_slice::<Value>(&prefix).expect("recorded JSON")
            )
        };
        let mut stream = adapter.open_stream(200, &headers).expect("stream");
        let mut items = Vec::new();
        let error_frame = format!(
            "{}data: {error}\n\n",
            event
                .map(|event| format!("event: {event}\n"))
                .unwrap_or_default()
        );
        let wire = format!("{prefix}{error_frame}data: {{}}\n\n");
        for chunk in wire.as_bytes().chunks(13) {
            items.extend(stream.push(chunk));
        }
        assert!(items.len() >= 2, "{items:?}");
        assert!(matches!(items[0], Ok(StreamEvent::Start { .. })));
        assert!(items[..items.len() - 1].iter().all(Result::is_ok));
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, Ok(StreamEvent::Stop { .. })))
        );
        assert_eq!(
            items.last(),
            Some(&Err(RelayError::Provider(ProviderError {
                category,
                code: code.map(str::to_owned),
                http_status: None,
                request_id: Some("upstream".into()),
                retry_after: Some(RetryAfter::Seconds { seconds: 4 }),
                message: error["error"]["message"]
                    .as_str()
                    .or_else(|| error["response"]["error"]["message"].as_str())
                    .or_else(|| error["message"].as_str())
                    .expect("message")
                    .into()
            })))
        );
        assert!(stream.finish().next().is_none());
    }
}

#[test]
fn http_success_error_envelopes_are_provider_errors_but_malformed_successes_are_decode_errors() {
    for adapter in adapters() {
        let RelayError::Provider(error) = adapter
            .parse_response(
                200,
                &Headers::default(),
                br#"{"error":{"type":"future","message":"declared"}}"#,
            )
            .expect_err("declared failure")
        else {
            panic!("provider")
        };
        assert_eq!(error.category, Category::Unknown);
        assert_eq!(error.http_status, Some(200));
        assert!(matches!(
            adapter.parse_response(200, &Headers::default(), b"invalid"),
            Err(RelayError::Decode(_))
        ));
    }
}
