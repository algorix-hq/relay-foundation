use relay_protocol::*;
use serde_json::{Map, Value, json};

use super::*;

pub(super) fn decode(body: &[u8]) -> Result<Input, BridgeError> {
    let value = serde_json::from_slice(body).map_err(|e| invalid("", e.to_string()))?;
    let mut native = object(value, "")?;
    let model = required(&mut native, "model", "")?;
    let messages: Vec<Value> = required(&mut native, "messages", "")?;
    let messages = messages
        .into_iter()
        .enumerate()
        .map(|(i, m)| decode_message(m, &format!("/messages/{i}")))
        .collect::<Result<_, _>>()?;
    let stream = optional(&mut native, "stream", "")?.unwrap_or(false);
    let old_max: Option<u64> = optional(&mut native, "max_tokens", "")?;
    let max: Option<u64> = optional(&mut native, "max_completion_tokens", "")?;
    if old_max.is_some() && max.is_some() && old_max != max {
        return Err(invalid(
            "/max_completion_tokens",
            "max_tokens and max_completion_tokens disagree",
        ));
    }
    let temperature = optional(&mut native, "temperature", "")?;
    let top_p = optional(&mut native, "top_p", "")?;
    let stop = match native.remove("stop").filter(|v| !v.is_null()) {
        Some(Value::String(s)) => vec![s],
        Some(value) => parse(value, "/stop")?,
        None => Vec::new(),
    };
    let parameters =
        (old_max.or(max).is_some() || temperature.is_some() || top_p.is_some() || !stop.is_empty())
            .then_some(Parameters {
                max_output_tokens: old_max.or(max),
                temperature,
                top_p,
                stop,
            });
    let definitions = native.remove("tools").filter(|v| !v.is_null());
    let legacy = native.remove("functions").filter(|v| !v.is_null());
    if definitions.is_some() && legacy.is_some() {
        return Err(invalid(
            "/functions",
            "functions and tools cannot both be represented",
        ));
    }
    let mut tools = Vec::new();
    if let Some(value) = definitions {
        for (i, value) in parse::<Vec<Value>>(value, "/tools")?
            .into_iter()
            .enumerate()
        {
            let path = format!("/tools/{i}");
            let mut value = object(value, &path)?;
            let kind: String = required(&mut value, "type", &path)?;
            if kind != "function" {
                return Err(unrepresentable(
                    &format!("{path}/type"),
                    "only function tools are representable",
                ));
            }
            tools.push(decode_function(
                required(&mut value, "function", &path)?,
                &format!("{path}/function"),
            )?);
            no_remaining(value, &path)?;
        }
    }
    if let Some(value) = legacy {
        for (i, value) in parse::<Vec<Value>>(value, "/functions")?
            .into_iter()
            .enumerate()
        {
            tools.push(decode_function(value, &format!("/functions/{i}"))?);
        }
    }
    let choice = native.remove("tool_choice").filter(|v| !v.is_null());
    let legacy_choice = native.remove("function_call").filter(|v| !v.is_null());
    if choice.is_some() && legacy_choice.is_some() {
        return Err(invalid(
            "/function_call",
            "function_call and tool_choice both present",
        ));
    }
    let choice = if let Some(value) = choice {
        Some(decode_choice(value, false, "/tool_choice")?)
    } else if let Some(value) = legacy_choice {
        Some(decode_choice(value, true, "/function_call")?)
    } else {
        None
    };
    let parallel = optional(&mut native, "parallel_tool_calls", "")?;
    let tools =
        (!tools.is_empty() || choice.is_some() || parallel.is_some()).then_some(ToolConfig {
            definitions: tools,
            choice,
            parallel,
        });
    let response_format = native
        .remove("response_format")
        .filter(|v| !v.is_null())
        .map(decode_format)
        .transpose()?;
    let reasoning = optional(&mut native, "reasoning_effort", "")?.map(|effort| Reasoning {
        effort: Some(effort),
        budget_tokens: None,
        summary: None,
        thinking: None,
    });
    Ok(Input::with_native(
        ApiFormat::CHAT_COMPLETIONS,
        Request {
            version: Version::V1,
            model,
            messages,
            tools,
            parameters,
            response_format,
            reasoning,
        },
        stream,
        native,
    ))
}

fn decode_function(value: Value, path: &str) -> Result<Tool, BridgeError> {
    let mut value = object(value, path)?;
    let parameters = value.remove("parameters").ok_or_else(|| {
        unrepresentable(
            &format!("{path}/parameters"),
            "function without a schema has no protocol representation",
        )
    })?;
    if !parameters.is_object() {
        return Err(invalid(
            &format!("{path}/parameters"),
            "expected schema object",
        ));
    }
    let tool = Tool::Function {
        name: required(&mut value, "name", path)?,
        description: optional(&mut value, "description", path)?,
        parameters,
        strict: optional(&mut value, "strict", path)?,
        cache_control: None,
    };
    no_remaining(value, path)?;
    Ok(tool)
}

fn decode_choice(value: Value, legacy: bool, path: &str) -> Result<ToolChoice, BridgeError> {
    if let Value::String(value) = &value {
        return match value.as_str() {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" if !legacy => Ok(ToolChoice::Required),
            _ => Err(unrepresentable(path, "unsupported tool choice")),
        };
    }
    let mut value = object(value, path)?;
    let name = if legacy {
        required(&mut value, "name", path)?
    } else {
        let kind: String = required(&mut value, "type", path)?;
        if kind != "function" {
            return Err(unrepresentable(
                path,
                "only a function choice is representable",
            ));
        }
        let mut function = object(
            required(&mut value, "function", path)?,
            &format!("{path}/function"),
        )?;
        let name = required(&mut function, "name", &format!("{path}/function"))?;
        no_remaining(function, &format!("{path}/function"))?;
        name
    };
    no_remaining(value, path)?;
    Ok(ToolChoice::Function { name })
}

fn decode_format(value: Value) -> Result<ResponseFormat, BridgeError> {
    let path = "/response_format";
    let mut value = object(value, path)?;
    let kind: String = required(&mut value, "type", path)?;
    let result = match kind.as_str() {
        "text" => ResponseFormat::Text,
        "json_object" => ResponseFormat::JsonObject,
        "json_schema" => {
            let path = "/response_format/json_schema";
            let mut schema = object(
                required(&mut value, "json_schema", "/response_format")?,
                path,
            )?;
            let schema_value = schema.remove("schema").ok_or_else(|| {
                unrepresentable(
                    &format!("{path}/schema"),
                    "schema is required by the protocol",
                )
            })?;
            if !schema_value.is_object() {
                return Err(invalid(&format!("{path}/schema"), "expected schema object"));
            }
            let result = ResponseFormat::JsonSchema {
                name: required(&mut schema, "name", path)?,
                description: optional(&mut schema, "description", path)?,
                schema: schema_value,
                strict: optional(&mut schema, "strict", path)?,
            };
            no_remaining(schema, path)?;
            result
        }
        _ => {
            return Err(unrepresentable(
                path,
                "response format has no protocol representation",
            ));
        }
    };
    no_remaining(value, path)?;
    Ok(result)
}

fn decode_text(value: Value, path: &str) -> Result<Vec<TextPart>, BridgeError> {
    if let Value::String(text) = value {
        return Ok(vec![TextPart {
            text,
            cache_control: None,
        }]);
    }
    parse::<Vec<Value>>(value, path)?
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            let path = format!("{path}/{i}");
            let mut part = object(part, &path)?;
            let kind: String = required(&mut part, "type", &path)?;
            if kind != "text" {
                return Err(unrepresentable(&path, "expected text content"));
            }
            let text = required(&mut part, "text", &path)?;
            no_remaining(part, &path)?;
            Ok(TextPart {
                text,
                cache_control: None,
            })
        })
        .collect()
}

fn decode_message(value: Value, path: &str) -> Result<Message, BridgeError> {
    let mut value = object(value, path)?;
    let role: String = required(&mut value, "role", path)?;
    let content = value.remove("content");
    let content_path = format!("{path}/content");
    let result = match role.as_str() {
        "system" | "developer" => {
            let content = decode_text(
                content.ok_or_else(|| invalid(&content_path, "required content missing"))?,
                &content_path,
            )?;
            if role == "system" {
                Message::System { content }
            } else {
                Message::Developer { content }
            }
        }
        "user" => {
            let content =
                content.ok_or_else(|| invalid(&content_path, "required content missing"))?;
            let content = if let Value::String(text) = content {
                vec![UserContent::Text(TextPart {
                    text,
                    cache_control: None,
                })]
            } else {
                parse::<Vec<Value>>(content, &content_path)?
                    .into_iter()
                    .enumerate()
                    .map(|(i, part)| decode_user_part(part, &format!("{content_path}/{i}")))
                    .collect::<Result<_, _>>()?
            };
            Message::User { content }
        }
        "tool" => {
            let tool_call_id = required(&mut value, "tool_call_id", path)?;
            let content = decode_text(
                content.ok_or_else(|| invalid(&content_path, "required content missing"))?,
                &content_path,
            )?
            .into_iter()
            .map(ToolResultContent::Text)
            .collect();
            Message::User {
                content: vec![UserContent::ToolResult {
                    tool_call_id,
                    content,
                    is_error: None,
                    cache_control: None,
                }],
            }
        }
        "assistant" => {
            let mut parts = content
                .filter(|v| !v.is_null())
                .map(|v| decode_text(v, &content_path))
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(AssistantContent::Text)
                .collect::<Vec<_>>();
            if let Some(calls) = value.remove("tool_calls") {
                for (i, call) in parse::<Vec<Value>>(calls, &format!("{path}/tool_calls"))?
                    .into_iter()
                    .enumerate()
                {
                    let path = format!("{path}/tool_calls/{i}");
                    let mut call = object(call, &path)?;
                    let id = required(&mut call, "id", &path)?;
                    let kind: String = required(&mut call, "type", &path)?;
                    if kind != "function" {
                        return Err(unrepresentable(
                            &path,
                            "custom tool call has no protocol representation",
                        ));
                    }
                    let mut function = object(
                        required(&mut call, "function", &path)?,
                        &format!("{path}/function"),
                    )?;
                    let name = required(&mut function, "name", &format!("{path}/function"))?;
                    let arguments =
                        required(&mut function, "arguments", &format!("{path}/function"))?;
                    no_remaining(function, &format!("{path}/function"))?;
                    no_remaining(call, &path)?;
                    parts.push(AssistantContent::ToolCall {
                        id,
                        name,
                        arguments,
                        cache_control: None,
                    });
                }
            }
            Message::Assistant { content: parts }
        }
        "function" => {
            return Err(unrepresentable(
                path,
                "legacy function result has no tool call id",
            ));
        }
        _ => {
            return Err(unrepresentable(
                &format!("{path}/role"),
                "role has no protocol representation",
            ));
        }
    };
    no_remaining(value, path)?;
    Ok(result)
}

fn decode_user_part(value: Value, path: &str) -> Result<UserContent, BridgeError> {
    let mut value = object(value, path)?;
    let kind: String = required(&mut value, "type", path)?;
    let result = match kind.as_str() {
        "text" => UserContent::Text(TextPart {
            text: required(&mut value, "text", path)?,
            cache_control: None,
        }),
        "image_url" => {
            let p = format!("{path}/image_url");
            let mut image = object(required(&mut value, "image_url", path)?, &p)?;
            let url = required(&mut image, "url", &p)?;
            let detail = optional(&mut image, "detail", &p)?;
            no_remaining(image, &p)?;
            UserContent::Image(ImagePart {
                source: decode_data_url(url, &format!("{p}/url"), true)?,
                detail,
                cache_control: None,
            })
        }
        "file" => {
            let p = format!("{path}/file");
            let mut file = object(required(&mut value, "file", path)?, &p)?;
            let data = file
                .remove("file_data")
                .ok_or_else(|| unrepresentable(&p, "file input requires inline data"))?;
            let source = decode_data_url(
                parse(data, &format!("{p}/file_data"))?,
                &format!("{p}/file_data"),
                false,
            )?;
            let filename = optional(&mut file, "filename", &p)?;
            no_remaining(file, &p)?;
            UserContent::Document(DocumentPart {
                source,
                filename,
                title: None,
                cache_control: None,
            })
        }
        _ => {
            return Err(unrepresentable(
                &format!("{path}/type"),
                "content kind has no protocol representation",
            ));
        }
    };
    no_remaining(value, path)?;
    Ok(result)
}

fn decode_data_url(url: String, path: &str, allow_url: bool) -> Result<MediaSource, BridgeError> {
    if let Some(data) = url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .ok_or_else(|| invalid(path, "expected base64 data URL"))?;
        if media_type.is_empty() {
            return Err(invalid(path, "data URL media type missing"));
        }
        return Ok(MediaSource::Base64 {
            media_type: media_type.into(),
            data: data.into(),
        });
    }
    if allow_url {
        Ok(MediaSource::Url { url })
    } else {
        Err(unrepresentable(
            path,
            "file data requires a media-typed base64 data URL",
        ))
    }
}

pub(super) fn encode(
    input: &Input,
    options: &ConversionOptions,
    max_field: MaxTokensField,
) -> Result<Encoded, BridgeError> {
    let request = &input.request;
    let mut losses = Vec::new();
    let mut messages = Vec::new();
    for (i, message) in request.messages.iter().enumerate() {
        encode_message(
            message,
            &format!("/messages/{i}"),
            options,
            &mut losses,
            &mut messages,
        )?;
    }
    let mut body = Map::from_iter([
        ("model".into(), json!(request.model)),
        ("messages".into(), json!(messages)),
    ]);
    body.insert("stream".into(), json!(input.stream));
    if let Some(parameters) = &request.parameters {
        if let Some(max) = parameters.max_output_tokens {
            body.insert(
                match max_field {
                    MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
                    MaxTokensField::MaxTokens => "max_tokens",
                }
                .into(),
                json!(max),
            );
        }
        if let Some(value) = parameters.temperature {
            if !value.is_finite() {
                return Err(invalid(
                    "/parameters/temperature",
                    "expected finite JSON number",
                ));
            }
            body.insert("temperature".into(), json!(value));
        }
        if let Some(value) = parameters.top_p {
            if !value.is_finite() {
                return Err(invalid("/parameters/top_p", "expected finite JSON number"));
            }
            body.insert("top_p".into(), json!(value));
        }
        if !parameters.stop.is_empty() {
            body.insert("stop".into(), json!(parameters.stop));
        }
    }
    if let Some(reasoning) = &request.reasoning {
        for (name, present) in [
            ("budget_tokens", reasoning.budget_tokens.is_some()),
            ("summary", reasoning.summary.is_some()),
            ("thinking", reasoning.thinking.is_some()),
        ] {
            if present {
                return Err(unrepresentable(
                    &format!("/reasoning/{name}"),
                    "reasoning setting has no Chat Completions representation",
                ));
            }
        }
        if let Some(effort) = &reasoning.effort {
            body.insert("reasoning_effort".into(), json!(effort.0));
        }
    }
    if let Some(config) = &request.tools {
        if !config.definitions.is_empty() {
            let mut tools = Vec::new();
            for (i, tool) in config.definitions.iter().enumerate() {
                let Tool::Function {
                    name,
                    description,
                    parameters,
                    strict,
                    cache_control,
                } = tool;
                cache(
                    cache_control,
                    &format!("/tools/definitions/{i}/cache_control"),
                    options,
                    &mut losses,
                )?;
                if !parameters.is_object() {
                    return Err(unrepresentable(
                        &format!("/tools/definitions/{i}/parameters"),
                        "Chat Completions requires a schema object",
                    ));
                }
                let mut function = Map::from_iter([
                    ("name".into(), json!(name)),
                    ("parameters".into(), parameters.clone()),
                ]);
                if let Some(value) = description {
                    function.insert("description".into(), json!(value));
                }
                if let Some(value) = strict {
                    function.insert("strict".into(), json!(value));
                }
                tools.push(json!({"type":"function","function":function}));
            }
            body.insert("tools".into(), json!(tools));
        }
        if let Some(choice) = &config.choice {
            body.insert(
                "tool_choice".into(),
                match choice {
                    ToolChoice::None => json!("none"),
                    ToolChoice::Auto => json!("auto"),
                    ToolChoice::Required => json!("required"),
                    ToolChoice::Function { name } => {
                        json!({"type":"function","function":{"name":name}})
                    }
                },
            );
        }
        if let Some(parallel) = config.parallel {
            body.insert("parallel_tool_calls".into(), json!(parallel));
        }
    }
    if let Some(format) = &request.response_format {
        body.insert(
            "response_format".into(),
            match format {
                ResponseFormat::Text => json!({"type":"text"}),
                ResponseFormat::JsonObject => json!({"type":"json_object"}),
                ResponseFormat::JsonSchema {
                    name,
                    description,
                    schema,
                    strict,
                } => {
                    if !schema.is_object() {
                        return Err(unrepresentable(
                            "/response_format/schema",
                            "Chat Completions requires a schema object",
                        ));
                    }
                    let mut inner = Map::from_iter([
                        ("name".into(), json!(name)),
                        ("schema".into(), schema.clone()),
                    ]);
                    if let Some(value) = description {
                        inner.insert("description".into(), json!(value));
                    }
                    if let Some(value) = strict {
                        inner.insert("strict".into(), json!(value));
                    }
                    json!({"type":"json_schema","json_schema":inner})
                }
            },
        );
    }
    merge(&mut body, input.native_for(&ApiFormat::CHAT_COMPLETIONS))?;
    Ok(Encoded {
        body: Value::Object(body),
        losses,
    })
}

pub(super) fn cache(
    value: &Option<CacheControl>,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<(), BridgeError> {
    if value.is_some() {
        omit(options, losses, LossScope::CacheControl, path)?;
    }
    Ok(())
}

fn text_parts(
    parts: &[TextPart],
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<Value, BridgeError> {
    let mut result = Vec::new();
    for (i, text) in parts.iter().enumerate() {
        cache(
            &text.cache_control,
            &format!("{path}/{i}/cache_control"),
            options,
            losses,
        )?;
        result.push(json!({"type":"text","text":text.text}));
    }
    Ok(compact_text(result))
}

fn compact_text(mut parts: Vec<Value>) -> Value {
    if parts.len() == 1 && parts[0]["type"] == "text" {
        return parts.remove(0)["text"].take();
    }
    json!(parts)
}

fn encode_message(
    message: &Message,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
    out: &mut Vec<Value>,
) -> Result<(), BridgeError> {
    let content_path = format!("{path}/content");
    match message {
        Message::System { content } | Message::Developer { content } => out.push(json!({"role":if matches!(message, Message::System { .. }) { "system" } else { "developer" },"content":text_parts(content, &content_path, options, losses)?})),
        Message::Assistant { content } => {
            let (text, calls) = super::response::encode_content(content, &content_path, options, losses)?;
            let mut message = json!({"role":"assistant","content":text});
            if !calls.is_empty() { message["tool_calls"] = json!(calls); }
            out.push(message);
        }
        Message::User { content } => {
            let mut parts = Vec::new();
            for (i, part) in content.iter().enumerate() {
                let p = format!("{content_path}/{i}");
                match part {
                    UserContent::Text(text) => {
                        cache(&text.cache_control, &format!("{p}/cache_control"), options, losses)?;
                        parts.push(json!({"type":"text","text":text.text}));
                    }
                    UserContent::Image(image) => {
                        cache(&image.cache_control, &format!("{p}/cache_control"), options, losses)?;
                        let mut image_url = json!({"url":source_url(&image.source)});
                        if let Some(detail) = &image.detail { image_url["detail"] = json!(detail.0); }
                        parts.push(json!({"type":"image_url","image_url":image_url}));
                    }
                    UserContent::Document(document) => {
                        cache(&document.cache_control, &format!("{p}/cache_control"), options, losses)?;
                        if document.title.is_some() { return Err(unrepresentable(&format!("{p}/title"), "document title has no representation")); }
                        if matches!(document.source, MediaSource::Url { .. }) { return Err(unrepresentable(&format!("{p}/source"), "URL documents have no representation")); }
                        let mut file = json!({"file_data":source_url(&document.source)});
                        if let Some(filename) = &document.filename { file["filename"] = json!(filename); }
                        parts.push(json!({"type":"file","file":file}));
                    }
                    UserContent::ToolResult { tool_call_id, content, is_error, cache_control } => {
                        if *is_error == Some(true) { return Err(unrepresentable(&format!("{p}/is_error"), "tool error flag has no representation")); }
                        cache(cache_control, &format!("{p}/cache_control"), options, losses)?;
                        let mut texts = Vec::new();
                        for (j, result) in content.iter().enumerate() {
                            match result { ToolResultContent::Text(text) => texts.push(text.clone()), _ => return Err(unrepresentable(&format!("{p}/content/{j}"), "only text tool results are representable")) }
                        }
                        let text = text_parts(&texts, &format!("{p}/content"), options, losses)?;
                        if !parts.is_empty() { out.push(json!({"role":"user","content":compact_text(std::mem::take(&mut parts))})); }
                        out.push(json!({"role":"tool","tool_call_id":tool_call_id,"content":text}));
                    }
                }
            }
            if !parts.is_empty() || content.is_empty() { out.push(json!({"role":"user","content":compact_text(parts)})); }
        }
    }
    Ok(())
}

fn source_url(source: &MediaSource) -> String {
    match source {
        MediaSource::Url { url } => url.clone(),
        MediaSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
    }
}
