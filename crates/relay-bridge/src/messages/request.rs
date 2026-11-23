use relay_protocol::{
    Delivery, Message, Parameters, Reasoning, ReasoningEffort, Request, ResponseFormat,
    ThinkingMode, Tool, ToolChoice, ToolConfig, Version,
};
use serde_json::{Value, json};

use super::{content::*, *};
use crate::service_tier::MESSAGES;

pub(super) fn decode(body: &[u8]) -> Result<Request, BridgeError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| invalid("", e.to_string()))?;
    let mut remaining = object(&value, "")?.clone();
    let model = string(&take(&mut remaining, "model"), "/model")?;
    let max_output_tokens = unsigned(&take(&mut remaining, "max_tokens"), "/max_tokens")?;
    let wire_messages = take(&mut remaining, "messages");
    let wire_messages = array(&wire_messages, "/messages")?;
    validate_system_positions(wire_messages)?;
    let mut messages = Vec::new();
    if let Some(system) = remaining.remove("system") {
        messages.push(Message::System {
            content: text_blocks(&system, "/system")?,
        });
    }
    for (index, value) in wire_messages.iter().enumerate() {
        let path = format!("/messages/{index}");
        fields(value, &["role", "content"], &path)?;
        let content_path = pointer(&path, "content");
        let message = match string(&value["role"], &pointer(&path, "role"))?.as_str() {
            "system" => Message::System {
                content: text_blocks(&value["content"], &content_path)?,
            },
            "user" => Message::User {
                content: decode_user_blocks(&value["content"], &content_path)?,
            },
            "assistant" => {
                let content = if value["content"].is_string() {
                    text_blocks(&value["content"], &content_path)?
                        .into_iter()
                        .enumerate()
                        .map(|(i, part)| relay_protocol::AssistantContent::Text {
                            id: format!("{path}:{i}"),
                            provider_id: None,
                            text: part.text,
                            cache_control: part.cache_control,
                        })
                        .collect()
                } else {
                    array(&value["content"], &content_path)?
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            decode_assistant(
                                v,
                                &format!("{content_path}/{i}"),
                                true,
                                &format!("{path}:{i}"),
                            )
                        })
                        .collect::<Result<_, _>>()?
                };
                Message::Assistant { content }
            }
            _ => return Err(unrepresentable(&pointer(&path, "role"), "unsupported role")),
        };
        messages.push(message);
    }
    let stream = remaining
        .remove("stream")
        .map(|v| boolean(&v, "/stream"))
        .transpose()?
        .unwrap_or(false);
    let temperature = remaining
        .remove("temperature")
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| invalid("/temperature", "expected a number"))
        })
        .transpose()?;
    let top_p = remaining
        .remove("top_p")
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| invalid("/top_p", "expected a number"))
        })
        .transpose()?;
    let service_tier = remaining
        .remove("service_tier")
        .map(|v| MESSAGES.decode(v))
        .transpose()?;
    let stop = remaining
        .remove("stop_sequences")
        .map(|v| {
            array(&v, "/stop_sequences")?
                .iter()
                .enumerate()
                .map(|(i, v)| string(v, &format!("/stop_sequences/{i}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut reasoning = Reasoning {
        effort: None,
        budget_tokens: None,
        summary: None,
        thinking: None,
    };
    let mut has_reasoning = false;
    if let Some(thinking) = remaining.remove("thinking") {
        fields(&thinking, &["type", "budget_tokens"], "/thinking")?;
        let mode = match string(&thinking["type"], "/thinking/type")?.as_str() {
            "enabled" => ThinkingMode::Enabled,
            "disabled" => ThinkingMode::Disabled,
            "adaptive" => ThinkingMode::Adaptive,
            _ => {
                return Err(unrepresentable(
                    "/thinking/type",
                    "thinking mode has no neutral representation",
                ));
            }
        };
        reasoning.thinking = Some(mode);
        reasoning.budget_tokens = thinking
            .get("budget_tokens")
            .map(|v| unsigned(v, "/thinking/budget_tokens"))
            .transpose()?;
        validate_thinking(&reasoning, "/thinking/budget_tokens")?;
        has_reasoning = true;
    }
    let mut response_format = None;
    if let Some(config) = remaining.remove("output_config") {
        fields(&config, &["effort", "format"], "/output_config")?;
        if let Some(effort) = config.get("effort").filter(|v| !v.is_null()) {
            let effort = string(effort, "/output_config/effort")?;
            validate_effort(&effort, "/output_config/effort")?;
            reasoning.effort = Some(ReasoningEffort(effort));
            has_reasoning = true;
        }
        if let Some(format) = config.get("format").filter(|v| !v.is_null()) {
            fields(format, &["type", "schema"], "/output_config/format")?;
            if string(&format["type"], "/output_config/format/type")? != "json_schema" {
                return Err(unrepresentable(
                    "/output_config/format/type",
                    "unsupported output format",
                ));
            }
            object(&format["schema"], "/output_config/format/schema")?;
            response_format = Some(ResponseFormat::JsonSchema {
                name: None,
                description: None,
                schema: format["schema"].clone(),
                strict: None,
            });
        }
    }
    let definitions = remaining
        .remove("tools")
        .map(|tools| {
            array(&tools, "/tools")?
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let path = format!("/tools/{i}");
                    if let Some(kind) = value.get("type").filter(|v| !v.is_null())
                        && string(kind, &pointer(&path, "type"))? != "custom"
                    {
                        return Err(unrepresentable(
                            &pointer(&path, "type"),
                            "server tools have no neutral representation",
                        ));
                    }
                    fields(
                        value,
                        &[
                            "type",
                            "name",
                            "description",
                            "input_schema",
                            "strict",
                            "cache_control",
                        ],
                        &path,
                    )?;
                    object(&value["input_schema"], &pointer(&path, "input_schema"))?;
                    if value["input_schema"]["type"] != "object" {
                        return Err(invalid(
                            &format!("{path}/input_schema/type"),
                            "tool schema must have object type",
                        ));
                    }
                    Ok(Tool::Function {
                        name: string(&value["name"], &pointer(&path, "name"))?,
                        description: optional_string(value, "description", &path)?,
                        parameters: value["input_schema"].clone(),
                        strict: value
                            .get("strict")
                            .map(|v| boolean(v, &pointer(&path, "strict")))
                            .transpose()?,
                        cache_control: decode_cache(
                            &value["cache_control"],
                            &pointer(&path, "cache_control"),
                        )?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut choice = None;
    let mut parallel = None;
    if let Some(value) = remaining.remove("tool_choice") {
        let kind = string(&value["type"], "/tool_choice/type")?;
        fields(
            &value,
            match kind.as_str() {
                "none" => &["type"][..],
                "tool" => &["type", "name", "disable_parallel_tool_use"][..],
                _ => &["type", "disable_parallel_tool_use"][..],
            },
            "/tool_choice",
        )?;
        choice = Some(match kind.as_str() {
            "auto" => ToolChoice::Auto,
            "any" => ToolChoice::Required,
            "none" => ToolChoice::None,
            "tool" => ToolChoice::Function {
                name: string(&value["name"], "/tool_choice/name")?,
            },
            _ => {
                return Err(unrepresentable(
                    "/tool_choice/type",
                    "unsupported tool choice",
                ));
            }
        });
        parallel = value
            .get("disable_parallel_tool_use")
            .map(|v| boolean(v, "/tool_choice/disable_parallel_tool_use").map(|v| !v))
            .transpose()?;
    }
    let tools = if definitions.is_some() || choice.is_some() {
        Some(ToolConfig {
            definitions: definitions.unwrap_or_default(),
            choice,
            parallel,
        })
    } else {
        None
    };
    fields(&Value::Object(remaining), &[], "")?;
    Ok(Request {
        version: Version::V1,
        model,
        messages,
        tools,
        parameters: Some(Parameters {
            max_output_tokens: Some(max_output_tokens),
            temperature,
            top_p,
            service_tier,
            stop,
        }),
        response_format,
        reasoning: has_reasoning.then_some(reasoning),
        delivery: stream.then_some(Delivery {
            stream,
            stream_usage: false,
        }),
    })
}

fn take(remaining: &mut Map<String, Value>, key: &str) -> Value {
    remaining.remove(key).unwrap_or(Value::Null)
}

fn validate_effort(effort: &str, path: &str) -> Result<(), BridgeError> {
    if !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") {
        return Err(unrepresentable(path, "effort is not a Messages wire value"));
    }
    Ok(())
}

fn validate_thinking(reasoning: &Reasoning, path: &str) -> Result<(), BridgeError> {
    match (reasoning.thinking, reasoning.budget_tokens) {
        (Some(ThinkingMode::Enabled), Some(_)) => Ok(()),
        (Some(ThinkingMode::Enabled), None) => {
            Err(invalid(path, "enabled thinking requires a budget"))
        }
        (_, Some(_)) => Err(invalid(path, "a thinking budget requires enabled thinking")),
        _ => Ok(()),
    }
}

pub(super) fn validate_system_positions(messages: &[Value]) -> Result<(), BridgeError> {
    let mut index = 0;
    while index < messages.len() {
        if messages[index]["role"] != "system" {
            index += 1;
            continue;
        }
        let start = index;
        let mut carries_content = false;
        while index < messages.len() && messages[index]["role"] == "system" {
            carries_content |= match &messages[index]["content"] {
                Value::Array(content) => !content.is_empty(),
                Value::String(content) => !content.is_empty(),
                _ => true,
            };
            index += 1;
        }
        if !carries_content {
            continue;
        }
        let follows_allowed = start > 0
            && (messages[start - 1]["role"] == "user"
                || assistant_server_result(&messages[start - 1]));
        let precedes_allowed = index == messages.len() || messages[index]["role"] == "assistant";
        if !follows_allowed || !precedes_allowed {
            return Err(invalid(
                &format!("/messages/{start}"),
                "system content must follow a user turn or assistant server-tool result and precede an assistant turn or end the array",
            ));
        }
    }
    Ok(())
}

fn assistant_server_result(message: &Value) -> bool {
    message["role"] == "assistant"
        && message["content"]
            .as_array()
            .and_then(|content| content.last())
            .and_then(|block| block["type"].as_str())
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "web_search_tool_result"
                        | "web_fetch_tool_result"
                        | "code_execution_tool_result"
                        | "bash_code_execution_tool_result"
                        | "text_editor_code_execution_tool_result"
                        | "tool_search_tool_result"
                )
            })
}

pub(super) fn encode(
    request: &Request,
    options: &ConversionOptions,
    default_max_tokens: Option<u64>,
) -> Result<Encoded, BridgeError> {
    let parameters = request.parameters.as_ref();
    let max_tokens = parameters
        .and_then(|p| p.max_output_tokens)
        .or(default_max_tokens)
        .ok_or_else(|| {
            unrepresentable(
                "/parameters/max_output_tokens",
                "Messages requires a consumer-supplied max_tokens",
            )
        })?;
    let mut losses = Vec::new();
    if let Some(parameters) = parameters {
        for (name, present) in [
            ("temperature", parameters.temperature.is_some()),
            ("top_p", parameters.top_p.is_some()),
        ] {
            if present {
                allow_loss(
                    options,
                    &mut losses,
                    LossScope::Sampling,
                    &format!("/parameters/{name}"),
                    "Messages cannot express this sampling parameter",
                )?;
            }
        }
    }
    let mut system = Vec::new();
    let mut has_system = false;
    let mut messages = Vec::new();
    let mut message_paths = Vec::new();
    for (i, message) in request.messages.iter().enumerate() {
        let path = format!("/messages/{i}");
        let (role, content) = match message {
            Message::System { content } | Message::Developer { content } => {
                if matches!(message, Message::Developer { .. }) {
                    allow_loss(
                        options,
                        &mut losses,
                        LossScope::DeveloperRole,
                        &pointer(&path, "role"),
                        "developer role converted to system",
                    )?;
                }
                let blocks = content
                    .iter()
                    .enumerate()
                    .map(|(j, part)| {
                        encode_text(part, &format!("{path}/content/{j}"), options, &mut losses)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if messages.is_empty() {
                    system.extend(blocks);
                    has_system = true;
                    continue;
                }
                ("system", blocks)
            }
            Message::User { content } => (
                "user",
                content
                    .iter()
                    .enumerate()
                    .map(|(j, part)| {
                        encode_user(part, &format!("{path}/content/{j}"), options, &mut losses)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Message::Assistant { content } => {
                let mut blocks = Vec::new();
                for (j, part) in content.iter().enumerate() {
                    if let Some(block) = encode_assistant(
                        part,
                        &format!("{path}/content/{j}"),
                        false,
                        options,
                        &mut losses,
                    )? {
                        blocks.push(block);
                    }
                }
                ("assistant", blocks)
            }
        };
        messages.push(json!({"role":role, "content":content}));
        message_paths.push(i);
    }
    if let Err(error) = validate_system_positions(&messages) {
        let BridgeError::Conversion(mut error) = error else {
            return Err(error);
        };
        if let Some(index) = error
            .path
            .strip_prefix("/messages/")
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|i| message_paths.get(i))
        {
            error.path = format!("/messages/{index}");
        }
        return Err(error.into());
    }
    let mut body = json!({"model":request.model, "max_tokens":max_tokens, "messages":messages});
    if has_system {
        body["system"] = json!(system);
    }
    if request.delivery.is_some_and(|delivery| delivery.stream) {
        body["stream"] = json!(true);
    }
    if let Some(parameters) = parameters {
        if let Some(tier) = &parameters.service_tier {
            body["service_tier"] = json!(MESSAGES.encode(tier)?);
        }
        if !parameters.stop.is_empty() {
            body["stop_sequences"] = json!(parameters.stop);
        }
    }
    let mut output_config = json!({});
    if let Some(reasoning) = &request.reasoning {
        if reasoning.summary.is_some() {
            return Err(unrepresentable(
                "/reasoning/summary",
                "Messages cannot express reasoning summary settings",
            ));
        }
        validate_thinking(reasoning, "/reasoning/budget_tokens")?;
        if let Some(mode) = reasoning.thinking {
            body["thinking"] = match mode {
                ThinkingMode::Enabled => {
                    json!({"type":"enabled", "budget_tokens":reasoning.budget_tokens})
                }
                ThinkingMode::Disabled => json!({"type":"disabled"}),
                ThinkingMode::Adaptive => json!({"type":"adaptive"}),
            };
        }
        if let Some(effort) = &reasoning.effort {
            validate_effort(&effort.0, "/reasoning/effort")?;
            output_config["effort"] = json!(effort.0);
        }
    }
    match &request.response_format {
        Some(ResponseFormat::JsonObject) => {
            return Err(unrepresentable(
                "/response_format",
                "Messages cannot express json_object mode",
            ));
        }
        Some(ResponseFormat::JsonSchema {
            name,
            description,
            schema,
            strict,
        }) => {
            for (key, present) in [
                ("name", name.is_some()),
                ("description", description.is_some()),
                ("strict", strict.is_some()),
            ] {
                if present {
                    return Err(unrepresentable(
                        &format!("/response_format/{key}"),
                        "Messages JSON output format only carries a schema",
                    ));
                }
            }
            object(schema, "/response_format/schema")?;
            output_config["format"] = json!({"type":"json_schema", "schema":schema});
        }
        _ => {}
    }
    if output_config.as_object().is_some_and(|v| !v.is_empty()) {
        body["output_config"] = output_config;
    }
    if let Some(tools) = &request.tools {
        let mut definitions = Vec::new();
        for (i, tool) in tools.definitions.iter().enumerate() {
            let Tool::Function {
                name,
                description,
                parameters,
                strict,
                cache_control,
            } = tool;
            let path = format!("/tools/definitions/{i}");
            object(parameters, &pointer(&path, "parameters"))?;
            if parameters["type"] != "object" {
                return Err(invalid(
                    &format!("{path}/parameters/type"),
                    "tool schema must have object type",
                ));
            }
            let mut wire = json!({"name":name, "input_schema":parameters});
            put_optional(
                &mut wire,
                "description",
                description.as_ref().map(|v| json!(v)),
            );
            put_optional(&mut wire, "strict", strict.map(Value::Bool));
            put_optional(
                &mut wire,
                "cache_control",
                encode_cache(
                    *cache_control,
                    &pointer(&path, "cache_control"),
                    options,
                    &mut losses,
                )?,
            );
            definitions.push(wire);
        }
        if !definitions.is_empty() {
            body["tools"] = json!(definitions);
        }
        if tools.choice.is_some() || tools.parallel.is_some() {
            let mut choice = match &tools.choice {
                None | Some(ToolChoice::Auto) => json!({"type":"auto"}),
                Some(ToolChoice::None) => json!({"type":"none"}),
                Some(ToolChoice::Required) => json!({"type":"any"}),
                Some(ToolChoice::Function { name }) => json!({"type":"tool", "name":name}),
            };
            if let Some(parallel) = tools.parallel {
                if matches!(tools.choice, Some(ToolChoice::None)) {
                    return Err(invalid(
                        "/tools/parallel",
                        "tool choice none cannot carry a parallel setting",
                    ));
                }
                choice["disable_parallel_tool_use"] = json!(!parallel);
            }
            body["tool_choice"] = choice;
        }
    }
    Ok(Encoded { body, losses })
}
