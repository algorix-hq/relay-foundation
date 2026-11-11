use relay_protocol::{AssistantContent, Response, StopReason, TextPart, Usage, Version};
use serde_json::{Value, json};

use super::{request, wire::*};
use crate::{ApiFormat, BridgeError, ConversionOptions, Encoded, LossScope, NativeFields, Output};

pub(super) fn decode(body: &[u8]) -> Result<Output, BridgeError> {
    let value =
        serde_json::from_slice(body).map_err(|error| decode_error("", error.to_string()))?;
    decode_value(&value)
}

pub(super) fn decode_value(value: &Value) -> Result<Output, BridgeError> {
    let mut native = decode_object(value, "")?;
    reject_failure(value)?;
    let id = Some(field!(value, "id", "")?);
    let model = field!(value, "model", "")?;
    let items: Vec<Value> = field!(value, "output", "")?;
    let mut content = Vec::new();
    let mut metadata = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let start = content.len();
        decode_item(item, &format!("/output/{index}"), &mut content)?;
        for _ in start..content.len() {
            metadata.push(if item.get("type").and_then(Value::as_str) == Some("message") {
                json!({"id":item.get("id").cloned().unwrap_or(Value::Null), "status":item.get("status").cloned().unwrap_or(Value::Null)})
            } else { json!({}) });
        }
    }
    let stop_reason = stop_reason(value)?;
    let usage = decode_usage(value.get("usage"))?;
    for key in ["id", "model", "output", "output_text", "usage"] {
        native.remove(key);
    }
    native.insert("output".into(), json!(metadata));
    if let Some(value) = value.get("usage").filter(|value| !value.is_null()) {
        let mut residual = decode_object(value, "/usage")?;
        residual.remove("input_tokens");
        residual.remove("output_tokens");
        for (key, represented) in [
            (
                "input_tokens_details",
                &["cached_tokens", "cache_write_tokens"][..],
            ),
            ("output_tokens_details", &["reasoning_tokens"][..]),
        ] {
            if let Some(details) = residual.get_mut(key).and_then(Value::as_object_mut) {
                for key in represented {
                    details.remove(*key);
                }
            }
        }
        native.insert("usage".into(), residual.into());
    }
    Ok(Output::with_native(
        ApiFormat::RESPONSES,
        Response {
            version: Version::V1,
            id,
            model,
            content,
            stop_reason,
            usage,
        },
        native,
    ))
}

pub(super) fn reject_failure(value: &Value) -> Result<(), BridgeError> {
    if value.get("status").and_then(Value::as_str) == Some("failed")
        || value.get("error").is_some_and(|value| !value.is_null())
    {
        return Err(decode_error(
            "/error",
            "adapter should have removed the failed response before bridge decoding",
        ));
    }
    Ok(())
}

pub(super) fn decode_item(
    value: &Value,
    path: &str,
    out: &mut Vec<AssistantContent>,
) -> Result<(), BridgeError> {
    let kind: String = field!(value, "type", path)?;
    match kind.as_str() {
        "message" => {
            let parts: Vec<Value> = field!(value, "content", path)?;
            for (index, part) in parts.into_iter().enumerate() {
                let path = format!("{path}/content/{index}");
                let kind: String = field!(&part, "type", &path)?;
                if kind == "output_text" {
                    out.push(AssistantContent::Text(TextPart {
                        text: field!(&part, "text", &path)?,
                        cache_control: None,
                    }));
                } else {
                    out.push(AssistantContent::Unknown { value: part });
                }
            }
        }
        "function_call" => out.push(AssistantContent::ToolCall {
            id: field!(value, "call_id", path)?,
            name: field!(value, "name", path)?,
            arguments: field!(value, "arguments", path)?,
            cache_control: None,
        }),
        "reasoning" => {
            let id = Some(field!(value, "id", path)?);
            let summary: Vec<Value> = field!(value, "summary", path)?;
            let summary = summary
                .iter()
                .enumerate()
                .map(|(index, part)| field!(part, "text", &format!("{path}/summary/{index}")))
                .collect::<Result<_, _>>()?;
            let content: Option<Vec<Value>> = field!(value, "content", path)?;
            let text = content.map(|parts| parts.iter().enumerate().map(|(index, part)| field!(String; part, "text", &format!("{path}/content/{index}"))).collect::<Result<Vec<_>, _>>().map(|parts| parts.concat())).transpose()?;
            let opaque = field!(value, "encrypted_content", path)?;
            out.push(AssistantContent::Reasoning {
                id,
                text,
                summary,
                opaque,
            });
        }
        _ => out.push(AssistantContent::Unknown {
            value: value.clone(),
        }),
    }
    Ok(())
}

pub(super) fn stop_reason(value: &Value) -> Result<StopReason, BridgeError> {
    reject_failure(value)?;
    let status: String = field!(value, "status", "")?;
    match status.as_str() {
        "completed" => {
            let output: Vec<Value> = field!(value, "output", "")?;
            Ok(
                if output
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
            )
        }
        "incomplete" => {
            let reason: String = field!(
                value.get("incomplete_details").unwrap_or(&Value::Null),
                "reason",
                "/incomplete_details"
            )?;
            Ok(match reason.as_str() {
                "max_output_tokens" => StopReason::MaxTokens,
                "content_filter" => StopReason::ContentFilter,
                _ => StopReason::Other(reason),
            })
        }
        _ => Err(decode_error(
            "/status",
            "expected completed or incomplete response",
        )),
    }
}

pub(super) fn decode_usage(value: Option<&Value>) -> Result<Option<Usage>, BridgeError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let input_details = value.get("input_tokens_details").unwrap_or(&Value::Null);
    let output_details = value.get("output_tokens_details").unwrap_or(&Value::Null);
    for (key, details) in [
        ("input_tokens_details", input_details),
        ("output_tokens_details", output_details),
    ] {
        if !details.is_null() && !details.is_object() {
            return Err(decode_error(
                &format!("/usage/{key}"),
                "expected token details object",
            ));
        }
    }
    Ok(Some(Usage {
        input_tokens: field!(value, "input_tokens", "/usage")?,
        output_tokens: field!(value, "output_tokens", "/usage")?,
        cache_read_tokens: field!(
            input_details,
            "cached_tokens",
            "/usage/input_tokens_details"
        )?,
        cache_write_tokens: field!(
            input_details,
            "cache_write_tokens",
            "/usage/input_tokens_details"
        )?,
        reasoning_tokens: field!(
            output_details,
            "reasoning_tokens",
            "/usage/output_tokens_details"
        )?,
    }))
}

pub(super) fn encode(output: &Output, options: &ConversionOptions) -> Result<Encoded, BridgeError> {
    let response = &output.response;
    let mut native = output
        .native_for(&ApiFormat::RESPONSES)
        .cloned()
        .unwrap_or_default();
    let metadata = native
        .remove("output")
        .map(|value| parse!(Vec<NativeFields>; value, "/output"))
        .transpose()?
        .unwrap_or_default();
    if !metadata.is_empty() && metadata.len() != response.content.len() {
        return Err(invalid(
            "/output",
            "native output metadata must align with protocol content",
        ));
    }
    let mut losses = Vec::new();
    let mut items = Vec::<Value>::new();
    for (index, content) in response.content.iter().enumerate() {
        let path = format!("/content/{index}");
        let mut item_metadata = metadata.get(index).cloned().unwrap_or_default();
        match content {
            AssistantContent::Text(part) => {
                request::cache(&part.cache_control, &path, options, &mut losses)?;
                let id: String =
                    required_native!(&mut item_metadata, "id", &format!("/output/{index}"))?;
                let status: String =
                    required_native!(&mut item_metadata, "status", &format!("/output/{index}"))?;
                if !matches!(status.as_str(), "completed" | "incomplete" | "in_progress") {
                    return Err(invalid(
                        &format!("/output/{index}/status"),
                        "invalid message status",
                    ));
                }
                reject_remaining(&item_metadata, &format!("/output/{index}"))?;
                let part = json!({"type":"output_text", "text":part.text, "annotations":[]});
                if let Some(last) = items.last_mut().filter(|last| {
                    last.get("type") == Some(&json!("message"))
                        && last.get("id") == Some(&json!(id))
                        && last.get("status") == Some(&json!(status))
                }) {
                    if let Some(parts) = last.get_mut("content").and_then(Value::as_array_mut) {
                        parts.push(part);
                    }
                } else {
                    items.push(json!({"type":"message", "id":id, "role":"assistant", "status":status, "content":[part]}));
                }
            }
            AssistantContent::ToolCall {
                id,
                name,
                arguments,
                cache_control,
            } => {
                request::cache(cache_control, &path, options, &mut losses)?;
                let mut item = NativeFields::from_iter([
                    ("type".into(), json!("function_call")),
                    ("call_id".into(), json!(id)),
                    ("name".into(), json!(name)),
                    ("arguments".into(), json!(arguments)),
                ]);
                merge(&mut item, &item_metadata, &format!("/output/{index}"))?;
                items.push(item.into());
            }
            AssistantContent::Reasoning {
                id,
                text,
                summary,
                opaque,
            } => {
                if let Some(value) = request::encode_reasoning(
                    id,
                    text,
                    summary,
                    opaque,
                    &path,
                    options,
                    &mut losses,
                )? {
                    let mut item = object(&value, &path)?;
                    merge(&mut item, &item_metadata, &format!("/output/{index}"))?;
                    items.push(item.into());
                }
            }
            AssistantContent::Unknown { .. } => {
                drop_allowed(LossScope::UnknownContent, &path, options, &mut losses)?
            }
        }
    }
    let usage_native = native.remove("usage");
    let usage = encode_usage(response.usage.as_ref(), usage_native)?;
    let id = native
        .remove("id")
        .or_else(|| response.id.as_ref().map(|id| json!(id)))
        .ok_or_else(|| unrepresentable("/id", "response ID is required"))?;
    let id: String = parse!(id, "/id")?;
    let created_at = native
        .remove("created_at")
        .or_else(|| options.created_at.map(|time| json!(time)))
        .ok_or_else(|| {
            unrepresentable(
                "/created_at",
                "creation time is required; bridge never reads a clock",
            )
        })?;
    if !created_at.is_number() {
        return Err(invalid("/created_at", "expected a numeric creation time"));
    }
    let (status, incomplete_details) = encode_stop(&response.stop_reason)?;
    let output_text: String = items
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    let mut body = NativeFields::from_iter([
        ("id".into(), json!(id)),
        ("model".into(), json!(response.model)),
        ("created_at".into(), json!(created_at)),
        ("object".into(), json!("response")),
        ("error".into(), Value::Null),
        ("status".into(), json!(status)),
        ("incomplete_details".into(), incomplete_details),
        ("output".into(), json!(items)),
        ("output_text".into(), json!(output_text)),
    ]);
    for key in [
        "object",
        "error",
        "status",
        "incomplete_details",
        "output_text",
    ] {
        if let Some(value) = native.remove(key)
            && body.get(key) != Some(&value)
        {
            return Err(invalid(
                &format!("/{key}"),
                "native field conflicts with derived response fact",
            ));
        }
    }
    for key in [
        "access_programs",
        "instructions",
        "metadata",
        "parallel_tool_calls",
        "temperature",
        "tool_choice",
        "tools",
        "top_p",
    ] {
        let value = native.remove(key).ok_or_else(|| {
            unrepresentable(
                &format!("/{key}"),
                "required response fact must be supplied natively",
            )
        })?;
        validate_response_fact(key, &value)?;
        body.insert(key.into(), value);
    }
    if let Some(usage) = usage {
        body.insert("usage".into(), usage);
    }
    merge(&mut body, &native, "")?;
    Ok(Encoded {
        body: body.into(),
        losses,
    })
}

pub(super) fn encode_stop(reason: &StopReason) -> Result<(&'static str, Value), BridgeError> {
    Ok(match reason {
        StopReason::EndTurn | StopReason::ToolUse => ("completed", Value::Null),
        StopReason::MaxTokens => ("incomplete", json!({"reason":"max_output_tokens"})),
        StopReason::ContentFilter => ("incomplete", json!({"reason":"content_filter"})),
        StopReason::Other(reason)
            if matches!(
                reason.as_str(),
                "max_output_tokens" | "max_messages" | "content_filter" | "steered"
            ) =>
        {
            ("incomplete", json!({"reason":reason}))
        }
        _ => {
            return Err(unrepresentable(
                "/stop_reason",
                "stop reason has no exact Responses representation",
            ));
        }
    })
}

fn validate_response_fact(key: &str, value: &Value) -> Result<(), BridgeError> {
    let valid = match key {
        "parallel_tool_calls" => value.is_boolean(),
        "temperature" | "top_p" => value.is_null() || value.is_number(),
        "tools" => value.is_array(),
        "tool_choice" => value.is_string() || value.is_object(),
        "instructions" => value.is_null() || value.is_string() || value.is_array(),
        _ => value.is_null() || value.is_object(),
    };
    if !valid {
        return Err(invalid(
            &format!("/{key}"),
            "invalid required response fact",
        ));
    }
    Ok(())
}

pub(super) fn encode_usage(
    usage: Option<&Usage>,
    native: Option<Value>,
) -> Result<Option<Value>, BridgeError> {
    let Some(usage) = usage else {
        if native.is_some() {
            return Err(invalid(
                "/usage",
                "native usage requires protocol token counts",
            ));
        }
        return Ok(None);
    };
    let mut native = native
        .map(|value| object(&value, "/usage"))
        .transpose()?
        .unwrap_or_default();
    let mut input_details = native
        .remove("input_tokens_details")
        .map(|value| object(&value, "/usage/input_tokens_details"))
        .transpose()?
        .unwrap_or_default();
    let mut output_details = native
        .remove("output_tokens_details")
        .map(|value| object(&value, "/usage/output_tokens_details"))
        .transpose()?
        .unwrap_or_default();
    for (key, count) in [
        ("cached_tokens", usage.cache_read_tokens),
        ("cache_write_tokens", usage.cache_write_tokens),
    ] {
        let count = match count {
            Some(count) => {
                if input_details.contains_key(key) {
                    return Err(invalid(
                        &format!("/usage/input_tokens_details/{key}"),
                        "native field collides with protocol counter",
                    ));
                }
                count
            }
            None => required_native!(&mut input_details, key, "/usage/input_tokens_details")?,
        };
        input_details.insert(key.into(), json!(count));
    }
    let reasoning_tokens = match usage.reasoning_tokens {
        Some(count) => {
            if output_details.contains_key("reasoning_tokens") {
                return Err(invalid(
                    "/usage/output_tokens_details/reasoning_tokens",
                    "native field collides with protocol counter",
                ));
            }
            count
        }
        None => required_native!(
            &mut output_details,
            "reasoning_tokens",
            "/usage/output_tokens_details"
        )?,
    };
    output_details.insert("reasoning_tokens".into(), json!(reasoning_tokens));
    let total = usage
        .input_tokens
        .checked_add(usage.output_tokens)
        .ok_or_else(|| invalid("/usage", "token total overflows u64"))?;
    if let Some(original) = native.remove("total_tokens")
        && original != json!(total)
    {
        return Err(invalid(
            "/usage/total_tokens",
            "native token total disagrees with protocol counts",
        ));
    }
    let mut value = NativeFields::from_iter([
        ("input_tokens".into(), json!(usage.input_tokens)),
        ("output_tokens".into(), json!(usage.output_tokens)),
        ("total_tokens".into(), json!(total)),
        ("input_tokens_details".into(), input_details.into()),
        ("output_tokens_details".into(), output_details.into()),
    ]);
    merge(&mut value, &native, "/usage")?;
    Ok(Some(value.into()))
}
