use relay_protocol::{Response, StopReason, Usage, Version};
use serde_json::{Value, json};

use super::{content::*, *};

pub(super) fn decode(body: &[u8]) -> Result<Output, BridgeError> {
    decode_inner(body).map_err(decode_error)
}

fn decode_inner(body: &[u8]) -> Result<Output, BridgeError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| invalid("", e.to_string()))?;
    let mut native = object(&value, "")?.clone();
    let id = string(&value["id"], "/id")?;
    let model = string(&value["model"], "/model")?;
    let content = array(&value["content"], "/content")?
        .iter()
        .enumerate()
        .map(|(i, value)| decode_assistant(value, &format!("/content/{i}"), false))
        .collect::<Result<_, _>>()?;
    let stop_reason = decode_stop(&string(&value["stop_reason"], "/stop_reason")?);
    let usage = value
        .get("usage")
        .map(|value| decode_usage(value, "/usage"))
        .transpose()?;
    for key in ["id", "model", "content", "stop_reason", "usage"] {
        native.remove(key);
    }
    Ok(Output::with_native(
        ApiFormat::MESSAGES,
        Response {
            version: Version::V1,
            id: Some(id),
            model,
            content,
            stop_reason,
            usage,
        },
        native,
    ))
}

pub(super) fn decode_stop(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Refusal,
        "model_context_window_exceeded" => StopReason::ContextWindowExceeded,
        reason => StopReason::Other(reason.into()),
    }
}

pub(super) fn encode_stop(reason: &StopReason) -> Result<&str, BridgeError> {
    match reason {
        StopReason::EndTurn => Ok("end_turn"),
        StopReason::MaxTokens => Ok("max_tokens"),
        StopReason::StopSequence => Ok("stop_sequence"),
        StopReason::ToolUse => Ok("tool_use"),
        StopReason::Refusal => Ok("refusal"),
        StopReason::ContextWindowExceeded => Ok("model_context_window_exceeded"),
        StopReason::Other(reason)
            if matches!(
                reason.as_str(),
                "end_turn"
                    | "max_tokens"
                    | "stop_sequence"
                    | "tool_use"
                    | "pause_turn"
                    | "refusal"
                    | "model_context_window_exceeded"
            ) =>
        {
            Ok(reason)
        }
        _ => Err(unrepresentable(
            "/stop_reason",
            "stop reason has no Messages representation",
        )),
    }
}

fn count(value: &Value, key: &str, path: &str) -> Result<Option<u64>, BridgeError> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| unsigned(v, &pointer(path, key)))
        .transpose()
}

pub(super) fn decode_usage(value: &Value, path: &str) -> Result<Usage, BridgeError> {
    object(value, path)?;
    let input = count(value, "input_tokens", path)?.unwrap_or(0);
    let cache_read_tokens = count(value, "cache_read_input_tokens", path)?;
    let cache_write_tokens = count(value, "cache_creation_input_tokens", path)?;
    let input_tokens = input
        .checked_add(cache_read_tokens.unwrap_or(0))
        .and_then(|v| v.checked_add(cache_write_tokens.unwrap_or(0)))
        .ok_or_else(|| invalid(path, "input token total overflows"))?;
    let reasoning_tokens = value
        .get("output_tokens_details")
        .filter(|v| !v.is_null())
        .map(|v| {
            object(v, &pointer(path, "output_tokens_details"))?;
            count(
                v,
                "thinking_tokens",
                &pointer(path, "output_tokens_details"),
            )
        })
        .transpose()?
        .flatten();
    Ok(Usage {
        input_tokens,
        output_tokens: count(value, "output_tokens", path)?.unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    })
}

pub(super) fn encode_usage(usage: Option<&Usage>) -> Result<Value, BridgeError> {
    let usage = usage
        .ok_or_else(|| unrepresentable("/usage", "Messages requires provider-supplied usage"))?;
    let input = usage.input_tokens;
    let output = usage.output_tokens;
    let read = usage.cache_read_tokens;
    let write = usage.cache_write_tokens;
    let reasoning = usage.reasoning_tokens;
    let ordinary = input
        .checked_sub(read.unwrap_or(0))
        .and_then(|v| v.checked_sub(write.unwrap_or(0)))
        .ok_or_else(|| {
            invalid(
                "/usage/input_tokens",
                "cache token counts exceed total input tokens",
            )
        })?;
    if reasoning.is_some_and(|n| n > output) {
        return Err(invalid(
            "/usage/reasoning_tokens",
            "reasoning tokens exceed output tokens",
        ));
    }
    Ok(json!({
        "input_tokens":ordinary,
        "output_tokens":output,
        "cache_read_input_tokens":read,
        "cache_creation_input_tokens":write,
        "cache_creation":null,
        "inference_geo":null,
        "output_tokens_details":reasoning.map(|n| json!({"thinking_tokens":n})),
        "server_tool_use":null,
        "service_tier":null,
        "speed":null,
    }))
}

pub(super) fn message(id: &str, model: &str, usage: Value) -> Value {
    json!({
        "id":id, "type":"message", "role":"assistant", "model":model,
        "content":[], "stop_reason":null, "stop_sequence":null,
        "container":null, "diagnostics":null, "stop_details":null, "usage":usage,
    })
}

pub(super) fn encode(output: &Output, options: &ConversionOptions) -> Result<Encoded, BridgeError> {
    let response = &output.response;
    let mut native = output
        .native_for(&ApiFormat::MESSAGES)
        .cloned()
        .unwrap_or_default();
    let id = match native.remove("id") {
        Some(value) => string(&value, "/id")?,
        None => response
            .id
            .clone()
            .ok_or_else(|| unrepresentable("/id", "Messages responses require an ID"))?,
    };
    let mut body = message(&id, &response.model, encode_usage(response.usage.as_ref())?);
    for (key, expected) in [("type", "message"), ("role", "assistant")] {
        if let Some(value) = native.remove(key)
            && value != expected
        {
            return Err(invalid(&pointer("", key), format!("expected {expected}")));
        }
    }
    for key in ["stop_sequence", "container", "diagnostics", "stop_details"] {
        if let Some(value) = native.remove(key) {
            body[key] = value;
        }
    }
    if !body["stop_sequence"].is_null() {
        string(&body["stop_sequence"], "/stop_sequence")?;
    }
    let mut losses = Vec::new();
    let mut content = Vec::new();
    for (i, part) in response.content.iter().enumerate() {
        if let Some(block) =
            encode_assistant(part, &format!("/content/{i}"), true, options, &mut losses)?
        {
            content.push(block);
        }
    }
    body["content"] = json!(content);
    body["stop_reason"] = json!(encode_stop(&response.stop_reason)?);
    merge_native(&mut body, &native)?;
    Ok(Encoded { body, losses })
}
