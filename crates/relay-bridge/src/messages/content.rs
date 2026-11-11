use relay_protocol::{
    AssistantContent, CacheControl, DocumentPart, ImagePart, MediaSource, TextPart,
    ToolResultContent, UserContent,
};
use serde_json::{Value, json};

use super::*;

pub(super) fn decode_cache(value: &Value, path: &str) -> Result<Option<CacheControl>, BridgeError> {
    if value.is_null() {
        return Ok(None);
    }
    fields(value, &["type", "ttl"], path)?;
    if string(&value["type"], &pointer(path, "type"))? != "ephemeral" {
        return Err(unrepresentable(
            &pointer(path, "type"),
            "unsupported cache control type",
        ));
    }
    let ttl_seconds = match value.get("ttl") {
        None => None,
        Some(Value::String(ttl)) if ttl == "5m" => Some(300),
        Some(Value::String(ttl)) if ttl == "1h" => Some(3600),
        Some(_) => return Err(invalid(&pointer(path, "ttl"), "expected 5m or 1h")),
    };
    Ok(Some(CacheControl { ttl_seconds }))
}

pub(super) fn encode_cache(
    cache: Option<CacheControl>,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Option<Value>, BridgeError> {
    let Some(cache) = cache else { return Ok(None) };
    let ttl = match cache.ttl_seconds {
        None => None,
        Some(300) => Some("5m"),
        Some(3600) => Some("1h"),
        Some(_) => {
            allow_loss(
                options,
                losses,
                LossScope::CacheControl,
                path,
                "Messages cannot express this cache TTL",
            )?;
            return Ok(None);
        }
    };
    let mut wire = json!({"type":"ephemeral"});
    if let Some(ttl) = ttl {
        wire["ttl"] = json!(ttl);
    }
    Ok(Some(wire))
}

pub(super) fn decode_text(
    value: &Value,
    path: &str,
    strict: bool,
) -> Result<TextPart, BridgeError> {
    if strict {
        fields(value, &["type", "text", "cache_control"], path)?;
    }
    if string(&value["type"], &pointer(path, "type"))? != "text" {
        return Err(unrepresentable(path, "expected a text block"));
    }
    Ok(TextPart {
        text: string(&value["text"], &pointer(path, "text"))?,
        cache_control: if strict {
            decode_cache(&value["cache_control"], &pointer(path, "cache_control"))?
        } else {
            None
        },
    })
}

pub(super) fn text_blocks(value: &Value, path: &str) -> Result<Vec<TextPart>, BridgeError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![TextPart {
            text: text.into(),
            cache_control: None,
        }]);
    }
    array(value, path)?
        .iter()
        .enumerate()
        .map(|(i, block)| decode_text(block, &pointer(path, &i.to_string()), true))
        .collect()
}

pub(super) fn encode_text(
    part: &TextPart,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    let mut wire = json!({"type":"text", "text":part.text});
    put_optional(
        &mut wire,
        "cache_control",
        encode_cache(
            part.cache_control,
            &pointer(path, "cache_control"),
            options,
            losses,
        )?,
    );
    Ok(wire)
}

fn decode_source(value: &Value, image: bool, path: &str) -> Result<MediaSource, BridgeError> {
    match string(&value["type"], &pointer(path, "type"))?.as_str() {
        "url" => {
            fields(value, &["type", "url"], path)?;
            Ok(MediaSource::Url {
                url: string(&value["url"], &pointer(path, "url"))?,
            })
        }
        kind @ ("base64" | "text") => {
            fields(value, &["type", "media_type", "data"], path)?;
            let media_type = string(&value["media_type"], &pointer(path, "media_type"))?;
            let data = string(&value["data"], &pointer(path, "data"))?;
            if kind == "text" {
                if image || media_type != "text/plain" {
                    return Err(invalid(path, "text sources require a text/plain document"));
                }
                return Ok(MediaSource::Base64 {
                    media_type,
                    data: base64_encode(data.as_bytes()),
                });
            }
            validate_media_type(&media_type, image, &pointer(path, "media_type"))?;
            Ok(MediaSource::Base64 { media_type, data })
        }
        _ => Err(unrepresentable(
            &pointer(path, "type"),
            "source has no neutral representation",
        )),
    }
}

fn validate_media_type(media_type: &str, image: bool, path: &str) -> Result<(), BridgeError> {
    if (image
        && matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ))
        || (!image && media_type == "application/pdf")
    {
        return Ok(());
    }
    Err(unrepresentable(
        path,
        "media type is not accepted by Messages",
    ))
}

fn encode_source(source: &MediaSource, image: bool, path: &str) -> Result<Value, BridgeError> {
    match source {
        MediaSource::Url { url } => Ok(json!({"type":"url", "url":url})),
        MediaSource::Base64 { media_type, data } => {
            if !image && media_type == "text/plain" {
                let bytes = base64_decode(data, &pointer(path, "data"))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| invalid(&pointer(path, "data"), "text document is not UTF-8"))?;
                return Ok(json!({"type":"text", "media_type":"text/plain", "data":text}));
            }
            validate_media_type(media_type, image, &pointer(path, "media_type"))?;
            Ok(json!({"type":"base64", "media_type":media_type, "data":data}))
        }
    }
}

pub(super) fn decode_user(value: &Value, path: &str) -> Result<UserContent, BridgeError> {
    let kind = string(&value["type"], &pointer(path, "type"))?;
    match kind.as_str() {
        "text" => Ok(UserContent::Text(decode_text(value, path, true)?)),
        "image" => {
            fields(value, &["type", "source", "cache_control"], path)?;
            Ok(UserContent::Image(ImagePart {
                source: decode_source(&value["source"], true, &pointer(path, "source"))?,
                detail: None,
                cache_control: decode_cache(
                    &value["cache_control"],
                    &pointer(path, "cache_control"),
                )?,
            }))
        }
        "document" => {
            fields(value, &["type", "source", "title", "cache_control"], path)?;
            Ok(UserContent::Document(DocumentPart {
                source: decode_source(&value["source"], false, &pointer(path, "source"))?,
                title: optional_string(value, "title", path)?,
                filename: None,
                cache_control: decode_cache(
                    &value["cache_control"],
                    &pointer(path, "cache_control"),
                )?,
            }))
        }
        "tool_result" => {
            fields(
                value,
                &[
                    "type",
                    "tool_use_id",
                    "content",
                    "is_error",
                    "cache_control",
                ],
                path,
            )?;
            let content = match value.get("content") {
                None => Vec::new(),
                Some(value) => decode_user_blocks(value, &pointer(path, "content"))?
                    .into_iter()
                    .map(|part| match part {
                        UserContent::Text(part) => Ok(ToolResultContent::Text(part)),
                        UserContent::Image(part) => Ok(ToolResultContent::Image(part)),
                        UserContent::Document(part) => Ok(ToolResultContent::Document(part)),
                        UserContent::ToolResult { .. } => Err(invalid(
                            &pointer(path, "content"),
                            "tool results cannot be nested",
                        )),
                    })
                    .collect::<Result<_, _>>()?,
            };
            Ok(UserContent::ToolResult {
                tool_call_id: string(&value["tool_use_id"], &pointer(path, "tool_use_id"))?,
                content,
                is_error: value
                    .get("is_error")
                    .map(|v| boolean(v, &pointer(path, "is_error")))
                    .transpose()?,
                cache_control: decode_cache(
                    &value["cache_control"],
                    &pointer(path, "cache_control"),
                )?,
            })
        }
        _ => Err(unrepresentable(
            &pointer(path, "type"),
            "content block has no neutral representation",
        )),
    }
}

pub(super) fn decode_user_blocks(
    value: &Value,
    path: &str,
) -> Result<Vec<UserContent>, BridgeError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![UserContent::Text(TextPart {
            text: text.into(),
            cache_control: None,
        })]);
    }
    array(value, path)?
        .iter()
        .enumerate()
        .map(|(i, v)| decode_user(v, &pointer(path, &i.to_string())))
        .collect()
}

pub(super) fn encode_user(
    part: &UserContent,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    let (mut wire, cache) = match part {
        UserContent::Text(part) => return encode_text(part, path, options, losses),
        UserContent::Image(part) => {
            if part.detail.is_some() {
                allow_loss(
                    options,
                    losses,
                    LossScope::ImageDetail,
                    &pointer(path, "detail"),
                    "Messages cannot express image detail",
                )?;
            }
            (
                json!({"type":"image", "source":encode_source(&part.source, true, &pointer(path, "source"))?}),
                part.cache_control,
            )
        }
        UserContent::Document(part) => {
            if part.filename.is_some() {
                return Err(unrepresentable(
                    &pointer(path, "filename"),
                    "Messages cannot express a document filename",
                ));
            }
            let mut wire = json!({"type":"document", "source":encode_source(&part.source, false, &pointer(path, "source"))?});
            put_optional(&mut wire, "title", part.title.as_ref().map(|v| json!(v)));
            (wire, part.cache_control)
        }
        UserContent::ToolResult {
            tool_call_id,
            content,
            is_error,
            cache_control,
        } => {
            let content = content
                .iter()
                .enumerate()
                .map(|(i, part)| {
                    let part = match part {
                        ToolResultContent::Text(part) => UserContent::Text(part.clone()),
                        ToolResultContent::Image(part) => UserContent::Image(part.clone()),
                        ToolResultContent::Document(part) => UserContent::Document(part.clone()),
                    };
                    encode_user(&part, &format!("{path}/content/{i}"), options, losses)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut wire =
                json!({"type":"tool_result", "tool_use_id":tool_call_id, "content":content});
            put_optional(&mut wire, "is_error", is_error.map(Value::Bool));
            (wire, *cache_control)
        }
    };
    put_optional(
        &mut wire,
        "cache_control",
        encode_cache(cache, &pointer(path, "cache_control"), options, losses)?,
    );
    Ok(wire)
}

pub(super) fn decode_assistant(
    value: &Value,
    path: &str,
    strict: bool,
    id: &str,
) -> Result<AssistantContent, BridgeError> {
    let kind = string(&value["type"], &pointer(path, "type"))?;
    match kind.as_str() {
        "text" => {
            let part = decode_text(value, path, strict)?;
            Ok(AssistantContent::Text {
                id: id.into(),
                provider_id: None,
                text: part.text,
                cache_control: part.cache_control,
            })
        }
        "tool_use" => {
            if strict {
                fields(
                    value,
                    &["type", "id", "name", "input", "cache_control"],
                    path,
                )?;
            }
            let input = value
                .get("input")
                .ok_or_else(|| invalid(&pointer(path, "input"), "missing tool input"))?;
            Ok(AssistantContent::ToolCall {
                id: id.into(),
                provider_id: None,
                call_id: string(&value["id"], &pointer(path, "id"))?,
                name: string(&value["name"], &pointer(path, "name"))?,
                arguments: input.to_string(),
                cache_control: if strict {
                    decode_cache(&value["cache_control"], &pointer(path, "cache_control"))?
                } else {
                    None
                },
            })
        }
        "thinking" => {
            if strict {
                fields(value, &["type", "thinking", "signature"], path)?;
            }
            Ok(AssistantContent::Reasoning {
                id: id.into(),
                provider_id: None,
                text: Some(string(&value["thinking"], &pointer(path, "thinking"))?),
                summary: Vec::new(),
                opaque: Some(string(&value["signature"], &pointer(path, "signature"))?),
            })
        }
        "redacted_thinking" => {
            if strict {
                fields(value, &["type", "data"], path)?;
            }
            Ok(AssistantContent::Reasoning {
                id: id.into(),
                provider_id: None,
                text: None,
                summary: Vec::new(),
                opaque: Some(string(&value["data"], &pointer(path, "data"))?),
            })
        }
        _ if strict => Err(unrepresentable(
            &pointer(path, "type"),
            "assistant block has no neutral representation",
        )),
        _ => Ok(AssistantContent::Unknown {
            id: id.into(),
            provider_id: None,
            value: value.clone(),
        }),
    }
}

pub(super) fn encode_assistant(
    part: &AssistantContent,
    path: &str,
    response: bool,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Option<Value>, BridgeError> {
    let wire = match part {
        AssistantContent::Text {
            text,
            cache_control,
            ..
        } => {
            if response {
                if cache_control.is_some() {
                    allow_loss(
                        options,
                        losses,
                        LossScope::CacheControl,
                        &pointer(path, "cache_control"),
                        "Messages response text cannot carry cache control",
                    )?;
                }
                json!({"type":"text", "text":text, "citations":null})
            } else {
                encode_text(
                    &TextPart {
                        text: text.clone(),
                        cache_control: *cache_control,
                    },
                    path,
                    options,
                    losses,
                )?
            }
        }
        AssistantContent::ToolCall {
            call_id,
            name,
            arguments,
            cache_control,
            ..
        } => {
            let input: Value = serde_json::from_str(arguments)
                .map_err(|e| invalid(&pointer(path, "arguments"), e.to_string()))?;
            let mut wire = json!({"type":"tool_use", "id":call_id, "name":name, "input":input});
            if response {
                if cache_control.is_some() {
                    allow_loss(
                        options,
                        losses,
                        LossScope::CacheControl,
                        &pointer(path, "cache_control"),
                        "Messages response tool use cannot carry cache control",
                    )?;
                }
                wire["caller"] = json!({"type":"direct"});
            } else {
                put_optional(
                    &mut wire,
                    "cache_control",
                    encode_cache(
                        *cache_control,
                        &pointer(path, "cache_control"),
                        options,
                        losses,
                    )?,
                );
            }
            wire
        }
        AssistantContent::Reasoning {
            provider_id,
            text,
            summary,
            opaque,
            ..
        } => {
            if !response && provider_id.is_some() {
                allow_loss(
                    options,
                    losses,
                    LossScope::Reasoning,
                    path,
                    "Messages cannot replay reasoning with a provider item ID",
                )?;
                return Ok(None);
            }
            if !summary.is_empty() {
                return Err(unrepresentable(
                    &pointer(path, "summary"),
                    "Messages cannot express reasoning summaries",
                ));
            }
            let Some(opaque) = opaque else {
                allow_loss(
                    options,
                    losses,
                    LossScope::Reasoning,
                    path,
                    "Messages reasoning requires a signature or redacted data",
                )?;
                return Ok(None);
            };
            match text {
                Some(text) => json!({"type":"thinking", "thinking":text, "signature":opaque}),
                None => json!({"type":"redacted_thinking", "data":opaque}),
            }
        }
        AssistantContent::Unknown { .. } => {
            allow_loss(
                options,
                losses,
                LossScope::UnknownContent,
                path,
                "unknown assistant content cannot be encoded",
            )?;
            return Ok(None);
        }
    };
    Ok(Some(wire))
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        encoded.push(char::from(BASE64[usize::from(a >> 2)]));
        encoded.push(char::from(BASE64[usize::from(((a & 3) << 4) | (b >> 4))]));
        encoded.push(if chunk.len() > 1 {
            char::from(BASE64[usize::from(((b & 15) << 2) | (c >> 6))])
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            char::from(BASE64[usize::from(c & 63)])
        } else {
            '='
        });
    }
    encoded
}

fn base64_decode(text: &str, path: &str) -> Result<Vec<u8>, BridgeError> {
    if !text.len().is_multiple_of(4) {
        return Err(invalid(path, "invalid base64 text document"));
    }
    let mut bytes = Vec::new();
    let length = text.len() / 4;
    for (index, chunk) in text.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let mut digits = [0_u8; 4];
        let padding = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != length) {
            return Err(invalid(path, "invalid base64 padding"));
        }
        for i in 0..4 - padding {
            digits[i] = match chunk[i] {
                b'A'..=b'Z' => chunk[i] - b'A',
                b'a'..=b'z' => chunk[i] - b'a' + 26,
                b'0'..=b'9' => chunk[i] - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return Err(invalid(path, "invalid base64 character")),
            };
        }
        if (padding == 2 && digits[1] & 15 != 0) || (padding == 1 && digits[2] & 3 != 0) {
            return Err(invalid(path, "invalid base64 padding bits"));
        }
        bytes.push((digits[0] << 2) | (digits[1] >> 4));
        if padding < 2 {
            bytes.push((digits[1] << 4) | (digits[2] >> 2));
        }
        if padding == 0 {
            bytes.push((digits[2] << 6) | digits[3]);
        }
    }
    Ok(bytes)
}
