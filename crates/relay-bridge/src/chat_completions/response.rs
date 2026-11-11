use relay_protocol::*;
use serde_json::{Map, Value, json};

use super::*;

pub(super) fn read<T: FromValue>(value: &Value, path: &str) -> Result<T, BridgeError> {
    T::from_value(value.clone()).map_err(|e| decode_error(path, e))
}

pub(super) fn stop_reason(value: &str) -> StopReason {
    match value {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Other(other.into()),
    }
}

pub(super) fn finish_reason(reason: &StopReason) -> Result<&str, BridgeError> {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence => Ok("stop"),
        StopReason::MaxTokens | StopReason::ContextWindowExceeded => Ok("length"),
        StopReason::ToolUse => Ok("tool_calls"),
        StopReason::ContentFilter | StopReason::Refusal => Ok("content_filter"),
        StopReason::Other(value)
            if [
                "stop",
                "length",
                "tool_calls",
                "function_call",
                "content_filter",
            ]
            .contains(&value.as_str()) =>
        {
            Ok(value)
        }
        _ => Err(unrepresentable(
            "/stop_reason",
            "stop reason has no Chat Completions representation",
        )),
    }
}

pub(super) fn decode_usage(value: &Value) -> Result<Option<Usage>, BridgeError> {
    if value.is_null() {
        return Ok(None);
    }
    let input_tokens = read(&value["prompt_tokens"], "/usage/prompt_tokens")?;
    let output_tokens = read(&value["completion_tokens"], "/usage/completion_tokens")?;
    let cache_read_tokens = read(
        &value["prompt_tokens_details"]["cached_tokens"],
        "/usage/prompt_tokens_details/cached_tokens",
    )?;
    let reasoning_tokens = read(
        &value["completion_tokens_details"]["reasoning_tokens"],
        "/usage/completion_tokens_details/reasoning_tokens",
    )?;
    let cache_write_tokens = read(
        &value["prompt_tokens_details"]["cache_write_tokens"],
        "/usage/prompt_tokens_details/cache_write_tokens",
    )?;
    Ok(Some(Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        reasoning_tokens,
        cache_write_tokens,
    }))
}

pub(super) fn encode_usage(usage: &Usage) -> Result<Value, BridgeError> {
    let total = usage
        .input_tokens
        .checked_add(usage.output_tokens)
        .ok_or_else(|| invalid("/usage", "total token count overflows"))?;
    let mut value = json!({"prompt_tokens":usage.input_tokens,"completion_tokens":usage.output_tokens,"total_tokens":total});
    if let Some(tokens) = usage.cache_read_tokens {
        value["prompt_tokens_details"] = json!({"cached_tokens":tokens});
    }
    if let Some(tokens) = usage.cache_write_tokens {
        value["prompt_tokens_details"]["cache_write_tokens"] = json!(tokens);
    }
    if let Some(tokens) = usage.reasoning_tokens {
        value["completion_tokens_details"] = json!({"reasoning_tokens":tokens});
    }
    Ok(value)
}

pub(super) fn decode(body: &[u8]) -> Result<Output, BridgeError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| decode_error("", e.to_string()))?;
    let mut native = value
        .as_object()
        .cloned()
        .ok_or_else(|| decode_error("", "expected response object"))?;
    let _: u64 = read(&value["created"], "/created")?;
    if value["object"] != "chat.completion" {
        return Err(decode_error("/object", "expected chat.completion"));
    }
    let choices: Vec<Value> = read(&value["choices"], "/choices")?;
    if choices.len() > 1 {
        return Err(decode_error("/choices", "multiple choices are unsupported"));
    }
    let choice = choices
        .first()
        .ok_or_else(|| decode_error("/choices", "required choice missing"))?;
    let reason: String = read(&choice["finish_reason"], "/choices/0/finish_reason")?;
    let message = &choice["message"];
    if !message.is_object() {
        return Err(decode_error(
            "/choices/0/message",
            "required message object missing",
        ));
    }
    let role: String = read(&message["role"], "/choices/0/message/role")?;
    if role != "assistant" {
        return Err(decode_error(
            "/choices/0/message/role",
            "expected assistant role",
        ));
    }
    if message.get("content").is_none() {
        return Err(decode_error(
            "/choices/0/message/content",
            "required content field missing",
        ));
    }
    let mut content = Vec::new();
    match &message["content"] {
        Value::Null => {}
        Value::String(text) => content.push(AssistantContent::Text(TextPart {
            text: text.clone(),
            cache_control: None,
        })),
        Value::Array(parts) => {
            for (i, value) in parts.iter().enumerate() {
                if value["type"] == "text" {
                    content.push(AssistantContent::Text(TextPart {
                        text: read(
                            &value["text"],
                            &format!("/choices/0/message/content/{i}/text"),
                        )?,
                        cache_control: None,
                    }));
                } else {
                    content.push(AssistantContent::Unknown {
                        value: value.clone(),
                    });
                }
            }
        }
        _ => {
            return Err(decode_error(
                "/choices/0/message/content",
                "expected text or null",
            ));
        }
    }
    if reason == "content_filter" || message["refusal"].is_string() {
        content.push(AssistantContent::Unknown {
            value: message.clone(),
        });
    }
    if let Some(calls) = message.get("tool_calls") {
        for (i, call) in read::<Vec<Value>>(calls, "/choices/0/message/tool_calls")?
            .iter()
            .enumerate()
        {
            let path = format!("/choices/0/message/tool_calls/{i}");
            let kind: String = read(&call["type"], &format!("{path}/type"))?;
            if kind != "function" {
                content.push(AssistantContent::Unknown {
                    value: call.clone(),
                });
                continue;
            }
            content.push(AssistantContent::ToolCall {
                id: read(&call["id"], &format!("{path}/id"))?,
                name: read(&call["function"]["name"], &format!("{path}/function/name"))?,
                arguments: read(
                    &call["function"]["arguments"],
                    &format!("{path}/function/arguments"),
                )?,
                cache_control: None,
            });
        }
    }
    if let Some(call) = message.get("function_call").filter(|v| !v.is_null()) {
        content.push(AssistantContent::Unknown {
            value: json!({"function_call":call}),
        });
    }
    let response = Response {
        version: Version::V1,
        id: Some(read(&value["id"], "/id")?),
        model: read(&value["model"], "/model")?,
        content,
        stop_reason: stop_reason(&reason),
        usage: decode_usage(&value["usage"])?,
    };
    for key in ["id", "model", "choices", "usage"] {
        native.remove(key);
    }
    Ok(Output::with_native(
        ApiFormat::CHAT_COMPLETIONS,
        response,
        native,
    ))
}

pub(super) fn encode_content(
    content: &[AssistantContent],
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<(Option<String>, Vec<Value>), BridgeError> {
    let mut text: Option<String> = None;
    let mut calls = Vec::new();
    for (i, item) in content.iter().enumerate() {
        let path = format!("{path}/{i}");
        match item {
            AssistantContent::Text(part) => {
                super::request::cache(
                    &part.cache_control,
                    &format!("{path}/cache_control"),
                    options,
                    losses,
                )?;
                text.get_or_insert_default().push_str(&part.text);
            }
            AssistantContent::ToolCall {
                id,
                name,
                arguments,
                cache_control,
            } => {
                super::request::cache(
                    cache_control,
                    &format!("{path}/cache_control"),
                    options,
                    losses,
                )?;
                calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments}}));
            }
            AssistantContent::Reasoning { .. } => {
                omit(options, losses, LossScope::Reasoning, &path)?
            }
            AssistantContent::Unknown { .. } => {
                omit(options, losses, LossScope::UnknownContent, &path)?
            }
        }
    }
    Ok((text, calls))
}

pub(super) fn encode(output: &Output, options: &ConversionOptions) -> Result<Encoded, BridgeError> {
    let response = &output.response;
    let mut native = output
        .native_for(&ApiFormat::CHAT_COMPLETIONS)
        .cloned()
        .unwrap_or_default();
    let id: String = if let Some(id) = native.remove("id") {
        parse(id, "/id")?
    } else {
        response
            .id
            .clone()
            .ok_or_else(|| unrepresentable("/id", "required id missing"))?
    };
    let created: u64 = if let Some(created) = native.remove("created") {
        parse(created, "/created")?
    } else {
        options.created_at.ok_or_else(|| {
            unrepresentable(
                "/created",
                "required created missing; supply options.created_at",
            )
        })?
    };
    let object = native
        .remove("object")
        .unwrap_or_else(|| json!("chat.completion"));
    if object != "chat.completion" {
        return Err(invalid("/object", "expected chat.completion"));
    }
    let mut losses = Vec::new();
    let (text, calls) = encode_content(&response.content, "/content", options, &mut losses)?;
    let mut message = json!({"role":"assistant","content":text,"refusal":null});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    let mut body = Map::from_iter([
        ("id".into(), json!(id)),
        ("created".into(), json!(created)),
        ("model".into(), json!(response.model)),
        ("object".into(), object),
        (
            "choices".into(),
            json!([{"index":0,"message":message,"finish_reason":finish_reason(&response.stop_reason)?,"logprobs":null}]),
        ),
    ]);
    if let Some(usage) = &response.usage {
        body.insert("usage".into(), encode_usage(usage)?);
    }
    merge(&mut body, Some(&native))?;
    Ok(Encoded {
        body: Value::Object(body),
        losses,
    })
}
