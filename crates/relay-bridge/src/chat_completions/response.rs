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

pub(super) fn response_identity(
    value: &Value,
    context: &DecodeContext,
) -> Result<(String, Option<String>), BridgeError> {
    let provider_id: Option<String> = read(&value["id"], "/id")?;
    let id = provider_id
        .clone()
        .or_else(|| context.response_id.clone())
        .ok_or_else(|| {
            decode_error(
                "/id",
                "response ID missing and no adapter fallback supplied",
            )
        })?;
    Ok((id, provider_id))
}

fn unknown(response_id: &str, index: usize, value: Value) -> AssistantContent {
    AssistantContent::Unknown {
        id: format!("{response_id}:{index}"),
        provider_id: None,
        value,
    }
}

pub(super) fn decode(
    body: &[u8],
    context: &DecodeContext,
    reasoning_field: ReasoningField,
) -> Result<Response, BridgeError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| decode_error("", e.to_string()))?;
    if !value.is_object() {
        return Err(decode_error("", "expected response object"));
    }
    let (id, provider_id) = response_identity(&value, context)?;
    let created_at = Timestamp {
        observed_at: context.observed_at,
        provider_created_at: read(&value["created"], "/created")?,
    };
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
    if let Some(text) = decode_reasoning(message, "/choices/0/message", reasoning_field)? {
        content.push(AssistantContent::Reasoning {
            id: format!("{id}:0"),
            provider_id: None,
            text: Some(text),
            summary: Vec::new(),
            opaque: None,
        });
    }
    match &message["content"] {
        Value::Null => {}
        Value::String(text) => content.push(AssistantContent::Text {
            id: format!("{id}:{}", content.len()),
            provider_id: None,
            text: text.clone(),
            cache_control: None,
        }),
        Value::Array(parts) => {
            for (i, value) in parts.iter().enumerate() {
                if value["type"] == "text" {
                    content.push(AssistantContent::Text {
                        id: format!("{id}:{}", content.len()),
                        provider_id: None,
                        text: read(
                            &value["text"],
                            &format!("/choices/0/message/content/{i}/text"),
                        )?,
                        cache_control: None,
                    });
                } else {
                    content.push(unknown(&id, content.len(), value.clone()));
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
        content.push(unknown(&id, content.len(), message.clone()));
    }
    if let Some(calls) = message.get("tool_calls") {
        for (i, call) in read::<Vec<Value>>(calls, "/choices/0/message/tool_calls")?
            .iter()
            .enumerate()
        {
            let path = format!("/choices/0/message/tool_calls/{i}");
            let kind: String = read(&call["type"], &format!("{path}/type"))?;
            if kind != "function" {
                content.push(unknown(&id, content.len(), call.clone()));
                continue;
            }
            content.push(AssistantContent::ToolCall {
                id: format!("{id}:{}", content.len()),
                provider_id: None,
                call_id: read(&call["id"], &format!("{path}/id"))?,
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
        content.push(unknown(&id, content.len(), json!({"function_call":call})));
    }
    Ok(Response {
        version: Version::V1,
        id,
        provider_id,
        created_at,
        model: read(&value["model"], "/model")?,
        content,
        stop_reason: stop_reason(&reason),
        usage: decode_usage(&value["usage"])?,
    })
}

pub(super) struct EncodedContent {
    pub(super) text: Option<String>,
    pub(super) calls: Vec<Value>,
    pub(super) reasoning: Option<(&'static str, String)>,
}

pub(super) fn encode_content(
    content: &[AssistantContent],
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
    reasoning_field: ReasoningField,
) -> Result<EncodedContent, BridgeError> {
    let mut text: Option<String> = None;
    let mut calls = Vec::new();
    let mut reasoning = None;
    for (i, item) in content.iter().enumerate() {
        let path = format!("{path}/{i}");
        match item {
            AssistantContent::Text {
                text: part_text,
                cache_control,
                ..
            } => {
                super::request::cache(
                    cache_control,
                    &format!("{path}/cache_control"),
                    options,
                    losses,
                )?;
                text.get_or_insert_default().push_str(part_text);
            }
            AssistantContent::ToolCall {
                call_id,
                name,
                arguments,
                cache_control,
                ..
            } => {
                super::request::cache(
                    cache_control,
                    &format!("{path}/cache_control"),
                    options,
                    losses,
                )?;
                calls.push(json!({"id":call_id,"type":"function","function":{"name":name,"arguments":arguments}}));
            }
            AssistantContent::Reasoning {
                provider_id,
                text,
                summary,
                opaque,
                ..
            } => {
                if let (Some(name), Some(text)) = (reasoning_field.name(), text)
                    && reasoning.is_none()
                    && provider_id.is_none()
                    && summary.is_empty()
                    && opaque.is_none()
                {
                    reasoning = Some((name, text.clone()));
                } else {
                    omit(options, losses, LossScope::Reasoning, &path)?;
                }
            }
            AssistantContent::Unknown { .. } => {
                omit(options, losses, LossScope::UnknownContent, &path)?
            }
        }
    }
    Ok(EncodedContent {
        text,
        calls,
        reasoning,
    })
}

pub(super) fn encode(
    response: &Response,
    options: &ConversionOptions,
    reasoning_field: ReasoningField,
) -> Result<Encoded, BridgeError> {
    let created = response
        .created_at
        .provider_created_at
        .unwrap_or(response.created_at.observed_at);
    let mut losses = Vec::new();
    let content = encode_content(
        &response.content,
        "/content",
        options,
        &mut losses,
        reasoning_field,
    )?;
    let mut message = json!({"role":"assistant","content":content.text,"refusal":null});
    if let Some((name, text)) = content.reasoning {
        message[name] = json!(text);
    }
    if !content.calls.is_empty() {
        message["tool_calls"] = json!(content.calls);
    }
    let mut body = Map::from_iter([
        ("id".into(), json!(response.id)),
        ("created".into(), json!(created)),
        ("model".into(), json!(response.model)),
        ("object".into(), json!("chat.completion")),
        (
            "choices".into(),
            json!([{"index":0,"message":message,"finish_reason":finish_reason(&response.stop_reason)?,"logprobs":null}]),
        ),
    ]);
    if let Some(usage) = &response.usage {
        body.insert("usage".into(), encode_usage(usage)?);
    }
    Ok(Encoded {
        body: Value::Object(body),
        losses,
    })
}
