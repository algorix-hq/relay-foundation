use relay_protocol::{
    AssistantContent, ConversionLoss, Request, Response, StopReason, Timestamp, Usage, Version,
};
use serde_json::{Map, Value, json};

use super::{request, wire::*};
use crate::{BridgeError, ConversionOptions, DecodeContext, Encoded, LossScope};

pub(super) fn identity(
    value: &Value,
    context: &DecodeContext,
) -> Result<(String, Option<String>), BridgeError> {
    let provider_id: Option<String> = field!(value, "id", "")?;
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

pub(super) fn decode(body: &[u8], context: &DecodeContext) -> Result<Response, BridgeError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| decode_error("", error.to_string()))?;
    decode_object(&value, "")?;
    reject_failure(&value)?;
    let (id, provider_id) = identity(&value, context)?;
    let items: Vec<Value> = field!(&value, "output", "")?;
    let mut content = Vec::new();
    for (index, item) in items.iter().enumerate() {
        decode_item(item, &format!("/output/{index}"), &id, index, &mut content)?;
    }
    Ok(Response {
        version: Version::V1,
        id,
        provider_id,
        created_at: Timestamp {
            observed_at: context.observed_at,
            provider_created_at: field!(&value, "created_at", "")?,
        },
        model: field!(&value, "model", "")?,
        content,
        stop_reason: stop_reason(&value)?,
        usage: decode_usage(value.get("usage"))?,
    })
}

pub(super) fn reject_failure(value: &Value) -> Result<(), BridgeError> {
    if value["status"] == "failed" || value.get("error").is_some_and(|value| !value.is_null()) {
        return Err(decode_error(
            "/error",
            "provider failure should have been removed by the adapter",
        ));
    }
    Ok(())
}

pub(super) fn decode_item(
    value: &Value,
    path: &str,
    response_id: &str,
    index: usize,
    out: &mut Vec<AssistantContent>,
) -> Result<(), BridgeError> {
    let provider_id: Option<String> = field!(value, "id", path)?;
    let id = provider_id
        .clone()
        .unwrap_or_else(|| format!("{response_id}:{index}"));
    let kind: String = field!(value, "type", path)?;
    match kind.as_str() {
        "message" => {
            let parts: Vec<Value> = field!(value, "content", path)?;
            if parts.is_empty() {
                out.push(AssistantContent::Text {
                    id: id.clone(),
                    provider_id: provider_id.clone(),
                    text: String::new(),
                    cache_control: None,
                });
            }
            for (index, part) in parts.into_iter().enumerate() {
                let path = format!("{path}/content/{index}");
                let kind: String = field!(&part, "type", &path)?;
                if kind == "output_text" {
                    out.push(AssistantContent::Text {
                        id: id.clone(),
                        provider_id: provider_id.clone(),
                        text: field!(&part, "text", &path)?,
                        cache_control: None,
                    });
                } else {
                    out.push(AssistantContent::Unknown {
                        id: id.clone(),
                        provider_id: provider_id.clone(),
                        value: part,
                    });
                }
            }
        }
        "function_call" => out.push(AssistantContent::ToolCall {
            id,
            provider_id,
            call_id: field!(value, "call_id", path)?,
            name: field!(value, "name", path)?,
            arguments: field!(value, "arguments", path)?,
            cache_control: None,
        }),
        "reasoning" => {
            let summary: Vec<Value> = field!(value, "summary", path)?;
            let summary = summary
                .iter()
                .enumerate()
                .map(|(index, part)| field!(part, "text", &format!("{path}/summary/{index}")))
                .collect::<Result<_, _>>()?;
            let content: Option<Vec<Value>> = field!(value, "content", path)?;
            let text = content.map(|parts| parts.iter().enumerate().map(|(index, part)| field!(String; part, "text", &format!("{path}/content/{index}"))).collect::<Result<Vec<_>, _>>().map(|parts| parts.concat())).transpose()?;
            out.push(AssistantContent::Reasoning {
                id,
                provider_id,
                text,
                summary,
                opaque: field!(value, "encrypted_content", path)?,
            });
        }
        _ => out.push(AssistantContent::Unknown {
            id,
            provider_id,
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
                if output.iter().any(|item| item["type"] == "function_call") {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
            )
        }
        "incomplete" => {
            let reason: String = field!(
                &value["incomplete_details"],
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
    let input_details = &value["input_tokens_details"];
    let output_details = &value["output_tokens_details"];
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

pub(super) fn envelope(
    request: &Request,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Map<String, Value>, BridgeError> {
    let mut body = Map::from_iter([
        ("tools".into(), json!([])),
        ("tool_choice".into(), json!("auto")),
        ("parallel_tool_calls".into(), json!(true)),
        ("temperature".into(), Value::Null),
        ("top_p".into(), Value::Null),
        ("access_programs".into(), Value::Null),
        ("instructions".into(), Value::Null),
        ("metadata".into(), json!({})),
    ]);
    body.extend(request::encode_settings(request, options, losses)?);
    Ok(body)
}

pub(super) fn encode(
    request: &Request,
    response: &Response,
    options: &ConversionOptions,
) -> Result<Encoded, BridgeError> {
    let mut losses = Vec::new();
    let mut body = envelope(request, options, &mut losses)?;
    let (status, incomplete_details) = encode_stop(&response.stop_reason)?;
    let mut items = Vec::<Value>::new();
    for (index, content) in response.content.iter().enumerate() {
        let path = format!("/content/{index}");
        match content {
            AssistantContent::Text {
                id,
                text,
                cache_control,
                ..
            } => {
                request::cache(cache_control, &path, options, &mut losses)?;
                let part = json!({"type":"output_text", "text":text, "annotations":[]});
                if let Some(parts) = items
                    .last_mut()
                    .filter(|item| item["type"] == "message" && item["id"] == *id)
                    .and_then(|item| item["content"].as_array_mut())
                {
                    parts.push(part);
                } else {
                    items.push(json!({"type":"message", "id":id, "role":"assistant", "status":status, "content":[part]}));
                }
            }
            AssistantContent::ToolCall {
                id,
                call_id,
                name,
                arguments,
                cache_control,
                ..
            } => {
                request::cache(cache_control, &path, options, &mut losses)?;
                items.push(json!({"type":"function_call", "id":id, "call_id":call_id, "name":name, "arguments":arguments}));
            }
            AssistantContent::Reasoning {
                id,
                text,
                summary,
                opaque,
                ..
            } => {
                if let Some(item) = request::encode_reasoning(
                    &Some(id.clone()),
                    text,
                    summary,
                    opaque,
                    &path,
                    options,
                    &mut losses,
                )? {
                    items.push(item);
                }
            }
            AssistantContent::Unknown { .. } => {
                drop_allowed(LossScope::UnknownContent, &path, options, &mut losses)?
            }
        }
    }
    body.extend(Map::from_iter([
        ("id".into(), json!(response.id)),
        ("model".into(), json!(response.model)),
        (
            "created_at".into(),
            json!(
                response
                    .created_at
                    .provider_created_at
                    .unwrap_or(response.created_at.observed_at)
            ),
        ),
        ("object".into(), json!("response")),
        ("error".into(), Value::Null),
        ("status".into(), json!(status)),
        ("incomplete_details".into(), incomplete_details),
        ("output_text".into(), json!(output_text(&items))),
        ("output".into(), json!(items)),
    ]));
    if let Some(usage) = &response.usage {
        body.insert("usage".into(), encode_usage(usage)?);
    }
    Ok(Encoded {
        body: body.into(),
        losses,
    })
}

pub(super) fn output_text(items: &[Value]) -> String {
    items
        .iter()
        .filter(|item| item["type"] == "message")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect()
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

pub(super) fn encode_usage(usage: &Usage) -> Result<Value, BridgeError> {
    let cached = usage.cache_read_tokens.ok_or_else(|| {
        unrepresentable(
            "/usage/input_tokens_details/cached_tokens",
            "required counter missing",
        )
    })?;
    let reasoning = usage.reasoning_tokens.ok_or_else(|| {
        unrepresentable(
            "/usage/output_tokens_details/reasoning_tokens",
            "required counter missing",
        )
    })?;
    let cache_write = usage.cache_write_tokens.ok_or_else(|| {
        unrepresentable(
            "/usage/input_tokens_details/cache_write_tokens",
            "required counter missing",
        )
    })?;
    let total = usage
        .input_tokens
        .checked_add(usage.output_tokens)
        .ok_or_else(|| invalid("/usage", "token total overflows u64"))?;
    Ok(
        json!({"input_tokens":usage.input_tokens, "output_tokens":usage.output_tokens, "total_tokens":total, "input_tokens_details":{"cached_tokens":cached, "cache_write_tokens":cache_write}, "output_tokens_details":{"reasoning_tokens":reasoning}}),
    )
}
