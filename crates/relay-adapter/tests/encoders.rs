use relay_adapter::{ChatCompletionsErrors, ErrorEncoder, MessagesErrors, ResponsesErrors, Stage};
use relay_protocol::{
    ConfigurationError, ConversionError, ConversionErrorKind, DecodeError, Error, IncompleteStream,
    ProviderError, ProviderErrorCategory as Category, RetryAfter, UnsupportedOperation,
};
use serde_json::{Value, json};

fn provider(category: Category, http_status: Option<u16>) -> Error {
    ProviderError {
        category,
        http_status,
        message: "upstream failure".into(),
        code: Some("foreign-private-code".into()),
        request_id: Some("upstream-id".into()),
        retry_after: Some(RetryAfter::Seconds { seconds: 7 }),
    }
    .into()
}

fn encoders() -> [(&'static str, Box<dyn ErrorEncoder>); 3] {
    [
        ("chat_completions", Box::new(ChatCompletionsErrors::new())),
        ("responses", Box::new(ResponsesErrors::new())),
        ("messages", Box::new(MessagesErrors::new())),
    ]
}

#[test]
fn category_by_format_uses_neutral_statuses_and_never_forwards_foreign_codes_or_ids() {
    for (format, encoder) in encoders() {
        for (category, status, messages_status, kind, messages_kind, code) in [
            (
                Category::Authentication,
                401,
                401,
                "invalid_request_error",
                "authentication_error",
                Some("invalid_api_key"),
            ),
            (
                Category::PermissionDenied,
                403,
                403,
                "invalid_request_error",
                "permission_error",
                None,
            ),
            (
                Category::InsufficientQuota,
                429,
                402,
                "insufficient_quota",
                "billing_error",
                Some("insufficient_quota"),
            ),
            (
                Category::RateLimited,
                429,
                429,
                "rate_limit_error",
                "rate_limit_error",
                Some("rate_limit_exceeded"),
            ),
            (
                Category::Overloaded,
                503,
                529,
                "service_unavailable_error",
                "overloaded_error",
                Some("server_is_overloaded"),
            ),
            (
                Category::InvalidRequest,
                400,
                400,
                "invalid_request_error",
                "invalid_request_error",
                None,
            ),
            (
                Category::UnsupportedFeature,
                400,
                400,
                "invalid_request_error",
                "invalid_request_error",
                None,
            ),
            (
                Category::ModelNotFound,
                404,
                404,
                "invalid_request_error",
                "not_found_error",
                Some("model_not_found"),
            ),
            (
                Category::ContextLengthExceeded,
                400,
                400,
                "invalid_request_error",
                "invalid_request_error",
                Some("context_length_exceeded"),
            ),
            (
                Category::ServerError,
                500,
                500,
                "server_error",
                "api_error",
                Some("server_error"),
            ),
            (Category::Unknown, 418, 418, "api_error", "api_error", None),
        ] {
            let error = provider(category, Some(418));
            let encoded = encoder.encode_error(&error, Stage::Response);
            assert_eq!(
                encoded.status,
                if format == "messages" {
                    messages_status
                } else {
                    status
                },
                "{format} {category:?}"
            );
            let body: Value = serde_json::from_slice(&encoded.body).expect("error JSON");
            let expected = if format == "messages" {
                json!({"type":"error","error":{"type":messages_kind,"message":"upstream failure"},"request_id":null})
            } else {
                json!({"error":{"message":"upstream failure","type":kind,"code":code,"param":null}})
            };
            assert_eq!(body, expected, "{format} {category:?}");
            assert_eq!(encoded.headers.get("retry-after"), Some("7"));
            assert_eq!(
                encoded.headers.get("x-relay-upstream-request-id"),
                Some("upstream-id")
            );
            assert_eq!(encoded.headers.get("x-request-id"), None);
            assert_eq!(encoded.headers.get("request-id"), None);
            assert_eq!(
                encoded.headers.get("content-type"),
                Some("application/json")
            );

            let frames = encoder.encode_stream_error(&error);
            assert_eq!(frames.len(), 1);
            let frame = &frames[0];
            assert_eq!(
                frame.event.as_deref(),
                if format == "chat_completions" {
                    None
                } else {
                    Some("error")
                }
            );
            let body: Value = serde_json::from_str(&frame.data).expect("stream JSON");
            assert_eq!(
                body,
                if format == "responses" {
                    json!({"type":"error","code":code,"message":"upstream failure","param":null,"sequence_number":0})
                } else {
                    expected
                }
            );
        }
        assert_eq!(
            encoder
                .encode_error(&provider(Category::Unknown, None), Stage::Response)
                .status,
            502
        );
    }
}

#[test]
fn local_failures_use_stage_only_for_conversion_and_remain_sdk_encodable() {
    for (format, encoder) in encoders() {
        for (error, request_status, response_status) in [
            (
                Error::Conversion(ConversionError {
                    kind: ConversionErrorKind::Invalid,
                    path: "/messages".into(),
                    message: "bad input".into(),
                }),
                400,
                502,
            ),
            (
                Error::Decode(DecodeError {
                    path: "/content".into(),
                    message: "bad output".into(),
                }),
                502,
                502,
            ),
            (
                Error::IncompleteStream(IncompleteStream {
                    message: "no terminal event".into(),
                }),
                502,
                502,
            ),
            (
                Error::Configuration(ConfigurationError {
                    message: "missing key".into(),
                }),
                500,
                500,
            ),
            (
                Error::UnsupportedOperation(UnsupportedOperation {
                    operation: "list_models".into(),
                    message: "not implemented".into(),
                }),
                501,
                501,
            ),
        ] {
            for (stage, status) in [
                (Stage::Request, request_status),
                (Stage::Response, response_status),
            ] {
                let encoded = encoder.encode_error(&error, stage);
                assert_eq!(encoded.status, status, "{format} {error:?} {stage:?}");
                assert_eq!(encoded.headers.get("retry-after"), None);
                assert_eq!(encoded.headers.get("x-relay-upstream-request-id"), None);
                let body: Value = serde_json::from_slice(&encoded.body).expect("JSON");
                assert_eq!(body["error"]["message"], error.to_string());
                assert_eq!(
                    body["error"]["type"],
                    if matches!(error, Error::Conversion(_)) && stage == Stage::Request {
                        "invalid_request_error"
                    } else if format == "messages" {
                        "api_error"
                    } else {
                        "server_error"
                    }
                );
            }
            let frames = encoder.encode_stream_error(&error);
            assert_eq!(frames.len(), 1);
            serde_json::from_str::<Value>(&frames[0].data).expect("stream error frame is JSON");
        }
    }
}

#[test]
fn retry_dates_are_forwarded_verbatim_in_the_neutral_diagnostics_headers() {
    for (_, encoder) in encoders() {
        let mut error = provider(Category::RateLimited, Some(429));
        let Error::Provider(provider) = &mut error else {
            unreachable!()
        };
        provider.retry_after = Some(RetryAfter::HttpDate {
            date: "Wed, 11 Nov 2026 12:00:00 GMT".into(),
        });
        let encoded = encoder.encode_error(&error, Stage::Response);
        assert_eq!(
            encoded.headers.get("retry-after"),
            Some("Wed, 11 Nov 2026 12:00:00 GMT")
        );
    }
}
