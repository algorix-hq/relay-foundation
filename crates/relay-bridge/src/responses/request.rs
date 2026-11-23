use relay_protocol::{
    AssistantContent, CacheControl, ConversionLoss, Delivery, DocumentPart, ImageDetail, ImagePart,
    MediaSource, Message, Parameters, Reasoning, Request, ResponseFormat, TextPart, Tool,
    ToolChoice, ToolConfig, ToolResultContent, UserContent, Version,
};
use serde_json::{Map, Value, json};

use super::wire::*;
use crate::{BridgeError, ConversionOptions, Encoded, LossScope};

pub(super) fn decode(body: &[u8]) -> Result<Request, BridgeError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| invalid("", error.to_string()))?;
    let mut fields = object(&value, "")?;
    let model = take!(&mut fields, "model", "")?;
    let stream = optional!(&mut fields, "stream", "")?.unwrap_or(false);
    let mut messages = Vec::new();
    if let Some(text) = optional!(String; &mut fields, "instructions", "")? {
        messages.push(Message::System {
            content: vec![text_part(text)],
        });
    }
    if let Some(input) = fields.remove("input") {
        if let Some(text) = input.as_str() {
            messages.push(Message::User {
                content: vec![UserContent::Text(text_part(text.into()))],
            });
        } else {
            for (index, item) in array(&input, "/input")?.iter().enumerate() {
                messages.push(decode_item(item, &format!("/input/{index}"))?);
            }
        }
    }
    let definitions = fields
        .remove("tools")
        .map(|tools| {
            array(&tools, "/tools")?
                .iter()
                .enumerate()
                .map(|(index, tool)| decode_tool(tool, &format!("/tools/{index}")))
                .collect::<Result<Vec<_>, BridgeError>>()
        })
        .transpose()?;
    let choice = fields
        .remove("tool_choice")
        .map(decode_choice)
        .transpose()?;
    let parallel = optional!(&mut fields, "parallel_tool_calls", "")?;
    let tools =
        (definitions.is_some() || choice.is_some() || parallel.is_some()).then(|| ToolConfig {
            definitions: definitions.unwrap_or_default(),
            choice,
            parallel,
        });
    let max_output_tokens = optional!(&mut fields, "max_output_tokens", "")?;
    let temperature = optional!(&mut fields, "temperature", "")?;
    let top_p = optional!(&mut fields, "top_p", "")?;
    if fields.contains_key("stop") {
        return Err(unrepresentable(
            "/stop",
            "Responses has no stop sequence field",
        ));
    }
    let parameters = (max_output_tokens.is_some() || temperature.is_some() || top_p.is_some())
        .then_some(Parameters {
            max_output_tokens,
            temperature,
            top_p,
            service_tier: None,
            stop: Vec::new(),
        });
    let reasoning = fields
        .remove("reasoning")
        .filter(|value| !value.is_null())
        .map(|value| {
            let mut object = object(&value, "/reasoning")?;
            let effort = optional!(&mut object, "effort", "/reasoning")?;
            let summary = optional!(&mut object, "summary", "/reasoning")?;
            reject_remaining(&object, "/reasoning")?;
            Ok::<_, BridgeError>(Reasoning {
                effort,
                summary,
                budget_tokens: None,
                thinking: None,
            })
        })
        .transpose()?;
    let response_format = fields
        .remove("text")
        .map(|value| {
            let mut object = object(&value, "/text")?;
            let format = object
                .remove("format")
                .map(|value| decode_format(value, "/text/format"))
                .transpose()?;
            reject_remaining(&object, "/text")?;
            Ok::<_, BridgeError>(format)
        })
        .transpose()?
        .flatten();
    reject_remaining(&fields, "")?;
    Ok(Request {
        version: Version::V1,
        model,
        messages,
        tools,
        parameters,
        reasoning,
        response_format,
        delivery: Some(Delivery {
            stream,
            stream_usage: false,
        }),
    })
}

fn decode_item(value: &Value, path: &str) -> Result<Message, BridgeError> {
    let mut item = object(value, path)?;
    let kind: Option<String> = optional!(&mut item, "type", path)?;
    let message = match kind.as_deref().unwrap_or("message") {
        "message" => {
            allow_keys(&item, &["role", "content"], path)?;
            let role: String = take!(&mut item, "role", path)?;
            let content: Value = take!(&mut item, "content", path)?;
            let parts = if let Some(text) = content.as_str() {
                vec![UserContent::Text(text_part(text.into()))]
            } else {
                array(&content, &format!("{path}/content"))?
                    .iter()
                    .enumerate()
                    .map(|(index, part)| decode_part(part, &format!("{path}/content/{index}")))
                    .collect::<Result<Vec<_>, _>>()?
            };
            match role.as_str() {
                "user" => Message::User { content: parts },
                "system" | "developer" | "assistant" => {
                    let mut text = Vec::new();
                    for (index, part) in parts.into_iter().enumerate() {
                        let UserContent::Text(part) = part else {
                            return Err(unrepresentable(
                                &format!("{path}/content/{index}"),
                                "role only accepts text in protocol",
                            ));
                        };
                        text.push(part);
                    }
                    match role.as_str() {
                        "system" => Message::System { content: text },
                        "developer" => Message::Developer { content: text },
                        _ => Message::Assistant {
                            content: text
                                .into_iter()
                                .enumerate()
                                .map(|(index, part)| AssistantContent::Text {
                                    id: format!("{path}:{index}"),
                                    provider_id: None,
                                    text: part.text,
                                    cache_control: None,
                                })
                                .collect(),
                        },
                    }
                }
                _ => return Err(invalid(&format!("{path}/role"), "invalid message role")),
            }
        }
        "function_call" => {
            allow_keys(&item, &["id", "call_id", "name", "arguments"], path)?;
            let provider_id: Option<String> = optional!(&mut item, "id", path)?;
            Message::Assistant {
                content: vec![AssistantContent::ToolCall {
                    id: provider_id.clone().unwrap_or_else(|| format!("{path}:0")),
                    provider_id,
                    call_id: take!(&mut item, "call_id", path)?,
                    name: take!(&mut item, "name", path)?,
                    arguments: take!(&mut item, "arguments", path)?,
                    cache_control: None,
                }],
            }
        }
        "function_call_output" => {
            allow_keys(&item, &["call_id", "output"], path)?;
            let tool_call_id = take!(&mut item, "call_id", path)?;
            let output: Value = take!(&mut item, "output", path)?;
            let content = if let Some(text) = output.as_str() {
                vec![ToolResultContent::Text(text_part(text.into()))]
            } else {
                array(&output, &format!("{path}/output"))?
                    .iter()
                    .enumerate()
                    .map(|(index, part)| {
                        Ok(
                            match decode_part(part, &format!("{path}/output/{index}"))? {
                                UserContent::Text(part) => ToolResultContent::Text(part),
                                UserContent::Image(part) => ToolResultContent::Image(part),
                                UserContent::Document(part) => ToolResultContent::Document(part),
                                UserContent::ToolResult { .. } => {
                                    return Err(invalid(path, "nested tool result"));
                                }
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, BridgeError>>()?
            };
            Message::User {
                content: vec![UserContent::ToolResult {
                    tool_call_id,
                    content,
                    is_error: None,
                    cache_control: None,
                }],
            }
        }
        "reasoning" => {
            allow_keys(
                &item,
                &["id", "encrypted_content", "summary", "content"],
                path,
            )?;
            let id: String = take!(&mut item, "id", path)?;
            let opaque = optional!(&mut item, "encrypted_content", path)?;
            let summary = reasoning_parts(
                item.remove("summary").unwrap_or(Value::Null),
                "summary_text",
                &format!("{path}/summary"),
            )?;
            let text = item
                .remove("content")
                .map(|value| reasoning_parts(value, "reasoning_text", &format!("{path}/content")))
                .transpose()?;
            if text.as_ref().is_some_and(|parts| parts.len() > 1) {
                return Err(unrepresentable(
                    &format!("{path}/content"),
                    "protocol represents exactly one reasoning text part",
                ));
            }
            Message::Assistant {
                content: vec![AssistantContent::Reasoning {
                    provider_id: Some(id.clone()),
                    id,
                    opaque,
                    summary,
                    text: text.and_then(|mut parts| parts.pop()),
                }],
            }
        }
        _ => {
            return Err(unrepresentable(
                &format!("{path}/type"),
                "input item has no protocol representation",
            ));
        }
    };
    reject_remaining(&item, path)?;
    Ok(message)
}

fn reasoning_parts(value: Value, kind: &str, path: &str) -> Result<Vec<String>, BridgeError> {
    array(&value, path)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("{path}/{index}");
            let mut part = object(value, &path)?;
            let actual: String = take!(&mut part, "type", &path)?;
            if actual != kind {
                return Err(unrepresentable(
                    &format!("{path}/type"),
                    "unsupported reasoning part",
                ));
            }
            let text = take!(&mut part, "text", &path)?;
            reject_remaining(&part, &path)?;
            Ok(text)
        })
        .collect()
}

fn decode_part(value: &Value, path: &str) -> Result<UserContent, BridgeError> {
    let mut part = object(value, path)?;
    let kind: String = take!(&mut part, "type", path)?;
    let content = match kind.as_str() {
        "input_text" => UserContent::Text(text_part(take!(&mut part, "text", path)?)),
        "input_image" => {
            allow_keys(&part, &["image_url", "detail"], path)?;
            let url: String = take!(&mut part, "image_url", path)?;
            let detail = Some(take!(ImageDetail; &mut part, "detail", path)?);
            UserContent::Image(ImagePart {
                source: decode_source(&url, &format!("{path}/image_url"))?,
                detail,
                cache_control: None,
            })
        }
        "input_file" => {
            allow_keys(&part, &["filename", "file_data", "file_url"], path)?;
            let filename = optional!(&mut part, "filename", path)?;
            let file_data: Option<String> = optional!(&mut part, "file_data", path)?;
            let file_url: Option<String> = optional!(&mut part, "file_url", path)?;
            let source = match (file_data, file_url) {
                (Some(data), None) => {
                    let source = decode_source(&data, &format!("{path}/file_data"))?;
                    if !matches!(source, MediaSource::Base64 { .. }) {
                        return Err(unrepresentable(
                            &format!("{path}/file_data"),
                            "file_data requires a base64 data URL",
                        ));
                    }
                    source
                }
                (None, Some(url)) => MediaSource::Url { url },
                _ => return Err(invalid(path, "expected exactly one file_data or file_url")),
            };
            UserContent::Document(DocumentPart {
                source,
                title: None,
                filename,
                cache_control: None,
            })
        }
        _ => {
            return Err(unrepresentable(
                &format!("{path}/type"),
                "content part has no protocol representation",
            ));
        }
    };
    reject_remaining(&part, path)?;
    Ok(content)
}

fn decode_source(url: &str, path: &str) -> Result<MediaSource, BridgeError> {
    if let Some(data) = url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .ok_or_else(|| invalid(path, "expected a base64 data URL"))?;
        if media_type.is_empty() {
            return Err(invalid(path, "data URL has no media type"));
        }
        return Ok(MediaSource::Base64 {
            media_type: media_type.into(),
            data: data.into(),
        });
    }
    Ok(MediaSource::Url { url: url.into() })
}

fn decode_tool(value: &Value, path: &str) -> Result<Tool, BridgeError> {
    let mut tool = object(value, path)?;
    let kind: String = take!(&mut tool, "type", path)?;
    if kind != "function" {
        return Err(unrepresentable(
            &format!("{path}/type"),
            "only function tools are representable",
        ));
    }
    allow_keys(
        &tool,
        &["name", "description", "parameters", "strict"],
        path,
    )?;
    let name = take!(&mut tool, "name", path)?;
    let description = optional!(&mut tool, "description", path)?;
    let parameters = tool
        .remove("parameters")
        .ok_or_else(|| invalid(&format!("{path}/parameters"), "missing parameters"))?;
    if !parameters.is_object() && !parameters.is_null() {
        return Err(invalid(
            &format!("{path}/parameters"),
            "parameters must be an object or null",
        ));
    }
    let strict = optional!(&mut tool, "strict", path)?;
    reject_remaining(&tool, path)?;
    Ok(Tool::Function {
        name,
        description,
        parameters,
        strict,
        cache_control: None,
    })
}

fn decode_choice(value: Value) -> Result<ToolChoice, BridgeError> {
    if value.is_string() {
        return parse!(value, "/tool_choice");
    }
    let mut choice = object(&value, "/tool_choice")?;
    let kind: String = take!(&mut choice, "type", "/tool_choice")?;
    if kind != "function" {
        return Err(unrepresentable(
            "/tool_choice/type",
            "tool choice has no protocol representation",
        ));
    }
    let name = take!(&mut choice, "name", "/tool_choice")?;
    reject_remaining(&choice, "/tool_choice")?;
    Ok(ToolChoice::Function { name })
}

fn decode_format(value: Value, path: &str) -> Result<ResponseFormat, BridgeError> {
    let mut format = object(&value, path)?;
    let kind: String = take!(&mut format, "type", path)?;
    let result = match kind.as_str() {
        "text" => ResponseFormat::Text,
        "json_object" => ResponseFormat::JsonObject,
        "json_schema" => ResponseFormat::JsonSchema {
            name: Some(take!(&mut format, "name", path)?),
            description: optional!(&mut format, "description", path)?,
            schema: take!(&mut format, "schema", path)?,
            strict: optional!(&mut format, "strict", path)?,
        },
        _ => {
            return Err(unrepresentable(
                &format!("{path}/type"),
                "unsupported response format",
            ));
        }
    };
    if let ResponseFormat::JsonSchema { schema, .. } = &result
        && !schema.is_object()
    {
        return Err(invalid(
            &format!("{path}/schema"),
            "schema must be an object",
        ));
    }
    reject_remaining(&format, path)?;
    Ok(result)
}

pub(super) fn encode(
    request: &Request,
    options: &ConversionOptions,
    store: Option<bool>,
) -> Result<Encoded, BridgeError> {
    let mut losses = Vec::new();
    let mut items = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
        let path = format!("/messages/{message_index}/content");
        match message {
            Message::System { content } | Message::Developer { content } => {
                let role = if matches!(message, Message::System { .. }) {
                    "system"
                } else {
                    "developer"
                };
                let parts = content
                    .iter()
                    .enumerate()
                    .map(|(index, part)| {
                        encode_text(part, &format!("{path}/{index}"), options, &mut losses)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                items.push(json!({"type":"message", "role":role, "content":parts}));
            }
            Message::User { content } => {
                if content.is_empty() {
                    items.push(json!({"type":"message", "role":"user", "content":[]}));
                }
                let mut pending = Vec::new();
                for (index, part) in content.iter().enumerate() {
                    let path = format!("{path}/{index}");
                    match part {
                        UserContent::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                            cache_control,
                        } => {
                            flush_message(&mut pending, "user", &mut items);
                            cache(cache_control, &path, options, &mut losses)?;
                            if is_error.is_some() {
                                return Err(unrepresentable(
                                    &format!("{path}/is_error"),
                                    "Responses cannot express an error tool result",
                                ));
                            }
                            let parts = content
                                .iter()
                                .enumerate()
                                .map(|(index, part)| {
                                    let path = format!("{path}/content/{index}");
                                    match part {
                                        ToolResultContent::Text(part) => {
                                            encode_text(part, &path, options, &mut losses)
                                        }
                                        ToolResultContent::Image(part) => {
                                            encode_image(part, &path, options, &mut losses)
                                        }
                                        ToolResultContent::Document(part) => {
                                            encode_document(part, &path, options, &mut losses)
                                        }
                                    }
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            items.push(json!({"type":"function_call_output", "call_id":tool_call_id, "output":parts}));
                        }
                        UserContent::Text(part) => {
                            pending.push(encode_text(part, &path, options, &mut losses)?)
                        }
                        UserContent::Image(part) => {
                            pending.push(encode_image(part, &path, options, &mut losses)?)
                        }
                        UserContent::Document(part) => {
                            pending.push(encode_document(part, &path, options, &mut losses)?)
                        }
                    }
                }
                flush_message(&mut pending, "user", &mut items);
            }
            Message::Assistant { content } => {
                if content.is_empty() {
                    items.push(json!({"type":"message", "role":"assistant", "content":[]}));
                }
                let mut pending = Vec::new();
                for (index, part) in content.iter().enumerate() {
                    let path = format!("{path}/{index}");
                    match part {
                        AssistantContent::Text {
                            text,
                            cache_control,
                            ..
                        } => {
                            cache(cache_control, &path, options, &mut losses)?;
                            pending.push(json!({"type":"input_text", "text":text}));
                        }
                        AssistantContent::ToolCall {
                            provider_id,
                            call_id,
                            name,
                            arguments,
                            cache_control,
                            ..
                        } => {
                            flush_message(&mut pending, "assistant", &mut items);
                            cache(cache_control, &path, options, &mut losses)?;
                            let mut item = json!({"type":"function_call", "call_id":call_id, "name":name, "arguments":arguments});
                            if let Some(id) = provider_id {
                                item["id"] = json!(id);
                            }
                            items.push(item);
                        }
                        AssistantContent::Reasoning {
                            provider_id,
                            text,
                            summary,
                            opaque,
                            ..
                        } => {
                            flush_message(&mut pending, "assistant", &mut items);
                            if let Some(item) = encode_reasoning(
                                provider_id,
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
                            flush_message(&mut pending, "assistant", &mut items);
                            drop_allowed(LossScope::UnknownContent, &path, options, &mut losses)?
                        }
                    }
                }
                flush_message(&mut pending, "assistant", &mut items);
            }
        }
    }
    let mut body = Map::from_iter([
        ("model".into(), json!(request.model)),
        ("input".into(), json!(items)),
        (
            "stream".into(),
            json!(request.delivery.is_some_and(|delivery| delivery.stream)),
        ),
    ]);
    body.extend(encode_settings(request, options, &mut losses)?);
    put!(&mut body, "store", store);
    Ok(Encoded {
        body: body.into(),
        losses,
    })
}

pub(super) fn encode_settings(
    request: &Request,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Map<String, Value>, BridgeError> {
    let mut body = Map::new();
    if let Some(parameters) = &request.parameters {
        if parameters.service_tier.is_some() {
            return Err(unrepresentable(
                "/parameters/service_tier",
                "unsupported service tier",
            ));
        }
        if !parameters.stop.is_empty() {
            return Err(unrepresentable(
                "/parameters/stop",
                "Responses has no stop sequence field",
            ));
        }
        put!(&mut body, "max_output_tokens", parameters.max_output_tokens);
        for (key, value) in [
            ("temperature", parameters.temperature),
            ("top_p", parameters.top_p),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(invalid(
                    &format!("/parameters/{key}"),
                    "expected a finite number",
                ));
            }
            put!(&mut body, key, value);
        }
    }
    if let Some(reasoning) = &request.reasoning {
        if reasoning.budget_tokens.is_some() {
            return Err(unrepresentable(
                "/reasoning/budget_tokens",
                "Responses has no reasoning budget field",
            ));
        }
        if reasoning.thinking.is_some() {
            return Err(unrepresentable(
                "/reasoning/thinking",
                "Responses has no thinking mode field",
            ));
        }
        let mut value = Map::new();
        put!(&mut value, "effort", reasoning.effort.as_ref());
        put!(&mut value, "summary", reasoning.summary.as_ref());
        body.insert("reasoning".into(), value.into());
    }
    if let Some(format) = &request.response_format {
        let value = match format {
            ResponseFormat::Text => json!({"type":"text"}),
            ResponseFormat::JsonObject => json!({"type":"json_object"}),
            ResponseFormat::JsonSchema {
                name,
                description,
                schema,
                strict,
            } => {
                let name = name.as_ref().ok_or_else(|| {
                    unrepresentable("/response_format/name", "Responses requires a schema name")
                })?;
                if !schema.is_object() {
                    return Err(invalid(
                        "/response_format/schema",
                        "schema must be an object",
                    ));
                }
                let mut value = Map::from_iter([
                    ("type".into(), json!("json_schema")),
                    ("name".into(), json!(name)),
                    ("schema".into(), schema.clone()),
                ]);
                put!(&mut value, "description", description.as_ref());
                put!(&mut value, "strict", *strict);
                value.into()
            }
        };
        body.insert("text".into(), json!({"format":value}));
    }
    if let Some(tools) = &request.tools {
        let definitions = tools
            .definitions
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                let Tool::Function {
                    name,
                    description,
                    parameters,
                    strict,
                    cache_control,
                } = tool;
                let path = format!("/tools/definitions/{index}");
                cache(cache_control, &path, options, losses)?;
                if !parameters.is_object() && !parameters.is_null() {
                    return Err(invalid(
                        &format!("{path}/parameters"),
                        "parameters must be an object or null",
                    ));
                }
                let mut value = Map::from_iter([
                    ("type".into(), json!("function")),
                    ("name".into(), json!(name)),
                    ("parameters".into(), parameters.clone()),
                    ("strict".into(), json!(strict)),
                ]);
                put!(&mut value, "description", description.as_ref());
                Ok(Value::Object(value))
            })
            .collect::<Result<Vec<_>, BridgeError>>()?;
        body.insert("tools".into(), json!(definitions));
        put!(&mut body, "tool_choice", tools.choice.as_ref());
        put!(&mut body, "parallel_tool_calls", tools.parallel);
    }
    Ok(body)
}

fn flush_message(pending: &mut Vec<Value>, role: &str, items: &mut Vec<Value>) {
    if !pending.is_empty() {
        items.push(json!({"type":"message", "role":role, "content":std::mem::take(pending)}));
    }
}

pub(super) fn cache(
    cache_control: &Option<CacheControl>,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<(), BridgeError> {
    if cache_control.is_some() {
        drop_allowed(
            LossScope::CacheControl,
            &format!("{path}/cache_control"),
            options,
            losses,
        )?;
    }
    Ok(())
}

fn text_part(text: String) -> TextPart {
    TextPart {
        text,
        cache_control: None,
    }
}

fn encode_text(
    part: &TextPart,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    cache(&part.cache_control, path, options, losses)?;
    Ok(json!({"type":"input_text", "text":part.text}))
}

fn source_url(source: &MediaSource) -> String {
    match source {
        MediaSource::Url { url } => url.clone(),
        MediaSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
    }
}

fn encode_image(
    part: &ImagePart,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    cache(&part.cache_control, path, options, losses)?;
    Ok(
        json!({"type":"input_image", "image_url":source_url(&part.source), "detail":part.detail.as_ref().map(|detail| detail.0.as_str()).unwrap_or("auto")}),
    )
}

fn encode_document(
    part: &DocumentPart,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    cache(&part.cache_control, path, options, losses)?;
    if part.title.is_some() {
        return Err(unrepresentable(
            &format!("{path}/title"),
            "Responses input_file has no title",
        ));
    }
    let key = if matches!(part.source, MediaSource::Url { .. }) {
        "file_url"
    } else {
        "file_data"
    };
    let mut value = Map::from_iter([
        ("type".into(), json!("input_file")),
        (key.into(), json!(source_url(&part.source))),
    ]);
    put!(&mut value, "filename", part.filename.as_ref());
    Ok(value.into())
}

pub(super) fn encode_reasoning(
    id: &Option<String>,
    text: &Option<String>,
    summary: &[String],
    opaque: &Option<String>,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Option<Value>, BridgeError> {
    let Some(id) = id else {
        drop_allowed(LossScope::Reasoning, path, options, losses)?;
        return Ok(None);
    };
    let mut item = Map::from_iter([
        ("type".into(), json!("reasoning")),
        ("id".into(), json!(id)),
        (
            "summary".into(),
            json!(
                summary
                    .iter()
                    .map(|text| json!({"type":"summary_text", "text":text}))
                    .collect::<Vec<_>>()
            ),
        ),
    ]);
    put!(&mut item, "encrypted_content", opaque.as_ref());
    if let Some(text) = text {
        item.insert(
            "content".into(),
            json!([{"type":"reasoning_text", "text":text}]),
        );
    }
    Ok(Some(item.into()))
}
