//! Semantic recording preserves native content blocks and terminal state.
use crate::engine::scenario::TranscriptEvent;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Parse SSE events with LF, CRLF, or CR line endings and optional spaces
/// after field colons. Ignores comments and unrecorded fields such as `id`.
/// Also tolerates a missing trailing blank line in captured responses.
pub fn parse_sse_events(body: &[u8]) -> Result<Vec<TranscriptEvent>> {
    let text = std::str::from_utf8(body).context("sse body was not valid utf-8")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut events = Vec::new();
    let mut event = None;
    let mut data_lines = Vec::new();

    // The final empty line also flushes a capture without a terminating
    // blank line, preserving the recorder's existing EOF tolerance.
    for line in text.split('\n').chain(std::iter::once("")) {
        if line.is_empty() {
            if !data_lines.is_empty() {
                events.push(TranscriptEvent {
                    event: event.take(),
                    data: data_lines.join("\n"),
                });
            }
            event = None;
            data_lines.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }

        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_owned()),
            "data" => data_lines.push(value),
            _ => {}
        }
    }

    Ok(events)
}

/// Hash of a request body for transcript matching.
///
/// Object keys are sorted recursively before hashing, so whitespace and key
/// order do not affect matching. Array order is preserved. The hash is FNV-1a
/// over that canonical text — a matcher key, not a security boundary.
#[must_use]
pub fn request_hash(body: &[u8]) -> Option<String> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    value.sort_all_objects();
    let canonical = value.to_string();

    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(format!("{hash:016x}"))
}

pub fn derive_script(body: &Value) -> Result<Value> {
    anyhow::ensure!(body["type"] == "message", "not a Messages response");
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .context("missing content")?;
    let stop = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .context("missing stop_reason")?;
    let mut script = json!({"kind":"success","content":content,"stop_reason":stop});
    for field in ["usage", "stop_sequence", "stop_details"] {
        if let Some(value) = body.get(field) {
            script[field] = value.clone();
        }
    }
    Ok(script)
}

/// Reconstruct the native message from its SSE sequence before deriving it.
/// A truncated stream or an error event is never saved as a success.
pub fn message_from_events(events: &[TranscriptEvent]) -> Result<Value> {
    let mut message = None;
    let mut blocks: BTreeMap<u64, Value> = BTreeMap::new();
    let mut arguments: BTreeMap<u64, String> = BTreeMap::new();
    let mut open_blocks = BTreeSet::new();
    let mut saw_message_delta = false;
    let mut stopped = false;
    for event in events {
        let data: Value = serde_json::from_str(&event.data).context("invalid event JSON")?;
        anyhow::ensure!(data.is_object(), "event must be an object");
        anyhow::ensure!(!stopped, "event after message_stop");
        if let (Some(kind), Some(name)) = (data["type"].as_str(), event.event.as_deref()) {
            anyhow::ensure!(kind == name, "event name and data type disagree");
        }
        match data["type"].as_str().or(event.event.as_deref()) {
            Some("message_start") => {
                anyhow::ensure!(message.is_none(), "duplicate message_start");
                let initial = data.get("message").context("missing message")?;
                anyhow::ensure!(initial.is_object(), "message must be an object");
                anyhow::ensure!(
                    initial["content"].as_array().is_some_and(Vec::is_empty),
                    "message_start content must be empty"
                );
                message = Some(initial.clone());
            }
            Some("content_block_start") => {
                anyhow::ensure!(
                    message.is_some() && !saw_message_delta,
                    "block start outside content phase"
                );
                let index = data["index"].as_u64().context("missing block index")?;
                anyhow::ensure!(
                    index == u64::try_from(blocks.len())?,
                    "block indexes must start at zero and increase without gaps"
                );
                anyhow::ensure!(
                    data["content_block"].is_object(),
                    "content block must be an object"
                );
                blocks.insert(index, data["content_block"].clone());
                open_blocks.insert(index);
            }
            Some("content_block_delta") => {
                let index = data["index"].as_u64().context("missing block index")?;
                anyhow::ensure!(open_blocks.contains(&index), "delta without an open block");
                let block = blocks
                    .get_mut(&index)
                    .context("delta without block start")?;
                let delta = &data["delta"];
                anyhow::ensure!(delta.is_object(), "block delta must be an object");
                let compatible = match delta["type"].as_str() {
                    Some("text_delta" | "citations_delta") => block["type"] == "text",
                    Some("thinking_delta" | "signature_delta") => block["type"] == "thinking",
                    Some("input_json_delta") => {
                        matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use"))
                    }
                    _ => false,
                };
                anyhow::ensure!(
                    compatible,
                    "delta does not match block type; use transcript recording"
                );
                match delta["type"].as_str() {
                    Some("text_delta") => append(
                        block,
                        "text",
                        delta["text"].as_str().context("missing text")?,
                    )?,
                    Some("thinking_delta") => append(
                        block,
                        "thinking",
                        delta["thinking"].as_str().context("missing thinking")?,
                    )?,
                    Some("signature_delta") => append(
                        block,
                        "signature",
                        delta["signature"].as_str().context("missing signature")?,
                    )?,
                    Some("input_json_delta") => arguments.entry(index).or_default().push_str(
                        delta["partial_json"]
                            .as_str()
                            .context("missing partial_json")?,
                    ),
                    Some("citations_delta") => {
                        if block.get("citations").is_none() {
                            block["citations"] = json!([]);
                        }
                        block["citations"]
                            .as_array_mut()
                            .context("citations not an array")?
                            .push(delta["citation"].clone());
                    }
                    _ => anyhow::bail!("unsupported delta; use transcript recording"),
                }
            }
            Some("message_delta") => {
                anyhow::ensure!(
                    open_blocks.is_empty(),
                    "message_delta before content blocks closed"
                );
                let target = message
                    .as_mut()
                    .context("message_delta before message_start")?;
                let delta = data["delta"]
                    .as_object()
                    .context("message delta must be an object")?;
                for (key, value) in delta {
                    target[key] = value.clone();
                }
                if let Some(usage) = data.get("usage") {
                    let usage = usage.as_object().context("usage must be an object")?;
                    if !target["usage"].is_object() {
                        target["usage"] = json!({});
                    }
                    for (key, value) in usage {
                        target["usage"][key] = value.clone();
                    }
                }
                saw_message_delta = true;
            }
            Some("content_block_stop") => {
                let index = data["index"].as_u64().context("missing block index")?;
                anyhow::ensure!(open_blocks.remove(&index), "stop without an open block");
                if let Some(raw) = arguments.remove(&index) {
                    let input: Value = serde_json::from_str(&raw)
                        .context("incomplete tool input; use transcript recording")?;
                    anyhow::ensure!(input.is_object(), "tool input must be an object");
                    blocks.get_mut(&index).context("tool input without block")?["input"] = input;
                }
            }
            Some("message_stop") => {
                anyhow::ensure!(
                    saw_message_delta && open_blocks.is_empty(),
                    "message_stop before message completed"
                );
                anyhow::ensure!(
                    message
                        .as_ref()
                        .and_then(|m| m["stop_reason"].as_str())
                        .is_some(),
                    "missing terminal stop_reason"
                );
                stopped = true;
            }
            Some("error") => anyhow::bail!("upstream stream error"),
            Some("ping") => {}
            _ => anyhow::bail!("unsupported event; use transcript recording"),
        }
    }
    anyhow::ensure!(stopped, "stream did not reach message_stop");
    let mut message = message.context("missing message_start")?;
    message["content"] = Value::Array(blocks.into_values().collect());
    Ok(message)
}

fn append(block: &mut Value, field: &str, delta: &str) -> Result<()> {
    let value = block
        .get(field)
        .map_or(Some(""), Value::as_str)
        .context("delta target must be a string")?;
    block[field] = Value::String(format!("{value}{delta}"));
    Ok(())
}
