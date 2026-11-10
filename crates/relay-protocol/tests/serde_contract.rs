use relay_protocol::{
    AssistantContent, CacheControl, ConfigurationError, ConversionError, ConversionErrorKind,
    DecodeError, IncompleteStream, Message, ProviderError, ProviderErrorCategory, ReasoningEffort,
    RelayError, Request, Response, ResponseFormat, RetryAfter, StopReason, StreamEvent, ToolChoice,
    UnsupportedOperation, UserContent,
};
use serde_json::{Value, json};

#[test]
fn full_request_preserves_grouped_options_and_mixed_content() {
    let wire = json!({
        "version": "1",
        "model": "custom-model",
        "messages": [
            {"role": "system", "content": [{"type": "text", "text": "Be helpful",
                "cache_control": {"ttl_seconds": 3600}}]},
            {"role": "developer", "content": [{"type": "text", "text": "Use tools"}]},
            {"role": "assistant", "content": [
                {"type": "tool_call", "id": "call_1", "name": "lookup",
                    "arguments": "{ \"key\":1, \"key\": 2 }",
                    "cache_control": {}}
            ]},
            {"role": "user", "content": [
                {"type": "text", "text": "Before"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                    "data": "aW1hZ2U="}, "detail": "ultra"},
                {"type": "tool_result", "tool_call_id": "call_1", "content": [
                    {"type": "text", "text": "found"},
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"},
                        "detail": "high", "cache_control": {"ttl_seconds": 300}},
                    {"type": "document", "source": {"type": "base64", "media_type": "application/pdf",
                        "data": "cGRm"}, "title": "Report", "filename": "report.pdf"}
                ], "is_error": false, "cache_control": {}},
                {"type": "document", "source": {"type": "url", "url": "https://example.com/a.pdf"},
                    "title": "Reading", "filename": "a.pdf", "cache_control": {}},
                {"type": "text", "text": "After"}
            ]}
        ],
        "tools": {"definitions": [
            {"type": "function", "name": "lookup", "description": "Find a key",
                "parameters": {"type": "object", "properties": {"key": {"type": "integer"}}},
                "strict": true, "cache_control": {}}
        ], "choice": "auto", "parallel": true},
        "parameters": {"max_output_tokens": 1024, "temperature": 0.7,
            "top_p": 0.9, "stop": ["\n"]},
        "response_format": "json_object",
        "reasoning": {"effort": "ultra", "budget_tokens": 2048,
            "summary": "auto", "thinking": "adaptive"}
    });
    let request: Request = serde_json::from_value(wire.clone()).unwrap();
    let Message::Assistant { content } = &request.messages[2] else {
        panic!("assistant role lost");
    };
    let AssistantContent::ToolCall { arguments, .. } = &content[0] else {
        panic!("tool call lost");
    };
    assert_eq!(arguments, "{ \"key\":1, \"key\": 2 }");
    let Message::User { content } = &request.messages[3] else {
        panic!("user role lost");
    };
    assert!(matches!(content[2], UserContent::ToolResult { .. }));
    assert_eq!(
        request.reasoning.as_ref().unwrap().effort,
        Some(ReasoningEffort("ultra".into()))
    );
    assert_eq!(serde_json::to_value(request).unwrap(), wire);
}

#[test]
fn full_response_preserves_opaque_reasoning_usage_and_unknown_items() {
    let wire = json!({
        "version": "1", "id": "resp_1", "model": "custom-model",
        "content": [
            {"type": "reasoning", "text": "Thinking", "summary": ["One", "Two"],
                "opaque": "opaque-value"},
            {"type": "text", "text": "Answer", "cache_control": {}},
            {"type": "tool_call", "id": "call_1", "name": "lookup", "arguments": "{ \"a\": 1 }"},
            {"type": "unknown", "value": {"type": "unrecognized_output", "id": "opaque"}}
        ],
        "stop_reason": "pause_turn",
        "usage": {"input_tokens": 900, "output_tokens": 200, "cache_read_tokens": 500,
            "cache_write_tokens": 100, "reasoning_tokens": 120}
    });
    let response: Response = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(response.stop_reason, StopReason::Other("pause_turn".into()));
    assert_eq!(response.usage.as_ref().unwrap().input_tokens, 900);
    assert_eq!(serde_json::to_value(response).unwrap(), wire);
}

#[test]
fn unknown_fields_are_rejected_at_envelopes_and_nested_boundaries() {
    let base = json!({"version": "1", "model": "m", "messages": []});
    for (pointer, value) in [
        (
            "",
            json!({"version": "1", "model": "m", "messages": [], "seed": 1}),
        ),
        (
            "/messages",
            json!([{"role": "user", "content": [], "name": "unknown"}]),
        ),
        (
            "/messages",
            json!([{"role": "user", "content": [{"type": "text", "text": "x", "extra": 1}]}]),
        ),
        (
            "/messages",
            json!([{"role": "user", "content": [{"type": "image", "source": {"type": "url", "url": "u"}, "extra": 1}]}]),
        ),
        (
            "/messages",
            json!([{"role": "system", "content": [{"type": "text", "text": "x", "extra": 1}]}]),
        ),
        (
            "/messages",
            json!([{"role": "user", "content": [{"type": "document", "source": {"type": "url", "url": "u"}, "extra": 1}]}]),
        ),
    ] {
        let mut invalid = base.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(
            serde_json::from_value::<Request>(invalid).is_err(),
            "accepted {pointer}"
        );
    }
    for additional in [
        json!({"reasoning": {"effort": "low", "extra": true}}),
        json!({"parameters": {"max_output_tokens": 1, "top_k": 40}}),
        json!({"tools": {"definitions": [], "extra": true}}),
    ] {
        let mut invalid = base.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(additional.as_object().unwrap().clone());
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }
    assert!(
        serde_json::from_value::<Response>(json!({
            "version": "1", "model": "m", "content": [], "stop_reason": "end_turn", "extra": 1
        }))
        .is_err()
    );
    for item_type in ["text", "reasoning", "unknown"] {
        assert!(
            serde_json::from_value::<StreamEvent>(json!({
                "type": "item_start", "index": 0, "item": {"type": item_type, "extra": true}
            }))
            .is_err()
        );
    }
}

#[test]
fn envelopes_require_the_supported_string_version() {
    for version in [None, Some(json!("2")), Some(json!(1)), Some(Value::Null)] {
        let mut request = json!({"model": "m", "messages": []});
        let mut response = json!({"model": "m", "content": [], "stop_reason": "end_turn"});
        if let Some(version) = version {
            request["version"] = version.clone();
            response["version"] = version;
        }
        assert!(serde_json::from_value::<Request>(request).is_err());
        assert!(serde_json::from_value::<Response>(response).is_err());
    }
}

#[test]
fn unknown_stop_reason_retains_the_provider_string() {
    let context: StopReason = serde_json::from_str("\"context_window_exceeded\"").unwrap();
    assert_eq!(context, StopReason::ContextWindowExceeded);
    assert_eq!(
        serde_json::to_string(&context).unwrap(),
        "\"context_window_exceeded\""
    );
    let reason: StopReason = serde_json::from_str("\"provider_custom_stop\"").unwrap();
    assert_eq!(reason, StopReason::Other("provider_custom_stop".into()));
    assert_eq!(
        serde_json::to_string(&reason).unwrap(),
        "\"provider_custom_stop\""
    );
}

#[test]
fn empty_reasoning_effort_is_rejected() {
    assert!(serde_json::from_value::<ReasoningEffort>(json!("")).is_err());
}

#[test]
fn zero_cache_ttl_is_rejected() {
    assert!(serde_json::from_value::<CacheControl>(json!({"ttl_seconds": 0})).is_err());
}

#[test]
fn mixed_enums_preserve_strings_and_strict_tagged_objects() {
    for wire in [
        json!("auto"),
        json!("none"),
        json!("required"),
        json!({"type": "function", "name": "lookup"}),
    ] {
        let choice: ToolChoice = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(choice).unwrap(), wire);
    }
    for wire in [
        json!("text"),
        json!("json_object"),
        json!({"type": "json_schema", "name": "result", "description": "Answer",
            "schema": {"type": "object"}, "strict": true}),
        json!({"type": "json_schema", "name": "result", "schema": {}}),
    ] {
        let format: ResponseFormat = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(format).unwrap(), wire);
    }
    for wire in [
        json!("unknown"),
        json!("function"),
        json!({"type": "unknown"}),
        json!({"type": "auto"}),
        json!({"type": "function", "name": "f", "extra": true}),
    ] {
        assert!(serde_json::from_value::<ToolChoice>(wire).is_err());
    }
    for wire in [
        json!("unknown"),
        json!("json_schema"),
        json!({"type": "unknown"}),
        json!({"type": "text"}),
        json!({"type": "json_schema", "name": "s", "schema": {}, "extra": true}),
    ] {
        assert!(serde_json::from_value::<ResponseFormat>(wire).is_err());
    }
}

#[test]
fn absent_optional_fields_and_empty_collections_serialize_without_placeholders() {
    let request: Request = serde_json::from_value(json!({
        "version": "1", "model": "m", "messages": [],
        "reasoning": null
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "version": "1", "model": "m", "messages": []
        })
    );
    let content: AssistantContent = serde_json::from_value(json!({
        "type": "reasoning", "summary": [], "text": null
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(content).unwrap(),
        json!({"type": "reasoning"})
    );
}

#[test]
fn normal_stream_sequence_preserves_item_indices_and_delta_kinds() {
    let wire = json!([
        {"type": "response_start", "id": "resp_1", "model": "m"},
        {"type": "item_start", "index": 0, "item": {"type": "reasoning"}},
        {"type": "delta", "index": 0, "delta": {"type": "reasoning_text", "text": "Think"}},
        {"type": "delta", "index": 0, "delta": {"type": "reasoning_summary", "summary_index": 1, "text": "Summary"}},
        {"type": "delta", "index": 0, "delta": {"type": "reasoning_opaque", "value": "opaque-value"}},
        {"type": "item_end", "index": 0},
        {"type": "item_start", "index": 1, "item": {"type": "text"}},
        {"type": "delta", "index": 1, "delta": {"type": "text", "text": "Answer"}},
        {"type": "item_end", "index": 1},
        {"type": "item_start", "index": 2, "item": {"type": "tool_call", "id": "call_1", "name": "f"}},
        {"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "{ \"a\":"}},
        {"type": "delta", "index": 2, "delta": {"type": "tool_call_arguments", "text": "1 }"}},
        {"type": "item_end", "index": 2},
        {"type": "item_start", "index": 3, "item": {"type": "unknown"}},
        {"type": "delta", "index": 3, "delta": {"type": "unknown", "value": {"unrecognized_delta": "data"}}},
        {"type": "item_end", "index": 3},
        {"type": "stop", "stop_reason": "tool_use", "usage": {"input_tokens": 20, "output_tokens": 9}}
    ]);
    let events: Vec<StreamEvent> = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(events).unwrap(), wire);
}

#[test]
fn diagnostics_preserve_provider_headers_and_conversion_locations() {
    let wire = json!({"category": "rate_limited", "message": "Slow down", "code": "quota_rate",
        "http_status": 429, "request_id": "req_1", "retry_after": {"type": "http_date", "date": "unparsed-date"}});
    let error: ProviderError = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(error.category, ProviderErrorCategory::RateLimited);
    assert_eq!(
        error.retry_after,
        Some(RetryAfter::HttpDate {
            date: "unparsed-date".into()
        })
    );
    assert_eq!(serde_json::to_value(error).unwrap(), wire);
    let error: ConversionError = serde_json::from_value(json!({
        "kind": "unrepresentable", "path": "/messages/0/content/1", "message": "Cannot express image"
    })).unwrap();
    assert_eq!(error.kind, ConversionErrorKind::Unrepresentable);
    assert_eq!(
        error.to_string(),
        "/messages/0/content/1: Cannot express image"
    );
}

#[test]
fn relay_failures_preserve_tagged_causes_and_reject_unknown_fields() {
    use std::error::Error;

    let cases: [(RelayError, Value, &str); 6] = [
        (
            ProviderError {
                category: ProviderErrorCategory::Unknown,
                message: "declared failure".into(),
                code: None,
                http_status: Some(500),
                request_id: None,
                retry_after: None,
            }
            .into(),
            json!({"type": "provider", "category": "unknown", "message": "declared failure", "http_status": 500}),
            "declared failure",
        ),
        (
            ConversionError {
                kind: ConversionErrorKind::Unrepresentable,
                path: "/content/0".into(),
                message: "cannot express".into(),
            }
            .into(),
            json!({"type": "conversion", "kind": "unrepresentable", "path": "/content/0", "message": "cannot express"}),
            "/content/0: cannot express",
        ),
        (
            DecodeError {
                path: "/items".into(),
                message: "expected array".into(),
            }
            .into(),
            json!({"type": "decode", "path": "/items", "message": "expected array"}),
            "/items: expected array",
        ),
        (
            IncompleteStream {
                message: "missing terminal signal".into(),
            }
            .into(),
            json!({"type": "incomplete_stream", "message": "missing terminal signal"}),
            "missing terminal signal",
        ),
        (
            ConfigurationError {
                message: "missing setting".into(),
            }
            .into(),
            json!({"type": "configuration", "message": "missing setting"}),
            "missing setting",
        ),
        (
            UnsupportedOperation {
                operation: "list_models".into(),
                message: "not provided".into(),
            }
            .into(),
            json!({"type": "unsupported_operation", "operation": "list_models", "message": "not provided"}),
            "list_models: not provided",
        ),
    ];
    for (error, wire, source) in cases {
        assert_eq!(serde_json::to_value(&error).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<RelayError>(wire.clone()).unwrap(),
            error
        );
        assert_eq!(error.source().expect("preserved cause").to_string(), source);
        let mut invalid = wire;
        invalid["extra"] = json!(true);
        assert!(serde_json::from_value::<RelayError>(invalid).is_err());
    }
}
