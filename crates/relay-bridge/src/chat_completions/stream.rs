use std::collections::BTreeMap;

use relay_protocol::*;
use serde_json::{Value, json};

use super::{response::*, *};
use crate::Frame;

pub(super) struct Decoder {
    context: DecodeContext,
    response_id: Option<String>,
    model: Option<String>,
    done: bool,
    next_index: u32,
    text: Option<u32>,
    refusal: Option<u32>,
    tools: BTreeMap<u32, DecodedTool>,
    open: Vec<u32>,
    reason: Option<StopReason>,
    usage: Option<Usage>,
}

struct DecodedTool {
    index: u32,
    call_id: Option<String>,
    name: Option<String>,
    unknown: bool,
}

impl Decoder {
    pub(super) fn new(context: DecodeContext) -> Self {
        Self {
            context,
            response_id: None,
            model: None,
            done: false,
            next_index: 0,
            text: None,
            refusal: None,
            tools: BTreeMap::new(),
            open: Vec::new(),
            reason: None,
            usage: None,
        }
    }

    fn start_item(
        &mut self,
        item: StreamItem,
        out: &mut Vec<StreamEvent>,
    ) -> Result<u32, BridgeError> {
        let index = self.next_index;
        self.next_index = index
            .checked_add(1)
            .ok_or_else(|| decode_error("/choices/0/delta", "too many content items"))?;
        let response_id = self
            .response_id
            .as_ref()
            .ok_or_else(|| decode_error("/id", "Start required before items"))?;
        out.push(StreamEvent::ItemStart {
            index,
            id: format!("{response_id}:{index}"),
            provider_id: None,
            item,
        });
        self.open.push(index);
        Ok(index)
    }

    fn tool(&mut self, call: &Value, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        let wire_index: u32 = read(&call["index"], "/choices/0/delta/tool_calls/index")?;
        let path = format!("/choices/0/delta/tool_calls/{wire_index}");
        let call_id: Option<String> = read(&call["id"], &format!("{path}/id"))?;
        let name: Option<String> =
            read(&call["function"]["name"], &format!("{path}/function/name"))?;
        if !self.tools.contains_key(&wire_index) {
            let unknown = call.get("type").is_some_and(|kind| kind != "function");
            let item = if unknown {
                StreamItem::Unknown
            } else {
                StreamItem::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                }
            };
            let index = self.start_item(item, out)?;
            self.tools.insert(
                wire_index,
                DecodedTool {
                    index,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    unknown,
                },
            );
        }
        let tool = self
            .tools
            .get_mut(&wire_index)
            .ok_or_else(|| decode_error(&path, "tool state missing"))?;
        if tool.unknown {
            out.push(StreamEvent::Delta {
                index: tool.index,
                delta: StreamDelta::Unknown {
                    value: call.clone(),
                },
            });
            return Ok(());
        }
        if call.get("type").is_some_and(|kind| kind != "function") || call.get("custom").is_some() {
            return Err(decode_error(&path, "tool kind changed after item start"));
        }
        let call_id = call_id.filter(|call_id| tool.call_id.as_ref() != Some(call_id));
        let name = name.filter(|name| tool.name.as_ref() != Some(name));
        if call_id.is_some() || name.is_some() {
            if let Some(call_id) = &call_id {
                tool.call_id = Some(call_id.clone());
            }
            if let Some(name) = &name {
                tool.name = Some(name.clone());
            }
            out.push(StreamEvent::Metadata(MetadataScope::Item {
                index: tool.index,
                provider_id: None,
                call_id,
                name,
            }));
        }
        if let Some(text) = read::<Option<String>>(
            &call["function"]["arguments"],
            &format!("{path}/function/arguments"),
        )? {
            out.push(StreamEvent::Delta {
                index: tool.index,
                delta: StreamDelta::ToolCallArguments { text },
            });
        }
        Ok(())
    }
}

impl StreamDecoder for Decoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if self.done {
            return Err(decode_error("", "frame after [DONE]"));
        }
        if frame.data.trim() == "[DONE]" {
            let stop_reason = self.reason.clone().ok_or_else(|| {
                decode_error("/choices/0/finish_reason", "[DONE] without finish_reason")
            })?;
            for index in self.open.drain(..) {
                out.push(StreamEvent::ItemEnd { index });
            }
            out.push(StreamEvent::Stop {
                stop_reason,
                usage: self.usage.clone(),
            });
            self.done = true;
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(&frame.data).map_err(|e| decode_error("", e.to_string()))?;
        if value.get("error").is_some() {
            return Err(decode_error(
                "/error",
                "error payload should have been removed by the adapter",
            ));
        }
        let provider_created_at = read(&value["created"], "/created")?;
        if value["object"] != "chat.completion.chunk" {
            return Err(decode_error("/object", "expected chat.completion.chunk"));
        }
        let (id, provider_id) = response_identity(&value, &self.context)?;
        let model: String = read(&value["model"], "/model")?;
        let choices: Vec<Value> = read(&value["choices"], "/choices")?;
        if choices.len() > 1 {
            return Err(decode_error("/choices", "multiple choices are unsupported"));
        }
        if self.response_id.is_none() {
            out.push(StreamEvent::Start {
                response_id: id.clone(),
                provider_id,
                created_at: Timestamp {
                    observed_at: self.context.observed_at,
                    provider_created_at,
                },
                model: Some(model.clone()),
            });
            self.response_id = Some(id);
            self.model = Some(model.clone());
        } else if self.model.as_ref() != Some(&model) {
            out.push(StreamEvent::Metadata(MetadataScope::Response {
                usage: None,
                model: Some(model.clone()),
            }));
            self.model = Some(model);
        }
        if let Some(usage) = decode_usage(&value["usage"])? {
            self.usage = Some(usage);
        }
        let Some(choice) = choices.first() else {
            return Ok(());
        };
        let delta = &choice["delta"];
        if !delta.is_object() {
            return Err(decode_error(
                "/choices/0/delta",
                "required delta object missing",
            ));
        }
        if let Some(reason) =
            read::<Option<String>>(&choice["finish_reason"], "/choices/0/finish_reason")?
        {
            self.reason = Some(stop_reason(&reason));
        }
        if let Some(text) = read::<Option<String>>(&delta["content"], "/choices/0/delta/content")? {
            let index = match self.text {
                Some(index) => index,
                None => {
                    let index = self.start_item(StreamItem::Text, out)?;
                    self.text = Some(index);
                    index
                }
            };
            out.push(StreamEvent::Delta {
                index,
                delta: StreamDelta::Text { text },
            });
        }
        if delta["refusal"].is_string() || delta.get("function_call").is_some() {
            let index = match self.refusal {
                Some(index) => index,
                None => {
                    let index = self.start_item(StreamItem::Unknown, out)?;
                    self.refusal = Some(index);
                    index
                }
            };
            let mut value = serde_json::Map::new();
            if delta["refusal"].is_string() {
                value.insert("refusal".into(), delta["refusal"].clone());
            }
            if let Some(call) = delta.get("function_call") {
                value.insert("function_call".into(), call.clone());
            }
            let value = Value::Object(value);
            out.push(StreamEvent::Delta {
                index,
                delta: StreamDelta::Unknown { value },
            });
        }
        if let Some(calls) = delta.get("tool_calls") {
            for call in read::<Vec<Value>>(calls, "/choices/0/delta/tool_calls")? {
                self.tool(&call, out)?;
            }
        }
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if !self.done {
            return Err(IncompleteStream {
                message: "Chat Completions stream ended without [DONE]".into(),
            }
            .into());
        }
        Ok(())
    }
}

pub(super) struct Encoder {
    options: ConversionOptions,
    header: Option<Value>,
    items: BTreeMap<u32, EncodedItem>,
    next_tool_index: u32,
    stopped: bool,
}

struct EncodedItem {
    kind: EncodedKind,
    ended: bool,
}
enum EncodedKind {
    Text,
    Tool(u32),
    Omitted(LossScope),
}

impl Encoder {
    pub(super) fn new(options: ConversionOptions) -> Self {
        Self {
            options,
            header: None,
            items: BTreeMap::new(),
            next_tool_index: 0,
            stopped: false,
        }
    }

    fn chunk(
        &self,
        choices: Value,
        usage: Option<Value>,
        out: &mut Vec<Frame>,
    ) -> Result<(), BridgeError> {
        let mut chunk = self
            .header
            .clone()
            .ok_or_else(|| invalid("", "Start required before stream content"))?;
        chunk["choices"] = choices;
        if let Some(usage) = usage {
            chunk["usage"] = usage;
        }
        out.push(Frame {
            event: None,
            data: chunk.to_string(),
        });
        Ok(())
    }

    fn delta(&self, delta: Value, out: &mut Vec<Frame>) -> Result<(), BridgeError> {
        self.chunk(
            json!([{"index":0,"delta":delta,"finish_reason":null}]),
            None,
            out,
        )
    }
}

impl StreamEncoder for Encoder {
    fn push(
        &mut self,
        event: &StreamEvent,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if self.stopped {
            return Err(invalid("", "event after Stop"));
        }
        match event {
            StreamEvent::Start {
                response_id,
                created_at,
                model,
                ..
            } => {
                if self.header.is_some() {
                    return Err(invalid("", "duplicate Start"));
                }
                let model = model
                    .as_ref()
                    .ok_or_else(|| unrepresentable("/model", "required model missing"))?;
                let created = created_at
                    .provider_created_at
                    .unwrap_or(created_at.observed_at);
                self.header = Some(
                    json!({"id":response_id,"model":model,"created":created,"object":"chat.completion.chunk"}),
                );
                self.delta(json!({"role":"assistant"}), out)?;
            }
            StreamEvent::Metadata(scope) => {
                if self.header.is_none() {
                    return Err(invalid("", "Start required before metadata"));
                }
                match scope {
                    MetadataScope::Response { usage, model } => {
                        if usage.is_some() {
                            return Err(unrepresentable(
                                "/usage",
                                "provisional usage has no Chat Completions representation",
                            ));
                        }
                        if let Some(model) = model {
                            if let Some(header) = &mut self.header {
                                header["model"] = json!(model);
                            }
                            self.chunk(json!([]), None, out)?;
                        }
                    }
                    MetadataScope::Item {
                        index,
                        call_id,
                        name,
                        ..
                    } => {
                        let path = format!("/items/{index}");
                        let item = self
                            .items
                            .get(index)
                            .ok_or_else(|| invalid(&path, "metadata requires a started item"))?;
                        if call_id.is_some() || name.is_some() {
                            let EncodedKind::Tool(wire_index) = item.kind else {
                                return Err(invalid(&path, "tool metadata requires a tool call"));
                            };
                            let mut call = json!({"index":wire_index,"function":{}});
                            if let Some(call_id) = call_id {
                                call["id"] = json!(call_id);
                            }
                            if let Some(name) = name {
                                call["function"]["name"] = json!(name);
                            }
                            self.delta(json!({"tool_calls":[call]}), out)?;
                        }
                    }
                }
            }
            StreamEvent::ItemStart { index, item, .. } => {
                if self.header.is_none() {
                    return Err(invalid("", "Start required before stream content"));
                }
                let path = format!("/items/{index}");
                if self.items.contains_key(index) {
                    return Err(invalid(&path, "duplicate item index"));
                }
                if usize::try_from(*index).ok() != Some(self.items.len()) {
                    return Err(invalid(&path, "index must follow ItemStart order"));
                }
                let kind = match item {
                    StreamItem::Text => {
                        self.delta(json!({"content":""}), out)?;
                        EncodedKind::Text
                    }
                    StreamItem::ToolCall { call_id, name } => {
                        let wire_index = self.next_tool_index;
                        self.next_tool_index = wire_index
                            .checked_add(1)
                            .ok_or_else(|| invalid(&path, "too many tools"))?;
                        let mut call = json!({"index":wire_index,"type":"function","function":{}});
                        if let Some(id) = call_id {
                            call["id"] = json!(id);
                        }
                        if let Some(name) = name {
                            call["function"]["name"] = json!(name);
                        }
                        self.delta(json!({"tool_calls":[call]}), out)?;
                        EncodedKind::Tool(wire_index)
                    }
                    StreamItem::Reasoning => {
                        omit(&self.options, losses, LossScope::Reasoning, &path)?;
                        EncodedKind::Omitted(LossScope::Reasoning)
                    }
                    StreamItem::Unknown => {
                        omit(&self.options, losses, LossScope::UnknownContent, &path)?;
                        EncodedKind::Omitted(LossScope::UnknownContent)
                    }
                };
                self.items
                    .insert(*index, EncodedItem { kind, ended: false });
            }
            StreamEvent::Delta { index, delta } => {
                let path = format!("/items/{index}");
                let item = self
                    .items
                    .get(index)
                    .filter(|item| !item.ended)
                    .ok_or_else(|| invalid(&path, "delta requires an open item"))?;
                match (&item.kind, delta) {
                    (EncodedKind::Text, StreamDelta::Text { text }) => {
                        self.delta(json!({"content":text}), out)?
                    }
                    (EncodedKind::Tool(index), StreamDelta::ToolCallArguments { text }) => self
                        .delta(
                            json!({"tool_calls":[{"index":index,"function":{"arguments":text}}]}),
                            out,
                        )?,
                    (
                        EncodedKind::Omitted(LossScope::Reasoning),
                        StreamDelta::ReasoningText { .. }
                        | StreamDelta::ReasoningSummary { .. }
                        | StreamDelta::ReasoningOpaque { .. },
                    ) => {}
                    (
                        EncodedKind::Omitted(LossScope::UnknownContent),
                        StreamDelta::Unknown { .. },
                    ) => {}
                    _ => return Err(invalid(&path, "delta does not match item kind")),
                }
            }
            StreamEvent::ItemEnd { index } => {
                let item = self
                    .items
                    .get_mut(index)
                    .filter(|item| !item.ended)
                    .ok_or_else(|| {
                        invalid(&format!("/items/{index}"), "ItemEnd requires an open item")
                    })?;
                item.ended = true;
            }
            StreamEvent::Stop { stop_reason, usage } => {
                if self.items.values().any(|item| !item.ended) {
                    return Err(invalid("/items", "Stop requires all items to end"));
                }
                let reason = finish_reason(stop_reason)?;
                let usage = usage.as_ref().map(encode_usage).transpose()?;
                self.chunk(
                    json!([{"index":0,"delta":{},"finish_reason":reason}]),
                    None,
                    out,
                )?;
                if let Some(usage) = usage {
                    self.chunk(json!([]), Some(usage), out)?;
                }
                out.push(Frame {
                    event: None,
                    data: "[DONE]".into(),
                });
                self.stopped = true;
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        _out: &mut Vec<Frame>,
        _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(invalid("", "stream ended without Stop"));
        }
        Ok(())
    }
}
