use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A recorded SSE event, without provider-specific interpretation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TranscriptEvent {
    pub event: Option<String>,
    pub data: String,
}

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

pub fn render_event(event: &TranscriptEvent) -> String {
    let mut text = String::new();
    if let Some(name) = &event.event {
        text.push_str("event: ");
        text.push_str(name);
        text.push('\n');
    }
    for line in event.data.split('\n') {
        text.push_str("data: ");
        text.push_str(line);
        text.push('\n');
    }
    text.push('\n');
    text
}
