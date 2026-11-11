use std::collections::BTreeMap;

use relay_protocol::{StreamDelta, StreamEvent, StreamItem};
use serde_json::Value;

use crate::responses::{response, wire::*};
use crate::{BridgeError, Frame, StreamDecoder};

#[derive(Default)]
pub(in crate::responses) struct Decoder {
    items: BTreeMap<u32, DecodingItem>,
    next_index: u32,
    started: bool,
    stopped: bool,
}

struct DecodingItem {
    index: u32,
    kind: StreamItem,
    source_id: Option<String>,
    observed: BTreeMap<DeltaKey, String>,
    ended: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DeltaKey {
    Text(u32),
    Arguments,
    ReasoningText(u32),
    Summary(u32),
}

impl StreamDecoder for Decoder {
    fn push(&mut self, frame: Frame, out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        let value: Value = serde_json::from_str(&frame.data)
            .map_err(|error| decode_error("", error.to_string()))?;
        let kind: String = field!(&value, "type", "")?;
        if matches!(kind.as_str(), "error" | "response.failed") {
            return Err(decode_error(
                "/type",
                "adapter should have removed the provider failure event before bridge decoding",
            ));
        }
        if self.stopped {
            return Err(decode_error("/type", "event after terminal response event"));
        }
        match kind.as_str() {
            "response.created" => {
                if self.started {
                    return Err(decode_error("/type", "duplicate response.created"));
                }
                let response: Value = field!(&value, "response", "")?;
                response::reject_failure(&response)?;
                let id = Some(field!(&response, "id", "/response")?);
                let model = Some(field!(&response, "model", "/response")?);
                self.started = true;
                out.push(StreamEvent::ResponseStart { id, model });
            }
            "response.output_item.added" => {
                let output_index = field!(&value, "output_index", "")?;
                if self.items.contains_key(&output_index) {
                    return Err(decode_error("/output_index", "duplicate output item start"));
                }
                let item: Value = field!(&value, "item", "")?;
                let kind: String = field!(&item, "type", "/item")?;
                let kind = match kind.as_str() {
                    "message" => StreamItem::Text,
                    "function_call" => StreamItem::ToolCall {
                        id: Some(field!(&item, "call_id", "/item")?),
                        name: Some(field!(&item, "name", "/item")?),
                    },
                    "reasoning" => StreamItem::Reasoning {
                        id: Some(field!(&item, "id", "/item")?),
                    },
                    _ => StreamItem::Unknown,
                };
                let source_id = match &kind {
                    StreamItem::Text | StreamItem::Reasoning { .. } => {
                        Some(field!(&item, "id", "/item")?)
                    }
                    StreamItem::ToolCall { .. } => field!(&item, "id", "/item")?,
                    StreamItem::Unknown => None,
                };
                let index = self.allocate()?;
                out.push(StreamEvent::ItemStart {
                    index,
                    item: kind.clone(),
                });
                self.items.insert(
                    output_index,
                    DecodingItem {
                        index,
                        kind,
                        source_id,
                        observed: BTreeMap::new(),
                        ended: false,
                    },
                );
            }
            "response.output_text.delta"
            | "response.function_call_arguments.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.output_text.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_text.done" => self.text_event(&value, &kind, out)?,
            "response.output_item.done" => {
                let output_index = field!(&value, "output_index", "")?;
                let item: Value = field!(&value, "item", "")?;
                let active = self.active(output_index)?;
                let index = active.index;
                let started_kind = active.kind.clone();
                let kind: String = field!(&item, "type", "/item")?;
                if !matches!(active.kind, StreamItem::Unknown) {
                    let mut content = Vec::new();
                    response::decode_item(&item, "/item", &mut content)?;
                    let source_id: Option<String> = field!(&item, "id", "/item")?;
                    if source_id
                        .as_ref()
                        .zip(active.source_id.as_ref())
                        .is_some_and(|(done, started)| done != started)
                    {
                        return Err(decode_error("/item/id", "output item identity changed"));
                    }
                }
                match &started_kind {
                    StreamItem::Reasoning { id } => {
                        if kind != "reasoning"
                            || field!(Option<String>; &item, "id", "/item")? != *id
                        {
                            return Err(decode_error("/item/id", "reasoning identity changed"));
                        }
                        let content: Option<Vec<Value>> = field!(&item, "content", "/item")?;
                        for (content_index, part) in content.unwrap_or_default().iter().enumerate()
                        {
                            let content_index = u32::try_from(content_index).map_err(|error| {
                                decode_error("/item/content", error.to_string())
                            })?;
                            let text = field!(part, "text", "/item/content")?;
                            self.observe(
                                output_index,
                                DeltaKey::ReasoningText(content_index),
                                text,
                                true,
                                out,
                            )?;
                        }
                        let summary: Vec<Value> = field!(&item, "summary", "/item")?;
                        for (summary_index, part) in summary.iter().enumerate() {
                            let summary_index = u32::try_from(summary_index).map_err(|error| {
                                decode_error("/item/summary", error.to_string())
                            })?;
                            let text = field!(part, "text", "/item/summary")?;
                            self.observe(
                                output_index,
                                DeltaKey::Summary(summary_index),
                                text,
                                true,
                                out,
                            )?;
                        }
                        let opaque: Option<String> = field!(&item, "encrypted_content", "/item")?;
                        if let Some(value) = opaque {
                            out.push(StreamEvent::Delta {
                                index,
                                delta: StreamDelta::ReasoningOpaque { value },
                            });
                        }
                    }
                    StreamItem::ToolCall { id, name } => {
                        if kind != "function_call"
                            || field!(Option<String>; &item, "call_id", "/item")? != *id
                            || field!(Option<String>; &item, "name", "/item")? != *name
                        {
                            return Err(decode_error("/item", "function call identity changed"));
                        }
                        let text = field!(&item, "arguments", "/item")?;
                        self.observe(output_index, DeltaKey::Arguments, text, true, out)?;
                    }
                    StreamItem::Text if kind != "message" => {
                        return Err(decode_error("/item/type", "message kind changed"));
                    }
                    StreamItem::Text => {
                        let parts: Vec<Value> = field!(&item, "content", "/item")?;
                        for (content_index, part) in parts.into_iter().enumerate() {
                            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                                let content_index =
                                    u32::try_from(content_index).map_err(|error| {
                                        decode_error("/item/content", error.to_string())
                                    })?;
                                let text = field!(&part, "text", "/item/content")?;
                                self.observe(
                                    output_index,
                                    DeltaKey::Text(content_index),
                                    text,
                                    true,
                                    out,
                                )?;
                            } else {
                                out.push(StreamEvent::Delta {
                                    index,
                                    delta: StreamDelta::Unknown { value: part },
                                });
                            }
                        }
                    }
                    StreamItem::Unknown => out.push(StreamEvent::Delta {
                        index,
                        delta: StreamDelta::Unknown {
                            value: item.clone(),
                        },
                    }),
                }
                self.items
                    .get_mut(&output_index)
                    .ok_or_else(|| decode_error("/output_index", "unknown item"))?
                    .ended = true;
                out.push(StreamEvent::ItemEnd { index });
            }
            "response.completed" | "response.incomplete" => {
                let value: Value = field!(&value, "response", "")?;
                let stop_reason = response::stop_reason(&value)?;
                if (kind == "response.completed")
                    != (value.get("status").and_then(Value::as_str) == Some("completed"))
                {
                    return Err(decode_error(
                        "/response/status",
                        "terminal event and response status disagree",
                    ));
                }
                if self.items.values().any(|item| !item.ended) {
                    return Err(decode_error(
                        "/type",
                        "terminal response before output_item.done",
                    ));
                }
                let usage = response::decode_usage(value.get("usage"))?;
                out.push(StreamEvent::Stop { stop_reason, usage });
                self.stopped = true;
            }
            kind if informational(kind) => {}
            _ => {
                let output_index: Option<u32> = field!(&value, "output_index", "")?;
                let existing = output_index
                    .and_then(|index| self.items.get(&index))
                    .filter(|item| !item.ended)
                    .map(|item| item.index);
                let index = if let Some(index) = existing {
                    index
                } else {
                    let index = self.allocate()?;
                    out.push(StreamEvent::ItemStart {
                        index,
                        item: StreamItem::Unknown,
                    });
                    out.push(StreamEvent::Delta {
                        index,
                        delta: StreamDelta::Unknown { value },
                    });
                    out.push(StreamEvent::ItemEnd { index });
                    return Ok(());
                };
                out.push(StreamEvent::Delta {
                    index,
                    delta: StreamDelta::Unknown { value },
                });
            }
        }
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<StreamEvent>) -> Result<(), BridgeError> {
        if !self.stopped {
            return Err(relay_protocol::IncompleteStream {
                message: "Responses stream ended before response.completed or response.incomplete"
                    .into(),
            }
            .into());
        }
        Ok(())
    }
}

impl Decoder {
    fn text_event(
        &mut self,
        value: &Value,
        kind: &str,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), BridgeError> {
        let output_index = field!(value, "output_index", "")?;
        let source_id: String = field!(value, "item_id", "")?;
        let item = self
            .items
            .get_mut(&output_index)
            .filter(|item| !item.ended)
            .ok_or_else(|| {
                decode_error(
                    "/output_index",
                    "event references an unopened or ended item",
                )
            })?;
        if item.source_id.as_ref().is_some_and(|id| id != &source_id) {
            return Err(decode_error(
                "/item_id",
                "delta item identity disagrees with output_index",
            ));
        }
        item.source_id = Some(source_id);
        let complete = kind.ends_with(".done");
        let key = match kind {
            "response.output_text.delta" | "response.output_text.done" => {
                DeltaKey::Text(field!(value, "content_index", "")?)
            }
            "response.reasoning_text.delta" | "response.reasoning_text.done" => {
                DeltaKey::ReasoningText(field!(value, "content_index", "")?)
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_summary_text.done" => {
                DeltaKey::Summary(field!(value, "summary_index", "")?)
            }
            _ => DeltaKey::Arguments,
        };
        let field = if !complete {
            "delta"
        } else if key == DeltaKey::Arguments {
            "arguments"
        } else {
            "text"
        };
        let text = field!(value, field, "")?;
        self.observe(output_index, key, text, complete, out)
    }

    fn observe(
        &mut self,
        output_index: u32,
        key: DeltaKey,
        text: String,
        complete: bool,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), BridgeError> {
        let item = self
            .items
            .get_mut(&output_index)
            .filter(|item| !item.ended)
            .ok_or_else(|| {
                decode_error(
                    "/output_index",
                    "event references an unopened or ended item",
                )
            })?;
        let matches = matches!(
            (&item.kind, key),
            (StreamItem::Text, DeltaKey::Text(_))
                | (StreamItem::ToolCall { .. }, DeltaKey::Arguments)
                | (
                    StreamItem::Reasoning { .. },
                    DeltaKey::ReasoningText(_) | DeltaKey::Summary(_)
                )
        );
        if !matches {
            return Err(decode_error(
                "/type",
                "delta does not match output item kind",
            ));
        }
        let had_value = item.observed.contains_key(&key);
        let observed = item.observed.entry(key).or_default();
        let text = if complete {
            let suffix = text
                .strip_prefix(observed.as_str())
                .ok_or_else(|| {
                    decode_error("/text", "completed text disagrees with streamed prefix")
                })?
                .to_owned();
            observed.push_str(&suffix);
            if suffix.is_empty()
                && (had_value || matches!(key, DeltaKey::Text(_) | DeltaKey::Arguments))
            {
                return Ok(());
            }
            suffix
        } else {
            observed.push_str(&text);
            text
        };
        let delta = match key {
            DeltaKey::Text(_) => StreamDelta::Text { text },
            DeltaKey::Arguments => StreamDelta::ToolCallArguments { text },
            DeltaKey::ReasoningText(_) => StreamDelta::ReasoningText { text },
            DeltaKey::Summary(summary_index) => StreamDelta::ReasoningSummary {
                summary_index,
                text,
            },
        };
        out.push(StreamEvent::Delta {
            index: item.index,
            delta,
        });
        Ok(())
    }

    fn allocate(&mut self) -> Result<u32, BridgeError> {
        let index = self.next_index;
        self.next_index = index
            .checked_add(1)
            .ok_or_else(|| decode_error("/output_index", "too many output items"))?;
        Ok(index)
    }

    fn active(&self, output_index: u32) -> Result<&DecodingItem, BridgeError> {
        self.items
            .get(&output_index)
            .filter(|item| !item.ended)
            .ok_or_else(|| {
                decode_error(
                    "/output_index",
                    "event references an unopened or ended item",
                )
            })
    }
}

fn informational(kind: &str) -> bool {
    matches!(
        kind,
        "response.in_progress"
            | "response.queued"
            | "response.compaction.compacting"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.file_search_call.in_progress"
            | "response.file_search_call.searching"
            | "response.file_search_call.completed"
            | "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed"
            | "response.code_interpreter_call.in_progress"
            | "response.code_interpreter_call.interpreting"
            | "response.code_interpreter_call.completed"
            | "response.image_generation_call.in_progress"
            | "response.image_generation_call.generating"
            | "response.image_generation_call.completed"
            | "response.mcp_call.in_progress"
            | "response.mcp_call.completed"
            | "response.mcp_call.failed"
            | "response.mcp_list_tools.in_progress"
            | "response.mcp_list_tools.completed"
            | "response.mcp_list_tools.failed"
    )
}
