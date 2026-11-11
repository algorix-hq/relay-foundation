use std::collections::BTreeMap;

use relay_protocol::{
    ConversionLoss, Response, StopReason, StreamDelta, StreamEvent, StreamItem, Version,
};
use serde_json::{Value, json};

use crate::responses::{response, wire::*};
use crate::{
    ApiFormat, BridgeError, ConversionOptions, Frame, LossScope, NativeFields, Output,
    StreamEncoder,
};

pub(in crate::responses) struct Encoder {
    options: ConversionOptions,
    native: NativeFields,
    base: Option<NativeFields>,
    metadata: Vec<NativeFields>,
    items: BTreeMap<u32, EncodingItem>,
    next_output_index: u32,
    sequence_number: u64,
    stopped: bool,
}

struct EncodingItem {
    output_index: Option<u32>,
    wire: Value,
    id: String,
    final_status: Option<String>,
    ended: bool,
}

impl Encoder {
    pub(in crate::responses) fn new(options: ConversionOptions, native: NativeFields) -> Self {
        Self {
            options,
            native,
            base: None,
            metadata: Vec::new(),
            items: BTreeMap::new(),
            next_output_index: 0,
            sequence_number: 0,
            stopped: false,
        }
    }

    fn emit(
        &mut self,
        kind: &str,
        mut value: NativeFields,
        out: &mut Vec<Frame>,
    ) -> Result<(), BridgeError> {
        value.insert("type".into(), json!(kind));
        value.insert("sequence_number".into(), json!(self.sequence_number));
        let next = self
            .sequence_number
            .checked_add(1)
            .ok_or_else(|| invalid("/sequence_number", "sequence overflow"))?;
        let data = serde_json::to_string(&value).map_err(|error| invalid("", error.to_string()))?;
        out.push(Frame {
            event: Some(kind.into()),
            data,
        });
        self.sequence_number = next;
        Ok(())
    }

    fn emit_value(
        &mut self,
        kind: &str,
        value: Value,
        out: &mut Vec<Frame>,
    ) -> Result<(), BridgeError> {
        self.emit(kind, object(&value, "")?, out)
    }

    fn start(
        &mut self,
        id: &Option<String>,
        model: &Option<String>,
        out: &mut Vec<Frame>,
    ) -> Result<(), BridgeError> {
        if self.base.is_some() {
            return Err(invalid("/type", "duplicate response start"));
        }
        let mut native = self.native.clone();
        self.metadata = native
            .remove("output")
            .map(|value| parse!(value, "/output"))
            .transpose()?
            .unwrap_or_default();
        native.remove("usage");
        let model = if let Some(model) = model {
            if native.contains_key("model") {
                return Err(invalid(
                    "/model",
                    "native field collides with stream response model",
                ));
            }
            model.clone()
        } else {
            required_native!(&mut native, "model", "")?
        };
        let output = Output::with_native(
            ApiFormat::RESPONSES,
            Response {
                version: Version::V1,
                id: id.clone(),
                model,
                content: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
            native,
        );
        let mut base = object(&response::encode(&output, &self.options)?.body, "")?;
        base.insert("status".into(), json!("in_progress"));
        self.emit_value("response.created", json!({"response":base}), out)?;
        self.base = Some(base);
        Ok(())
    }

    fn item_start(
        &mut self,
        index: u32,
        item: &StreamItem,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if self.items.contains_key(&index) {
            return Err(invalid(&format!("/items/{index}"), "duplicate item start"));
        }
        let path = format!("/items/{index}");
        let mut metadata = usize::try_from(index)
            .ok()
            .and_then(|index| self.metadata.get(index))
            .cloned()
            .unwrap_or_default();
        let final_status: Option<String> = optional!(&mut metadata, "status", &path)?;
        if final_status
            .as_deref()
            .is_some_and(|status| !matches!(status, "completed" | "incomplete" | "in_progress"))
        {
            return Err(invalid(
                &format!("{path}/status"),
                "invalid output item status",
            ));
        }
        let (wire, id) = match item {
            StreamItem::Text => {
                let id: String = required_native!(&mut metadata, "id", &path)?;
                if final_status.is_none() {
                    return Err(unrepresentable(
                        &format!("{path}/status"),
                        "message final status must be supplied explicitly",
                    ));
                }
                (
                    json!({"type":"message", "id":id, "role":"assistant", "status":"in_progress", "content":[]}),
                    id,
                )
            }
            StreamItem::ToolCall { id: call_id, name } => {
                let call_id = call_id.as_ref().ok_or_else(|| {
                    unrepresentable(&format!("{path}/id"), "function call ID is required")
                })?;
                let name = name.as_ref().ok_or_else(|| {
                    unrepresentable(&format!("{path}/name"), "function name is required")
                })?;
                let id: String = required_native!(&mut metadata, "id", &path)?;
                (
                    json!({"type":"function_call", "id":id, "call_id":call_id, "name":name, "arguments":""}),
                    id,
                )
            }
            StreamItem::Reasoning { id: Some(id) } => (
                json!({"type":"reasoning", "id":id, "summary":[]}),
                id.clone(),
            ),
            StreamItem::Reasoning { id: None } | StreamItem::Unknown => {
                let scope = if matches!(item, StreamItem::Unknown) {
                    LossScope::UnknownContent
                } else {
                    LossScope::Reasoning
                };
                drop_allowed(scope, &path, &self.options, losses)?;
                self.items.insert(
                    index,
                    EncodingItem {
                        output_index: None,
                        wire: Value::Null,
                        id: String::new(),
                        final_status: None,
                        ended: false,
                    },
                );
                return Ok(());
            }
        };
        for key in metadata.keys() {
            if wire.get(key).is_some() {
                return Err(invalid(
                    &format!("{path}/{}", pointer_key(key)),
                    "native field collides with generated item field",
                ));
            }
        }
        reject_remaining(&metadata, &path)?;
        let output_index = self.next_output_index;
        self.next_output_index = output_index
            .checked_add(1)
            .ok_or_else(|| invalid(&path, "too many output items"))?;
        self.emit_value(
            "response.output_item.added",
            json!({"output_index":output_index, "item":wire}),
            out,
        )?;
        let mut wire = wire;
        if matches!(item, StreamItem::Text) {
            let part = json!({"type":"output_text", "text":"", "annotations":[]});
            self.emit_value(
                "response.content_part.added",
                json!({"output_index":output_index, "item_id":id, "content_index":0, "part":part}),
                out,
            )?;
            wire["content"] = json!([part]);
        }
        self.items.insert(
            index,
            EncodingItem {
                output_index: Some(output_index),
                wire,
                id,
                final_status,
                ended: false,
            },
        );
        Ok(())
    }

    fn delta(
        &mut self,
        index: u32,
        delta: &StreamDelta,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        let path = format!("/items/{index}");
        let item = self
            .items
            .get_mut(&index)
            .filter(|item| !item.ended)
            .ok_or_else(|| invalid(&path, "delta references an unopened or ended item"))?;
        let Some(output_index) = item.output_index else {
            return Ok(());
        };
        let id = item.id.clone();
        if matches!(delta, StreamDelta::Unknown { .. }) {
            return drop_allowed(
                LossScope::UnknownContent,
                &format!("{path}/delta"),
                &self.options,
                losses,
            );
        }
        let kind = item
            .wire
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut leading = None;
        let (event, fields) = match delta {
            StreamDelta::Text { text } if kind == "message" => {
                append(&mut item.wire["content"][0]["text"], text, &path)?;
                (
                    "response.output_text.delta",
                    json!({"output_index":output_index, "item_id":id, "content_index":0, "delta":text, "logprobs":[]}),
                )
            }
            StreamDelta::ToolCallArguments { text } if kind == "function_call" => {
                append(&mut item.wire["arguments"], text, &path)?;
                (
                    "response.function_call_arguments.delta",
                    json!({"output_index":output_index, "item_id":id, "delta":text}),
                )
            }
            StreamDelta::ReasoningText { text } if kind == "reasoning" => {
                if item.wire.get("content").is_none() {
                    let part = json!({"type":"reasoning_text", "text":""});
                    item.wire["content"] = json!([part]);
                    leading = Some((
                        "response.content_part.added",
                        json!({"output_index":output_index, "item_id":id, "content_index":0, "part":part}),
                    ));
                }
                append(&mut item.wire["content"][0]["text"], text, &path)?;
                (
                    "response.reasoning_text.delta",
                    json!({"output_index":output_index, "item_id":id, "content_index":0, "delta":text}),
                )
            }
            StreamDelta::ReasoningSummary {
                summary_index,
                text,
            } if kind == "reasoning" => {
                let summary = item
                    .wire
                    .get_mut("summary")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| invalid(&path, "missing reasoning summary"))?;
                let summary_index_usize = usize::try_from(*summary_index)
                    .map_err(|error| invalid(&path, error.to_string()))?;
                if summary_index_usize > summary.len() {
                    return Err(unrepresentable(
                        &format!("{path}/summary_index"),
                        "cannot encode sparse summaries without inventing missing parts",
                    ));
                }
                if summary_index_usize == summary.len() {
                    let part = json!({"type":"summary_text", "text":""});
                    summary.push(part.clone());
                    leading = Some((
                        "response.reasoning_summary_part.added",
                        json!({"output_index":output_index, "item_id":id, "summary_index":summary_index, "part":part}),
                    ));
                }
                append(&mut summary[summary_index_usize]["text"], text, &path)?;
                (
                    "response.reasoning_summary_text.delta",
                    json!({"output_index":output_index, "item_id":id, "summary_index":summary_index, "delta":text}),
                )
            }
            StreamDelta::ReasoningOpaque { value } if kind == "reasoning" => {
                if item.wire.get("encrypted_content").is_some() {
                    return Err(invalid(
                        &format!("{path}/opaque"),
                        "opaque reasoning is a complete value, not fragments",
                    ));
                }
                item.wire["encrypted_content"] = json!(value);
                return Ok(());
            }
            _ => {
                return Err(invalid(
                    &format!("{path}/delta"),
                    "delta does not match item kind",
                ));
            }
        };
        if let Some((event, fields)) = leading {
            self.emit_value(event, fields, out)?;
        }
        self.emit_value(event, fields, out)
    }

    fn item_end(&mut self, index: u32, out: &mut Vec<Frame>) -> Result<(), BridgeError> {
        let path = format!("/items/{index}");
        let item = self
            .items
            .get_mut(&index)
            .filter(|item| !item.ended)
            .ok_or_else(|| invalid(&path, "end references an unopened or ended item"))?;
        item.ended = true;
        let Some(output_index) = item.output_index else {
            return Ok(());
        };
        let id = item.id.clone();
        if let Some(status) = &item.final_status {
            item.wire["status"] = json!(status);
        }
        let wire = item.wire.clone();
        match wire.get("type").and_then(Value::as_str) {
            Some("message") => {
                let part = wire["content"][0].clone();
                self.emit_value("response.output_text.done", json!({"output_index":output_index, "item_id":id, "content_index":0, "text":part["text"], "logprobs":[]}), out)?;
                self.emit_value("response.content_part.done", json!({"output_index":output_index, "item_id":id, "content_index":0, "part":part}), out)?;
            }
            Some("function_call") => self.emit_value(
                "response.function_call_arguments.done",
                json!({"output_index":output_index, "item_id":id, "arguments":wire["arguments"]}),
                out,
            )?,
            Some("reasoning") => {
                if let Some(parts) = wire.get("content").and_then(Value::as_array) {
                    for (content_index, part) in parts.iter().enumerate() {
                        self.emit_value("response.reasoning_text.done", json!({"output_index":output_index, "item_id":id, "content_index":content_index, "text":part["text"]}), out)?;
                        self.emit_value("response.content_part.done", json!({"output_index":output_index, "item_id":id, "content_index":content_index, "part":part}), out)?;
                    }
                }
                if let Some(parts) = wire.get("summary").and_then(Value::as_array) {
                    for (summary_index, part) in parts.iter().enumerate() {
                        self.emit_value("response.reasoning_summary_text.done", json!({"output_index":output_index, "item_id":id, "summary_index":summary_index, "text":part["text"]}), out)?;
                        self.emit_value("response.reasoning_summary_part.done", json!({"output_index":output_index, "item_id":id, "summary_index":summary_index, "part":part}), out)?;
                    }
                }
            }
            _ => return Err(invalid(&path, "unsupported stream output item")),
        }
        self.emit_value(
            "response.output_item.done",
            json!({"output_index":output_index, "item":wire}),
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
            return Err(invalid("/type", "event after stop"));
        }
        if let StreamEvent::ResponseStart { id, model } = event {
            return self.start(id, model, out);
        }
        if self.base.is_none() {
            return Err(invalid(
                "/type",
                "response start is required before stream items",
            ));
        }
        match event {
            StreamEvent::ItemStart { index, item } => self.item_start(*index, item, out, losses),
            StreamEvent::Delta { index, delta } => self.delta(*index, delta, out, losses),
            StreamEvent::ItemEnd { index } => self.item_end(*index, out),
            StreamEvent::Stop { stop_reason, usage } => {
                if self.items.values().any(|item| !item.ended) {
                    return Err(invalid("/type", "stop before item end"));
                }
                let (status, incomplete_details) = response::encode_stop(stop_reason)?;
                let mut body = self
                    .base
                    .clone()
                    .ok_or_else(|| invalid("/type", "missing response start"))?;
                let mut items: Vec<_> = self
                    .items
                    .values()
                    .filter_map(|item| item.output_index.map(|index| (index, item.wire.clone())))
                    .collect();
                items.sort_by_key(|(index, _)| *index);
                let items: Vec<_> = items.into_iter().map(|(_, item)| item).collect();
                let text: String = items
                    .iter()
                    .filter_map(|item| item.get("content").and_then(Value::as_array))
                    .flatten()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                body.insert("output".into(), json!(items));
                body.insert("output_text".into(), json!(text));
                body.insert("status".into(), json!(status));
                body.insert("incomplete_details".into(), incomplete_details);
                if let Some(usage) =
                    response::encode_usage(usage.as_ref(), self.native.get("usage").cloned())?
                {
                    body.insert("usage".into(), usage);
                }
                let event = if status == "completed" {
                    "response.completed"
                } else {
                    "response.incomplete"
                };
                self.emit_value(event, json!({"response":body}), out)?;
                self.stopped = true;
                Ok(())
            }
            StreamEvent::ResponseStart { .. } => Err(invalid("/type", "duplicate response start")),
        }
    }

    fn finish(
        &mut self,
        _out: &mut Vec<Frame>,
        _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(unrepresentable(
                "/stop_reason",
                "cannot encode a complete stream without a stop event",
            ));
        }
        Ok(())
    }
}

fn append(value: &mut Value, text: &str, path: &str) -> Result<(), BridgeError> {
    let Value::String(value) = value else {
        return Err(invalid(path, "expected string delta target"));
    };
    value.push_str(text);
    Ok(())
}
