use std::collections::{BTreeMap, BTreeSet, VecDeque};

use relay_protocol::{
    AssistantContent, IncompleteStream, StopReason, StreamDelta, StreamEvent, StreamItem,
};
use serde_json::{Value, json};

use crate::{Frame, StreamDecoder, StreamEncoder};

use super::{content::encode_assistant, response::*, *};

#[derive(Default)]
pub(super) struct Decoder {
    started: bool,
    stopped: bool,
    next_index: u32,
    blocks: BTreeMap<u64, DecodedBlock>,
    stop_reason: Option<StopReason>,
    usage: Map<String, Value>,
}

struct DecodedBlock {
    index: u32,
    kind: StreamItem,
    opaque: String,
    open: bool,
}

impl Decoder {
    fn decode(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if frame.event.as_deref() == Some("error") {
            return Err(invalid(
                "/type",
                "adapter should have removed the error event",
            ));
        }
        let value: Value =
            serde_json::from_str(&frame.data).map_err(|e| invalid("", e.to_string()))?;
        let kind = string(&value["type"], "/type")?;
        if kind == "error" {
            return Err(invalid(
                "/type",
                "adapter should have removed the error event",
            ));
        }
        if let Some(event) = &frame.event
            && event != &kind
        {
            return Err(invalid("/type", "frame event and payload type differ"));
        }
        if kind == "ping" {
            return Ok(());
        }
        if self.stopped {
            return Err(invalid("/type", "event after message_stop"));
        }
        if kind == "message_start" {
            if self.started {
                return Err(invalid("/type", "duplicate message_start"));
            }
            let message = &value["message"];
            let id = string(&message["id"], "/message/id")?;
            let model = string(&message["model"], "/message/model")?;
            if !array(&message["content"], "/message/content")?.is_empty() {
                return Err(invalid(
                    "/message/content",
                    "message_start content must be empty",
                ));
            }
            self.update_usage(&message["usage"])?;
            self.started = true;
            out.push(StreamEvent::ResponseStart {
                id: Some(id),
                model: Some(model),
            });
            return Ok(());
        }
        if !self.started {
            return Err(invalid("/type", "event before message_start"));
        }
        match kind.as_str() {
            "content_block_start" => {
                let wire_index = unsigned(&value["index"], "/index")?;
                if self.blocks.contains_key(&wire_index) {
                    return Err(invalid("/index", "duplicate content block index"));
                }
                let block = &value["content_block"];
                object(block, "/content_block")?;
                let block_kind = string(&block["type"], "/content_block/type")?;
                let item = match block_kind.as_str() {
                    "text" => StreamItem::Text,
                    "tool_use" => StreamItem::ToolCall {
                        id: Some(string(&block["id"], "/content_block/id")?),
                        name: Some(string(&block["name"], "/content_block/name")?),
                    },
                    "thinking" | "redacted_thinking" => StreamItem::Reasoning { id: None },
                    _ => StreamItem::Unknown,
                };
                let index = self.next_index;
                self.next_index = self
                    .next_index
                    .checked_add(1)
                    .ok_or_else(|| invalid("/index", "too many stream items"))?;
                let mut opaque = String::new();
                let initial = match block_kind.as_str() {
                    "text" => {
                        let text = string(&block["text"], "/content_block/text")?;
                        (!text.is_empty()).then_some(StreamDelta::Text { text })
                    }
                    "tool_use" => {
                        let input = block
                            .get("input")
                            .ok_or_else(|| invalid("/content_block/input", "missing tool input"))?;
                        (input != &json!({})).then(|| StreamDelta::ToolCallArguments {
                            text: input.to_string(),
                        })
                    }
                    "thinking" => Some(StreamDelta::ReasoningText {
                        text: string(&block["thinking"], "/content_block/thinking")?,
                    }),
                    "redacted_thinking" => {
                        opaque = string(&block["data"], "/content_block/data")?;
                        Some(StreamDelta::ReasoningOpaque {
                            value: opaque.clone(),
                        })
                    }
                    _ => Some(StreamDelta::Unknown {
                        value: block.clone(),
                    }),
                };
                let signature = if block_kind == "thinking" {
                    let signature = string(&block["signature"], "/content_block/signature")?;
                    if !signature.is_empty() {
                        opaque = signature.clone();
                        Some(signature)
                    } else {
                        None
                    }
                } else {
                    None
                };
                out.push(StreamEvent::ItemStart {
                    index,
                    item: item.clone(),
                });
                if let Some(delta) = initial {
                    out.push(StreamEvent::Delta { index, delta });
                }
                if let Some(value) = signature {
                    out.push(StreamEvent::Delta {
                        index,
                        delta: StreamDelta::ReasoningOpaque { value },
                    });
                }
                self.blocks.insert(
                    wire_index,
                    DecodedBlock {
                        index,
                        kind: item,
                        opaque,
                        open: true,
                    },
                );
            }
            "content_block_delta" => {
                let wire_index = unsigned(&value["index"], "/index")?;
                let block = self
                    .blocks
                    .get_mut(&wire_index)
                    .filter(|b| b.open)
                    .ok_or_else(|| invalid("/index", "delta for unopened content block"))?;
                let delta = &value["delta"];
                object(delta, "/delta")?;
                let kind = string(&delta["type"], "/delta/type")?;
                let mapped = match (kind.as_str(), &block.kind) {
                    (_, StreamItem::Unknown) => StreamDelta::Unknown {
                        value: delta.clone(),
                    },
                    ("text_delta", StreamItem::Text) => StreamDelta::Text {
                        text: string(&delta["text"], "/delta/text")?,
                    },
                    ("input_json_delta", StreamItem::ToolCall { .. }) => {
                        StreamDelta::ToolCallArguments {
                            text: string(&delta["partial_json"], "/delta/partial_json")?,
                        }
                    }
                    ("thinking_delta", StreamItem::Reasoning { .. }) => {
                        StreamDelta::ReasoningText {
                            text: string(&delta["thinking"], "/delta/thinking")?,
                        }
                    }
                    ("signature_delta", StreamItem::Reasoning { .. }) => {
                        block
                            .opaque
                            .push_str(&string(&delta["signature"], "/delta/signature")?);
                        StreamDelta::ReasoningOpaque {
                            value: block.opaque.clone(),
                        }
                    }
                    (
                        "text_delta" | "input_json_delta" | "thinking_delta" | "signature_delta",
                        _,
                    ) => {
                        return Err(invalid(
                            "/delta/type",
                            "delta does not match content block kind",
                        ));
                    }
                    _ => StreamDelta::Unknown {
                        value: delta.clone(),
                    },
                };
                out.push(StreamEvent::Delta {
                    index: block.index,
                    delta: mapped,
                });
            }
            "content_block_stop" => {
                let wire_index = unsigned(&value["index"], "/index")?;
                let block = self
                    .blocks
                    .get_mut(&wire_index)
                    .filter(|b| b.open)
                    .ok_or_else(|| invalid("/index", "stop for unopened content block"))?;
                block.open = false;
                out.push(StreamEvent::ItemEnd { index: block.index });
            }
            "message_delta" => {
                object(&value["delta"], "/delta")?;
                if let Some(reason) = value["delta"].get("stop_reason").filter(|v| !v.is_null()) {
                    self.stop_reason = Some(decode_stop(&string(reason, "/delta/stop_reason")?));
                }
                self.update_usage(&value["usage"])?;
            }
            "message_stop" => {
                if self.blocks.values().any(|b| b.open) {
                    return Err(invalid("/type", "message_stop before content_block_stop"));
                }
                let stop_reason = self.stop_reason.clone().ok_or_else(|| {
                    invalid("/delta/stop_reason", "message_stop without a stop reason")
                })?;
                let usage = decode_usage(&Value::Object(self.usage.clone()), "/usage")?;
                out.push(StreamEvent::Stop {
                    stop_reason,
                    usage: Some(usage),
                });
                self.stopped = true;
            }
            _ => return Err(invalid("/type", "unknown stream event type")),
        }
        Ok(())
    }

    fn update_usage(&mut self, usage: &Value) -> Result<(), BridgeError> {
        let usage = object(usage, "/usage")?;
        for (key, value) in usage {
            if !value.is_null() {
                self.usage.insert(key.clone(), value.clone());
            }
        }
        decode_usage(&Value::Object(self.usage.clone()), "/usage")?;
        Ok(())
    }
}

impl StreamDecoder for Decoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        self.decode(frame, out).map_err(decode_error)
    }

    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(IncompleteStream {
                message: "Messages stream ended before message_stop".into(),
            }
            .into());
        }
        Ok(())
    }
}

pub(super) struct Encoder {
    options: ConversionOptions,
    started: bool,
    stopped: bool,
    next_index: u32,
    blocks: BTreeMap<u32, EncodedKind>,
    seen: BTreeSet<u32>,
    wire_indices: BTreeMap<u32, u32>,
    pending: VecDeque<PendingFrame>,
}

enum PendingFrame {
    Data { index: Option<u32>, value: Value },
    Reasoning { index: u32, content: Option<Value> },
}

enum EncodedKind {
    Text,
    Tool {
        arguments: String,
    },
    Reasoning {
        text: Option<String>,
        opaque: Option<String>,
    },
    Dropped,
}

impl Encoder {
    pub(super) fn new(options: ConversionOptions) -> Self {
        Self {
            options,
            started: false,
            stopped: false,
            next_index: 0,
            blocks: BTreeMap::new(),
            seen: BTreeSet::new(),
            wire_indices: BTreeMap::new(),
            pending: VecDeque::new(),
        }
    }
}

fn frame(out: &mut Vec<Frame>, value: Value) {
    out.push(Frame {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    });
}

impl Encoder {
    fn encode(
        &mut self,
        event: &StreamEvent,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if self.stopped {
            return Err(invalid("", "event after Stop"));
        }
        if let StreamEvent::ResponseStart { id, model } = event {
            if self.started {
                return Err(invalid("", "duplicate ResponseStart"));
            }
            let id = id.as_ref().ok_or_else(|| {
                unrepresentable("/id", "Messages streaming requires a response ID")
            })?;
            let model = model
                .as_ref()
                .ok_or_else(|| unrepresentable("/model", "Messages streaming requires a model"))?;
            self.pending.push_back(PendingFrame::Data { index: None, value: json!({"type":"message_start", "message":message(id, model, encode_usage(None)?)}) });
            self.started = true;
            return Ok(());
        }
        if !self.started {
            return Err(invalid("", "event before ResponseStart"));
        }
        match event {
            StreamEvent::ResponseStart { .. } => {
                return Err(invalid("", "duplicate ResponseStart"));
            }
            StreamEvent::ItemStart { index, item } => {
                let path = format!("/items/{index}");
                if self.seen.contains(index) {
                    return Err(invalid(&path, "duplicate item index"));
                }
                let (kind, block) = match item {
                    StreamItem::Text => (
                        EncodedKind::Text,
                        Some(json!({"type":"text", "text":"", "citations":null})),
                    ),
                    StreamItem::ToolCall { id, name } => {
                        let id = id.as_ref().ok_or_else(|| {
                            unrepresentable(
                                &pointer(&path, "id"),
                                "Messages tool calls require an ID",
                            )
                        })?;
                        let name = name.as_ref().ok_or_else(|| {
                            unrepresentable(
                                &pointer(&path, "name"),
                                "Messages tool calls require a name",
                            )
                        })?;
                        (
                            EncodedKind::Tool {
                                arguments: String::new(),
                            },
                            Some(
                                json!({"type":"tool_use", "id":id, "name":name, "input":{}, "caller":{"type":"direct"}}),
                            ),
                        )
                    }
                    StreamItem::Reasoning { id } => {
                        if id.is_some() {
                            allow_loss(
                                &self.options,
                                losses,
                                LossScope::Reasoning,
                                &pointer(&path, "id"),
                                "Messages cannot express reasoning item IDs",
                            )?;
                        }
                        (
                            EncodedKind::Reasoning {
                                text: None,
                                opaque: None,
                            },
                            None,
                        )
                    }
                    StreamItem::Unknown => {
                        allow_loss(
                            &self.options,
                            losses,
                            LossScope::UnknownContent,
                            &path,
                            "unknown stream item omitted",
                        )?;
                        (EncodedKind::Dropped, None)
                    }
                };
                if let Some(block) = block {
                    self.pending.push_back(PendingFrame::Data {
                        index: Some(*index),
                        value: json!({"type":"content_block_start", "content_block":block}),
                    });
                } else if matches!(kind, EncodedKind::Reasoning { .. }) {
                    self.pending.push_back(PendingFrame::Reasoning {
                        index: *index,
                        content: None,
                    });
                }
                self.blocks.insert(*index, kind);
                self.seen.insert(*index);
            }
            StreamEvent::Delta { index, delta } => {
                let path = format!("/items/{index}");
                let block = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| invalid(&path, "delta for unopened item"))?;
                if matches!(block, EncodedKind::Dropped) {
                    return Ok(());
                }
                let value = match (block, delta) {
                    (EncodedKind::Text, StreamDelta::Text { text }) => {
                        Some(json!({"type":"text_delta", "text":text}))
                    }
                    (EncodedKind::Tool { arguments }, StreamDelta::ToolCallArguments { text }) => {
                        arguments.push_str(text);
                        Some(json!({"type":"input_json_delta", "partial_json":text}))
                    }
                    (
                        EncodedKind::Reasoning { text, .. },
                        StreamDelta::ReasoningText { text: fragment },
                    ) => {
                        text.get_or_insert_default().push_str(fragment);
                        None
                    }
                    (
                        EncodedKind::Reasoning { opaque, .. },
                        StreamDelta::ReasoningOpaque { value },
                    ) => {
                        *opaque = Some(value.clone());
                        None
                    }
                    (_, StreamDelta::ReasoningSummary { .. }) => {
                        return Err(unrepresentable(
                            &pointer(&path, "summary"),
                            "Messages cannot express reasoning summaries",
                        ));
                    }
                    (_, StreamDelta::Unknown { .. }) => {
                        allow_loss(
                            &self.options,
                            losses,
                            LossScope::UnknownContent,
                            &pointer(&path, "delta"),
                            "unknown stream delta omitted",
                        )?;
                        None
                    }
                    _ => {
                        return Err(invalid(
                            &pointer(&path, "delta"),
                            "delta does not match item kind",
                        ));
                    }
                };
                if let Some(delta) = value {
                    self.pending.push_back(PendingFrame::Data {
                        index: Some(*index),
                        value: json!({"type":"content_block_delta", "delta":delta}),
                    });
                }
            }
            StreamEvent::ItemEnd { index } => {
                let path = format!("/items/{index}");
                let block = self
                    .blocks
                    .get(index)
                    .ok_or_else(|| invalid(&path, "end for unopened item"))?;
                match block {
                    EncodedKind::Dropped => {
                        self.blocks.remove(index);
                        return Ok(());
                    }
                    EncodedKind::Tool { arguments } if !arguments.is_empty() => {
                        serde_json::from_str::<Value>(arguments)
                            .map_err(|e| invalid(&pointer(&path, "arguments"), e.to_string()))?;
                    }
                    EncodedKind::Reasoning { text, opaque } => {
                        let part = AssistantContent::Reasoning {
                            id: None,
                            text: text.clone(),
                            summary: Vec::new(),
                            opaque: opaque.clone(),
                        };
                        let value = encode_assistant(&part, &path, true, &self.options, losses)?;
                        let position = self.pending.iter().position(|pending| matches!(pending, PendingFrame::Reasoning { index: pending_index, .. } if pending_index == index)).ok_or_else(|| invalid(&path, "missing buffered reasoning start"))?;
                        if let Some(value) = value {
                            if let Some(PendingFrame::Reasoning { content, .. }) =
                                self.pending.get_mut(position)
                            {
                                *content = Some(value);
                            }
                        } else {
                            self.pending.remove(position);
                        }
                        self.blocks.remove(index);
                        return Ok(());
                    }
                    _ => {}
                }
                self.pending.push_back(PendingFrame::Data {
                    index: Some(*index),
                    value: json!({"type":"content_block_stop"}),
                });
                self.blocks.remove(index);
            }
            StreamEvent::Stop { stop_reason, usage } => {
                if !self.blocks.is_empty() {
                    return Err(invalid("/items", "Stop before ItemEnd"));
                }
                let reason = encode_stop(stop_reason)?;
                let usage = encode_usage(usage.as_ref())?;
                let usage = json!({
                    "input_tokens":usage["input_tokens"], "output_tokens":usage["output_tokens"],
                    "cache_read_input_tokens":usage["cache_read_input_tokens"], "cache_creation_input_tokens":usage["cache_creation_input_tokens"],
                    "output_tokens_details":usage["output_tokens_details"], "server_tool_use":null,
                });
                self.pending.push_back(PendingFrame::Data { index: None, value: json!({"type":"message_delta", "delta":{"stop_reason":reason, "stop_sequence":null, "container":null, "stop_details":null}, "usage":usage}) });
                self.pending.push_back(PendingFrame::Data {
                    index: None,
                    value: json!({"type":"message_stop"}),
                });
                self.stopped = true;
            }
        }
        Ok(())
    }

    fn allocate_index(&mut self, index: u32) -> Result<u32, BridgeError> {
        let wire_index = self.next_index;
        self.next_index = self
            .next_index
            .checked_add(1)
            .ok_or_else(|| invalid("/items", "too many stream items"))?;
        self.wire_indices.insert(index, wire_index);
        Ok(wire_index)
    }

    fn flush(&mut self, out: &mut Vec<Frame>) -> Result<(), BridgeError> {
        while let Some(pending) = self.pending.pop_front() {
            match pending {
                PendingFrame::Reasoning {
                    index,
                    content: None,
                } => {
                    self.pending.push_front(PendingFrame::Reasoning {
                        index,
                        content: None,
                    });
                    break;
                }
                PendingFrame::Reasoning {
                    index,
                    content: Some(value),
                } => {
                    let index = self.allocate_index(index)?;
                    if value["type"] == "redacted_thinking" {
                        frame(
                            out,
                            json!({"type":"content_block_start", "index":index, "content_block":value}),
                        );
                    } else {
                        frame(
                            out,
                            json!({"type":"content_block_start", "index":index, "content_block":{"type":"thinking", "thinking":"", "signature":""}}),
                        );
                        frame(
                            out,
                            json!({"type":"content_block_delta", "index":index, "delta":{"type":"thinking_delta", "thinking":value["thinking"]}}),
                        );
                        frame(
                            out,
                            json!({"type":"content_block_delta", "index":index, "delta":{"type":"signature_delta", "signature":value["signature"]}}),
                        );
                    }
                    frame(out, json!({"type":"content_block_stop", "index":index}));
                }
                PendingFrame::Data { index, mut value } => {
                    if let Some(index) = index {
                        let wire_index = if value["type"] == "content_block_start" {
                            self.allocate_index(index)?
                        } else {
                            *self.wire_indices.get(&index).ok_or_else(|| {
                                invalid(
                                    &format!("/items/{index}"),
                                    "item has not started on the wire",
                                )
                            })?
                        };
                        value["index"] = json!(wire_index);
                    }
                    frame(out, value);
                }
            }
        }
        Ok(())
    }
}

impl StreamEncoder for Encoder {
    fn push(
        &mut self,
        event: &StreamEvent,
        out: &mut Vec<Frame>,
        losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        let result = self.encode(event, losses);
        self.flush(out)?;
        result
    }

    fn finish(
        &mut self,
        _out: &mut Vec<Frame>,
        _losses: &mut Vec<ConversionLoss>,
    ) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(invalid(
                "",
                "Messages stream encoding requires a Stop event",
            ));
        }
        Ok(())
    }
}
